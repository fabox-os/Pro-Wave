//! The compiled C fixture: metadata, controls, audio and a no-input generator.

use prowave_ladspa::{Library, PortKind};

/// The compiled fixture, or `None` when the build had no C compiler (CI Windows).
fn fixture() -> Option<Library> {
    let path = option_env!("LADSPA_FIXTURE")?;
    Some(Library::load(path).expect("the test plug-in loads"))
}

#[test]
fn reads_metadata_and_processes_audio() {
    let Some(library) = fixture() else {
        return;
    };
    let plugins = library.plugins();
    assert_eq!(plugins.len(), 2);
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.label == "gain")
        .expect("the gain fixture");
    assert_eq!(plugin.unique_id, 1_234_567);
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

    let index = plugins
        .iter()
        .position(|plugin| plugin.label == "gain")
        .expect("the gain index");
    let mut instance = library.instantiate(index, 48_000).expect("instantiate");
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

    // The block is the shortest of all input and output slices.
    let mut short = [9.0f32; 3];
    instance.set_control(2, 1.0).expect("unity gain");
    instance
        .process(&[&input], &mut [&mut short])
        .expect("a short output block");
    assert_eq!(short, [1.0, -1.0, 0.5]);

    assert!(
        instance.set_control(0, 0.5).is_err(),
        "an audio port is not a control"
    );
    assert!(instance.set_control(2, f32::NAN).is_err());
}

#[test]
fn a_generator_with_no_audio_inputs_runs() {
    let Some(library) = fixture() else {
        return;
    };
    let plugins = library.plugins();
    let index = plugins
        .iter()
        .position(|plugin| plugin.label == "constant")
        .expect("the constant fixture");
    let plugin = &plugins[index];
    assert_eq!(plugin.ports.len(), 2);
    assert_eq!(plugin.ports[0].kind, PortKind::AudioOutput);
    assert_eq!(plugin.ports[1].kind, PortKind::ControlInput);
    assert_eq!(
        plugin.ports[1].default, 1.0,
        "LADSPA_HINT_DEFAULT_1 is 0x240"
    );

    let mut instance = library.instantiate(index, 48_000).expect("instantiate");
    assert_eq!(instance.audio_port_counts(), (0, 1));
    let mut output = [0.0f32; 4];
    instance
        .process(&[], &mut [&mut output])
        .expect("generator block");
    assert_eq!(output, [1.0; 4]);

    instance.set_control(1, 0.25).expect("set level");
    instance
        .process(&[], &mut [&mut output])
        .expect("generator block");
    assert_eq!(output, [0.25; 4]);
}

#[test]
fn refuses_bad_libraries_and_port_counts() {
    assert!(Library::load("/no/such/ladspa/plugin.so").is_err());
    assert!(
        Library::load(file!()).is_err(),
        "a text file is not a library"
    );

    let Some(library) = fixture() else {
        return;
    };
    assert!(library.instantiate(9, 48_000).is_err());

    let mut instance = library.instantiate(0, 48_000).expect("instantiate");
    let input = [0.0f32; 4];
    let mut output = [0.0f32; 4];
    assert!(
        instance.process(&[&input], &mut []).is_err(),
        "missing output port"
    );
    assert!(
        instance
            .process(&[&input, &input], &mut [&mut output])
            .is_err(),
        "extra input port"
    );
}
