//! Criterion microbenchmarks for the SPSC queue only (no matching engine).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use titan_hft::spsc;

fn same_thread(c: &mut Criterion) {
    let mut g = c.benchmark_group("spsc/same_thread");
    g.throughput(Throughput::Elements(1));
    let (mut tx, mut rx) = spsc::channel::<u64>(1024).unwrap();
    g.bench_function("push_pop_u64", |b| {
        b.iter(|| {
            tx.try_push(black_box(7)).unwrap();
            black_box(rx.try_pop())
        })
    });
    g.finish();
}

/// Moves `n` items from a producer thread to the measuring thread.
fn transfer(n: u64, cap: usize) -> Duration {
    let (mut tx, mut rx) = spsc::channel::<u64>(cap).unwrap();
    let producer = std::thread::spawn(move || {
        for i in 0..n {
            let mut v = i;
            while let Err(back) = tx.try_push(v) {
                v = back;
                std::hint::spin_loop();
            }
        }
    });
    let start = Instant::now();
    let mut got = 0;
    while got < n {
        if let Some(v) = rx.try_pop() {
            black_box(v);
            got += 1;
        } else {
            std::hint::spin_loop();
        }
    }
    let elapsed = start.elapsed();
    producer.join().unwrap();
    elapsed
}

fn cross_thread(c: &mut Criterion) {
    let mut g = c.benchmark_group("spsc/cross_thread");
    const BATCH: u64 = 100_000;
    g.throughput(Throughput::Elements(BATCH));
    for cap in [64usize, 1024, 16_384] {
        g.bench_function(format!("cap_{cap}"), |b| {
            b.iter_custom(|iters| (0..iters).map(|_| transfer(BATCH, cap)).sum())
        });
    }
    g.finish();
}

criterion_group!(benches, same_thread, cross_thread);
criterion_main!(benches);
