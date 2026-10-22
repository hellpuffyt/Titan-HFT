//! Criterion end-to-end benchmarks for the matching engine (single thread, no queue).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use titan_hft::engine::MatchingEngine;
use titan_hft::types::CountingSink;
use titan_hft::workload::WorkloadConfig;

fn engine(c: &mut Criterion) {
    let w = WorkloadConfig {
        commands: 100_000,
        ..Default::default()
    };
    let commands = w.generate();
    let cfg = w.book_config(1 << 17);
    let mut g = c.benchmark_group("engine");
    g.throughput(Throughput::Elements(commands.len() as u64));
    g.sample_size(30);
    g.bench_function("workload_100k_default_mix", |b| {
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
    g.finish();
}

criterion_group!(benches, engine);
criterion_main!(benches);
