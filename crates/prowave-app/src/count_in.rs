//! Count-in gating shared by MIDI and audio recording.

use crate::model::Project;

/// Whether a take should start or continue on this frame.
///
/// `count_in_started` belongs to one Record+Play attempt. It is forgotten when that attempt
/// ends before a take begins, so the next attempt cannot start early from a stale count-in.
pub(crate) fn wants_recording(p: &Project, count_in_started: &mut bool, active_take: bool) -> bool {
    let attempt_ended = !active_take && (!p.recording || !p.playing);
    let started = !attempt_ended && (p.counting_in || *count_in_started);
    *count_in_started = started;
    let count_in_pending = p.count_in && !p.counting_in && !started;
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
        let mut started = false;
        p.recording = true;
        p.playing = true;
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut started, false));
        assert!(started);

        p.playing = false;
        p.recording = false;
        assert!(!wants_recording(&p, &mut started, false));
        assert!(!started, "the aborted count-in is gone");

        p.recording = true;
        p.playing = true;
        p.counting_in = false;
        assert!(!wants_recording(&p, &mut started, false));
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut started, false));
        p.counting_in = false;
        assert!(wants_recording(&p, &mut started, false));
    }

    #[test]
    fn an_active_take_keeps_the_count_in_state_until_it_finishes() {
        let mut p = project();
        let mut started = false;
        p.counting_in = true;
        assert!(!wants_recording(&p, &mut started, false));
        p.playing = false;
        p.recording = false;
        assert!(!wants_recording(&p, &mut started, true));
        assert!(started, "the take, not this frame, ends the attempt");
    }
}
