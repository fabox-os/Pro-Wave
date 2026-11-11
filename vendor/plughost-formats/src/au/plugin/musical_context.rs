//! The musical context an Audio Unit asks the host for while it renders.
use objc2::runtime::Bool;
use objc2_foundation::NSInteger;

use super::put;

/// Where an Audio Unit wants its musical context; each is null when the unit does not need it.
pub(super) struct Outputs {
    pub tempo: *mut f64,
    pub numerator: *mut f64,
    pub denominator: *mut NSInteger,
    pub beat: *mut f64,
    pub to_next_beat: *mut NSInteger,
    pub downbeat: *mut f64,
}

/// Fills the musical context an Audio Unit asked for from the block's transport. It fails only
/// when a value the unit asked for is unknown; a tempo alone still reaches a unit that asks only
/// for the tempo.
pub(super) fn fill(
    transport: Option<plughost_core::Transport>,
    sample_rate: f64,
    outputs: Outputs,
) -> Bool {
    let Some(value) = transport else {
        return Bool::NO;
    };
    let asked = |known: bool, pointers: &[bool]| known || pointers.iter().all(|&null| null);
    let (tempo, meter, beats, bar) = (
        value.tempo,
        value.time_signature,
        value.beat_position,
        value.bar_position,
    );
    if !asked(tempo.is_some(), &[outputs.tempo.is_null()])
        || !asked(
            meter.is_some(),
            &[outputs.numerator.is_null(), outputs.denominator.is_null()],
        )
        || !asked(beats.is_some(), &[outputs.beat.is_null()])
        || !asked(
            beats.is_some() && tempo.is_some(),
            &[outputs.to_next_beat.is_null()],
        )
        || !asked(bar.is_some(), &[outputs.downbeat.is_null()])
    {
        return Bool::NO;
    }
    if let Some(bpm) = tempo {
        put(outputs.tempo, bpm);
    }
    if let Some(meter) = meter {
        put(outputs.numerator, f64::from(meter.numerator));
        put(outputs.denominator, meter.denominator as NSInteger);
    }
    if let Some(beats) = beats {
        put(outputs.beat, beats);
        if let Some(bpm) = tempo {
            let samples_per_beat = sample_rate * 60.0 / bpm;
            put(
                outputs.to_next_beat,
                ((beats.ceil() - beats) * samples_per_beat) as NSInteger,
            );
        }
    }
    if let Some(bar) = bar {
        put(outputs.downbeat, bar.start);
    }
    Bool::YES
}

#[cfg(test)]
#[path = "musical_context_tests.rs"]
mod tests;
