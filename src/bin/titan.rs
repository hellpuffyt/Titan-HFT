//! `titan` — run order flow through the engine, record journals, replay them.

use std::error::Error;
use std::fs;
use std::process::ExitCode;

use titan_hft::book::BookConfig;
use titan_hft::engine::MatchingEngine;
use titan_hft::journal::{self, Journal};
use titan_hft::types::Side;
use titan_hft::workload::WorkloadConfig;

const USAGE: &str = "\
titan — educational matching engine

USAGE:
  titan run <commands.txt> [--journal <out>] [--quiet] [BOOK OPTIONS]
      Run a command file, print events, the top of book, and the state hash.
  titan replay <journal>
      Re-execute a journal and verify every event and the final book hash.
      Exit code 0 = identical, 1 = divergence, 2 = usage/parse error.
  titan gen [--seed N] [--count N] [--mid P] [--spread T] [--out <file>]
      Write a deterministic synthetic command file.

BOOK OPTIONS (defaults in brackets):
  --tick N [1]  --min N [1]  --max N [100000]  --max-orders N [65536]

COMMAND FILE LINES:
  L <B|S> <id> <price> <qty>   limit order
  M <B|S> <id> <qty>           market order (immediate-or-cancel)
  C <id>                       cancel
  # comment";

type Res<T> = Result<T, Box<dyn Error>>;

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: impl Iterator<Item = String>) -> Res<Self> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.peekable();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if name == "quiet" || name == "help" {
                    flags.push((name.to_string(), None));
                } else {
                    let v = it.next().ok_or(format!("--{name} needs a value"))?;
                    flags.push((name.to_string(), Some(v)));
                }
            } else {
                positional.push(a);
            }
        }
        Ok(Self { positional, flags })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }

    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Res<T> {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("--{name} expects a number, got '{v}'").into()),
        }
    }

    fn check_known(&self, known: &[&str]) -> Res<()> {
        for (k, _) in &self.flags {
            if !known.contains(&k.as_str()) {
                return Err(format!("unknown option --{k}").into());
            }
        }
        Ok(())
    }
}

fn book_config(a: &Args) -> Res<BookConfig> {
    let cfg = BookConfig {
        tick: a.num("tick", 1)?,
        min_price: a.num("min", 1)?,
        max_price: a.num("max", 100_000)?,
        max_orders: a.num("max-orders", 65_536)?,
    };
    cfg.validate()?;
    Ok(cfg)
}

fn read(path: &str) -> Res<String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}").into())
}

fn cmd_run(a: &Args) -> Res<ExitCode> {
    a.check_known(&["journal", "quiet", "tick", "min", "max", "max-orders"])?;
    let path = a.positional.get(1).ok_or("run needs a command file")?;
    let cfg = book_config(a)?;
    let commands = journal::parse_commands(&read(path)?).map_err(|e| format!("{path}: {e}"))?;
    let mut engine = MatchingEngine::new(cfg)?;
    let mut events = Vec::new();
    let quiet = a.has("quiet");
    for &c in &commands {
        events.clear();
        engine.process(c, &mut events);
        if !quiet {
            println!("> {c}");
            for e in &events {
                println!("< {e}");
            }
        }
    }
    let book = engine.book();
    println!(
        "--- {} commands, {} resting orders",
        commands.len(),
        book.len()
    );
    let asks = book.depth(Side::Sell);
    for (p, q, n) in asks.iter().take(5).rev() {
        println!("  ask {p:>8} {q:>8} ({n} orders)");
    }
    for (p, q, n) in book.depth(Side::Buy).iter().take(5) {
        println!("  bid {p:>8} {q:>8} ({n} orders)");
    }
    println!("hash {:016x}", book.state_hash());
    if let Some(out) = a.get("journal") {
        let j = Journal::record(cfg, &commands)?;
        fs::write(out, j.to_text()).map_err(|e| format!("cannot write {out}: {e}"))?;
        println!("journal written to {out}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_replay(a: &Args) -> Res<ExitCode> {
    a.check_known(&[])?;
    let path = a.positional.get(1).ok_or("replay needs a journal file")?;
    let j = Journal::parse(&read(path)?).map_err(|e| format!("{path}: {e}"))?;
    match j.verify() {
        Ok(h) => {
            let events: usize = j.entries.iter().map(|e| e.events.len()).sum();
            println!(
                "OK: {} commands, {events} events reproduced exactly; final hash {h:016x}",
                j.entries.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(d) => {
            eprintln!("DIVERGENCE: {d}");
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_gen(a: &Args) -> Res<ExitCode> {
    a.check_known(&["seed", "count", "mid", "spread", "out"])?;
    let d = WorkloadConfig::default();
    let w = WorkloadConfig {
        seed: a.num("seed", d.seed)?,
        commands: a.num("count", d.commands)?,
        mid: a.num("mid", d.mid)?,
        spread_ticks: a.num("spread", d.spread_ticks)?,
        ..d
    };
    let mut text = format!(
        "# titan gen --seed {} --count {} --mid {} --spread {}\n",
        w.seed, w.commands, w.mid, w.spread_ticks
    );
    for c in w.generate() {
        text.push_str(&c.to_string());
        text.push('\n');
    }
    match a.get("out") {
        Some(out) => fs::write(out, text).map_err(|e| format!("cannot write {out}: {e}"))?,
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.has("help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match args.positional.first().map(String::as_str) {
        Some("run") => cmd_run(&args),
        Some("replay") => cmd_replay(&args),
        Some("gen") => cmd_gen(&args),
        Some("help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown subcommand '{other}'").into()),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
