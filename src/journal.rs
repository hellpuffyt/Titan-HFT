//! A line-oriented, human-readable journal that records every command, the events it
//! produced, and the final book fingerprint — and can be replayed to prove the engine
//! is deterministic.
//!
//! ```text
//! titan-journal v1
//! config tick=1 min=1 max=1000 max_orders=1024
//! > L S 1 100 5
//! < ACK 1
//! > L B 2 100 3
//! < ACK 2
//! < TRADE 2 1 100 3
//! hash 9e3c1d8a7f6b5e40
//! ```
//!
//! `>` lines are inputs, `<` lines are the outputs of the preceding input. Blank lines
//! and lines starting with `#` are ignored. Plain command files (used by `titan run`)
//! contain only command lines, without the `>` prefix.

use core::fmt::Write as _;

use crate::book::BookConfig;
use crate::engine::MatchingEngine;
use crate::types::{CancelReason, Command, Event, RejectReason, Side};

/// Journal format header.
pub const HEADER: &str = "titan-journal v1";

/// A parse or semantic error, with the 1-based line it came from.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {msg}")]
pub struct JournalError {
    /// 1-based line number (0 = whole file).
    pub line: usize,
    /// What went wrong.
    pub msg: String,
}

fn err(line: usize, msg: impl Into<String>) -> JournalError {
    JournalError {
        line,
        msg: msg.into(),
    }
}

/// One recorded command with its events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The input.
    pub command: Command,
    /// The outputs it produced.
    pub events: Vec<Event>,
    /// Line number of the `>` line (for diagnostics; 0 if not parsed from text).
    pub line: usize,
}

/// A complete recorded session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    /// Book configuration used.
    pub config: BookConfig,
    /// Inputs and outputs in order.
    pub entries: Vec<Entry>,
    /// `OrderBook::state_hash` after the last command.
    pub final_hash: u64,
}

/// Where a replay diverged from the recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// A command produced different events.
    Events {
        /// 0-based command index.
        index: usize,
        /// Journal line of the command.
        line: usize,
        /// The command.
        command: Command,
        /// Recorded events.
        expected: Vec<Event>,
        /// Replayed events.
        actual: Vec<Event>,
    },
    /// All events matched but the final book differs.
    FinalHash {
        /// Recorded hash.
        expected: u64,
        /// Replayed hash.
        actual: u64,
    },
}

impl core::fmt::Display for Divergence {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Divergence::Events {
                index,
                line,
                command,
                expected,
                actual,
            } => {
                writeln!(f, "divergence at command #{index} (line {line}): {command}")?;
                writeln!(f, "  expected:")?;
                for e in expected {
                    writeln!(f, "    < {e}")?;
                }
                writeln!(f, "  actual:")?;
                for e in actual {
                    writeln!(f, "    < {e}")?;
                }
                Ok(())
            }
            Divergence::FinalHash { expected, actual } => write!(
                f,
                "final book hash mismatch: expected {expected:016x}, actual {actual:016x}"
            ),
        }
    }
}

fn num(tok: Option<&str>, line: usize, what: &str) -> Result<u64, JournalError> {
    let t = tok.ok_or_else(|| err(line, format!("missing {what}")))?;
    t.parse::<u64>().map_err(|_| {
        err(
            line,
            format!("{what} '{t}' is not an integer in 0..={}", u64::MAX),
        )
    })
}

fn side(tok: Option<&str>, line: usize) -> Result<Side, JournalError> {
    match tok {
        Some("B") => Ok(Side::Buy),
        Some("S") => Ok(Side::Sell),
        Some(t) => Err(err(line, format!("side must be B or S, got '{t}'"))),
        None => Err(err(line, "missing side")),
    }
}

fn no_more<'a>(mut it: impl Iterator<Item = &'a str>, line: usize) -> Result<(), JournalError> {
    match it.next() {
        Some(t) => Err(err(line, format!("unexpected trailing token '{t}'"))),
        None => Ok(()),
    }
}

/// Parses `L B <id> <price> <qty>`, `M S <id> <qty>`, or `C <id>`.
pub fn parse_command(text: &str, line: usize) -> Result<Command, JournalError> {
    let mut it = text.split_whitespace();
    let cmd = match it.next() {
        Some("L") => Command::Limit {
            side: side(it.next(), line)?,
            id: num(it.next(), line, "id")?,
            price: num(it.next(), line, "price")?,
            qty: num(it.next(), line, "qty")?,
        },
        Some("M") => Command::Market {
            side: side(it.next(), line)?,
            id: num(it.next(), line, "id")?,
            qty: num(it.next(), line, "qty")?,
        },
        Some("C") => Command::Cancel {
            id: num(it.next(), line, "id")?,
        },
        Some(t) => {
            return Err(err(
                line,
                format!("unknown command '{t}' (expected L, M or C)"),
            ));
        }
        None => return Err(err(line, "empty command")),
    };
    no_more(it, line)?;
    Ok(cmd)
}

/// Parses one event line body (without the `<` prefix).
pub fn parse_event(text: &str, line: usize) -> Result<Event, JournalError> {
    let mut it = text.split_whitespace();
    let ev = match it.next() {
        Some("ACK") => Event::Accepted {
            id: num(it.next(), line, "id")?,
        },
        Some("REJ") => {
            let id = num(it.next(), line, "id")?;
            let r = it
                .next()
                .ok_or_else(|| err(line, "missing reject reason"))?;
            let reason = RejectReason::parse(r)
                .ok_or_else(|| err(line, format!("unknown reject reason '{r}'")))?;
            Event::Rejected { id, reason }
        }
        Some("TRADE") => Event::Trade {
            taker: num(it.next(), line, "taker")?,
            maker: num(it.next(), line, "maker")?,
            price: num(it.next(), line, "price")?,
            qty: num(it.next(), line, "qty")?,
        },
        Some("CXL") => {
            let id = num(it.next(), line, "id")?;
            let qty = num(it.next(), line, "qty")?;
            let reason = match it.next() {
                Some("user") => CancelReason::User,
                Some("ioc") => CancelReason::Ioc,
                Some(t) => return Err(err(line, format!("unknown cancel reason '{t}'"))),
                None => return Err(err(line, "missing cancel reason")),
            };
            Event::Cancelled { id, qty, reason }
        }
        Some(t) => return Err(err(line, format!("unknown event '{t}'"))),
        None => return Err(err(line, "empty event")),
    };
    no_more(it, line)?;
    Ok(ev)
}

fn significant(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
}

/// Parses a plain command file (one command per line).
pub fn parse_commands(text: &str) -> Result<Vec<Command>, JournalError> {
    significant(text)
        .map(|(n, l)| parse_command(l, n))
        .collect()
}

fn parse_config(text: &str, line: usize) -> Result<BookConfig, JournalError> {
    let mut cfg = BookConfig {
        tick: 0,
        min_price: 0,
        max_price: 0,
        max_orders: 0,
    };
    let mut seen = 0u8;
    for kv in text.split_whitespace() {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| err(line, format!("config item '{kv}' is not key=value")))?;
        let n = num(Some(v), line, k)?;
        match k {
            "tick" => (cfg.tick, seen) = (n, seen | 1),
            "min" => (cfg.min_price, seen) = (n, seen | 2),
            "max" => (cfg.max_price, seen) = (n, seen | 4),
            "max_orders" => {
                cfg.max_orders = u32::try_from(n)
                    .map_err(|_| err(line, format!("max_orders {n} does not fit in u32")))?;
                seen |= 8;
            }
            _ => return Err(err(line, format!("unknown config key '{k}'"))),
        }
    }
    if seen != 15 {
        return Err(err(line, "config needs tick, min, max and max_orders"));
    }
    cfg.validate()
        .map_err(|e| err(line, format!("invalid config: {e}")))?;
    Ok(cfg)
}

impl Journal {
    /// Runs `commands` through a fresh engine and records everything.
    pub fn record(
        config: BookConfig,
        commands: &[Command],
    ) -> Result<Self, crate::book::ConfigError> {
        let mut engine = MatchingEngine::new(config)?;
        let entries = commands
            .iter()
            .map(|&command| {
                let mut events = Vec::new();
                engine.process(command, &mut events);
                Entry {
                    command,
                    events,
                    line: 0,
                }
            })
            .collect();
        Ok(Self {
            config,
            entries,
            final_hash: engine.book().state_hash(),
        })
    }

    /// Serializes to the text format.
    pub fn to_text(&self) -> String {
        let c = &self.config;
        let mut s = String::new();
        // Writing into a String cannot fail.
        let _ = writeln!(s, "{HEADER}");
        let _ = writeln!(
            s,
            "config tick={} min={} max={} max_orders={}",
            c.tick, c.min_price, c.max_price, c.max_orders
        );
        for e in &self.entries {
            let _ = writeln!(s, "> {}", e.command);
            for ev in &e.events {
                let _ = writeln!(s, "< {ev}");
            }
        }
        let _ = writeln!(s, "hash {:016x}", self.final_hash);
        s
    }

    /// Parses the text format, reporting the first malformed line.
    pub fn parse(text: &str) -> Result<Self, JournalError> {
        let mut lines = significant(text);
        match lines.next() {
            Some((_, HEADER)) => {}
            Some((n, other)) => {
                return Err(err(n, format!("expected header '{HEADER}', got '{other}'")));
            }
            None => return Err(err(0, "empty journal")),
        }
        let config = match lines.next() {
            Some((n, l)) => match l.strip_prefix("config ") {
                Some(rest) => parse_config(rest, n)?,
                None => return Err(err(n, "expected 'config ...' line")),
            },
            None => return Err(err(0, "missing config line")),
        };
        let mut entries: Vec<Entry> = Vec::new();
        let mut final_hash = None;
        for (n, l) in lines {
            if final_hash.is_some() {
                return Err(err(n, "content after final 'hash' line"));
            }
            if let Some(rest) = l.strip_prefix('>') {
                entries.push(Entry {
                    command: parse_command(rest, n)?,
                    events: Vec::new(),
                    line: n,
                });
            } else if let Some(rest) = l.strip_prefix('<') {
                let ev = parse_event(rest, n)?;
                entries
                    .last_mut()
                    .ok_or_else(|| err(n, "event before any command"))?
                    .events
                    .push(ev);
            } else if let Some(h) = l.strip_prefix("hash ") {
                final_hash = Some(
                    u64::from_str_radix(h.trim(), 16)
                        .map_err(|_| err(n, format!("hash '{h}' is not 64-bit hex")))?,
                );
            } else {
                return Err(err(n, format!("unrecognized line '{l}'")));
            }
        }
        let final_hash = final_hash.ok_or_else(|| err(0, "missing final 'hash' line"))?;
        Ok(Self {
            config,
            entries,
            final_hash,
        })
    }

    /// Re-executes the recorded commands and checks every event and the final hash.
    pub fn verify(&self) -> Result<u64, Divergence> {
        // Config was validated when parsed or recorded; a failure here means the
        // struct was built by hand with a bad config, which we report as a hash diff.
        let Ok(mut engine) = MatchingEngine::new(self.config) else {
            return Err(Divergence::FinalHash {
                expected: self.final_hash,
                actual: 0,
            });
        };
        let mut actual = Vec::new();
        for (index, e) in self.entries.iter().enumerate() {
            actual.clear();
            engine.process(e.command, &mut actual);
            if actual != e.events {
                return Err(Divergence::Events {
                    index,
                    line: e.line,
                    command: e.command,
                    expected: e.events.clone(),
                    actual,
                });
            }
        }
        let h = engine.book().state_hash();
        if h != self.final_hash {
            return Err(Divergence::FinalHash {
                expected: self.final_hash,
                actual: h,
            });
        }
        Ok(h)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn cfg() -> BookConfig {
        BookConfig {
            tick: 1,
            min_price: 1,
            max_price: 1000,
            max_orders: 64,
        }
    }

    #[test]
    fn round_trip() {
        let cmds = parse_commands("L S 1 100 5\nL B 2 100 3\n# c\n\nM B 3 9\nC 1\nC 1\n").unwrap();
        let j = Journal::record(cfg(), &cmds).unwrap();
        let text = j.to_text();
        let back = Journal::parse(&text).unwrap();
        assert_eq!(back.entries.len(), 5);
        assert_eq!(back.config, j.config);
        assert_eq!(back.final_hash, j.final_hash);
        assert!(back.verify().is_ok());
    }

    #[test]
    fn malformed_lines_report_line_numbers() {
        assert_eq!(parse_commands("L B 1 100\n").unwrap_err().line, 1);
        assert_eq!(parse_commands("\nX 1\n").unwrap_err().line, 2);
        assert!(
            parse_commands("L Q 1 1 1")
                .unwrap_err()
                .msg
                .contains("side")
        );
        assert!(parse_commands("L B -1 1 1").unwrap_err().msg.contains("id"));
        assert!(
            parse_commands("C 1 2")
                .unwrap_err()
                .msg
                .contains("trailing")
        );
        assert!(Journal::parse("nope").is_err());
        assert!(
            Journal::parse("titan-journal v1\nconfig tick=0 min=1 max=2 max_orders=1\nhash 0")
                .is_err()
        );
        assert!(
            Journal::parse(
                "titan-journal v1\nconfig tick=1 min=1 max=2 max_orders=1\n< ACK 1\nhash 0"
            )
            .is_err()
        );
        assert!(
            Journal::parse("titan-journal v1\nconfig tick=1 min=1 max=2 max_orders=1\n").is_err()
        );
        assert!(
            Journal::parse("titan-journal v1\nconfig tick=1 min=1 max=2 max_orders=1\nhash zz")
                .is_err()
        );
    }
}
