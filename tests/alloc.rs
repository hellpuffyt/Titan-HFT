//! Allocation instrumentation: proves (or disproves) that the steady-state matching
//! hot path performs zero heap allocations. Counts only on the test's own thread.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use titan_hft::engine::MatchingEngine;
use titan_hft::types::CountingSink;
use titan_hft::workload::WorkloadConfig;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn bump() {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

// SAFETY: forwards every call unchanged to `System`; only adds a thread-local counter
// (const-initialized, no destructor, so it never allocates or recurses).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn count<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNT.with(|c| c.set(0));
    COUNTING.with(|c| c.set(true));
    let r = f();
    COUNTING.with(|c| c.set(false));
    (r, COUNT.with(Cell::get))
}

#[test]
fn counter_detects_allocations() {
    // Sanity check of the instrument itself.
    let (_, n) = count(|| std::hint::black_box(vec![1u8; 64]));
    assert!(n >= 1);
}

#[test]
fn steady_state_matching_does_not_allocate() {
    let w = WorkloadConfig {
        commands: 1_000_000,
        seed: 9,
        ..Default::default()
    };
    let flow = w.generate();
    let max_orders = 1 << 17;
    let mut engine = MatchingEngine::new(w.book_config(max_orders)).unwrap();
    let mut sink = CountingSink::default();
    // Warm-up half (fills the book to its working size), then measure the rest.
    let (warm, hot) = flow.split_at(flow.len() / 2);
    for &c in warm {
        engine.process(c, &mut sink);
    }
    let resting_before = engine.book().len();
    let (_, allocs) = count(|| {
        for &c in hot {
            engine.process(c, &mut sink);
        }
    });
    println!(
        "measured {} commands, {} trades, book size {} -> {}, allocations = {allocs}",
        hot.len(),
        sink.trades,
        resting_before,
        engine.book().len()
    );
    assert!(sink.trades > 10_000, "workload must actually trade");
    assert_eq!(allocs, 0, "matching hot path allocated {allocs} times");
}

#[test]
fn steady_state_matching_with_replaces_does_not_allocate() {
    // 20% replaces (half in-place reductions, half requeues that may cross).
    let w = WorkloadConfig {
        commands: 1_000_000,
        seed: 11,
        replace_pct: 20,
        ..Default::default()
    };
    let flow = w.generate();
    let mut engine = MatchingEngine::new(w.book_config(1 << 17)).unwrap();
    let mut sink = CountingSink::default();
    let (warm, hot) = flow.split_at(flow.len() / 2);
    for &c in warm {
        engine.process(c, &mut sink);
    }
    let replaces = hot
        .iter()
        .filter(|c| matches!(c, titan_hft::types::Command::Replace { .. }))
        .count();
    let (_, allocs) = count(|| {
        for &c in hot {
            engine.process(c, &mut sink);
        }
    });
    println!(
        "measured {} commands ({replaces} replaces), {} trades, allocations = {allocs}",
        hot.len(),
        sink.trades
    );
    assert!(replaces > 50_000);
    assert_eq!(allocs, 0, "replace path allocated {allocs} times");
}

#[test]
fn reduce_only_and_move_only_replace_mixes_do_not_allocate() {
    use titan_hft::types::{Command, Event};
    use titan_hft::workload::ReplaceMix;
    for mix in [ReplaceMix::ReduceOnly, ReplaceMix::MoveOnly] {
        let w = WorkloadConfig {
            commands: 600_000,
            seed: 13,
            replace_pct: 30,
            replace_mix: mix,
            ..Default::default()
        };
        let flow = w.generate();
        let mut engine = MatchingEngine::new(w.book_config(1 << 17)).unwrap();
        let mut sink = CountingSink::default();
        let (warm, hot) = flow.split_at(flow.len() / 3);
        for &c in warm {
            engine.process(c, &mut sink);
        }
        let (_, allocs) = count(|| {
            for &c in hot {
                engine.process(c, &mut sink);
            }
        });
        // Outside the measured window: check the mix really exercised its path.
        let mut probe = MatchingEngine::new(w.book_config(1 << 17)).unwrap();
        let (mut keep, mut requeue) = (0u64, 0u64);
        let mut ev = Vec::new();
        for &c in &flow {
            ev.clear();
            probe.process(c, &mut ev);
            if let (Command::Replace { .. }, Some(Event::Replaced { kept_priority, .. })) =
                (c, ev.first())
            {
                if *kept_priority {
                    keep += 1
                } else {
                    requeue += 1
                }
            }
        }
        println!(
            "{mix:?}: {} measured commands, keep={keep} requeue={requeue}, allocations = {allocs}",
            hot.len()
        );
        assert_eq!(allocs, 0, "{mix:?} allocated {allocs} times");
        match mix {
            ReplaceMix::ReduceOnly => assert!(keep > requeue && keep > 10_000),
            _ => assert!(requeue > 10_000 && keep * 20 < requeue),
        }
    }
}

#[test]
fn cold_start_matching_does_not_allocate_either() {
    // The book is preallocated, so even the first commands should not allocate.
    let w = WorkloadConfig {
        commands: 200_000,
        seed: 3,
        ..Default::default()
    };
    let flow = w.generate();
    let mut engine = MatchingEngine::new(w.book_config(1 << 16)).unwrap();
    let mut sink = CountingSink::default();
    let (_, allocs) = count(|| {
        for &c in &flow {
            engine.process(c, &mut sink);
        }
    });
    assert_eq!(allocs, 0);
}
