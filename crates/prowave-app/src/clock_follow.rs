//! Following MIDI clock from an input (Logic Pro: Synchronization > MIDI Clock, "Follow MIDI
//! Clock"): the song starts, continues, stops and jumps where the device says, and its tempo
//! is the tempo the device's Timing Clock messages run at.
//!
//! This is the pure part: the messages in, what the transport should do out. Timestamps are
//! microseconds from the input port (midir gives them), so the tests can feed exact timings.

use crate::model::Project;
use std::collections::VecDeque;

/// Timing Clock messages per quarter note.
const TICKS_PER_BEAT: f64 = 24.0;
/// How many Timing Clock intervals the tempo estimate averages over (two beats).
const WINDOW: usize = 48;
/// How many ticks go by between two tempo reports (a quarter note): fewer reports, steadier tempo.
const REPORT_EVERY: u64 = 24;

/// What the followed clock asks the transport to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FollowEvent {
    /// Start (0xFA): play from the beginning.
    Start,
    /// Continue (0xFB): play on from where the song is.
    Continue,
    /// Stop (0xFC).
    Stop,
    /// Song Position Pointer (0xF2): the position in beats (quarter notes).
    Locate(f64),
    /// The tempo the Timing Clock runs at, in beats per minute.
    Tempo(f64),
}

/// Turns the clock's messages into transport events.
#[derive(Default)]
pub struct ClockFollower {
    /// Arrival times (µs) of the latest Timing Clock messages.
    times: VecDeque<u64>,
    /// Timing Clock messages since the last Start or Continue.
    ticks: u64,
}

impl ClockFollower {
    /// Takes one message that arrived at `at_us`. Returns the event it makes, if any. Messages
    /// that are not clock messages (notes, controllers) are ignored.
    pub fn message(&mut self, at_us: u64, msg: &[u8]) -> Option<FollowEvent> {
        match msg.first()? {
            0xf8 => {
                self.times.push_back(at_us);
                while self.times.len() > WINDOW {
                    self.times.pop_front();
                }
                self.ticks += 1;
                if self.ticks.is_multiple_of(REPORT_EVERY) {
                    self.tempo()
                } else {
                    None
                }
            }
            0xfa => {
                self.restart();
                Some(FollowEvent::Start)
            }
            0xfb => {
                self.restart();
                Some(FollowEvent::Continue)
            }
            0xfc => Some(FollowEvent::Stop),
            0xf2 => {
                let [_, lsb, msb, ..] = *msg else {
                    return None;
                };
                if lsb > 127 || msb > 127 {
                    return None;
                }
                // Sixteenth notes since the start: a sixteenth is a quarter of a beat.
                let sixteenths = f64::from(u16::from(lsb) | (u16::from(msb) << 7));
                Some(FollowEvent::Locate(sixteenths / 4.0))
            }
            _ => None,
        }
    }

    /// Start and Continue: the tick count and the intervals before them do not belong to the
    /// song any more.
    fn restart(&mut self) {
        self.times.clear();
        self.ticks = 0;
    }

    /// The tempo from the intervals of the latest ticks. `None` until there are enough of them.
    fn tempo(&self) -> Option<FollowEvent> {
        if self.times.len() < 2 {
            return None;
        }
        let first = *self.times.front()?;
        let last = *self.times.back()?;
        let intervals = (self.times.len() - 1) as f64;
        let interval_us = (last - first) as f64 / intervals;
        if interval_us <= 0.0 {
            return None;
        }
        let bpm = 60_000_000.0 / (interval_us * TICKS_PER_BEAT);
        Some(FollowEvent::Tempo(bpm))
    }
}

/// Makes the project do what the clock asks: play from the start, play on, stop, jump, or take
/// the clock's tempo (a change under a twentieth of a beat per minute is not worth a new tempo).
pub fn apply(p: &mut Project, event: FollowEvent) {
    match event {
        FollowEvent::Start => {
            p.position_beats = 0.0;
            p.playing = true;
        }
        FollowEvent::Continue => p.playing = true,
        FollowEvent::Stop => p.playing = false,
        FollowEvent::Locate(beats) => p.position_beats = beats,
        FollowEvent::Tempo(bpm) => {
            let tempo = project_tempo(bpm);
            if (tempo - p.tempo).abs() >= 0.05 {
                p.tempo = tempo;
            }
        }
    }
}

/// The tempo a project takes from the clock: rounded to a tenth, within the project's range.
pub fn project_tempo(bpm: f64) -> f32 {
    ((bpm * 10.0).round() / 10.0).clamp(20.0, 300.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `ticks` Timing Clock messages at `bpm`, starting at `t0` µs.
    fn ticks(f: &mut ClockFollower, t0: u64, bpm: f64, ticks: u64) -> Vec<FollowEvent> {
        let interval = 60_000_000.0 / (bpm * TICKS_PER_BEAT);
        (0..ticks)
            .filter_map(|i| {
                let at = t0 + (i as f64 * interval).round() as u64;
                f.message(at, &[0xf8])
            })
            .collect()
    }

    #[test]
    fn the_tempo_is_the_clock_s_tempo() {
        let mut f = ClockFollower::default();
        let reports = ticks(&mut f, 0, 120.0, 96);
        assert!(!reports.is_empty());
        for r in &reports {
            let FollowEvent::Tempo(bpm) = r else {
                panic!("only tempo reports: {r:?}");
            };
            assert!((bpm - 120.0).abs() < 0.05, "{bpm}");
        }
        // A tempo report every quarter note.
        assert_eq!(reports.len(), 4);
    }

    #[test]
    fn a_change_of_tempo_is_followed() {
        let mut f = ClockFollower::default();
        ticks(&mut f, 0, 100.0, 48);
        let last_at = 48.0 * 60_000_000.0 / (100.0 * TICKS_PER_BEAT);
        let reports = ticks(&mut f, last_at.round() as u64, 150.0, 96);
        let FollowEvent::Tempo(bpm) = *reports.last().expect("a report") else {
            panic!("a tempo report");
        };
        assert!((bpm - 150.0).abs() < 0.5, "the window moved on: {bpm}");
    }

    #[test]
    fn start_continue_stop_and_position_map_to_transport_events() {
        let mut f = ClockFollower::default();
        assert_eq!(f.message(0, &[0xfa]), Some(FollowEvent::Start));
        assert_eq!(f.message(1, &[0xfb]), Some(FollowEvent::Continue));
        assert_eq!(f.message(2, &[0xfc]), Some(FollowEvent::Stop));
        // 16 sixteenths: bar 2 of 4/4, beat 4.
        assert_eq!(f.message(3, &[0xf2, 16, 0]), Some(FollowEvent::Locate(4.0)));
        // 200 sixteenths: LSB 72, MSB 1.
        assert_eq!(
            f.message(4, &[0xf2, 72, 1]),
            Some(FollowEvent::Locate(50.0))
        );
    }

    #[test]
    fn a_start_forgets_the_intervals_of_the_last_song() {
        let mut f = ClockFollower::default();
        ticks(&mut f, 0, 120.0, 30);
        f.message(1_000_000, &[0xfa]);
        // Fewer than two ticks since the start: no tempo yet.
        assert_eq!(ticks(&mut f, 1_000_000, 120.0, 1), vec![]);
    }

    #[test]
    fn other_messages_are_ignored_and_a_damaged_position_is_too() {
        let mut f = ClockFollower::default();
        assert_eq!(f.message(0, &[0x90, 60, 100]), None, "a note");
        assert_eq!(f.message(0, &[0xb0, 7, 100]), None, "a controller");
        assert_eq!(f.message(0, &[0xf2, 200, 0]), None, "a damaged position");
        assert_eq!(f.message(0, &[]), None);
    }

    #[test]
    fn the_events_change_the_project_the_way_the_clock_asks() {
        let mut p = crate::model::Project::demo();
        p.position_beats = 12.0;
        apply(&mut p, FollowEvent::Start);
        assert!(p.playing && p.position_beats == 0.0);
        apply(&mut p, FollowEvent::Stop);
        assert!(!p.playing);
        apply(&mut p, FollowEvent::Continue);
        assert!(p.playing);
        apply(&mut p, FollowEvent::Locate(20.0));
        assert_eq!(p.position_beats, 20.0);
        let before = p.tempo;
        apply(&mut p, FollowEvent::Tempo(before as f64 + 0.01));
        assert_eq!(p.tempo, before, "a hair's change is ignored");
        apply(&mut p, FollowEvent::Tempo(133.0));
        assert_eq!(p.tempo, 133.0);
    }

    #[test]
    fn a_project_tempo_is_rounded_and_kept_in_range() {
        assert_eq!(project_tempo(119.96), 120.0);
        assert_eq!(project_tempo(90.04), 90.0);
        assert_eq!(project_tempo(5.0), 20.0);
        assert_eq!(project_tempo(900.0), 300.0);
    }
}
