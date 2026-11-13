//! Loads one installed Audio Unit and runs a second of audio through it, for checking the
//! whole path (helper, loading, parameters, state, realtime adapter) by hand.
//!
//! `cargo run -p prowave-plugin-host --example probe`

use std::time::{Duration, Instant};

fn main() {
    let catalog = match prowave_plugin_host::scan(&[]) {
        Ok(catalog) => catalog,
        Err(error) => {
            println!("scan failed: {error}");
            return;
        }
    };
    let Some(plugin) = catalog
        .plugins()
        .into_iter()
        .find(|plugin| plugin.name == "AUPeakLimiter")
    else {
        println!("AUPeakLimiter is not installed");
        return;
    };
    println!("loading {} ({})", plugin.name, plugin.format.badge());
    let quantum = prowave_plugin_host::DEFAULT_QUANTUM;
    let mut hosted = match catalog.instantiate(&plugin.key, 48_000.0, quantum, None) {
        Ok(hosted) => hosted,
        Err(error) => {
            println!("load failed: {error}");
            return;
        }
    };
    println!(
        "latency {} frames, {} parameters",
        hosted.latency,
        hosted.parameters.len()
    );
    for parameter in hosted.parameters.iter().take(8) {
        println!(
            "  {:<28} {:>8.3} {}",
            parameter.title,
            parameter.value.unwrap_or(f64::NAN),
            parameter.units
        );
    }
    match hosted.control.state() {
        Ok(state) => println!("state: {} bytes", state.len()),
        Err(error) => println!("state failed: {error}"),
    }

    // One second of a 220 Hz sine at 0.5, paced at the quantum's real time so the helper's
    // realtime ring behaves as it would under the engine.
    let frames = 48_000;
    let mut peak_in = 0.0f32;
    let mut peak_out = 0.0f32;
    let mut phase = 0.0f32;
    let started = Instant::now();
    for frame in 0..frames {
        let sample = 0.5 * (phase * std::f32::consts::TAU).sin();
        phase = (phase + 220.0 / 48_000.0).fract();
        peak_in = peak_in.max(sample.abs());
        let (left, right) = hosted.effect.process(sample, sample);
        if frame > hosted.latency as usize + quantum {
            peak_out = peak_out.max(left.abs()).max(right.abs());
        }
        if frame % quantum == quantum - 1 {
            let target = Duration::from_secs_f64((frame + 1) as f64 / 48_000.0);
            let elapsed = started.elapsed();
            if target > elapsed {
                std::thread::sleep(target - elapsed);
            }
        }
        if frame == frames - 1 {
            println!("last output: {left:.4} {right:.4}");
        }
    }
    println!("peak in {peak_in:.3}, peak out {peak_out:.3}");
}
