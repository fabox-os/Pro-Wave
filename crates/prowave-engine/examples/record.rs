//! Tries audio recording without the app: `cargo run -p prowave-engine --example record`
//! lists the audio inputs, then records 3 seconds from the default input (Input 1) into
//! `record-test.wav` and shows the level of each channel while it does.
//!
//! `--input N` picks input N of the list, `--seconds S` records for S seconds, `--channel C`
//! records channel C (1 is the first), `--out FILE` names the file.
use prowave_engine::{input_devices, AudioFile, Capture, InputSource};
use std::time::{Duration, Instant};

fn arg<T: std::str::FromStr>(name: &str) -> Option<T> {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1)?.parse().ok()
}

fn main() {
    let devices = input_devices();
    if devices.is_empty() {
        eprintln!("no audio input found");
        std::process::exit(1);
    }
    for (i, d) in devices.iter().enumerate() {
        let default = if d.is_default { "  (default)" } else { "" };
        println!("{}: {} - {} channels{default}", i + 1, d.name, d.channels);
    }
    let pick = arg::<usize>("--input").unwrap_or(1).clamp(1, devices.len()) - 1;
    let seconds = arg::<u64>("--seconds").unwrap_or(3);
    let channel = arg::<u16>("--channel").unwrap_or(1).max(1) - 1;
    let out = arg::<String>("--out").unwrap_or_else(|| "record-test.wav".into());

    let mut capture = match Capture::open(Some(&devices[pick].id)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot open {}: {e}", devices[pick].name);
            std::process::exit(1);
        }
    };
    println!(
        "\nopened {} at {} Hz, {} channels",
        capture.device_name(),
        capture.sample_rate(),
        capture.channels()
    );
    let id = match capture.start(out.as_ref(), InputSource::Mono(channel), 0) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("cannot record: {e}");
            std::process::exit(1);
        }
    };
    println!("recording {seconds} s into {out}: make some noise");
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut peaks = Vec::new();
    let mut loudest = 0f32;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        capture.take_peaks(&mut peaks);
        let level = peaks.get(usize::from(channel)).copied().unwrap_or(0.0);
        loudest = loudest.max(level);
        let bar = "#".repeat((level.min(1.0) * 40.0) as usize);
        println!("{bar:<40} {level:.3}");
    }
    match capture.stop(id) {
        Ok(done) => {
            let secs = done.frames as f64 / f64::from(done.sample_rate);
            println!("\nwrote {} ({secs:.2} s)", done.path.display());
            if done.lost {
                println!("warning: some audio was lost");
            }
            println!("device glitches: {}", capture.xruns());
            match AudioFile::open(&done.path) {
                Ok(f) => println!("reads back: {} frames, {} Hz", f.frames, f.sample_rate),
                Err(e) => println!("cannot read it back: {e}"),
            }
        }
        Err(e) => eprintln!("cannot finish the recording: {e}"),
    }
    if loudest == 0.0 {
        println!("the input was silent: check that the right input is selected and not muted");
    }
}
