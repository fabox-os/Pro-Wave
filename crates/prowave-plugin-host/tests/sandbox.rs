//! A plug-in in a process of its own: the same answers as in-process, and a plug-in that
//! crashes or hangs while playing costs its slot, not the host.
#![cfg(unix)]

use prowave_plugin_host::{load_sandboxed, scan_file, Format, Instance};
use std::path::Path;
use std::time::{Duration, Instant};

/// The fixture in its own process, or `None` when the build had no C compiler.
fn fixture() -> Option<Instance> {
    let path = option_env!("CLAP_FIXTURE")?;
    let info = scan_file(Format::Clap, Path::new(path))
        .expect("scan")
        .remove(0);
    let program = Path::new(env!("CARGO_BIN_EXE_prowave-plugin-host"));
    Some(load_sandboxed(&info, 48_000.0, 64, program).expect("the process starts"))
}

/// Waits until `done` is true (the other process answers within a few ticks).
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_plug_in_in_its_own_process_answers_like_one_in_ours() {
    let Some(instance) = fixture() else {
        return;
    };
    assert_eq!(instance.params().len(), 3);
    assert_eq!(instance.params()[0].name, "Gain");
    assert_eq!(instance.param(0), 0.5);
    assert_eq!(instance.param_text(0), "loud");
    assert_eq!(instance.latency_samples(), 7);
    assert!(!instance.crashed());

    let mut processor = instance.processor();
    let (mut left, mut right) = ([0.5f32; 64], [-0.25f32; 64]);
    processor.process(&mut left, &mut right);
    assert_eq!((left[0], right[63]), (0.5, -0.25), "the default gain is 1");

    // A move on the main thread crosses the pipe, then the plug-in's own queue.
    instance.set_param(0, 0.25);
    assert_eq!(instance.param(0), 0.25);
    eventually("the gain is heard", || {
        let (mut l, mut r) = ([1.0f32; 64], [1.0f32; 64]);
        processor.process(&mut l, &mut r);
        l[0] == 0.5
    });
    assert_eq!(instance.param_text(0), "quiet");

    // Automation on the audio thread crosses in the shared memory with the block.
    processor.set_param_norm(0, 1.0);
    let (mut l, mut r) = ([0.5f32; 16], [0.5f32; 16]);
    processor.process(&mut l, &mut r);
    assert_eq!((l[0], r[15]), (1.0, 1.0));
    // ...and the plug-in's process reports the move back.
    eventually("the automated position is known", || {
        instance.param(0) == 1.0
    });
}

#[test]
fn state_crosses_the_process_boundary() {
    let (Some(a), Some(b)) = (fixture(), fixture()) else {
        return;
    };
    a.set_param(0, 0.75);
    let mut processor = a.processor();
    eventually("the move reached the plug-in", || {
        let (mut l, mut r) = ([1.0f32; 64], [1.0f32; 64]);
        processor.process(&mut l, &mut r);
        l[0] == 1.5
    });
    let state = a.save_state().expect("the plug-in has a state");
    assert_eq!(state, 1.5f64.to_ne_bytes());
    assert!(b.load_state(&state));
    eventually("the loaded position is known", || b.param(0) == 0.75);
    assert!(!b.load_state(&[1, 2, 3]), "a short state is refused");
}

#[test]
fn a_plug_in_that_crashes_while_playing_leaves_the_sound_dry() {
    let Some(instance) = fixture() else {
        return;
    };
    let mut processor = instance.processor();
    instance.set_param(0, 0.0);
    eventually("the plug-in is silencing the sound", || {
        let (mut l, mut r) = ([1.0f32; 64], [1.0f32; 64]);
        processor.process(&mut l, &mut r);
        l[0] == 0.0
    });
    // "Crash" past half: the plug-in calls abort() in its process callback.
    processor.set_param_norm(1, 1.0);
    let (mut l, mut r) = ([0.5f32; 64], [0.25f32; 64]);
    processor.process(&mut l, &mut r);
    eventually("the crash is noticed", || instance.crashed());
    let (mut l, mut r) = ([0.5f32; 64], [0.25f32; 64]);
    let started = Instant::now();
    processor.process(&mut l, &mut r);
    assert!(
        started.elapsed() < Duration::from_millis(5),
        "no waiting on a dead process"
    );
    assert_eq!(
        (l[0], r[63]),
        (0.5, 0.25),
        "the sound passes through the slot"
    );
    // The main-thread side answers at once too, with nothing.
    assert_eq!(instance.save_state(), None);
    assert!(!instance.has_editor());
}

#[test]
fn a_plug_in_that_hangs_costs_one_late_block() {
    let Some(instance) = fixture() else {
        return;
    };
    let mut processor = instance.processor();
    let (mut l, mut r) = ([0.5f32; 64], [0.5f32; 64]);
    processor.process(&mut l, &mut r);
    // "Hang" past half: the plug-in never returns from its process callback.
    processor.set_param_norm(2, 1.0);
    let started = Instant::now();
    processor.process(&mut l, &mut r);
    let late = started.elapsed();
    assert!(
        late < Duration::from_millis(500),
        "the wait is bounded ({late:?})"
    );
    assert_eq!(l[0], 0.5, "the late block passes dry");
    for _ in 0..100 {
        let started = Instant::now();
        processor.process(&mut l, &mut r);
        assert!(
            started.elapsed() < Duration::from_millis(5),
            "no more waiting"
        );
        assert_eq!((l[0], r[0]), (0.5, 0.5));
    }
    assert!(!instance.crashed(), "it is stuck, not gone");
    // Dropping the instance ends the stuck process.
}
