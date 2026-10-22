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
        }
    }

    fn validate_limit(&self, id: OrderId, price: Price, qty: Qty) -> Result<(), RejectReason> {
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
}
