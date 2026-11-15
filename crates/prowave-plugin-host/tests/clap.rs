//! The compiled C fixture: a CLAP gain with one parameter, a latency and a state.

use prowave_plugin_host::{load, scan_file, Format, PluginInfo};
use std::path::Path;

/// The fixture's entry in a scan, or `None` when the build had no C compiler (CI Windows).
fn fixture() -> Option<PluginInfo> {
    let path = option_env!("CLAP_FIXTURE")?;
    let found = scan_file(Format::Clap, Path::new(path)).expect("the test plug-in scans");
    assert_eq!(found.len(), 1, "the instrument in the file is left out");
    found.into_iter().next()
}

#[test]
fn scans_loads_and_processes() {
    let Some(info) = fixture() else {
        return;
    };
    assert_eq!(info.id, "org.prowave.test-gain");
    assert_eq!(info.name, "Test Gain");
    assert_eq!(info.vendor, "Pro Wave");

    let instance = load(&info, 48_000.0, 64).expect("load");
    assert_eq!(instance.params().len(), 3);
    assert_eq!(instance.params()[0].name, "Gain");
    assert_eq!(instance.params()[0].default, 0.5);
    assert_eq!(instance.param(0), 0.5);
    assert_eq!(instance.param_text(0), "loud");
    assert_eq!(instance.latency_samples(), 7);

    let mut processor = instance.processor();
    let mut left = [0.5f32; 64];
    let mut right = [-0.25f32; 64];
    processor.process(&mut left, &mut right);
    assert_eq!((left[0], right[63]), (0.5, -0.25), "the default gain is 1");

    // A move on the main thread is heard at the next block.
    instance.set_param(0, 0.25);
    assert_eq!(instance.param(0), 0.25);
    assert_eq!(instance.param_text(0), "quiet");
    processor.process(&mut left, &mut right);
    assert_eq!((left[0], right[63]), (0.25, -0.125));

    // Automation on the audio thread, through a second handle on the same plug-in.
    let mut other = processor.clone();
    other.set_param_norm(0, 1.0);
    let mut l = [0.5f32; 16];
    let mut r = [0.5f32; 16];
    other.process(&mut l, &mut r);
    assert_eq!((l[0], r[15]), (1.0, 1.0));
}

#[test]
fn state_round_trips() {
    let Some(info) = fixture() else {
        return;
    };
    let a = load(&info, 44_100.0, 32).expect("load");
    a.set_param(0, 0.75);
    let (mut l, mut r) = ([1.0f32; 32], [1.0f32; 32]);
    a.processor().process(&mut l, &mut r);
    let state = a.save_state().expect("the plug-in has a state");
    assert_eq!(state, 1.5f64.to_ne_bytes());

    let b = load(&info, 44_100.0, 32).expect("load");
    assert!(b.load_state(&state));
    assert_eq!(
        b.param(0),
        0.75,
        "the parameters are read again after a load"
    );
    assert!(!b.load_state(&[1, 2, 3]), "a short state is refused");
}

#[test]
fn bad_input_is_an_error_not_a_crash() {
    let missing = Path::new("/nonexistent/none.clap");
    assert!(scan_file(Format::Clap, missing).is_err());
    assert!(scan_file(Format::Vst3, missing).is_err());
    let Some(mut info) = fixture() else {
        return;
    };
    info.id = "org.prowave.unknown".into();
    assert!(load(&info, 48_000.0, 64).is_err());
    info.format = Format::Vst3;
    info.id = "not hexadecimal".into();
    assert!(load(&info, 48_000.0, 64).is_err());
}
