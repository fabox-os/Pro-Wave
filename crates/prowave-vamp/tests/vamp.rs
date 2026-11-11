//! The compiled C fixture: metadata, outputs, block analysis, parameters and errors.

use prowave_vamp::{Error, InputDomain, Library, SampleType};

/// The compiled fixture, or `None` when the build had no C compiler (CI Windows).
fn fixture() -> Option<Library> {
    let path = option_env!("VAMP_FIXTURE")?;
    Some(Library::load(path).expect("the test plug-in loads"))
}

#[test]
fn reads_metadata_and_outputs() {
    let Some(library) = fixture() else {
        return;
    };
    let plugins = library.plugins();
    assert_eq!(plugins.len(), 1);
    let plugin = &plugins[0];
    assert_eq!(plugin.api_version, 2);
    assert_eq!(plugin.identifier, "vamp_test_rms");
    assert_eq!(plugin.name, "Test RMS");
    assert_eq!(plugin.description, "Mean absolute level");
    assert_eq!(plugin.maker, "Pro Wave");
    assert_eq!(plugin.plugin_version, 1);
    assert_eq!(plugin.copyright, "");
    assert_eq!(plugin.input_domain, InputDomain::Time);
    assert_eq!(plugin.parameters.len(), 1);
    let parameter = &plugin.parameters[0];
    assert_eq!(parameter.identifier, "gain");
    assert_eq!(parameter.name, "Gain");
    assert_eq!(parameter.min, 0.0);
    assert_eq!(parameter.max, 2.0);
    assert_eq!(parameter.default, 1.0);
    assert!(!parameter.quantized);
    assert!(parameter.value_names.is_empty());
    assert_eq!(plugin.programs, ["Default"]);

    let instance = library.instantiate(0, 44_100).expect("instantiate");
    assert_eq!(instance.channel_range(), (1, 2));
    assert_eq!(instance.preferred_block_size(), Some(1024));
    assert_eq!(instance.preferred_step_size(), None, "the host picks");
    let outputs = instance.outputs().expect("outputs");
    assert_eq!(outputs.len(), 1);
    let output = &outputs[0];
    assert_eq!(output.identifier, "rms");
    assert_eq!(output.name, "RMS");
    assert_eq!(output.bin_count, 1);
    assert_eq!(output.bin_names, ["rms"]);
    assert!(output.has_known_extents);
    assert_eq!(output.min, 0.0);
    assert_eq!(output.max, 10.0);
    assert_eq!(output.sample_type, SampleType::OneSamplePerStep);
    assert!(!output.has_duration);
}

#[test]
fn analyses_a_block_and_reports_a_feature() {
    let Some(library) = fixture() else {
        return;
    };
    let mut instance = library.instantiate(0, 44_100).expect("instantiate");
    instance.initialize_preferred(1).expect("initialise");

    let block = [0.5f32; 1024];
    let features = instance
        .process(&[&block], 2, 500_000_000)
        .expect("analyse");
    assert_eq!(features.len(), 1);
    let feature = &features[0];
    assert_eq!(
        feature.timestamp,
        Some(2.5),
        "a feature without its own timestamp sits at the block start"
    );
    assert_eq!(feature.duration, None);
    assert_eq!(feature.label, None);
    assert_eq!(feature.values.len(), 1);
    assert!((feature.values[0] - 0.5).abs() < 1e-6);

    assert!(instance.finish().expect("finish").is_empty());
}

#[test]
fn controls_parameters_and_programs() {
    let Some(library) = fixture() else {
        return;
    };
    let mut instance = library.instantiate(0, 48_000).expect("instantiate");
    assert_eq!(instance.parameter(0), Some(1.0));
    instance.set_parameter(0, 2.0).expect("set gain");
    assert_eq!(instance.parameter(0), Some(2.0));
    assert_eq!(instance.current_program(), Some(0));
    assert!(instance.select_program(0).is_ok());

    instance.initialize(1, 512, 1024).expect("initialise");
    let block = [0.25f32; 1024];
    let features = instance.process(&[&block], 0, 0).expect("analyse");
    assert!(
        (features[0].values[0] - 0.5).abs() < 1e-6,
        "gain doubles it"
    );

    assert!(matches!(
        instance.set_parameter(0, 1.0),
        Err(Error::AlreadyInitialized(_))
    ));
    assert!(matches!(
        instance.select_program(0),
        Err(Error::AlreadyInitialized(_))
    ));
    assert!(matches!(
        instance.set_parameter(9, 1.0),
        Err(Error::BadParameter(9))
    ));

    let mut fresh = library.instantiate(0, 48_000).expect("instantiate");
    assert!(matches!(
        fresh.set_parameter(0, f32::NAN),
        Err(Error::BadValue)
    ));
    assert!(matches!(fresh.select_program(3), Err(Error::BadProgram(3))));
}

#[test]
fn refuses_bad_libraries_blocks_and_channels() {
    assert!(Library::load("/no/such/vamp/plugin.so").is_err());
    assert!(
        Library::load(file!()).is_err(),
        "a text file is not a library"
    );

    let Some(library) = fixture() else {
        return;
    };
    assert!(library.instantiate(9, 44_100).is_err());

    let mut instance = library.instantiate(0, 44_100).expect("instantiate");
    let block = [0.0f32; 1024];
    assert!(matches!(
        instance.process(&[&block], 0, 0),
        Err(Error::NotInitialized)
    ));
    assert!(matches!(
        instance.initialize(4, 512, 1024),
        Err(Error::ChannelRange {
            min: 1,
            max: 2,
            got: 4
        })
    ));
    instance.initialize(1, 512, 1024).expect("initialise");
    assert!(matches!(
        instance.process(&[&block, &block], 0, 0),
        Err(Error::ChannelCount {
            expected: 1,
            got: 2
        })
    ));
    assert!(matches!(
        instance.process(&[&block[..100]], 0, 0),
        Err(Error::BlockSize {
            expected: 1024,
            got: 100
        })
    ));
}
