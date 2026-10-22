//! Latency-distribution harness (not Criterion): warm-up, repeated runs, HDR
//! histograms, percentile reporting, allocation counting, and machine metadata.
//!
//! Run with `cargo bench --bench latency`. Optional env vars:
//! `TITAN_RUNS` (default 5), `TITAN_COMMANDS` (default 1_000_000),
//! `TITAN_ROUNDTRIPS` (default 1_000_000).
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
use titan_hft::types::CountingSink;
use titan_hft::workload::WorkloadConfig;

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

fn engine_latency(runs: u64, commands: usize) {
    let w = WorkloadConfig {
        commands,
        ..Default::default()
    };
    let flow = w.generate();
    let cfg = w.book_config(1 << 20);
    println!(
        "\n## Engine: per-command latency (single thread, CountingSink, workload seed={} commands={commands})",
        w.seed
    );
    // Warm-up: one full untimed pass (caches, branch predictors, page faults).
    {
        let mut e = MatchingEngine::new(cfg).unwrap();
        let mut sink = CountingSink::default();
        for &c in &flow {
            e.process(c, &mut sink);
        }
        black_box(sink);
    }
    let mut all = hist();
    for run in 1..=runs {
        let mut e = MatchingEngine::new(cfg).unwrap();
        let mut h = hist();
        let mut sink = CountingSink::default();
        let before = ALLOCS.load(Ordering::Relaxed);
        let start = Instant::now();
        for &c in &flow {
            let t = Instant::now();
            e.process(c, &mut sink);
            h.record(t.elapsed().as_nanos().max(1) as u64).unwrap();
        }
        let wall = start.elapsed();
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        report(&format!("run {run}"), &h);
        println!(
            "{:<28} throughput={:.2} M cmd/s (incl. timer overhead)  trades={}  allocations_during_run={allocs}",
            "",
            flow.len() as f64 / wall.as_secs_f64() / 1e6,
            sink.trades
        );
        all.add(&h).unwrap();
    }
    report("all runs", &all);
}

fn engine_throughput(runs: u64, commands: usize) {
    let w = WorkloadConfig {
        commands,
        ..Default::default()
    };
    let flow = w.generate();
    let cfg = w.book_config(1 << 20);
    println!("\n## Engine: throughput without per-command timers ({runs} runs + 1 warm-up)");
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
        black_box(sink);
        if run == 0 {
            continue;
        }
        println!(
            "run {run}: {:.2} M cmd/s  ({:.1} ns/cmd mean)  allocations_during_run={allocs}",
            flow.len() as f64 / wall.as_secs_f64() / 1e6,
            wall.as_nanos() as f64 / flow.len() as f64
        );
    }
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
    engine_latency(runs, commands);
    engine_throughput(runs, commands);
    queue_roundtrip(runs, roundtrips);
    pipeline_throughput(runs, commands);
}
