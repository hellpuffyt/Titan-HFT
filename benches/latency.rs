//! Latency-distribution harness (not Criterion): warm-up, repeated runs, HDR
//! histograms, percentile reporting, allocation counting, and machine metadata.
//!
//! Run with `cargo bench --bench latency`. Optional env vars:
//! `TITAN_RUNS` (default 5), `TITAN_COMMANDS` (default 1_000_000),
//! `TITAN_ROUNDTRIPS` (default 1_000_000).
//!
//! Every engine workload asserts that its timed runs perform zero heap allocations;
//! the bench exits non-zero otherwise.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

#[path = "support/mod.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use hdrhistogram::Histogram;
use titan_hft::engine::MatchingEngine;
use titan_hft::pipeline;
use titan_hft::spsc;
use titan_hft::types::{Command, CountingSink, Event, EventSink};
use titan_hft::workload::{ReplaceMix, WorkloadConfig};

/// Counts every allocation in the process.
struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: delegates every call unchanged to the system allocator; only adds a counter.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn hist() -> Histogram<u64> {
    // 1 ns .. 10 s, 3 significant digits; preallocated, never resizes.
    Histogram::new_with_bounds(1, 10_000_000_000, 3).unwrap()
}

fn report(label: &str, h: &Histogram<u64>) {
    println!(
        "{label:<28} n={:>9}  p50={:>6}  p95={:>6}  p99={:>6}  p99.9={:>7}  max={:>9}  (ns)",
        h.len(),
        h.value_at_quantile(0.50),
        h.value_at_quantile(0.95),
        h.value_at_quantile(0.99),
        h.value_at_quantile(0.999),
        h.max()
    );
}

/// Cost of the measurement itself: two back-to-back `Instant::now()` calls.
fn timer_overhead() -> Histogram<u64> {
    let mut h = hist();
    for _ in 0..1_000_000 {
        let a = Instant::now();
        let b = Instant::now();
        h.record((b - a).as_nanos().max(1) as u64).unwrap();
    }
    h
}

/// Command kinds by *outcome* (what the engine did), not by intent.
const KINDS: [&str; 6] = [
    "limit",
    "market",
    "cancel",
    "replace_keep",
    "replace_requeue",
    "rejected",
];

/// Counts events (like `CountingSink`) and remembers the first event of the current
/// command, which identifies its outcome. Never allocates.
#[derive(Default)]
struct KindSink {
    counts: CountingSink,
    first: Option<Event>,
}

impl EventSink for KindSink {
    #[inline]
    fn emit(&mut self, event: Event) {
        if self.first.is_none() {
            self.first = Some(event);
        }
        self.counts.emit(event);
    }
}

fn kind_of(cmd: Command, first: Option<Event>) -> usize {
    match (cmd, first) {
        (_, Some(Event::Rejected { .. })) => 5,
        (
            Command::Replace { .. },
            Some(Event::Replaced {
                kept_priority: true,
                ..
            }),
        ) => 3,
        (Command::Replace { .. }, _) => 4,
        (Command::Limit { .. }, _) => 0,
        (Command::Market { .. }, _) => 1,
        (Command::Cancel { .. }, _) => 2,
        _ => 5,
    }
}

fn workloads(commands: usize) -> Vec<(&'static str, WorkloadConfig)> {
    let base = WorkloadConfig {
        commands,
        ..Default::default()
    };
    let with = |pct, mix| WorkloadConfig {
        replace_pct: pct,
        replace_mix: mix,
        ..base
    };
    vec![
        ("baseline (replace 0%, = 0.1.0 workload)", base),
        ("replace 10% mixed", with(10, ReplaceMix::Mixed)),
        ("replace 30% mixed", with(30, ReplaceMix::Mixed)),
        ("replace 30% reduce-only", with(30, ReplaceMix::ReduceOnly)),
        ("replace 30% move-only", with(30, ReplaceMix::MoveOnly)),
    ]
}

fn spread(label: &str, values: &mut [f64], unit: &str) {
    values.sort_by(f64::total_cmp);
    let (min, max) = (values[0], values[values.len() - 1]);
    let median = values[values.len() / 2];
    println!(
        "{label}: min {min:.2} / median {median:.2} / max {max:.2} {unit}  (max/min = {:.2}, n={})",
        max / min,
        values.len()
    );
}

/// Per-command latency split by outcome, plus throughput with and without timers,
/// for one workload. Panics if any timed run allocates.
fn engine_workload(label: &str, w: &WorkloadConfig, runs: u64) {
    let flow = w.generate();
    let cfg = w.book_config(1 << 20);
    println!(
        "\n## Engine workload: {label}  (seed={} commands={} cancel={}% replace={}% {:?} market={}%)",
        w.seed, w.commands, w.cancel_pct, w.replace_pct, w.replace_mix, w.market_pct
    );
    println!("load before: {}", support::load());
    // Warm-up: one full untimed pass (caches, branch predictors, page faults).
    {
        let mut e = MatchingEngine::new(cfg).unwrap();
        let mut sink = CountingSink::default();
        for &c in &flow {
            e.process(c, &mut sink);
        }
        black_box(sink);
    }

    // Per-command latency by kind.
    let mut all = hist();
    let mut by_kind: Vec<Histogram<u64>> = KINDS.iter().map(|_| hist()).collect();
    let mut run_h: Vec<Histogram<u64>> = KINDS.iter().map(|_| hist()).collect();
    let mut kind_p50: Vec<Vec<u64>> = KINDS
        .iter()
        .map(|_| Vec::with_capacity(runs as usize))
        .collect();
    let mut kind_p99: Vec<Vec<u64>> = KINDS
        .iter()
        .map(|_| Vec::with_capacity(runs as usize))
        .collect();
    let mut timed_tput = Vec::with_capacity(runs as usize);
    for run in 1..=runs {
        let mut e = MatchingEngine::new(cfg).unwrap();
        let mut h = hist();
        for rh in &mut run_h {
            rh.reset();
        }
        let mut sink = KindSink::default();
        let before = ALLOCS.load(Ordering::Relaxed);
        let start = Instant::now();
        for &c in &flow {
            sink.first = None;
            let t = Instant::now();
            e.process(c, &mut sink);
            let ns = t.elapsed().as_nanos().max(1) as u64;
            h.record(ns).unwrap();
            run_h[kind_of(c, sink.first)].record(ns).unwrap();
        }
        let wall = start.elapsed();
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        assert_eq!(allocs, 0, "{label}: run {run} allocated {allocs} times");
        let tput = flow.len() as f64 / wall.as_secs_f64() / 1e6;
        timed_tput.push(tput);
        report(&format!("run {run} (all kinds)"), &h);
        println!(
            "{:<28} throughput={tput:.2} M cmd/s (incl. timer overhead)  trades={}  allocations_during_run={allocs}",
            "", sink.counts.trades
        );
        all.add(&h).unwrap();
        for (k, rh) in run_h.iter().enumerate() {
            if !rh.is_empty() {
                by_kind[k].add(rh).unwrap();
                kind_p50[k].push(rh.value_at_quantile(0.50));
                kind_p99[k].push(rh.value_at_quantile(0.99));
            }
        }
    }
    report("all runs (all kinds)", &all);
    println!("by outcome, all {runs} runs combined (count is per run):");
    for (k, name) in KINDS.iter().enumerate() {
        let h = &by_kind[k];
        if h.is_empty() {
            println!("  {name:<16} n=0");
            continue;
        }
        println!(
            "  {name:<16} n/run={:>7}  p50={:>5}  p95={:>5}  p99={:>5}  p99.9={:>6}  max={:>9}  | per-run p50 {}..{}  p99 {}..{} (ns)",
            h.len() / runs,
            h.value_at_quantile(0.50),
            h.value_at_quantile(0.95),
            h.value_at_quantile(0.99),
            h.value_at_quantile(0.999),
            h.max(),
            kind_p50[k].iter().min().unwrap(),
            kind_p50[k].iter().max().unwrap(),
            kind_p99[k].iter().min().unwrap(),
            kind_p99[k].iter().max().unwrap(),
        );
    }
    spread("throughput with per-op timers", &mut timed_tput, "M cmd/s");

    // Throughput without per-command timers.
    let mut tput = Vec::with_capacity(runs as usize);
    for run in 0..=runs {
        let mut e = MatchingEngine::new(cfg).unwrap();
        let mut sink = CountingSink::default();
        let before = ALLOCS.load(Ordering::Relaxed);
        let start = Instant::now();
        for &c in &flow {
            e.process(c, &mut sink);
        }
        let wall = start.elapsed();
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        assert_eq!(
            allocs, 0,
            "{label}: untimed run {run} allocated {allocs} times"
        );
        black_box(sink);
        if run == 0 {
            continue; // warm-up
        }
        let t = flow.len() as f64 / wall.as_secs_f64() / 1e6;
        println!(
            "untimed run {run}: {t:.2} M cmd/s  ({:.1} ns/cmd mean)  allocations_during_run={allocs}",
            wall.as_nanos() as f64 / flow.len() as f64
        );
        tput.push(t);
    }
    spread("throughput without timers", &mut tput, "M cmd/s");
    println!("load after:  {}", support::load());
}

fn queue_roundtrip(runs: u64, n: u64) {
    println!(
        "\n## SPSC queue: cross-thread round-trip (ping-pong over two queues, cap 1024; RTT includes both hops)"
    );
    let mut all = hist();
    for run in 0..=runs {
        let (mut ping_tx, mut ping_rx) = spsc::channel::<u64>(1024).unwrap();
        let (mut pong_tx, mut pong_rx) = spsc::channel::<u64>(1024).unwrap();
        let echo = std::thread::spawn(move || {
            let mut seen = 0;
            while seen < n {
                if let Some(v) = ping_rx.try_pop() {
                    let mut v = v;
                    while let Err(back) = pong_tx.try_push(v) {
                        v = back;
                    }
                    seen += 1;
                } else {
                    std::hint::spin_loop();
                }
            }
        });
        let mut h = hist();
        for i in 0..n {
            let t = Instant::now();
            let mut v = i;
            while let Err(back) = ping_tx.try_push(v) {
                v = back;
            }
            loop {
                if let Some(r) = pong_rx.try_pop() {
                    black_box(r);
                    break;
                }
                std::hint::spin_loop();
            }
            h.record(t.elapsed().as_nanos().max(1) as u64).unwrap();
        }
        echo.join().unwrap();
        if run == 0 {
            continue; // warm-up
        }
        report(&format!("run {run} RTT"), &h);
        all.add(&h).unwrap();
    }
    report("all runs RTT", &all);
}

fn pipeline_throughput(runs: u64, commands: usize) {
    let w = WorkloadConfig {
        commands,
        ..Default::default()
    };
    println!(
        "\n## Pipeline: producer thread -> SPSC (cap 4096) -> engine thread ({runs} runs + 1 warm-up)"
    );
    for run in 0..=runs {
        let flow = w.generate();
        let start = Instant::now();
        let r = pipeline::run(w.book_config(1 << 20), flow, 4096).unwrap();
        let wall = start.elapsed();
        if run == 0 {
            continue;
        }
        println!(
            "run {run}: {:.2} M cmd/s end-to-end  producer_full_spins={}  final_hash={:016x}",
            r.commands as f64 / wall.as_secs_f64() / 1e6,
            r.full_spins,
            r.final_hash
        );
    }
}

fn main() {
    let runs = env_or("TITAN_RUNS", 5);
    let commands = env_or("TITAN_COMMANDS", 1_000_000) as usize;
    let roundtrips = env_or("TITAN_ROUNDTRIPS", 1_000_000);
    println!("# Titan-HFT latency report");
    support::print_metadata();
    println!("config:  runs={runs} commands={commands} roundtrips={roundtrips}");
    let t = timer_overhead();
    report("timer overhead (now→now)", &t);
    for (label, w) in workloads(commands) {
        engine_workload(label, &w, runs);
    }
    println!("\nload before queue/pipeline: {}", support::load());
    queue_roundtrip(runs, roundtrips);
    pipeline_throughput(runs, commands);
    println!("load at end: {}", support::load());
}
