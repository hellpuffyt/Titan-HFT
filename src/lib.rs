//! # Titan-HFT
//!
//! An educational, research-grade order-matching engine built to show — with
//! measurements, not adjectives — how low-latency systems are put together:
//!
//! * [`spsc`] — a bounded, wait-free single-producer/single-consumer ring buffer with
//!   documented Acquire/Release reasoning and loom model tests.
//! * [`book`] — a price-ladder order book with intrusive FIFO levels, O(1) cancel, and
//!   all memory preallocated.
//! * [`engine`] — a deterministic price-time-priority matching engine.
//! * [`journal`] — a replayable text journal that proves determinism.
//! * [`workload`] — seeded synthetic order flow.
//! * [`pipeline`] — producer thread → SPSC queue → engine thread.
//!
//! This is **not** a trading system: there is no exchange connectivity, no risk
//! checks, no persistence guarantees. See the README.
//!
//! ```
//! use titan_hft::{book::BookConfig, engine::MatchingEngine, types::{Command, Event, Side}};
//!
//! let mut engine = MatchingEngine::new(BookConfig {
//!     tick: 1, min_price: 1, max_price: 1_000, max_orders: 1_024,
//! })?;
//! let mut events = Vec::new();
//! engine.process(Command::Limit { id: 1, side: Side::Sell, price: 100, qty: 5 }, &mut events);
//! engine.process(Command::Limit { id: 2, side: Side::Buy, price: 101, qty: 3 }, &mut events);
//! assert!(events.contains(&Event::Trade { taker: 2, maker: 1, price: 100, qty: 3 }));
//! assert_eq!(engine.book().best_ask(), Some((100, 2)));
//! # Ok::<(), titan_hft::book::ConfigError>(())
//! ```

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod book;
pub mod engine;
pub mod journal;
pub mod pipeline;
pub mod spsc;
mod sync;
pub mod types;
pub mod workload;
