//! A two-thread pipeline: a producer thread feeds commands through the SPSC queue to
//! the engine thread. This is the shape of a typical single-writer matching engine:
//! one gateway thread, one sequencer/matcher thread, no locks between them.

use std::thread;

use crate::book::{BookConfig, ConfigError};
use crate::engine::MatchingEngine;
use crate::spsc::{self, CapacityError};
use crate::types::{Command, CountingSink};

/// Outcome of [`run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineReport {
    /// Commands the engine processed.
    pub commands: u64,
    /// Event counts.
    pub sink: CountingSink,
    /// Times the producer found the queue full and had to retry (back-pressure).
    pub full_spins: u64,
    /// Final book fingerprint.
    pub final_hash: u64,
}

/// Pipeline construction error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// Bad queue size.
    #[error(transparent)]
    Capacity(#[from] CapacityError),
    /// Bad book configuration.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A pipeline thread panicked.
    #[error("pipeline thread panicked")]
    Panicked,
}

/// Sends `commands` from a producer thread through a queue of `queue_capacity` to an
/// engine on the calling thread. Back-pressure is a spin-retry on a full queue; the
/// producer never drops a command.
pub fn run(
    config: BookConfig,
    commands: Vec<Command>,
    queue_capacity: usize,
) -> Result<PipelineReport, PipelineError> {
    let mut engine = MatchingEngine::new(config)?;
    // `None` is the end-of-stream marker.
    let (mut tx, mut rx) = spsc::channel::<Option<Command>>(queue_capacity)?;
    let producer = thread::spawn(move || {
        let mut full_spins = 0u64;
        for cmd in commands.into_iter().map(Some).chain(core::iter::once(None)) {
            let mut item = cmd;
            loop {
                match tx.try_push(item) {
                    Ok(()) => break,
                    Err(back) => {
                        item = back;
                        full_spins += 1;
                        std::hint::spin_loop();
                    }
                }
            }
        }
        full_spins
    });
    let mut sink = CountingSink::default();
    loop {
        match rx.try_pop() {
            Some(Some(cmd)) => engine.process(cmd, &mut sink),
            Some(None) => break,
            None => std::hint::spin_loop(),
        }
    }
    let full_spins = producer.join().map_err(|_| PipelineError::Panicked)?;
    Ok(PipelineReport {
        commands: engine.commands_processed(),
        sink,
        full_spins,
        final_hash: engine.book().state_hash(),
    })
}
