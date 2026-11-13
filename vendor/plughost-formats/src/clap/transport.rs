use clack_host::events::event_types::{TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use plughost_core::Transport;

use super::errors::ClapError;

pub(super) fn event(value: Transport, rate: f64) -> Result<TransportEvent, ClapError> {
    if value.beat_position.is_some() && value.bar_position.is_none() {
        return Err(ClapError::BarPositionRequired);
    }
    let seconds = value.sample_position as f64 / rate;
    if !(-4_294_967_296.0..4_294_967_296.0).contains(&seconds) {
        return Err(ClapError::Input(plughost_core::InputError::Transport));
    }
    if value.loop_region.is_some_and(|region| {
        [region.sample_start, region.sample_end]
            .iter()
            .any(|sample| !(-4_294_967_296.0..4_294_967_296.0).contains(&(*sample as f64 / rate)))
    }) {
        return Err(ClapError::Input(plughost_core::InputError::Transport));
    }
    let mut flags = TransportFlags::HAS_SECONDS_TIMELINE;
    flags.set(TransportFlags::IS_PLAYING, value.playing);
    flags.set(TransportFlags::HAS_TEMPO, value.tempo.is_some());
    flags.set(
        TransportFlags::HAS_BEATS_TIMELINE,
        value.beat_position.is_some(),
    );
    flags.set(
        TransportFlags::HAS_TIME_SIGNATURE,
        value.time_signature.is_some(),
    );
    flags.set(TransportFlags::IS_LOOP_ACTIVE, value.loop_region.is_some());
    Ok(TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: value.beat_position.unwrap_or(0.0).into(),
        song_pos_seconds: seconds.into(),
        tempo: value.tempo.unwrap_or(0.0),
        tempo_inc: 0.0,
        loop_start_beats: value.loop_region.map_or(0.0, |r| r.start).into(),
        loop_end_beats: value.loop_region.map_or(0.0, |r| r.end).into(),
        loop_start_seconds: value
            .loop_region
            .map_or(0.0, |region| region.sample_start as f64 / rate)
            .into(),
        loop_end_seconds: value
            .loop_region
            .map_or(0.0, |region| region.sample_end as f64 / rate)
            .into(),
        bar_start: value.bar_position.map_or(0.0, |bar| bar.start).into(),
        bar_number: value.bar_position.map_or(-1, |bar| bar.number),
        time_signature_numerator: value.time_signature.map_or(0, |v| v.numerator),
        time_signature_denominator: value.time_signature.map_or(0, |v| v.denominator),
    })
}
