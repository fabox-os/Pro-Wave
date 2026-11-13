use super::*;
use plughost_core::{BarPosition, TimeSignature, Transport};

fn transport() -> Transport {
    Transport::new(0, true)
}

fn nothing() -> Outputs {
    Outputs {
        tempo: std::ptr::null_mut(),
        numerator: std::ptr::null_mut(),
        denominator: std::ptr::null_mut(),
        beat: std::ptr::null_mut(),
        to_next_beat: std::ptr::null_mut(),
        downbeat: std::ptr::null_mut(),
    }
}

#[test]
fn a_tempo_alone_reaches_a_unit_that_asks_only_for_the_tempo() {
    let tempo_only = {
        let mut transport = transport();
        transport.tempo = Some(90.0);
        transport
    };
    let mut tempo = 0.0;
    let result = fill(
        Some(tempo_only),
        48_000.0,
        Outputs {
            tempo: &mut tempo,
            ..nothing()
        },
    );
    assert_eq!((result, tempo), (Bool::YES, 90.0));

    let mut beat = -1.0;
    let result = fill(
        Some(tempo_only),
        48_000.0,
        Outputs {
            tempo: &mut tempo,
            beat: &mut beat,
            ..nothing()
        },
    );
    assert_eq!((result, beat), (Bool::NO, -1.0));
    assert_eq!(fill(None, 48_000.0, nothing()), Bool::NO);
}

#[test]
fn a_full_transport_fills_every_value() {
    let full = {
        let mut transport = transport();
        transport.tempo = Some(120.0);
        transport.beat_position = Some(4.5);
        transport.bar_position = Some(BarPosition::new(4.0, 1));
        transport.time_signature = Some(TimeSignature::new(3, 4));
        transport
    };
    let (mut tempo, mut numerator, mut denominator) = (0.0, 0.0, 0);
    let (mut beat, mut to_next_beat, mut downbeat) = (0.0, 0, 0.0);
    let result = fill(
        Some(full),
        48_000.0,
        Outputs {
            tempo: &mut tempo,
            numerator: &mut numerator,
            denominator: &mut denominator,
            beat: &mut beat,
            to_next_beat: &mut to_next_beat,
            downbeat: &mut downbeat,
        },
    );
    assert_eq!(result, Bool::YES);
    assert_eq!(
        (tempo, numerator, denominator, beat, to_next_beat, downbeat),
        (120.0, 3.0, 4, 4.5, 12_000, 4.0)
    );
}
