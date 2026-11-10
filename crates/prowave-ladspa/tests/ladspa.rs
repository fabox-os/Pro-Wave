//! The compiled C fixture: metadata, control and audio through the real LADSPA ABI.

use prowave_ladspa::{Library, PortKind};

fn fixture() -> Library {
    Library::load(env!("LADSPA_FIXTURE")).expect("the test plug-in loads")
}

#[test]
fn reads_metadata_and_processes_audio() {
    let library = fixture();
    let plugins = library.plugins();
    assert_eq!(plugins.len(), 1);
    let plugin = &plugins[0];
    assert_eq!(plugin.unique_id, 1_234_567);
    assert_eq!(plugin.label, "gain");
    assert_eq!(plugin.name, "Test Gain");
    assert_eq!(plugin.maker, "Pro Wave");
    assert_eq!(plugin.ports.len(), 3);
    assert_eq!(plugin.ports[0].kind, PortKind::AudioInput);
    assert_eq!(plugin.ports[1].kind, PortKind::AudioOutput);
    assert_eq!(plugin.ports[2].kind, PortKind::ControlInput);
    assert_eq!(plugin.ports[2].index, 2);
    assert_eq!(plugin.ports[2].min, 0.0);
    assert_eq!(plugin.ports[2].max, 1.0);
    assert_eq!(plugin.ports[2].default, 0.0);

    let mut instance = library.instantiate(0, 48_000).expect("instantiate");
    assert_eq!(instance.audio_port_counts(), (1, 1));

    let input = [1.0f32, -1.0, 0.5, 0.0];
    let mut output = [9.0f32; 4];
    instance
        .process(&[&input], &mut [&mut output])
        .expect("first block");
    assert_eq!(output, [0.0, 0.0, 0.0, 0.0], "the default gain is zero");

    instance.set_control(2, 0.5).expect("set gain");
    instance
        .process(&[&input], &mut [&mut output])
        .expect("second block");
    assert_eq!(output, [0.5, -0.5, 0.25, 0.0]);
    assert_eq!(instance.control(2), Some(0.5));

    assert!(
        instance.set_control(0, 0.5).is_err(),
        "an audio port is not a control"
    );
    assert!(instance.set_control(2, f32::NAN).is_err());
}

#[test]
fn refuses_bad_libraries_and_buffers() {
    assert!(Library::load("/no/such/ladspa/plugin.so").is_err());
    assert!(
        Library::load(file!()).is_err(),
        "a text file is not a library"
    );

    let library = fixture();
    assert!(library.instantiate(9, 48_000).is_err());

    let mut instance = library.instantiate(0, 48_000).expect("instantiate");
    let input = [0.0f32; 4];
    let mut short = [0.0f32; 3];
    assert!(instance.process(&[&input], &mut [&mut short]).is_err());
    let mut output = [0.0f32; 4];
    assert!(instance
        .process(&[&input, &input], &mut [&mut output])
        .is_err());
}
