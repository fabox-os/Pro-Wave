//! The compiled C fixture: the metadata the loader reads, and bad libraries.
#![cfg(feature = "in-process")]

use prowave_dssi::{Library, PortKind};

/// The compiled fixture, or `None` when the build had no C compiler (CI Windows).
fn fixture() -> Option<Library> {
    let path = option_env!("DSSI_FIXTURE")?;
    Some(Library::load(path).expect("the test plug-in loads"))
}

fn synth_index(library: &Library) -> usize {
    library
        .plugins()
        .iter()
        .position(|plugin| plugin.label == "dssi_test_synth")
        .expect("the synth fixture")
}

#[test]
fn reads_metadata() {
    let Some(library) = fixture() else {
        return;
    };
    let plugins = library.plugins();
    assert_eq!(plugins.len(), 2);
    let plugin = &plugins[synth_index(&library)];
    assert_eq!(plugin.api_version, 1);
    assert_eq!(plugin.unique_id, 7_654_321);
    assert_eq!(plugin.name, "Test Synth");
    assert_eq!(plugin.maker, "Pro Wave");
    assert!(plugin.has_programs);
    assert!(plugin.has_configure);
    assert!(plugin.has_run_synth);
    assert_eq!(plugin.ports.len(), 4);
    assert_eq!(plugin.ports[0].kind, PortKind::AudioOutput);
    assert_eq!(plugin.ports[0].name, "Out");
    assert_eq!(plugin.ports[1].kind, PortKind::ControlInput);
    assert_eq!(plugin.ports[1].min, Some(0.0));
    assert_eq!(plugin.ports[1].max, Some(1.0));
    assert_eq!(plugin.ports[1].default, 0.5, "LADSPA_HINT_DEFAULT_MIDDLE");
    assert_eq!(plugin.ports[2].kind, PortKind::ControlOutput);
    assert_eq!(plugin.ports[3].kind, PortKind::ControlOutput);

    let dc = plugins
        .iter()
        .find(|plugin| plugin.label == "dssi_test_dc")
        .expect("the generator fixture");
    assert!(!dc.has_programs);
    assert!(!dc.has_configure);
    assert!(!dc.has_run_synth);
    assert_eq!(dc.ports.len(), 1);
}

#[test]
fn refuses_files_that_are_not_libraries() {
    assert!(Library::load("/no/such/dssi/plugin.so").is_err());
    let text = std::env::temp_dir().join(format!(
        "prowave-dssi-not-a-library-{}.so",
        std::process::id()
    ));
    std::fs::write(&text, b"this text file is not a shared library").expect("write the file");
    let not_a_library = Library::load(&text);
    let _ = std::fs::remove_file(&text);
    assert!(
        not_a_library.is_err(),
        "a text file with a library's name is not a library"
    );
}
