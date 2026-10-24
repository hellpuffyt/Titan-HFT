//! Concurrency and saturation tests: producer thread -> SPSC queue -> engine thread
//! must give exactly the same result as running the engine directly.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use titan_hft::engine::MatchingEngine;
use titan_hft::pipeline;
use titan_hft::spsc;
use titan_hft::types::CountingSink;
use titan_hft::workload::WorkloadConfig;

fn direct(w: &WorkloadConfig, max_orders: u32) -> (CountingSink, u64) {
    let mut e = MatchingEngine::new(w.book_config(max_orders)).unwrap();
    let mut sink = CountingSink::default();
    for c in w.generate() {
        e.process(c, &mut sink);
    }
    (sink, e.book().state_hash())
}

#[test]
fn pipeline_equals_direct_under_saturation() {
    // Capacity 1 and 2 keep the queue full almost all the time.
    for (seed, cap) in [(1u64, 1usize), (2, 2), (3, 64), (4, 4096)] {
        let w = WorkloadConfig {
            seed,
            commands: 200_000,
            ..Default::default()
        };
        let (want_sink, want_hash) = direct(&w, 1 << 16);
        let r = pipeline::run(w.book_config(1 << 16), w.generate(), cap).unwrap();
        assert_eq!(r.commands, 200_000);
        assert_eq!(r.sink, want_sink, "seed {seed} cap {cap}");
        assert_eq!(r.final_hash, want_hash, "seed {seed} cap {cap}");
        if cap <= 2 {
            assert!(
                r.full_spins > 0,
                "a tiny queue must have exerted back-pressure"
            );
        }
    }
}

#[test]
fn pipeline_equals_direct_with_replaces() {
    let w = WorkloadConfig {
        seed: 21,
        commands: 200_000,
        replace_pct: 25,
        ..Default::default()
    };
    let (want_sink, want_hash) = direct(&w, 1 << 16);
    let r = pipeline::run(w.book_config(1 << 16), w.generate(), 2).unwrap();
    assert_eq!(r.sink, want_sink);
    assert_eq!(r.final_hash, want_hash);
}

#[test]
fn queue_transfers_every_item_once_in_order_under_contention() {
    const N: u64 = 2_000_000;
    let (mut tx, mut rx) = spsc::channel::<u64>(8).unwrap();
    let producer = std::thread::spawn(move || {
        let mut full = 0u64;
        for i in 0..N {
            let mut v = i;
            while let Err(back) = tx.try_push(v) {
                v = back;
                full += 1;
                std::hint::spin_loop();
            }
        }
        full
    });
    let mut next = 0u64;
    let mut sum = 0u64;
    while next < N {
        match rx.try_pop() {
            Some(v) => {
                assert_eq!(v, next);
                sum = sum.wrapping_add(v);
                next += 1;
            }
            None => std::hint::spin_loop(),
        }
    }
    assert_eq!(rx.try_pop(), None);
    let _full = producer.join().unwrap();
    assert_eq!(sum, N * (N - 1) / 2);
}

#[test]
fn heap_payloads_are_neither_leaked_nor_double_freed() {
    // Boxes would trip ASan/Miri or crash on double free; drop count proves no leak.
    use std::sync::Arc;
    let token = Arc::new(());
    let (mut tx, mut rx) = spsc::channel::<Arc<()>>(16).unwrap();
    let t2 = token.clone();
    let producer = std::thread::spawn(move || {
        for _ in 0..100_000 {
            let mut v = t2.clone();
            while let Err(back) = tx.try_push(v) {
                v = back;
                std::hint::spin_loop();
            }
        }
        // Leave some items in the queue: they must be dropped with the queue.
    });
    let mut got = 0;
    while got < 99_990 {
        if rx.try_pop().is_some() {
            got += 1;
        }
    }
    producer.join().unwrap();
    drop(rx);
    assert_eq!(Arc::strong_count(&token), 1);
}
