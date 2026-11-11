use std::ffi::c_void;
use std::io::Write;
use std::time::Instant;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRunLoopRunResult, CFRunLoopTimer, CFRunLoopTimerContext,
};

use super::*;

#[test]
fn completion_can_arrive_from_another_thread_or_the_waiting_run_loop() {
    std::thread::spawn(|| {
        let completed = Slot::<u32>::new();
        completed.fill(11);
        assert_eq!(completed.wait(), 11);
        let other = completed.clone();
        let thread = std::thread::spawn(move || other.fill(23));
        assert_eq!(completed.wait(), 23);
        thread.join().unwrap();

        unsafe extern "C-unwind" fn tick(_: *mut CFRunLoopTimer, context: *mut c_void) {
            // The timer is invalidated before this slot goes out of scope.
            unsafe { &*context.cast::<Slot<u32>>() }.fill(37);
        }
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::from_ref(&completed).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                CFAbsoluteTimeGetCurrent() + 0.001,
                0.0,
                0,
                0,
                Some(tick),
                &mut context,
            )
        }
        .unwrap();
        CFRunLoop::current()
            .unwrap()
            .add_timer(Some(&timer), unsafe { kCFRunLoopDefaultMode });
        assert_eq!(completed.wait(), 37);
        timer.invalidate();
    })
    .join()
    .unwrap();
}

fn thread_cpu() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
        0
    );
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

#[test]
#[ignore = "AU wait CPU benchmark; run in release mode with --nocapture"]
fn au_wait_benchmark() {
    // A fresh non-main thread has no run-loop sources, reproducing the busy-spin path.
    std::thread::spawn(|| {
        assert_eq!(
            unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.0, true) },
            CFRunLoopRunResult::Finished
        );
        for completion in [false, true] {
            let duration = Duration::from_millis(100);
            let started = Instant::now();
            let cpu = thread_cpu();
            if completion {
                let slot = Slot::new();
                let other = slot.clone();
                let thread = std::thread::spawn(move || {
                    std::thread::sleep(duration);
                    other.fill(41);
                });
                assert_eq!(slot.wait(), 41);
                thread.join().unwrap();
            } else {
                run_loop_for(duration);
            }
            let cpu = thread_cpu() - cpu;
            let elapsed = started.elapsed();
            assert!(elapsed >= duration);
            writeln!(
                std::io::stdout(),
                "completion={completion} wall_ms={:.3} cpu_ms={:.3}",
                elapsed.as_secs_f64() * 1000.0,
                cpu.as_secs_f64() * 1000.0
            )
            .unwrap();
        }
    })
    .join()
    .unwrap();
}
