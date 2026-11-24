//! Repro of the Linux "no audio arrived from the input (…, N frames)" bug.
//! Mirrors the app: output engine running, input opened and delivering for a while,
//! then a recording started later (so the recording's `from_frame` is large).
use prowave_engine::{Capture, Engine, InputSource};
use std::time::{Duration, Instant};

fn main() {
    let _engine = Engine::start().expect("output engine");
    println!("output engine started: {}", _engine.device_name());

    let mut capture = Capture::open(None).expect("open input");
    println!(
        "input {} at {} Hz, {} channels",
        capture.device_name(),
        capture.sample_rate(),
        capture.channels()
    );

    // "Armed" for a while: no recording, frames accumulate.
    let armed = Instant::now() + Duration::from_secs(2);
    while Instant::now() < armed {
        std::thread::sleep(Duration::from_millis(100));
    }
    let before = capture.frames_received();
    println!("after arming 2 s: frames_received = {before}");

    let path = std::env::temp_dir().join("prowave-repro-take.wav");
    let id = capture
        .start(&path, InputSource::Mono(0), 0)
        .expect("start");
    println!(
        "recording started (from_frame = {})",
        capture.frames_received()
    );

    let rec = Instant::now() + Duration::from_secs(2);
    while Instant::now() < rec {
        std::thread::sleep(Duration::from_millis(100));
    }
    let heard = capture.frames_received();
    let done = capture.stop(id).expect("stop");
    println!("after recording 2 s: frames_received = {heard}");
    println!(
        "take: {} frames ({} s), lost = {}, latency_frames = {}",
        done.frames,
        done.frames as f64 / f64::from(done.sample_rate),
        done.lost,
        done.latency_frames
    );
    if done.frames == 0 {
        println!("BUG REPRODUCED: {} frames delivered, 0 written", heard);
    } else {
        println!("ok: the take holds audio");
    }
    let _ = std::fs::remove_file(&path);
}
