//! Count-in gating shared by MIDI and audio recording.

use crate::model::Project;

/// What one recorder remembers about Record+Play attempts between frames.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CountIn {
    /// The count-in has started in this attempt.
    started: bool,
    /// The attempt began while the transport was already running, so there is no count-in
    /// to wait for (Logic only counts in when playback starts from a stop).
    skip: bool,
    /// The transport was running on the previous frame.
    was_playing: bool,
}

/// Whether a take should start or continue on this frame.
///
/// An attempt is Record+Play from stop until recording or playback ends, or until a take
/// that is already running ends. When the attempt ends before a take begins, its state is
/// forgotten, so the next attempt waits for its own count-in.
pub(crate) fn wants_recording(p: &Project, state: &mut CountIn, active_take: bool) -> bool {
    let was_playing = state.was_playing;
    state.was_playing = p.playing;
    let attempt_ended = !active_take && (!p.recording || !p.playing);
    if attempt_ended {
        *state = CountIn {
            was_playing: p.playing,
            ..CountIn::default()
        };
    } else if p.counting_in {
        state.started = true;
    } else if p.recording && p.playing && !state.started && was_playing {
        state.skip = true;
    }
    let count_in_pending = p.count_in && !p.counting_in && !state.started && !state.skip;
    p.recording && p.playing && !p.counting_in && !count_in_pending
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn project() -> Project {
        let mut p = Project::empty();
        p.count_in = true;
        p
    }

    #[test]
    fn a_stale_count_in_is_forgotten_when_the_attempt_ends() {
        let mut p = project();
        let mut state = CountIn::default();
        p.recording = true;
        p.playing = true;
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut state, false));
        assert!(state.started);

        p.playing = false;
        p.recording = false;
        assert!(!wants_recording(&p, &mut state, false));
        assert!(!state.started, "the aborted count-in is gone");

        p.recording = true;
        p.playing = true;
        p.counting_in = false;
        assert!(!wants_recording(&p, &mut state, false));
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut state, false));
        p.counting_in = false;
        assert!(wants_recording(&p, &mut state, false));
    }

    #[test]
    fn an_active_take_keeps_the_count_in_state_until_it_finishes() {
        let mut p = project();
        let mut state = CountIn::default();
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut state, false));
        p.playing = false;
        p.recording = false;
        assert!(!wants_recording(&p, &mut state, true));
        assert!(state.started, "the take, not this frame, ends the attempt");
    }

    #[test]
    fn recording_pressed_while_already_playing_starts_at_once() {
        let mut p = project();
        let mut state = CountIn::default();
        // Transport already running on earlier frames; Record is pressed now.
        p.playing = true;
        assert!(!wants_recording(&p, &mut state, false));
        p.recording = true;
        assert!(wants_recording(&p, &mut state, false));
        assert!(wants_recording(&p, &mut state, false));
    }

    #[test]
    fn record_and_play_from_stop_still_counts_in() {
        let mut p = project();
        let mut state = CountIn::default();
        // Record is on while stopped, then Play starts; the engine has not yet begun counting.
        p.recording = true;
        assert!(!wants_recording(&p, &mut state, false));
        p.playing = true;
        assert!(
            !wants_recording(&p, &mut state, false),
            "wait for the count-in"
        );
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut state, false));
        p.counting_in = false;
        assert!(wants_recording(&p, &mut state, false));
    }
}
