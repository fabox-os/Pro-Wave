//! The compiled C fixture: metadata, MIDI rendering, programs, configuration and errors.

use prowave_dssi::{Error, Event, Library, PortKind, TimedEvent, DSSI_CC_BITS, MAX_EVENTS};

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
    assert_eq!(plugin.ports[1].min, 0.0);
    assert_eq!(plugin.ports[1].max, 1.0);
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
fn delivers_midi_events_at_the_frames_the_host_asks_for() {
    let Some(library) = fixture() else {
        return;
    };
    let mut instance = library
        .instantiate(synth_index(&library), 48_000)
        .expect("instantiate");
    assert_eq!(instance.audio_port_counts(), (0, 1));

    let mut output = [9.0f32; 4];
    instance.process(&[], &mut [&mut output]).expect("silence");
    assert_eq!(output, [0.0; 4], "no note has been played");

    let events = [
        TimedEvent {
            frame: 2,
            event: Event::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        },
        TimedEvent {
            frame: 3,
            event: Event::ControlChange {
                channel: 0,
                controller: 7,
                value: 90,
            },
        },
    ];
    instance
        .process_with_events(&[], &mut [&mut output], &events)
        .expect("note block");
    let expected = 0.5 * 100.0 / 127.0;
    for sample in output {
        assert!(
            (sample - expected).abs() < 1e-6,
            "{sample} is not {expected}"
        );
    }
    assert_eq!(instance.control(1), Some(0.5), "the Volume default");
    assert_eq!(instance.control(2), Some(3.0), "the last event frame");
    assert_eq!(instance.control(3), Some(60.0), "the note number");

    instance.set_control(1, 0.75).expect("set volume");
    instance
        .process_with_events(
            &[],
            &mut [&mut output],
            &[TimedEvent {
                frame: 0,
                event: Event::NoteOff {
                    channel: 0,
                    note: 60,
                    velocity: 0,
                },
            }],
        )
        .expect("note-off block");
    assert_eq!(output, [0.0; 4], "the note was released");

    instance
        .process_with_events(
            &[],
            &mut [&mut output],
            &[TimedEvent {
                frame: 0,
                event: Event::NoteOn {
                    channel: 0,
                    note: 64,
                    velocity: 127,
                },
            }],
        )
        .expect("note block");
    for sample in output {
        assert!((sample - 0.75).abs() < 1e-6, "{sample} is not 0.75");
    }

    instance.set_control(1, 5.0).expect("clamp volume");
    assert_eq!(instance.control(1), Some(1.0));
}

#[test]
fn lists_and_selects_programs() {
    let Some(library) = fixture() else {
        return;
    };
    let mut instance = library
        .instantiate(synth_index(&library), 48_000)
        .expect("instantiate");
    let programs = instance.programs();
    assert_eq!(programs.len(), 2);
    assert_eq!(programs[0].bank, 0);
    assert_eq!(programs[0].program, 0);
    assert_eq!(programs[0].name, "Init");
    assert_eq!(programs[1].bank, 1);
    assert_eq!(programs[1].program, 2);
    assert_eq!(programs[1].name, "Bright");

    instance.select_program(1, 2).expect("select Bright");
    assert_eq!(
        instance.control(1),
        Some(0.25),
        "the synth rewrote its Volume control, and the host sees it"
    );

    assert!(instance.configure("DSSI:TEST", "1").is_ok());
    match instance.configure("NoSuchKey", "1") {
        Err(Error::Configured(message)) => assert_eq!(message, "unknown configure key"),
        other => panic!("expected the plug-in's message, got {other:?}"),
    }

    assert_eq!(
        instance.midi_controller_for_port(1),
        Some(DSSI_CC_BITS | 7),
        "Volume follows CC 7"
    );
    assert_eq!(instance.midi_controller_for_port(0), None);
}

#[test]
fn a_synth_without_dssi_extensions_uses_ladspa_run() {
    let Some(library) = fixture() else {
        return;
    };
    let index = library
        .plugins()
        .iter()
        .position(|plugin| plugin.label == "dssi_test_dc")
        .expect("the generator fixture");
    let mut instance = library.instantiate(index, 48_000).expect("instantiate");
    assert!(instance.programs().is_empty());
    assert!(instance.select_program(0, 0).is_err());
    assert!(instance.configure("K", "V").is_err());
    assert_eq!(instance.midi_controller_for_port(0), None);

    let mut output = [0.0f32; 4];
    instance.process(&[], &mut [&mut output]).expect("render");
    assert_eq!(output, [0.25; 4]);

    let events = [TimedEvent {
        frame: 0,
        event: Event::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    }];
    match instance.process_with_events(&[], &mut [&mut output], &events) {
        Err(Error::Unsupported("run_synth")) => {}
        other => panic!("expected run_synth to be missing, got {other:?}"),
    }
}

#[test]
fn refuses_bad_libraries_blocks_and_events() {
    assert!(Library::load("/no/such/dssi/plugin.so").is_err());
    assert!(
        Library::load(file!()).is_err(),
        "a text file is not a library"
    );

    let Some(library) = fixture() else {
        return;
    };
    assert!(library.instantiate(9, 48_000).is_err());

    let mut instance = library
        .instantiate(synth_index(&library), 48_000)
        .expect("instantiate");
    let mut output = [0.0f32; 4];
    let input = [0.0f32; 4];
    assert!(
        instance.process(&[&input], &mut [&mut output]).is_err(),
        "the synth has no audio input"
    );
    assert!(
        instance.process(&[], &mut []).is_err(),
        "the output port is missing"
    );
    assert!(
        instance.set_control(0, 0.5).is_err(),
        "an audio port is not a control"
    );
    assert!(instance.set_control(1, f32::NAN).is_err());

    let unordered = [
        TimedEvent {
            frame: 5,
            event: Event::NoteOn {
                channel: 0,
                note: 60,
                velocity: 1,
            },
        },
        TimedEvent {
            frame: 3,
            event: Event::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0,
            },
        },
    ];
    assert!(matches!(
        instance.process_with_events(&[], &mut [&mut output], &unordered),
        Err(Error::UnorderedEvents {
            previous: 5,
            frame: 3
        })
    ));

    let too_many: Vec<TimedEvent> = (0..=MAX_EVENTS)
        .map(|frame| TimedEvent {
            frame: frame as u32,
            event: Event::NoteOn {
                channel: 0,
                note: 60,
                velocity: 1,
            },
        })
        .collect();
    assert!(matches!(
        instance.process_with_events(&[], &mut [&mut output], &too_many),
        Err(Error::TooManyEvents {
            max: MAX_EVENTS,
            ..
        })
    ));
}
