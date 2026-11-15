//! Audio Units, with the effects every macOS has (Apple's AUDelay and AULowpass).
#![cfg(target_os = "macos")]

use prowave_plugin_host::{load, scan_audio_units, Format, PluginInfo};

fn apple(name: &str) -> PluginInfo {
    scan_audio_units()
        .into_iter()
        .find(|info| info.vendor == "Apple" && info.name == name)
        .unwrap_or_else(|| panic!("macOS has {name}"))
}

fn sine(block: usize, left: &mut [f32], right: &mut [f32], hz: f32) {
    let len = left.len();
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = (block * len + i) as f32 / 48_000.0;
        *l = 0.5 * (t * hz * std::f32::consts::TAU).sin();
        *r = *l;
    }
}

#[test]
fn the_system_lists_its_effects() {
    let all = scan_audio_units();
    assert!(all.iter().all(|info| info.format == Format::AudioUnit));
    assert!(all.iter().any(|info| info.name == "AUDelay"));
}

/// The energy of an 8 kHz tone through AULowpass with its cutoff at `position`.
fn through_lowpass(position: f32) -> f64 {
    let instance = load(&apple("AULowpass"), 48_000.0, 64).expect("load");
    assert_eq!(instance.params()[0].name, "Cutoff Frequency");
    instance.set_param(0, position);
    let mut processor = instance.processor();
    let mut sum = 0.0f64;
    for block in 0..100 {
        let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
        sine(block, &mut l, &mut r, 8_000.0);
        processor.process(&mut l, &mut r);
        assert!(l.iter().chain(&r).all(|s| s.is_finite()));
        sum += l.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
    }
    sum
}

#[test]
fn a_lowpass_takes_the_highs_out_when_its_cutoff_is_low() {
    let open = through_lowpass(1.0);
    let closed = through_lowpass(0.02);
    assert!(open > 100.0, "an open filter passes 8 kHz ({open})");
    assert!(closed < open / 100.0, "a closed one does not ({closed})");
}

#[test]
fn state_round_trips() {
    let info = apple("AUDelay");
    let a = load(&info, 48_000.0, 64).expect("load");
    a.set_param(0, 0.25);
    let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
    a.processor().process(&mut l, &mut r);
    let state = a.save_state().expect("an Audio Unit has a state");
    let b = load(&info, 48_000.0, 64).expect("load");
    assert!(
        (b.param(0) - 0.25).abs() > 0.01,
        "a new unit has its default"
    );
    assert!(b.load_state(&state));
    assert!((b.param(0) - 0.25).abs() < 1e-3);
    assert!(!b.load_state(b"not a property list"));
}
