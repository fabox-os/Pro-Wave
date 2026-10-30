//! Lock-free triple buffer: one writer (the audio thread) publishes whole values, one reader
//! (the UI) always gets the newest complete one. Neither side ever waits, locks or allocates,
//! and the reader never sees half of one block and half of the next.
//!
//! Three slots: the writer fills its own, then swaps it with the shared "back" slot; the
//! reader swaps its own with the back slot when the writer has published something new.
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Set in `back` when the back slot holds a value the reader has not taken yet.
const FRESH: u8 = 0b100;
const INDEX: u8 = 0b011;

struct Slots<T> {
    slots: [UnsafeCell<T>; 3],
    /// Index of the back slot, plus [`FRESH`].
    back: AtomicU8,
}

// SAFETY: each slot is only touched by the side that currently owns its index. Ownership moves
// only through the atomic swap of `back` (AcqRel), which also orders the slot's contents.
unsafe impl<T: Send> Sync for Slots<T> {}

/// The writing half. Not `Clone`: there is exactly one.
pub struct Writer<T> {
    shared: Arc<Slots<T>>,
    index: u8,
}

/// The reading half. Not `Clone`: there is exactly one.
pub struct Reader<T> {
    shared: Arc<Slots<T>>,
    index: u8,
}

/// A triple buffer whose three slots all start as `init`.
pub fn triple_buffer<T: Clone + Send>(init: T) -> (Writer<T>, Reader<T>) {
    let shared = Arc::new(Slots {
        slots: [
            UnsafeCell::new(init.clone()),
            UnsafeCell::new(init.clone()),
            UnsafeCell::new(init),
        ],
        back: AtomicU8::new(2),
    });
    (
        Writer {
            shared: shared.clone(),
            index: 0,
        },
        Reader { shared, index: 1 },
    )
}

impl<T> Writer<T> {
    /// The slot being written. Its contents are whatever was written two publishes ago (or the
    /// initial value), so write every field before [`Writer::publish`].
    pub fn input(&mut self) -> &mut T {
        // SAFETY: `index` is owned by the writer until it is given away in `publish`.
        unsafe { &mut *self.shared.slots[self.index as usize].get() }
    }

    /// Makes the written slot the newest value. Wait-free.
    pub fn publish(&mut self) {
        let old = self.shared.back.swap(self.index | FRESH, Ordering::AcqRel);
        self.index = old & INDEX;
    }
}

impl<T> Reader<T> {
    /// The newest published value (or the previous one again if nothing new was published).
    /// Wait-free.
    pub fn read(&mut self) -> &T {
        if self.shared.back.load(Ordering::Relaxed) & FRESH != 0 {
            let old = self.shared.back.swap(self.index, Ordering::AcqRel);
            self.index = old & INDEX;
        }
        // SAFETY: `index` is owned by the reader; the writer never touches it.
        unsafe { &*self.shared.slots[self.index as usize].get() }
    }
}

// SAFETY: each half owns one slot index; the slots themselves are `Sync` through `Slots`.
unsafe impl<T: Send> Send for Writer<T> {}
unsafe impl<T: Send> Send for Reader<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_gets_the_newest_published_value() {
        let (mut w, mut r) = triple_buffer(0u32);
        assert_eq!(*r.read(), 0);
        *w.input() = 1;
        assert_eq!(*r.read(), 0, "nothing published yet");
        w.publish();
        *w.input() = 2;
        w.publish();
        *w.input() = 3;
        assert_eq!(
            *r.read(),
            2,
            "the newest published, not the one being written"
        );
        assert_eq!(*r.read(), 2, "reading again gives the same value");
        w.publish();
        assert_eq!(*r.read(), 3);
    }

    #[test]
    fn a_value_is_never_torn_across_threads() {
        // Each value is an array filled with one number; a torn read would mix two numbers.
        let (mut w, mut r) = triple_buffer([0u64; 64]);
        let writer = std::thread::spawn(move || {
            for n in 1..=200_000u64 {
                *w.input() = [n; 64];
                w.publish();
            }
        });
        let mut last = 0;
        loop {
            let done = writer.is_finished();
            let v = r.read();
            let n = v[0];
            assert!(v.iter().all(|&x| x == n), "torn value");
            assert!(n >= last, "went back in time: {n} after {last}");
            last = n;
            if done {
                break;
            }
        }
        // `is_finished` does not order anything; `join` does, so now the last value must show.
        assert!(writer.join().is_ok());
        assert_eq!(r.read()[0], 200_000);
    }
}
