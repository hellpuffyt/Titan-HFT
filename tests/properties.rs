//! Property tests: the engine against a naive reference model, plus book invariants.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use proptest::prelude::*;
use titan_hft::book::BookConfig;
use titan_hft::engine::MatchingEngine;
use titan_hft::types::{CancelReason, Command, Event, MAX_ORDER_QTY, RejectReason, Side};

const CFG: BookConfig = BookConfig {
    tick: 1,
    min_price: 1,
    max_price: 20,
    max_orders: 12,
};

/// Obviously-correct (and slow) reference implementation of the documented rules.
#[derive(Default)]
struct Model {
    resting: Vec<(u64, u64, Side, u64, u64)>, // (arrival seq, id, side, price, qty)
    seq: u64,
}

impl Model {
    fn process(&mut self, cmd: Command) -> Vec<Event> {
        let mut out = Vec::new();
        let has = |m: &Model, id| m.resting.iter().any(|o| o.1 == id);
        match cmd {
            Command::Cancel { id } => match self.resting.iter().position(|o| o.1 == id) {
                Some(i) => {
                    let o = self.resting.remove(i);
                    out.push(Event::Cancelled {
                        id,
                        qty: o.4,
                        reason: CancelReason::User,
                    });
                }
                None => out.push(Event::Rejected {
                    id,
                    reason: RejectReason::UnknownOrder,
                }),
            },
            Command::Limit {
                id,
                side,
                price,
                qty,
            } => {
                let reason = if qty == 0 {
                    Some(RejectReason::ZeroQty)
                } else if qty > MAX_ORDER_QTY {
                    Some(RejectReason::QtyTooLarge)
                } else if !(CFG.min_price..=CFG.max_price).contains(&price) {
                    // tick = 1 here, so BadTick cannot occur (covered by engine unit tests).
                    Some(RejectReason::PriceOutOfBand)
                } else if has(self, id) {
                    Some(RejectReason::DuplicateId)
                } else if self.resting.len() >= CFG.max_orders as usize {
                    Some(RejectReason::BookFull)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    out.push(Event::Rejected { id, reason });
                    return out;
                }
                out.push(Event::Accepted { id });
                let left = self.sweep(id, side, Some(price), qty, &mut out);
                if left > 0 {
                    self.seq += 1;
                    self.resting.push((self.seq, id, side, price, left));
                }
            }
            Command::Market { id, side, qty } => {
                if qty == 0 {
                    out.push(Event::Rejected {
                        id,
                        reason: RejectReason::ZeroQty,
                    });
                    return out;
                }
                if qty > MAX_ORDER_QTY {
                    out.push(Event::Rejected {
                        id,
                        reason: RejectReason::QtyTooLarge,
                    });
                    return out;
                }
                if has(self, id) {
                    out.push(Event::Rejected {
                        id,
                        reason: RejectReason::DuplicateId,
                    });
                    return out;
                }
                out.push(Event::Accepted { id });
                let left = self.sweep(id, side, None, qty, &mut out);
                if left > 0 {
                    out.push(Event::Cancelled {
                        id,
                        qty: left,
                        reason: CancelReason::Ioc,
                    });
                }
            }
        }
        out
    }

    fn sweep(
        &mut self,
        taker: u64,
        side: Side,
        limit: Option<u64>,
        mut qty: u64,
        out: &mut Vec<Event>,
    ) -> u64 {
        while qty > 0 {
            // Best opposite order: best price, then earliest arrival.
            let best = self
                .resting
                .iter()
                .enumerate()
                .filter(|(_, o)| o.2 != side)
                .filter(|(_, o)| match (side, limit) {
                    (_, None) => true,
                    (Side::Buy, Some(l)) => o.3 <= l,
                    (Side::Sell, Some(l)) => o.3 >= l,
                })
                .min_by_key(|(_, o)| match side {
                    Side::Buy => (o.3, o.0),
                    Side::Sell => (u64::MAX - o.3, o.0),
                })
                .map(|(i, _)| i);
            let Some(i) = best else { break };
            let t = qty.min(self.resting[i].4);
            out.push(Event::Trade {
                taker,
                maker: self.resting[i].1,
                price: self.resting[i].3,
                qty: t,
            });
            self.resting[i].4 -= t;
            qty -= t;
            if self.resting[i].4 == 0 {
                self.resting.remove(i);
            }
        }
        qty
    }
}

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn qty(max: u64) -> impl Strategy<Value = u64> {
    // Mostly small; occasionally oversized to exercise QtyTooLarge.
    prop_oneof![20 => 0u64..max, 1 => Just(MAX_ORDER_QTY + 1)]
}

fn command() -> impl Strategy<Value = Command> {
    // Small id/price spaces force duplicates, unknown cancels, deep levels and crosses.
    prop_oneof![
        6 => (0u64..24, side(), 0u64..23, qty(8)).prop_map(|(id, side, price, qty)| Command::Limit { id, side, price, qty }),
        2 => (0u64..24, side(), qty(15)).prop_map(|(id, side, qty)| Command::Market { id, side, qty }),
        3 => (0u64..24).prop_map(|id| Command::Cancel { id }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn engine_matches_reference_model(cmds in prop::collection::vec(command(), 0..200)) {
        let mut engine = MatchingEngine::new(CFG).unwrap();
        let mut model = Model::default();
        let mut arrival: HashMap<u64, u64> = HashMap::new();
        let mut live_qty: i128 = 0;
        for (n, &cmd) in cmds.iter().enumerate() {
            let mut got = Vec::new();
            engine.process(cmd, &mut got);
            let want = model.process(cmd);
            prop_assert_eq!(&got, &want, "command #{} {}", n, cmd);

            // Quantity conservation: accepted qty enters, each trade removes it from
            // both sides, cancels remove it.
            for e in &got {
                match *e {
                    Event::Accepted { .. } => live_qty += match cmd {
                        Command::Limit { qty, .. } | Command::Market { qty, .. } => i128::from(qty),
                        Command::Cancel { .. } => 0,
                    },
                    Event::Trade { qty, .. } => live_qty -= 2 * i128::from(qty),
                    Event::Cancelled { qty, .. } => live_qty -= i128::from(qty),
                    Event::Rejected { .. } => {}
                }
            }
            let mut resting: i128 = 0;
            engine.book().for_each_order(|o| resting += i128::from(o.qty));
            prop_assert_eq!(resting, live_qty);

            // Never crossed after matching.
            if let (Some((b, _)), Some((a, _))) = (engine.book().best_bid(), engine.book().best_ask()) {
                prop_assert!(b < a, "crossed book: bid {} >= ask {}", b, a);
            }

            // FIFO within each level: arrival order strictly increasing.
            if let Command::Limit { id, .. } = cmd {
                if engine.book().contains(id) && !arrival.contains_key(&id) {
                    arrival.insert(id, n as u64);
                }
            }
            arrival.retain(|id, _| engine.book().contains(*id));
            let mut last: Option<(Side, u64, u64)> = None;
            let mut fifo_ok = true;
            engine.book().for_each_order(|o| {
                let seq = arrival[&o.id];
                if let Some((s, p, prev)) = last {
                    if s == o.side && p == o.price && prev >= seq {
                        fifo_ok = false;
                    }
                }
                last = Some((o.side, o.price, seq));
            });
            prop_assert!(fifo_ok, "FIFO violated at a price level");
        }
    }

    #[test]
    fn same_input_same_output(cmds in prop::collection::vec(command(), 0..200)) {
        let run = |cmds: &[Command]| {
            let mut e = MatchingEngine::new(CFG).unwrap();
            let mut ev = Vec::new();
            for &c in cmds { e.process(c, &mut ev); }
            (ev, e.book().state_hash())
        };
        prop_assert_eq!(run(&cmds), run(&cmds));
    }
}
