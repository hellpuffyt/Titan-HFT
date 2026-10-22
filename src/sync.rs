//! Synchronization primitives that switch to `loom` under `--cfg loom`.
//!
//! The queue is written once against this tiny shim so the exact same code can be
//! model-checked by loom (which explores every legal interleaving and memory-ordering
//! outcome) and run at full speed with `std` atomics in normal builds.

#[cfg(loom)]
pub(crate) use loom::sync::Arc;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(loom))]
pub(crate) use std::sync::Arc;
#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicUsize, Ordering};

/// An `UnsafeCell` with the closure-based API loom requires.
#[cfg(loom)]
#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(loom::cell::UnsafeCell<T>);

#[cfg(loom)]
impl<T> UnsafeCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(loom::cell::UnsafeCell::new(value))
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        self.0.with_mut(f)
    }
}

/// An `UnsafeCell` with the closure-based API loom requires.
#[cfg(not(loom))]
#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(core::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(core::cell::UnsafeCell::new(value))
    }

    #[inline(always)]
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}
