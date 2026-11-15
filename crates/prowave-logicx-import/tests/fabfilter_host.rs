//! Loads the FabFilter Pro-Q 4 state that `read_full` finds in the real project into the real
//! Audio Unit, through the plug-in host's own `load_state`.
//!
//! macOS only, ignored by default: it needs the real project on this Mac and Pro-Q 4 installed.
//! Run with `cargo test -p prowave-logicx-import --test fabfilter_host -- --ignored --nocapture`.

#![cfg(target_os = "macos")]

use prowave_logicx_import::{read_full, PluginFormat, TrackKind};
use prowave_plugin_host::{Format, PluginInfo};
use std::fs;
use std::path::Path;

const REAL_PROJECT: &str = "/Users/maximerandazzo/Downloads/Projet.logicx";

/// The Pro-Q 4 codes as the host names them: `type:subtype:manufacturer`, each a big-endian
/// four-character code in eight hexadecimal digits (`aumf`, `FQ4p`, `FabF`).
const PRO_Q_4_ID: &str = "61756d66:46513470:46616246";

#[test]
#[ignore = "needs the real project and Pro-Q 4 on this Mac"]
fn the_project_state_loads_into_pro_q_4() {
    let real = Path::new(REAL_PROJECT);
    if !real.join("Alternatives/000/ProjectData").exists() {
        eprintln!("skipping: {REAL_PROJECT} is not on this machine");
        return;
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    let bundle = dir.path().join("Projet.logicx");
    for relative in [
        "Alternatives/000/MetaData.plist",
        "Alternatives/000/ProjectData",
        "Resources/ProjectInformation.plist",
    ] {
        let target = bundle.join(relative);
        fs::create_dir_all(target.parent().expect("a parent")).expect("the folder");
        fs::copy(real.join(relative), target).expect("the copy");
    }

    let project = read_full(&bundle).expect("read_full");
    let inst = project
        .tracks
        .iter()
        .find(|t| t.name == "Inst 1" && t.kind == TrackKind::Instrument)
        .expect("the Inst 1 strip");
    let slot = inst
        .plugins
        .iter()
        .find(|p| p.name == "Pro-Q 4" && p.format == PluginFormat::AudioUnit)
        .expect("the Pro-Q 4 slot");
    let state = slot.state.clone().expect("the class-info state");

    let info = PluginInfo {
        format: Format::AudioUnit,
        id: PRO_Q_4_ID.to_string(),
        name: "Pro-Q 4".to_string(),
        vendor: "FabFilter".to_string(),
        path: Default::default(),
    };
    let fresh =
        prowave_plugin_host::load(&info, 48_000.0, 512).expect("Pro-Q 4 loads: is it installed?");
    let loaded = prowave_plugin_host::load(&info, 48_000.0, 512).expect("a second instance");

    let before: Vec<f32> = (0..fresh.params().len()).map(|i| fresh.param(i)).collect();
    let accepted = loaded.load_state(&state);
    let after: Vec<f32> = (0..loaded.params().len())
        .map(|i| loaded.param(i))
        .collect();
    let changed = before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| (*a - *b).abs() > 1e-6)
        .count();
    eprintln!(
        "load_state accepted: {accepted}; {} parameters, {changed} changed from a fresh instance",
        before.len()
    );
    assert!(accepted, "Pro-Q 4 accepted the project's state");
    assert!(
        loaded.save_state().is_some(),
        "the loaded instance can save its state again"
    );
}
