//! LV2 load/process smoke test for plug-ins installed on this machine.
//! Run with `cargo test -p prowave-plugin-host --test lv2_load -- --ignored --nocapture`;
//! `PROWAVE_TEST_PLUGINS` narrows the bundles scanned to those whose path contains it.
//! `PROWAVE_TEST_PLUGIN_NAME` is a comma-separated list of name fragments, limiting which
//! plug-ins are loaded. `PROWAVE_TEST_LIMIT` caps how many plug-ins are processed

use prowave_plugin_host::{installed, load, scan_file, Error, Format};

#[test]
#[ignore = "needs LV2 plug-ins installed on this machine"]
fn lv2_plugins_load_and_process() {
    let only = std::env::var("PROWAVE_TEST_PLUGINS").unwrap_or_default();
    let names: Vec<String> = std::env::var("PROWAVE_TEST_PLUGIN_NAME")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let limit = std::env::var("PROWAVE_TEST_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let mut tested = 0;
    'outer: for path in installed(Format::Lv2) {
        if tested >= limit {
            break;
        }
        if !path.to_string_lossy().contains(&only) {
            continue;
        }
        let found = match scan_file(Format::Lv2, &path) {
            Ok(found) => found,
            Err(error) => {
                println!("{}: {error}", path.display());
                continue;
            }
        };
        for info in found {
            if !names.is_empty() && !names.iter().any(|n| info.name.contains(n)) {
                continue;
            }
            let instance = match load(&info, 48_000.0, 64) {
                Ok(instance) => instance,
                // Instruments and MIDI effects have no audio input, so they are not effects this
                // host's audio-path test can drive.
                Err(Error::NotAnEffect) => {
                    println!("SKIP (not an audio effect): {}", info.name);
                    continue;
                }
                Err(error) => panic!("{} ({}): {error}", info.name, path.display()),
            };
            let mut processor = instance.processor();
            for block in 0..128 {
                let mut left = [0.0f32; 64];
                let mut right = [0.0f32; 64];
                for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                    let t = (block * 64 + i) as f32 / 48_000.0;
                    *l = 0.25 * (t * 440.0 * std::f32::consts::TAU).sin();
                    *r = *l;
                }
                processor.process(&mut left, &mut right);
                assert!(
                    left.iter().chain(&right).all(|s| s.is_finite()),
                    "{} produced non-finite samples in block {block}: left={left:?} right={right:?}",
                    info.name
                );
            }
            println!("LV2 OK: {} ({} params)", info.name, instance.params().len());
            tested += 1;
            if tested >= limit {
                break 'outer;
            }
        }
    }
    if tested == 0 {
        println!("No installed LV2 plug-in matched {only:?}");
    }
}
