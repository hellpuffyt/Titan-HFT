//! Criterion end-to-end benchmarks for the matching engine (single thread, no queue).
//!
//! `workload_100k_default_mix` is the 0.1.0 baseline (no replaces) and keeps its name
//! so results stay comparable across versions. The replace workloads differ from it
//! only in `replace_pct` / `replace_mix`, so each one consumes the same cancel/market
//! budget as the baseline.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use titan_hft::engine::MatchingEngine;
use titan_hft::types::CountingSink;
use titan_hft::workload::{ReplaceMix, WorkloadConfig};

fn workloads() -> Vec<(&'static str, WorkloadConfig)> {
    let base = WorkloadConfig {
        commands: 100_000,
        ..Default::default()
    };
    let with = |pct, mix| WorkloadConfig {
        replace_pct: pct,
        replace_mix: mix,
        ..base
    };
    vec![
        ("workload_100k_default_mix", base),
        ("workload_100k_replace10_mixed", with(10, ReplaceMix::Mixed)),
        ("workload_100k_replace30_mixed", with(30, ReplaceMix::Mixed)),
        (
            "workload_100k_replace30_reduce_only",
            with(30, ReplaceMix::ReduceOnly),
        ),
        (
            "workload_100k_replace30_move_only",
            with(30, ReplaceMix::MoveOnly),
        ),
    ]
}

fn engine(c: &mut Criterion) {
    let mut g = c.benchmark_group("engine");
    g.sample_size(30);
    for (name, w) in workloads() {
        let commands = w.generate();
        let cfg = w.book_config(1 << 17);
        g.throughput(Throughput::Elements(commands.len() as u64));
        g.bench_function(name, |b| {
            b.iter_batched(
                || MatchingEngine::new(cfg).unwrap(), // allocation happens here, outside the timing
                |mut e| {
                    let mut sink = CountingSink::default();
                    for &cmd in &commands {
                        e.process(cmd, &mut sink);
                    }
                    black_box(sink)
                },
                BatchSize::PerIteration,
            )
        });
    }
    g.finish();
}

criterion_group!(benches, engine);
criterion_main!(benches);
