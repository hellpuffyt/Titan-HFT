//! Deterministic synthetic order flow for tests, fixtures, and benchmarks.
//!
//! The same [`WorkloadConfig`] (including `seed`) always yields the same command
//! sequence, on every platform, because it uses a self-contained SplitMix64 PRNG.

use crate::book::BookConfig;
use crate::types::{Command, OrderId, Price, Qty, Side};

/// SplitMix64: tiny, fast, statistically decent, and fully deterministic.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    /// Seeds the generator.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..n` (`n > 0`; slight modulo bias is irrelevant here).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

/// Which replaces [`WorkloadConfig::generate`] produces.
///
/// The generator only knows what it *sent*, not what has since filled, so the intent
/// is not always the outcome. A "reduce" of a partially filled order can turn out to
/// be an increase (and requeue), and replaces of filled or cancelled orders are
/// rejected. Benchmarks therefore classify by the engine's actual events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ReplaceMix {
    /// 50/50 same-price reduction vs. new price and quantity (the 0.1.x behavior).
    #[default]
    Mixed,
    /// Only same-price reductions (intended to keep priority).
    ReduceOnly,
    /// Only moves to a new price with a new quantity (intended to requeue).
    MoveOnly,
}

/// Parameters of the synthetic order flow.
///
/// Fields may be added in minor versions. Build it with `..Default::default()` so new
/// fields do not break your code:
///
/// ```
/// use titan_hft::workload::{ReplaceMix, WorkloadConfig};
///
/// let w = WorkloadConfig {
///     seed: 7,
///     commands: 1_000,
///     replace_pct: 30,
///     replace_mix: ReplaceMix::MoveOnly,
///     ..Default::default()
/// };
/// assert_eq!(w.generate().len(), 1_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadConfig {
    /// PRNG seed.
    pub seed: u64,
    /// Number of commands to generate.
    pub commands: usize,
    /// Reference price; limit prices are drawn around it.
    pub mid: Price,
    /// Tick size.
    pub tick: Price,
    /// Limit prices are `mid ± U[0, spread_ticks] * tick`.
    pub spread_ticks: u64,
    /// Order quantity is `U[1, max_qty]`.
    pub max_qty: Qty,
    /// Percentage of commands that cancel a previously sent order.
    pub cancel_pct: u64,
    /// Percentage of commands that replace a previously sent order: half reduce its
    /// quantity at the same price, half move it to a new price and quantity.
    /// Default 0, so seeded flows from earlier versions are reproduced exactly.
    pub replace_pct: u64,
    /// What kind of replaces to generate.
    pub replace_mix: ReplaceMix,
    /// Percentage of commands that are market orders.
    pub market_pct: u64,
}

impl Default for WorkloadConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            commands: 100_000,
            mid: 10_000,
            tick: 1,
            spread_ticks: 50,
            max_qty: 100,
            cancel_pct: 30,
            replace_pct: 0,
            replace_mix: ReplaceMix::Mixed,
            market_pct: 10,
        }
    }
}

impl WorkloadConfig {
    /// A book configuration whose band covers every price this workload generates.
    pub fn book_config(&self, max_orders: u32) -> BookConfig {
        let half = (self.spread_ticks + 1) * self.tick;
        let mid = self.mid - self.mid % self.tick;
        BookConfig {
            tick: self.tick,
            min_price: mid.saturating_sub(half).max(self.tick),
            max_price: mid + half,
            max_orders,
        }
    }

    /// Draws a limit price around `mid`. Consumes exactly two random numbers.
    fn limit_price(&self, rng: &mut SplitMix64, side: Side, mid: Price) -> Price {
        let off = rng.below(self.spread_ticks + 1) * self.tick;
        // Bias each side slightly away from mid so the book builds depth while still
        // crossing often enough to trade.
        match (side, rng.below(2) == 0) {
            (Side::Buy, true) | (Side::Sell, false) => mid.saturating_sub(off).max(self.tick),
            _ => mid + off,
        }
    }

    /// Generates the command sequence.
    pub fn generate(&self) -> Vec<Command> {
        let mut rng = SplitMix64::new(self.seed);
        let mut out = Vec::with_capacity(self.commands);
        // (id, side, price, qty as sent/last replaced). Entries may refer to orders
        // that have since filled; commands against them are rejected, as in real flow.
        let mut sent: Vec<(OrderId, Side, Price, Qty)> = Vec::new();
        let mut next_id: OrderId = 1;
        let mid = self.mid - self.mid % self.tick;
        for _ in 0..self.commands {
            let roll = rng.below(100);
            if roll < self.cancel_pct && !sent.is_empty() {
                let i = rng.below(sent.len() as u64) as usize;
                let (id, ..) = sent.swap_remove(i);
                out.push(Command::Cancel { id });
                continue;
            }
            if roll < self.cancel_pct + self.replace_pct && !sent.is_empty() {
                let i = rng.below(sent.len() as u64) as usize;
                let (id, side, old_price, old_qty) = sent[i];
                // The coin is always drawn so every mix consumes the same random
                // numbers as `Mixed`, which keeps `Mixed` flows byte-identical.
                let coin = rng.below(2) == 0;
                let reduce = match self.replace_mix {
                    ReplaceMix::Mixed => coin,
                    ReplaceMix::ReduceOnly => true,
                    ReplaceMix::MoveOnly => false,
                };
                let (price, qty) = if reduce {
                    (old_price, 1 + rng.below(old_qty))
                } else {
                    (
                        self.limit_price(&mut rng, side, mid),
                        1 + rng.below(self.max_qty),
                    )
                };
                sent[i] = (id, side, price, qty);
                out.push(Command::Replace { id, price, qty });
                continue;
            }
            let side = if rng.below(2) == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let qty = 1 + rng.below(self.max_qty);
            let id = next_id;
            next_id += 1;
            if roll < self.cancel_pct + self.replace_pct + self.market_pct {
                out.push(Command::Market { id, side, qty });
            } else {
                let price = self.limit_price(&mut rng, side, mid);
                out.push(Command::Limit {
                    id,
                    side,
                    price,
                    qty,
                });
                sent.push((id, side, price, qty));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_flow() {
        let c = WorkloadConfig {
            commands: 1_000,
            ..Default::default()
        };
        assert_eq!(c.generate(), c.generate());
        let other = WorkloadConfig { seed: 7, ..c };
        assert_ne!(c.generate(), other.generate());
    }

    /// Computed with the v0.1.0 generator (commit 1cfd689), before `replace_pct`.
    const GOLDEN_DEFAULT_FLOW_DIGEST: u64 = 0x194f_f068_ae7a_8f0b;

    #[test]
    fn replace_pct_zero_reproduces_the_original_flow() {
        // Golden digest of the default 100k flow, recorded before `replace_pct`
        // existed (it equals the flow behind tests/fixtures/seed42_5k.*).
        let flow = WorkloadConfig::default().generate();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for c in &flow {
            for b in c.to_string().bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        assert_eq!(h, GOLDEN_DEFAULT_FLOW_DIGEST);
    }

    #[test]
    fn replace_pct_generates_replaces() {
        let c = WorkloadConfig {
            commands: 10_000,
            replace_pct: 20,
            ..Default::default()
        };
        let n = c
            .generate()
            .iter()
            .filter(|c| matches!(c, Command::Replace { .. }))
            .count();
        assert!((1_500..2_500).contains(&n), "{n} replaces");
    }

    #[test]
    fn replace_mixes_produce_the_intended_kind() {
        let base = WorkloadConfig {
            commands: 5_000,
            replace_pct: 30,
            ..Default::default()
        };
        let sent_prices = |w: &WorkloadConfig| {
            let mut price_of = std::collections::HashMap::new();
            let (mut same, mut moved) = (0, 0);
            for c in w.generate() {
                match c {
                    Command::Limit { id, price, .. } => {
                        price_of.insert(id, price);
                    }
                    Command::Replace { id, price, .. } => {
                        if price_of.insert(id, price) == Some(price) {
                            same += 1;
                        } else {
                            moved += 1;
                        }
                    }
                    _ => {}
                }
            }
            (same, moved)
        };
        let (same, _) = sent_prices(&WorkloadConfig {
            replace_mix: ReplaceMix::ReduceOnly,
            ..base
        });
        let (_, moved) = sent_prices(&WorkloadConfig {
            replace_mix: ReplaceMix::MoveOnly,
            ..base
        });
        let (s, m) = sent_prices(&base);
        assert!(same > 1_000 && moved > 1_000 && s > 0 && m > 0);
        let (s_only, _) = sent_prices(&WorkloadConfig {
            replace_mix: ReplaceMix::MoveOnly,
            ..base
        });
        // A random new price can coincide with the old one; that must be rare.
        assert!(s_only * 10 < moved, "{s_only} vs {moved}");
    }

    #[test]
    fn prices_fit_the_suggested_band() {
        let c = WorkloadConfig {
            commands: 5_000,
            tick: 5,
            mid: 1_003,
            ..Default::default()
        };
        let b = c.book_config(16);
        assert!(b.validate().is_ok());
        for cmd in c.generate() {
            if let Command::Limit { price, .. } | Command::Replace { price, .. } = cmd {
                assert!(price >= b.min_price && price <= b.max_price && price % 5 == 0);
            }
        }
    }
}
