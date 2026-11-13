//! Portable note expressions as VST3 note expression values. VST3 values are normalized; the
//! standard types map them to the portable plain ranges. Pressure is VST3 poly pressure instead.

use plughost_core::ExpressionKind;
use vst3::Steinberg::Vst::NoteExpressionTypeID;
use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::{
    kBrightnessTypeID, kExpressionTypeID, kPanTypeID, kTuningTypeID, kVibratoTypeID, kVolumeTypeID,
};

/// The standard type of an expression, `None` for pressure.
pub(crate) fn type_id(kind: ExpressionKind) -> Option<NoteExpressionTypeID> {
    Some(match kind {
        ExpressionKind::Volume => kVolumeTypeID,
        ExpressionKind::Pan => kPanTypeID,
        ExpressionKind::Tuning => kTuningTypeID,
        ExpressionKind::Vibrato => kVibratoTypeID,
        ExpressionKind::Expression => kExpressionTypeID,
        ExpressionKind::Brightness => kBrightnessTypeID,
        _ => return None,
    })
}

/// The expression of a standard type.
pub(crate) fn kind(type_id: NoteExpressionTypeID) -> Option<ExpressionKind> {
    ExpressionKind::ALL
        .into_iter()
        .find(|&kind| self::type_id(kind) == Some(type_id))
}

/// The normalized value of a plain one: volume is `4 × normalized` (0.25 is unity gain), tuning is
/// `240 × (normalized − 0.5)` semitones, and the others are already normalized.
pub(crate) fn normalized(kind: ExpressionKind, value: f64) -> f64 {
    match kind {
        ExpressionKind::Volume => value / 4.0,
        ExpressionKind::Tuning => value / 240.0 + 0.5,
        _ => value,
    }
}

pub(crate) fn plain(kind: ExpressionKind, normalized: f64) -> f64 {
    match kind {
        ExpressionKind::Volume => normalized * 4.0,
        ExpressionKind::Tuning => (normalized - 0.5) * 240.0,
        _ => normalized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_types_round_trip_through_normalized_values() {
        for kind in ExpressionKind::ALL {
            let Some(id) = type_id(kind) else {
                assert_eq!(kind, ExpressionKind::Pressure);
                continue;
            };
            assert_eq!(super::kind(id), Some(kind));
            let range = kind.range();
            assert_eq!(normalized(kind, *range.start()), 0.0, "{kind:?}");
            assert_eq!(normalized(kind, *range.end()), 1.0, "{kind:?}");
            assert_eq!(plain(kind, 0.5), (range.start() + range.end()) / 2.0);
        }
        assert_eq!(normalized(ExpressionKind::Volume, 1.0), 0.25);
        assert_eq!(normalized(ExpressionKind::Tuning, 12.0), 0.55);
    }
}
