//! The deterministic matching engine.
//!
//! `MatchingEngine::process` is a pure state transition: given the same configuration
//! and the same command sequence it emits the same events and reaches the same book
//! state, on any machine. It reads no clock, no randomness, and no environment.
//!
//! Matching rules (documented in the README):
//! * Price-time priority: better price first; at equal price, earlier arrival first.
//! * Trades execute at the **resting (maker) order's price**.
//! * Limit orders match while they cross, then rest the remainder.
//! * Market orders are immediate-or-cancel: they match against whatever is on the
//!   opposite side and the unfilled remainder is cancelled (`CXL id qty ioc`).
//! * No self-trade prevention: the engine has no notion of account/owner.
//! * Validation happens before any state change; a rejected command changes nothing.

use crate::book::{BookConfig, ConfigError, OrderBook};
use crate::types::{
    CancelReason, Command, Event, EventSink, MAX_ORDER_QTY, OrderId, Price, Qty, RejectReason, Side,
};

/// Engine configuration. Currently just the book layout.
pub type EngineConfig = BookConfig;

/// Single-threaded matching engine owning one order book.
#[derive(Debug)]
pub struct MatchingEngine {
    book: OrderBook,
    commands: u64,
}

impl MatchingEngine {
    /// Creates an engine with preallocated storage.
    pub fn new(cfg: EngineConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            book: OrderBook::new(cfg)?,
            commands: 0,
        })
    }

    /// Read-only access to the book.
    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    /// Number of commands processed so far (accepted or rejected).
    pub fn commands_processed(&self) -> u64 {
        self.commands
    }

    /// Applies one command, emitting its events to `sink` in order.
    #[inline]
    pub fn process<S: EventSink>(&mut self, cmd: Command, sink: &mut S) {
        self.commands += 1;
        match cmd {
            Command::Limit {
                id,
                side,
                price,
                qty,
            } => self.limit(id, side, price, qty, sink),
            Command::Market { id, side, qty } => self.market(id, side, qty, sink),
            Command::Cancel { id } => match self.book.remove(id) {
                Some(qty) => sink.emit(Event::Cancelled {
                    id,
                    qty,
                    reason: CancelReason::User,
                }),
                None => sink.emit(Event::Rejected {
                    id,
                    reason: RejectReason::UnknownOrder,
                }),
            },
            Command::Replace { id, price, qty } => self.replace(id, price, qty, sink),
        }
    }

    fn validate_price_qty(&self, price: Price, qty: Qty) -> Result<(), RejectReason> {
        let cfg = self.book.config();
        if qty == 0 {
            return Err(RejectReason::ZeroQty);
        }
        if qty > MAX_ORDER_QTY {
            return Err(RejectReason::QtyTooLarge);
        }
        if price % cfg.tick != 0 {
            return Err(RejectReason::BadTick);
        }
        if price < cfg.min_price || price > cfg.max_price {
            return Err(RejectReason::PriceOutOfBand);
        }
        Ok(())
    }

    /// Replace semantics are documented on [`Command::Replace`]. Validation order:
    /// `zero_qty`, `qty_too_large`, `bad_tick`, `price_out_of_band`, `unknown_order`.
    /// A rejected replace leaves the original order untouched.
    fn replace<S: EventSink>(&mut self, id: OrderId, price: Price, qty: Qty, sink: &mut S) {
        let current = match self.validate_price_qty(price, qty) {
            Err(reason) => Err(reason),
            Ok(()) => self.book.order(id).ok_or(RejectReason::UnknownOrder),
        };
        let current = match current {
            Ok(c) => c,
            Err(reason) => {
                sink.emit(Event::Rejected { id, reason });
                return;
            }
        };
        if price == current.price && qty <= current.qty && self.book.reduce(id, qty) {
            sink.emit(Event::Replaced {
                id,
                price,
                qty,
                prev_qty: current.qty,
                kept_priority: true,
            });
            return;
        }
        // Lose priority: unlink (frees a node, so re-inserting cannot hit BookFull),
        // then behave like a new limit order with the same id and side.
        self.book.remove(id);
        sink.emit(Event::Replaced {
            id,
            price,
            qty,
            prev_qty: current.qty,
            kept_priority: false,
        });
        let level = self.book.level_of(price);
        let remaining = self.sweep(id, current.side, Some(level), qty, sink);
        if remaining > 0
            && self
                .book
                .insert(id, current.side, level, remaining)
                .is_err()
        {
            // Unreachable: a node was freed just above. Kept for event consistency.
            sink.emit(Event::Cancelled {
                id,
                qty: remaining,
                reason: CancelReason::Ioc,
            });
        }
    }

    fn validate_limit(&self, id: OrderId, price: Price, qty: Qty) -> Result<(), RejectReason> {
        self.validate_price_qty(price, qty)?;
        if self.book.contains(id) {
            return Err(RejectReason::DuplicateId);
        }
        // Conservative: require a free node up front so a limit order is never
        // half-executed and then unable to rest. This can reject an order that would
        // have filled completely; see docs/LIMITATIONS.md.
        if !self.book.has_capacity() {
            return Err(RejectReason::BookFull);
        }
        Ok(())
    }

    fn limit<S: EventSink>(
        &mut self,
        id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
        sink: &mut S,
    ) {
        if let Err(reason) = self.validate_limit(id, price, qty) {
            sink.emit(Event::Rejected { id, reason });
            return;
        }
        sink.emit(Event::Accepted { id });
        let level = self.book.level_of(price);
        let remaining = self.sweep(id, side, Some(level), qty, sink);
        if remaining > 0 {
            // Unreachable in practice: capacity was checked in validation and matching
            // only frees nodes. Handled anyway so the event stream stays consistent.
            if self.book.insert(id, side, level, remaining).is_err() {
                sink.emit(Event::Cancelled {
                    id,
                    qty: remaining,
                    reason: CancelReason::Ioc,
                });
            }
        }
    }

    fn market<S: EventSink>(&mut self, id: OrderId, side: Side, qty: Qty, sink: &mut S) {
        if qty == 0 {
            sink.emit(Event::Rejected {
                id,
                reason: RejectReason::ZeroQty,
            });
            return;
        }
        if qty > MAX_ORDER_QTY {
            sink.emit(Event::Rejected {
                id,
                reason: RejectReason::QtyTooLarge,
            });
            return;
        }
        if self.book.contains(id) {
            sink.emit(Event::Rejected {
                id,
                reason: RejectReason::DuplicateId,
            });
            return;
        }
        sink.emit(Event::Accepted { id });
        let remaining = self.sweep(id, side, None, qty, sink);
        if remaining > 0 {
            sink.emit(Event::Cancelled {
                id,
                qty: remaining,
                reason: CancelReason::Ioc,
            });
        }
    }

    /// Matches an incoming order against the opposite side. `limit` is a ladder index
    /// (None = market). Returns the unfilled quantity.
    fn sweep<S: EventSink>(
        &mut self,
        taker: OrderId,
        side: Side,
        limit: Option<u32>,
        mut qty: Qty,
        sink: &mut S,
    ) -> Qty {
        let opp = side.opposite();
        while qty > 0 {
            let Some(front) = self.book.front(opp) else {
                break;
            };
            if let Some(lim) = limit {
                let crosses = match side {
                    Side::Buy => front.level <= lim,
                    Side::Sell => front.level >= lim,
                };
                if !crosses {
                    break;
                }
            }
            let traded = qty.min(front.qty);
            sink.emit(Event::Trade {
                taker,
                maker: front.id,
                price: self.book.price_of(front.level),
                qty: traded,
            });
            self.book.fill_front(opp, front, traded);
            qty -= traded;
        }
        qty
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::types::Side::{Buy, Sell};

    fn engine() -> MatchingEngine {
        MatchingEngine::new(BookConfig {
            tick: 1,
            min_price: 1,
            max_price: 1_000,
            max_orders: 16,
        })
        .unwrap()
    }

    fn run(e: &mut MatchingEngine, cmd: Command) -> Vec<Event> {
        let mut v = Vec::new();
        e.process(cmd, &mut v);
        v
    }

    fn lim(id: u64, side: Side, price: u64, qty: u64) -> Command {
        Command::Limit {
            id,
            side,
            price,
            qty,
        }
    }

    #[test]
    fn price_then_time_priority() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 101, 5));
        run(&mut e, lim(2, Sell, 100, 5)); // better price, later
        run(&mut e, lim(3, Sell, 100, 5)); // same price, later still
        let ev = run(&mut e, lim(10, Buy, 101, 12));
        assert_eq!(
            ev,
            vec![
                Event::Accepted { id: 10 },
                Event::Trade {
                    taker: 10,
                    maker: 2,
                    price: 100,
                    qty: 5
                },
                Event::Trade {
                    taker: 10,
                    maker: 3,
                    price: 100,
                    qty: 5
                },
                Event::Trade {
                    taker: 10,
                    maker: 1,
                    price: 101,
                    qty: 2
                },
            ]
        );
        assert_eq!(e.book().order(1).unwrap().qty, 3);
        assert_eq!(e.book().best_ask(), Some((101, 3)));
        assert_eq!(e.book().best_bid(), None);
    }

    #[test]
    fn limit_rests_remainder_at_its_price() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 2));
        let ev = run(&mut e, lim(2, Buy, 105, 5));
        assert_eq!(ev.len(), 2);
        assert_eq!(e.book().best_bid(), Some((105, 3)));
        assert_eq!(e.book().best_ask(), None);
    }

    #[test]
    fn non_crossing_limit_just_rests() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 2));
        let ev = run(&mut e, lim(2, Buy, 99, 5));
        assert_eq!(ev, vec![Event::Accepted { id: 2 }]);
        assert_eq!(e.book().best_bid(), Some((99, 5)));
    }

    #[test]
    fn market_is_ioc() {
        let mut e = engine();
        run(&mut e, lim(1, Buy, 50, 3));
        let ev = run(
            &mut e,
            Command::Market {
                id: 2,
                side: Sell,
                qty: 10,
            },
        );
        assert_eq!(
            ev,
            vec![
                Event::Accepted { id: 2 },
                Event::Trade {
                    taker: 2,
                    maker: 1,
                    price: 50,
                    qty: 3
                },
                Event::Cancelled {
                    id: 2,
                    qty: 7,
                    reason: CancelReason::Ioc
                },
            ]
        );
        assert!(e.book().is_empty());
        let ev = run(
            &mut e,
            Command::Market {
                id: 3,
                side: Buy,
                qty: 1,
            },
        );
        assert_eq!(
            ev[1],
            Event::Cancelled {
                id: 3,
                qty: 1,
                reason: CancelReason::Ioc
            }
        );
    }

    #[test]
    fn rejections_change_nothing() {
        let mut e = engine();
        run(&mut e, lim(1, Buy, 50, 3));
        let h = e.book().state_hash();
        for (cmd, reason) in [
            (lim(2, Buy, 50, 0), RejectReason::ZeroQty),
            (
                lim(2, Buy, 50, MAX_ORDER_QTY + 1),
                RejectReason::QtyTooLarge,
            ),
            (
                Command::Market {
                    id: 6,
                    side: Sell,
                    qty: u64::MAX,
                },
                RejectReason::QtyTooLarge,
            ),
            (lim(2, Buy, 0, 1), RejectReason::PriceOutOfBand),
            (lim(2, Buy, 1_001, 1), RejectReason::PriceOutOfBand),
            (lim(1, Sell, 60, 1), RejectReason::DuplicateId),
            (Command::Cancel { id: 99 }, RejectReason::UnknownOrder),
            (
                Command::Market {
                    id: 5,
                    side: Sell,
                    qty: 0,
                },
                RejectReason::ZeroQty,
            ),
            (
                Command::Market {
                    id: 1,
                    side: Sell,
                    qty: 1,
                },
                RejectReason::DuplicateId,
            ),
        ] {
            let ev = run(&mut e, cmd);
            assert_eq!(
                ev,
                vec![Event::Rejected {
                    id: cmd.id(),
                    reason
                }],
                "{cmd}"
            );
            assert_eq!(e.book().state_hash(), h);
        }
    }

    #[test]
    fn bad_tick_rejected() {
        let mut e = MatchingEngine::new(BookConfig {
            tick: 5,
            min_price: 5,
            max_price: 500,
            max_orders: 4,
        })
        .unwrap();
        assert_eq!(
            run(&mut e, lim(1, Buy, 12, 1)),
            vec![Event::Rejected {
                id: 1,
                reason: RejectReason::BadTick
            }]
        );
    }

    #[test]
    fn cancel_removes_and_reports_remaining() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 10));
        run(&mut e, lim(2, Buy, 100, 4));
        assert_eq!(
            run(&mut e, Command::Cancel { id: 1 }),
            vec![Event::Cancelled {
                id: 1,
                qty: 6,
                reason: CancelReason::User
            }]
        );
        assert_eq!(
            run(&mut e, Command::Cancel { id: 1 }),
            vec![Event::Rejected {
                id: 1,
                reason: RejectReason::UnknownOrder
            }]
        );
    }

    #[test]
    fn filled_order_id_can_be_reused() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 1));
        run(&mut e, lim(2, Buy, 100, 1));
        assert!(e.book().is_empty());
        assert_eq!(
            run(&mut e, lim(1, Sell, 100, 1)),
            vec![Event::Accepted { id: 1 }]
        );
    }

    #[test]
    fn book_full_rejects_new_limits() {
        let mut e = MatchingEngine::new(BookConfig {
            tick: 1,
            min_price: 1,
            max_price: 10,
            max_orders: 2,
        })
        .unwrap();
        run(&mut e, lim(1, Buy, 1, 1));
        run(&mut e, lim(2, Buy, 1, 1));
        assert_eq!(
            run(&mut e, lim(3, Buy, 1, 1)),
            vec![Event::Rejected {
                id: 3,
                reason: RejectReason::BookFull
            }]
        );
        // Market orders never rest, so they still work when the pool is full.
        assert_eq!(
            run(
                &mut e,
                Command::Market {
                    id: 4,
                    side: Sell,
                    qty: 1
                }
            )
            .len(),
            2
        );
    }

    fn rpl(id: u64, price: u64, qty: u64) -> Command {
        Command::Replace { id, price, qty }
    }

    fn resting_ids(e: &MatchingEngine) -> Vec<u64> {
        let mut v = Vec::new();
        e.book().for_each_order(|o| v.push(o.id));
        v
    }

    #[test]
    fn replace_reduce_same_price_keeps_priority() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 10));
        run(&mut e, lim(2, Sell, 100, 10));
        assert_eq!(
            run(&mut e, rpl(1, 100, 4)),
            vec![Event::Replaced {
                id: 1,
                price: 100,
                qty: 4,
                prev_qty: 10,
                kept_priority: true
            }]
        );
        assert_eq!(resting_ids(&e), vec![1, 2], "order 1 is still first");
        assert_eq!(e.book().best_ask(), Some((100, 14)));
        // The front order still trades first.
        let ev = run(&mut e, lim(9, Buy, 100, 5));
        assert_eq!(
            ev[1],
            Event::Trade {
                taker: 9,
                maker: 1,
                price: 100,
                qty: 4
            }
        );
        assert_eq!(
            ev[2],
            Event::Trade {
                taker: 9,
                maker: 2,
                price: 100,
                qty: 1
            }
        );
    }

    #[test]
    fn replace_same_price_same_qty_is_a_priority_keeping_no_op() {
        let mut e = engine();
        run(&mut e, lim(1, Buy, 50, 3));
        run(&mut e, lim(2, Buy, 50, 3));
        let h = e.book().state_hash();
        assert_eq!(
            run(&mut e, rpl(1, 50, 3)),
            vec![Event::Replaced {
                id: 1,
                price: 50,
                qty: 3,
                prev_qty: 3,
                kept_priority: true
            }]
        );
        assert_eq!(e.book().state_hash(), h);
    }

    #[test]
    fn replace_qty_increase_loses_priority() {
        let mut e = engine();
        run(&mut e, lim(1, Buy, 50, 3));
        run(&mut e, lim(2, Buy, 50, 3));
        assert_eq!(
            run(&mut e, rpl(1, 50, 5)),
            vec![Event::Replaced {
                id: 1,
                price: 50,
                qty: 5,
                prev_qty: 3,
                kept_priority: false
            }]
        );
        assert_eq!(resting_ids(&e), vec![2, 1], "order 1 moved to the back");
        assert_eq!(e.book().best_bid(), Some((50, 8)));
    }

    #[test]
    fn replace_price_change_loses_priority_even_when_reducing() {
        let mut e = engine();
        run(&mut e, lim(1, Buy, 50, 5));
        run(&mut e, lim(2, Buy, 51, 5));
        // Move order 1 up to 51 with a smaller quantity: it goes behind order 2.
        assert_eq!(
            run(&mut e, rpl(1, 51, 2)),
            vec![Event::Replaced {
                id: 1,
                price: 51,
                qty: 2,
                prev_qty: 5,
                kept_priority: false
            }]
        );
        assert_eq!(resting_ids(&e), vec![2, 1]);
        assert_eq!(e.book().order(1).map(|o| (o.price, o.qty)), Some((51, 2)));
        assert_eq!(e.book().depth(Side::Buy), vec![(51, 7, 2)]);
    }

    #[test]
    fn replace_into_a_cross_matches_immediately_as_taker() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 105, 3));
        run(&mut e, lim(2, Sell, 106, 3));
        run(&mut e, lim(3, Buy, 100, 10));
        let ev = run(&mut e, rpl(3, 105, 10));
        assert_eq!(
            ev,
            vec![
                Event::Replaced {
                    id: 3,
                    price: 105,
                    qty: 10,
                    prev_qty: 10,
                    kept_priority: false
                },
                Event::Trade {
                    taker: 3,
                    maker: 1,
                    price: 105,
                    qty: 3
                },
            ]
        );
        // Remainder rests at the new price; the 106 ask is not reachable at 105.
        assert_eq!(e.book().best_bid(), Some((105, 7)));
        assert_eq!(e.book().best_ask(), Some((106, 3)));
    }

    #[test]
    fn replace_that_fully_fills_leaves_nothing_resting() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 5));
        run(&mut e, lim(2, Buy, 90, 2));
        let ev = run(&mut e, rpl(2, 100, 2));
        assert_eq!(ev.len(), 2);
        assert_eq!(
            ev[1],
            Event::Trade {
                taker: 2,
                maker: 1,
                price: 100,
                qty: 2
            }
        );
        assert!(!e.book().contains(2));
        assert_eq!(e.book().best_ask(), Some((100, 3)));
        // Filled id is no longer replaceable.
        assert_eq!(
            run(&mut e, rpl(2, 95, 1)),
            vec![Event::Rejected {
                id: 2,
                reason: RejectReason::UnknownOrder
            }]
        );
    }

    #[test]
    fn replace_rejections_leave_the_order_untouched() {
        let mut e = MatchingEngine::new(BookConfig {
            tick: 5,
            min_price: 5,
            max_price: 500,
            max_orders: 4,
        })
        .unwrap();
        run(&mut e, lim(1, Buy, 100, 4));
        run(&mut e, lim(2, Buy, 100, 4));
        let h = e.book().state_hash();
        for (cmd, reason) in [
            (rpl(1, 100, 0), RejectReason::ZeroQty),
            (rpl(1, 100, MAX_ORDER_QTY + 1), RejectReason::QtyTooLarge),
            (rpl(1, 102, 4), RejectReason::BadTick),
            (rpl(1, 505, 4), RejectReason::PriceOutOfBand),
            (rpl(1, 0, 4), RejectReason::PriceOutOfBand),
            (rpl(77, 100, 1), RejectReason::UnknownOrder),
        ] {
            assert_eq!(
                run(&mut e, cmd),
                vec![Event::Rejected {
                    id: cmd.id(),
                    reason
                }],
                "{cmd}"
            );
            assert_eq!(e.book().state_hash(), h, "{cmd} changed the book");
        }
        assert_eq!(resting_ids(&e), vec![1, 2]);
    }

    #[test]
    fn replace_of_cancelled_order_is_unknown() {
        let mut e = engine();
        run(&mut e, lim(1, Sell, 100, 5));
        run(&mut e, Command::Cancel { id: 1 });
        assert_eq!(
            run(&mut e, rpl(1, 100, 5)),
            vec![Event::Rejected {
                id: 1,
                reason: RejectReason::UnknownOrder
            }]
        );
    }

    #[test]
    fn replace_requeue_works_when_the_pool_is_full() {
        let mut e = MatchingEngine::new(BookConfig {
            tick: 1,
            min_price: 1,
            max_price: 10,
            max_orders: 2,
        })
        .unwrap();
        run(&mut e, lim(1, Buy, 1, 1));
        run(&mut e, lim(2, Buy, 2, 1));
        assert!(!e.book().has_capacity());
        assert_eq!(
            run(&mut e, rpl(1, 3, 9)),
            vec![Event::Replaced {
                id: 1,
                price: 3,
                qty: 9,
                prev_qty: 1,
                kept_priority: false
            }]
        );
        assert_eq!(e.book().best_bid(), Some((3, 9)));
    }
}
