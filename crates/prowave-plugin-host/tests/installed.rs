//! Plug-ins that happen to be installed on this computer: run with
//! `cargo test -p prowave-plugin-host --test installed -- --ignored --nocapture`.
//! `PROWAVE_TEST_PLUGINS` narrows the files scanned to those whose path contains it.

use prowave_plugin_host::{installed, load, scan_audio_units, scan_file, Format, PluginInfo};

fn exercise(info: &PluginInfo) {
    let instance = match load(info, 48_000.0, 64) {
        Ok(instance) => instance,
        Err(error) => {
            println!("  {:?} {}: {error}", info.format, info.name);
            return;
        }
    };
    let mut processor = instance.processor();
    let mut energy = 0.0f64;
    for block in 0..200 {
        let mut left = [0.0f32; 64];
        let mut right = [0.0f32; 64];
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let t = (block * 64 + i) as f32 / 48_000.0;
            *l = 0.25 * (t * 440.0 * std::f32::consts::TAU).sin();
            *r = *l;
        }
        if block == 50 && !instance.params().is_empty() {
            instance.set_param(0, 0.75);
        }
        processor.process(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|s| s.is_finite()));
        energy += left.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
    }
    let state = instance.save_state();
    let restored = state.as_ref().map(|s| instance.load_state(s));
    println!(
        "  {:?} {} ({}): {} params, latency {}, energy {energy:.3}, state {:?} bytes, restored {restored:?}, first {:?} = {:?}",
        info.format,
        info.name,
        info.vendor,
        instance.params().len(),
        instance.latency_samples(),
        state.map(|s| s.len()),
        instance.params().first().map(|p| p.name.clone()),
        instance.param_text(0),
    );
}

#[test]
#[ignore = "needs plug-ins installed on this computer"]
fn installed_plugins_load_and_process() {
    let only = std::env::var("PROWAVE_TEST_PLUGINS").unwrap_or_default();
    for format in [Format::Clap, Format::Vst3] {
        for path in installed(format) {
            if !path.to_string_lossy().contains(&only) {
                continue;
            }
            println!("{}", path.display());
            match scan_file(format, &path) {
                Ok(found) => found.iter().for_each(exercise),
                Err(error) => println!("  {error}"),
            }
        }
    }
    for info in scan_audio_units() {
        if info.name.contains(&only) || info.vendor.contains(&only) {
            exercise(&info);
        }
    }
}
