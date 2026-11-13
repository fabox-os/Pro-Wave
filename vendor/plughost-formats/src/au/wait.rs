//! Waiting for Audio Unit completion handlers. They can be delivered through the calling thread's
//! run loop (the main queue on the main thread), so the wait keeps the run loop running instead
//! of blocking.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRunLoop, CFRunLoopRunResult, kCFRunLoopDefaultMode};

const SLICE: Duration = Duration::from_millis(10);

/// A value a completion handler fills in on some other thread or queue.
pub struct Slot<T> {
    value: Arc<(Mutex<Option<T>>, Condvar)>,
}

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Slot<T> {
        Slot {
            value: Arc::clone(&self.value),
        }
    }
}

impl<T> Slot<T> {
    pub fn new() -> Slot<T> {
        Slot {
            value: Arc::new((Mutex::new(None), Condvar::new())),
        }
    }

    pub fn fill(&self, value: T) {
        let (slot, ready) = &*self.value;
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
        ready.notify_one();
    }

    /// Runs the current thread's run loop until the value arrives. The helper's supervision
    /// bounds how long a plugin may take.
    pub fn wait(&self) -> T {
        let (slot, ready) = &*self.value;
        loop {
            if let Some(value) = slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                return value;
            }
            let started = Instant::now();
            // SAFETY: the default mode is a valid run loop mode.
            let result =
                unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, SLICE.as_secs_f64(), true) };
            if result == CFRunLoopRunResult::Finished {
                // No sources: Core Foundation returned immediately. Sleep until completion or
                // the next slice, when a newly scheduled run-loop callback can be delivered.
                let (mut value, _) = ready
                    .wait_timeout_while(
                        slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
                        SLICE.saturating_sub(started.elapsed()),
                        |value| value.is_none(),
                    )
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(value) = value.take() {
                    return value;
                }
            }
        }
    }
}

/// Runs the current thread's run loop for `duration`, delivering notifications and timers.
pub fn run_loop_for(duration: Duration) {
    let until = Instant::now() + duration;
    while let Some(remaining) = until.checked_duration_since(Instant::now()) {
        let slice = SLICE.min(remaining);
        let started = Instant::now();
        // SAFETY: the default mode is a valid run loop mode.
        let result =
            unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, slice.as_secs_f64(), true) };
        if result == CFRunLoopRunResult::Finished {
            std::thread::sleep(slice.saturating_sub(started.elapsed()));
        }
    }
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
