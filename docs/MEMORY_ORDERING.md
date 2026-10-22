# Memory ordering in the SPSC queue

This document justifies every atomic operation in [`src/spsc.rs`](../src/spsc.rs). It is written to be read alongside the code.

## The shared state

| Field | Written by | Read by |
|---|---|---|
| `tail: AtomicUsize` | producer only | consumer (and producer's own `len`) |
| `head: AtomicUsize` | consumer only | producer (and consumer's own `len`) |
| `slots[i]: UnsafeCell<MaybeUninit<T>>` | producer (write), consumer (move out) | — |

Both counters increase monotonically (with wrapping). The producer owns the slots with indices in `[tail, head + cap)`, and the consumer owns `[head, tail)`. **Ownership of a slot passes between threads only through a Release store paired with an Acquire load of a counter.** That is the whole correctness argument; everything below applies it to each access.

## Producer: `try_push`

1. `self.tail`: a plain local copy. The producer is the only writer of `tail`, so it never needs to read the atomic back.
2. `cached_head`: a local, possibly stale copy of `head`. A stale `head` is always *smaller* than the true value, so the queue looks *fuller* than it is. The cache can only make us refresh too often, never overwrite a slot the consumer still owns.
3. **`head.load(Acquire)`** (only when the cache says "full"): pairs with the consumer's `head.store(Release)`. If we observe `head = h`, the consumer's move-out of slot `h-1` *happens-before* this load, and therefore before our upcoming write to that slot. Without Acquire, the write to the slot could race with the consumer's read of the old value (a data race on `UnsafeCell`, which is undefined behavior).
4. Slot write: a non-atomic write into the slot we own.
5. **`tail.store(tail + 1, Release)`**: publishes the slot write. Every write before this store in program order (including the `T` bytes) becomes visible to any thread that loads `tail` with Acquire and sees the new value.

## Consumer: `try_pop`

1. `self.head`: a local copy (the consumer is the only writer).
2. `cached_tail`: a possibly stale copy of `tail`. A stale value is smaller, so the queue looks *emptier*. That is safe.
3. **`tail.load(Acquire)`** (only when the cache says "empty"): pairs with the producer's `tail.store(Release)`. Seeing `tail = t` guarantees the writes of all slots `< t` happen-before our reads.
4. Slot read (`assume_init_read`): moves `T` out; the slot is logically uninitialized again.
5. **`head.store(head + 1, Release)`**: hands the slot back to the producer. This orders our read of the slot before the producer's later overwrite (see producer step 3).

## Why not `SeqCst`?

`SeqCst` adds a single global order over all `SeqCst` operations. This protocol has exactly two one-directional handoffs (producer→consumer via `tail`, consumer→producer via `head`), and each needs only a pairwise happens-before edge, which Acquire/Release provides. On AArch64, Release/Acquire compile to `stlr`/`ldar`, which `SeqCst` would also use for plain loads and stores, so for these particular loads and stores the difference on this architecture is mainly in what the compiler may reorder. On x86-64, Acquire loads and Release stores are plain `mov`s, whereas a `SeqCst` store needs `xchg` (or `mov` + `mfence`), which costs real cycles on every push and pop. Using the weakest *correct* ordering documents intent: a reader can see exactly which edges matter.

## Why not `Relaxed` for the stores?

We tried it. Weakening the producer's `tail.store` to `Relaxed` makes the loom test fail:

```text
Causality violation: Concurrent write accesses to `UnsafeCell`.
```

Loom found an interleaving where the consumer's view of the slot is not ordered after the producer's write. This test is part of the development record and is easy to reproduce: change the ordering on line ~180 of `src/spsc.rs` and run `RUSTFLAGS="--cfg loom" cargo test --release --test loom`.

## Drop

`Shared::drop` runs with `&mut self`, so both endpoints are gone and no other thread can access the queue. `Relaxed` loads are enough, because the `Arc` drop that brought us here already synchronizes with the last use by the other thread: `Arc`'s decrement is `Release`, and the final one performs an `Acquire` fence. Remaining items in `[head, tail)` are dropped exactly once (tested with a drop counter and under loom).

## False sharing

`head` and `tail` are each wrapped in `CachePadded`, which is `#[repr(align(128))]`. Without padding they would sit on the same cache line, and every push (writes `tail`) would invalidate the consumer's copy of the line holding `head`, and vice versa, even though neither thread reads the other's counter on the fast path. 128 bytes is the line size on Apple M-series cores. It also covers x86 CPUs, whose spatial prefetcher pulls in adjacent 64-byte lines in pairs.

The *cached remote index* trick (`cached_head`, `cached_tail`) goes further: on the fast path the producer reads no cache line the consumer writes, except the slot itself.

## Verification

| Tool | Command | Result on 2026-10-22 |
|---|---|---|
| loom (exhaustive interleavings, small model) | `RUSTFLAGS="--cfg loom" cargo test --release --test loom` | 2/2 pass; fails as expected with `Relaxed` tail store |
| Miri (UB + data-race detector) | `cargo +nightly miri test --lib spsc` | 7/7 pass |
| Miri, 16 scheduler seeds | `MIRIFLAGS="-Zmiri-many-seeds=0..16" cargo +nightly miri test --lib spsc::tests::cross_thread` | pass on all 16 seeds |
| Stress | `cargo test --release --test stress` | pass |

Loom and Miri explore a bounded set of executions. They raise confidence; they are not proofs.
