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

/// Parameters of the synthetic order flow.
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

    /// Generates the command sequence.
    pub fn generate(&self) -> Vec<Command> {
        let mut rng = SplitMix64::new(self.seed);
        let mut out = Vec::with_capacity(self.commands);
        let mut sent: Vec<OrderId> = Vec::new();
        let mut next_id: OrderId = 1;
        let mid = self.mid - self.mid % self.tick;
        for _ in 0..self.commands {
            let roll = rng.below(100);
            if roll < self.cancel_pct && !sent.is_empty() {
                let i = rng.below(sent.len() as u64) as usize;
                let id = sent.swap_remove(i);
                out.push(Command::Cancel { id });
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
            if roll < self.cancel_pct + self.market_pct {
                out.push(Command::Market { id, side, qty });
            } else {
                let off = rng.below(self.spread_ticks + 1) * self.tick;
                // Bias each side slightly away from mid so the book builds depth
                // while still crossing often enough to trade.
                let price = match (side, rng.below(2) == 0) {
                    (Side::Buy, true) | (Side::Sell, false) => {
                        mid.saturating_sub(off).max(self.tick)
                    }
                    _ => mid + off,
                };
                out.push(Command::Limit {
                    id,
                    side,
                    price,
                    qty,
                });
                sent.push(id);
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
            if let Command::Limit { price, .. } = cmd {
                assert!(price >= b.min_price && price <= b.max_price && price % 5 == 0);
            }
        }
    }
}
