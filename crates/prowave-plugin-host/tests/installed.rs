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
    for format in [Format::Clap, Format::Vst3, Format::Lv2] {
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

/// Opens the own window of every installed plug-in whose name contains `PROWAVE_TEST_PLUGINS`,
/// each in its own process, and keeps them on screen for `PROWAVE_TEST_SECONDS` (default 5).
#[cfg(unix)]
#[test]
#[ignore = "needs plug-ins installed on this computer, and a screen"]
fn installed_plugins_show_their_own_window() {
    let only = std::env::var("PROWAVE_TEST_PLUGINS").unwrap_or_default();
    let seconds = std::env::var("PROWAVE_TEST_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let program = std::path::Path::new(env!("CARGO_BIN_EXE_prowave-plugin-host"));
    let mut found: Vec<PluginInfo> = scan_audio_units();
    for format in [Format::Clap, Format::Vst3, Format::Lv2] {
        for path in installed(format) {
            found.extend(scan_file(format, &path).unwrap_or_default());
        }
    }
    let mut open = Vec::new();
    for info in found.iter().filter(|info| info.name.contains(&only)) {
        match prowave_plugin_host::load_sandboxed(info, 48_000.0, 64, program) {
            Ok(instance) => {
                println!(
                    "{:?} {}: editor {}",
                    info.format,
                    info.name,
                    instance.has_editor()
                );
                instance.open_editor(&format!("{:?} · {}", info.format, info.name), false);
                open.push(instance);
            }
            Err(error) => println!("{:?} {}: {error}", info.format, info.name),
        }
    }
    let mut processors: Vec<_> = open.iter().map(|i| i.processor()).collect();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < until {
        for processor in &mut processors {
            let (mut l, mut r) = ([0.1f32; 64], [0.1f32; 64]);
            processor.process(&mut l, &mut r);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    for instance in &open {
        println!(
            "{:?} {}: window open {}, crashed {}",
            instance.info().format,
            instance.info().name,
            instance.editor_open(),
            instance.crashed()
        );
    }
}

#[cfg(target_os = "linux")]
fn assert_editor_is_available(format: Format) {
    let display = std::env::var_os("DISPLAY").expect("run this test on an X11 display");
    assert!(
        !display.to_string_lossy().is_empty(),
        "DISPLAY is set but empty"
    );
    let only = std::env::var("PROWAVE_TEST_PLUGINS").unwrap_or_default();
    let program = std::path::Path::new(env!("CARGO_BIN_EXE_prowave-plugin-host"));
    for path in installed(format) {
        if !path.to_string_lossy().contains(&only) {
            continue;
        }
        let found = match scan_file(format, &path) {
            Ok(found) => found,
            Err(error) => {
                println!("{}: {error}", path.display());
                continue;
            }
        };
        for info in found {
            match prowave_plugin_host::load_sandboxed(&info, 48_000.0, 64, program) {
                Ok(instance) => {
                    if instance.has_editor() {
                        println!("{:?} {}: editor available", info.format, info.name);
                        return;
                    }
                }
                Err(error) => println!("{:?} {}: {error}", info.format, info.name),
            }
        }
    }
    panic!("no installed {format:?} plug-in reported an editor on DISPLAY");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs CLAP plug-ins installed on this computer, and a screen"]
fn clap_editor_is_available_on_x11() {
    assert_editor_is_available(Format::Clap);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs VST3 plug-ins installed on this computer, and a screen"]
fn vst3_editor_is_available_on_x11() {
    assert_editor_is_available(Format::Vst3);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs LV2 plug-ins installed on this computer, and a screen"]
fn lv2_editor_is_available_on_x11() {
    assert_editor_is_available(Format::Lv2);
}
