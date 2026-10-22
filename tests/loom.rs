//! Loom model checks for the SPSC queue. Run with:
//! `RUSTFLAGS="--cfg loom" cargo test --release --test loom`
#![cfg(loom)]
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use titan_hft::spsc;

#[test]
fn loom_push_pop_is_ordered_and_complete() {
    loom::model(|| {
        let (mut tx, mut rx) = spsc::channel::<u32>(2).unwrap();
        let t = loom::thread::spawn(move || {
            for i in 0..3 {
                let mut v = i;
                loop {
                    match tx.try_push(v) {
                        Ok(()) => break,
                        Err(back) => {
                            v = back;
                            loom::thread::yield_now();
                        }
                    }
                }
            }
        });
        let mut next = 0;
        while next < 3 {
            match rx.try_pop() {
                Some(v) => {
                    assert_eq!(v, next);
                    next += 1;
                }
                None => loom::thread::yield_now(),
            }
        }
        t.join().unwrap();
    });
}

#[test]
fn loom_drop_with_items_in_flight() {
    loom::model(|| {
        let (mut tx, rx) = spsc::channel::<loom::sync::Arc<u8>>(2).unwrap();
        let item = loom::sync::Arc::new(1);
        let i2 = item.clone();
        let t = loom::thread::spawn(move || {
            let _ = tx.try_push(i2);
        });
        drop(rx);
        t.join().unwrap();
        assert_eq!(loom::sync::Arc::strong_count(&item), 1);
    });
}
