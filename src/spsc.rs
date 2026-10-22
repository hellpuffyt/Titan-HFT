//! A bounded, wait-free single-producer / single-consumer ring buffer.
//!
//! # Design
//!
//! * Capacity is rounded up to a power of two so `index & mask` replaces `index % cap`.
//! * `tail` is written only by the producer, `head` only by the consumer. Both are
//!   *monotonic* `usize` counters that wrap; `tail - head` (wrapping) is the length.
//! * Each counter lives on its own 128-byte aligned line ([`CachePadded`]) so the
//!   producer and consumer do not invalidate each other's cache line on every
//!   operation (false sharing). 128 bytes covers Apple M-series and the adjacent-line
//!   prefetcher on modern x86.
//! * Each side keeps a *cached* copy of the other side's counter and only re-reads the
//!   shared atomic when the cached value says the queue looks full (producer) or empty
//!   (consumer). In the common case an operation touches no line written by the other
//!   thread except the slot itself.
//!
//! The memory-ordering argument for every atomic access is in `docs/MEMORY_ORDERING.md`
//! and is repeated briefly at each access below.

use core::fmt;
use core::mem::MaybeUninit;

use crate::sync::{Arc, AtomicUsize, Ordering, UnsafeCell};

/// Pads and aligns a value to 128 bytes to keep it on its own cache line(s).
#[derive(Debug, Default)]
#[repr(align(128))]
pub struct CachePadded<T>(pub T);

struct Shared<T> {
    /// Next index the consumer will read. Written only by the consumer.
    head: CachePadded<AtomicUsize>,
    /// Next index the producer will write. Written only by the producer.
    tail: CachePadded<AtomicUsize>,
    mask: usize,
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: `Shared` hands out access to each slot to exactly one thread at a time:
// the producer owns slots in `[tail, head + cap)`, the consumer owns `[head, tail)`.
// Ownership moves only through the Release/Acquire pairs on `head`/`tail`, so a
// `T: Send` value is moved between threads, never aliased. `T: Sync` is not needed
// because no `&T` is ever shared.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: see the `Send` impl; all shared mutation goes through atomics or through
// slots whose exclusive ownership is established by those atomics.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // `drop` has `&mut self`: both endpoints are gone, no concurrency remains.
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Relaxed);
        let mut i = head;
        while i != tail {
            self.slots[i & self.mask].with_mut(|p| {
                // SAFETY: every index in [head, tail) was written by `try_push` and not
                // yet read by `try_pop`, so it holds an initialized `T` that nobody
                // else will drop.
                unsafe { (*p).assume_init_drop() }
            });
            i = i.wrapping_add(1);
        }
    }
}

/// Error returned by [`channel`] for an unusable capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CapacityError {
    /// Capacity must be at least 1.
    #[error("queue capacity must be at least 1")]
    Zero,
    /// Capacity rounded up to a power of two would overflow `usize`.
    #[error("queue capacity {0} is too large")]
    TooLarge(usize),
}

/// Creates a bounded SPSC queue holding at least `capacity` items.
///
/// The real capacity is `capacity.next_power_of_two()`; see [`Producer::capacity`].
pub fn channel<T: Send>(capacity: usize) -> Result<(Producer<T>, Consumer<T>), CapacityError> {
    if capacity == 0 {
        return Err(CapacityError::Zero);
    }
    let cap = capacity
        .checked_next_power_of_two()
        .ok_or(CapacityError::TooLarge(capacity))?;
    // Keep `tail - head <= cap` representable without ambiguity.
    if cap > usize::MAX / 2 {
        return Err(CapacityError::TooLarge(capacity));
    }
    let slots = (0..cap)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let shared = Arc::new(Shared {
        head: CachePadded(AtomicUsize::new(0)),
        tail: CachePadded(AtomicUsize::new(0)),
        mask: cap - 1,
        slots,
    });
    Ok((
        Producer {
            shared: shared.clone(),
            tail: 0,
            cached_head: 0,
        },
        Consumer {
            shared,
            head: 0,
            cached_tail: 0,
        },
    ))
}

/// The sending half. Not `Clone`: there is exactly one producer.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    /// Local copy of `shared.tail` (we are its only writer).
    tail: usize,
    /// Last observed value of `shared.head`; may be stale (too small), never too large.
    cached_head: usize,
}

/// The receiving half. Not `Clone`: there is exactly one consumer.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    /// Local copy of `shared.head` (we are its only writer).
    head: usize,
    /// Last observed value of `shared.tail`; may be stale (too small), never too large.
    cached_tail: usize,
}

impl<T> fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Producer")
            .field("capacity", &(self.shared.mask + 1))
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Consumer")
            .field("capacity", &(self.shared.mask + 1))
            .finish_non_exhaustive()
    }
}

impl<T: Send> Producer<T> {
    /// Usable capacity (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Pushes `value`, or returns it back in `Err` if the queue is full.
    ///
    /// Wait-free: a bounded number of steps regardless of the consumer.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let cap = self.shared.mask + 1;
        if self.tail.wrapping_sub(self.cached_head) == cap {
            // Looks full with our stale view; refresh.
            // ACQUIRE pairs with the consumer's RELEASE store of `head` in `try_pop`:
            // the consumer's read of slot `head-1` happens-before our overwrite of it.
            self.cached_head = self.shared.head.0.load(Ordering::Acquire);
            if self.tail.wrapping_sub(self.cached_head) == cap {
                return Err(value);
            }
        }
        self.shared.slots[self.tail & self.shared.mask].with_mut(|p| {
            // SAFETY: `tail - head < cap`, so slot `tail` is outside `[head, tail)` and is
            // owned by the producer: the consumer will not read it until we publish
            // `tail + 1` below, and any previous value in it was already moved out by
            // the consumer (synchronized by the Acquire load above or an earlier one).
            unsafe { (*p).write(value) };
        });
        self.tail = self.tail.wrapping_add(1);
        // RELEASE publishes the slot write above to the consumer's ACQUIRE load of `tail`.
        self.shared.tail.0.store(self.tail, Ordering::Release);
        Ok(())
    }

    /// Number of items currently queued (a snapshot; may be stale immediately).
    pub fn len(&self) -> usize {
        // ACQUIRE is not required for a statistic; Relaxed would do, but Acquire keeps
        // the value consistent with any slot reuse a caller might reason about.
        self.tail
            .wrapping_sub(self.shared.head.0.load(Ordering::Acquire))
    }

    /// Whether the queue is currently empty (a snapshot).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Send> Consumer<T> {
    /// Usable capacity (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Pops the oldest item, or `None` if the queue is empty.
    ///
    /// Wait-free: a bounded number of steps regardless of the producer.
    #[inline]
    pub fn try_pop(&mut self) -> Option<T> {
        if self.head == self.cached_tail {
            // ACQUIRE pairs with the producer's RELEASE store of `tail` in `try_push`:
            // the slot write happens-before our read of it.
            self.cached_tail = self.shared.tail.0.load(Ordering::Acquire);
            if self.head == self.cached_tail {
                return None;
            }
        }
        let value = self.shared.slots[self.head & self.shared.mask].with_mut(|p| {
            // SAFETY: `head < tail` as published by the producer (Acquire above or
            // earlier), so this slot holds an initialized `T` written by the producer,
            // and the producer will not touch it until we publish `head + 1`.
            unsafe { (*p).assume_init_read() }
        });
        self.head = self.head.wrapping_add(1);
        // RELEASE: our read of the slot happens-before the producer's ACQUIRE load of
        // `head`, after which it may overwrite the slot.
        self.shared.head.0.store(self.head, Ordering::Release);
        Some(value)
    }

    /// Number of items currently queued (a snapshot; may be stale immediately).
    pub fn len(&self) -> usize {
        self.shared
            .tail
            .0
            .load(Ordering::Acquire)
            .wrapping_sub(self.head)
    }

    /// Whether the queue is currently empty (a snapshot).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::sync::atomic::AtomicUsize as StdAtomic;

    #[test]
    fn rejects_zero_capacity() {
        assert_eq!(channel::<u8>(0).err(), Some(CapacityError::Zero));
    }

    #[test]
    fn rounds_capacity_up_to_power_of_two() {
        let (p, c) = channel::<u8>(5).unwrap();
        assert_eq!(p.capacity(), 8);
        assert_eq!(c.capacity(), 8);
    }

    #[test]
    fn fifo_and_full_empty_boundaries() {
        let (mut p, mut c) = channel::<u32>(4).unwrap();
        assert_eq!(c.try_pop(), None);
        for i in 0..4 {
            p.try_push(i).unwrap();
        }
        assert_eq!(p.try_push(99), Err(99), "fifth push must report full");
        assert_eq!(p.len(), 4);
        for i in 0..4 {
            assert_eq!(c.try_pop(), Some(i));
        }
        assert_eq!(c.try_pop(), None);
        assert!(c.is_empty());
    }

    #[test]
    fn wraps_around_many_times() {
        let (mut p, mut c) = channel::<usize>(2).unwrap();
        for i in 0..1_000 {
            p.try_push(i).unwrap();
            assert_eq!(c.try_pop(), Some(i));
        }
    }

    #[test]
    fn indices_survive_usize_wraparound() {
        let (mut p, mut c) = channel::<u8>(4).unwrap();
        // Start both counters just below usize::MAX to exercise wrapping arithmetic.
        let start = usize::MAX - 2;
        p.tail = start;
        p.cached_head = start;
        p.shared.tail.0.store(start, Ordering::Relaxed);
        p.shared.head.0.store(start, Ordering::Relaxed);
        c.head = start;
        c.cached_tail = start;
        for i in 0..4u8 {
            p.try_push(i).unwrap();
        }
        assert!(p.try_push(9).is_err());
        for i in 0..4u8 {
            assert_eq!(c.try_pop(), Some(i));
        }
        assert_eq!(c.try_pop(), None);
    }

    static DROPS: StdAtomic = StdAtomic::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn drops_unconsumed_items_exactly_once() {
        DROPS.store(0, std::sync::atomic::Ordering::Relaxed);
        let (mut p, mut c) = channel::<Counted>(8).unwrap();
        for _ in 0..5 {
            assert!(p.try_push(Counted).is_ok());
        }
        drop(c.try_pop()); // 1 dropped here
        drop(p);
        drop(c); // remaining 4 dropped by Shared::drop
        assert_eq!(DROPS.load(std::sync::atomic::Ordering::Relaxed), 5);
    }

    #[test]
    fn cross_thread_transfer_preserves_order() {
        let n: u64 = if cfg!(miri) { 2_000 } else { 200_000 };
        let (mut p, mut c) = channel::<u64>(64).unwrap();
        let t = std::thread::spawn(move || {
            for i in 0..n {
                let mut v = i;
                loop {
                    match p.try_push(v) {
                        Ok(()) => break,
                        Err(back) => {
                            v = back;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });
        let mut expected = 0;
        while expected < n {
            if let Some(v) = c.try_pop() {
                assert_eq!(v, expected);
                expected += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        t.join().unwrap();
    }
}
