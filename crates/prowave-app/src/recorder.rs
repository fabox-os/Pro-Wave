//! MIDI recording, as in Logic: while Record and Play are both on, the notes played on a
//! MIDI keyboard go into a new region on the selected instrument track. The region is red and
//! grows with the song position while it is recorded; the Event list shows the notes as they
//! arrive. When playback stops the region takes the track's colour and the song is rebuilt so
//! the new notes play back.

use crate::model::{Control, Note, Project, Region, RegionParams, Take, TakeFolder, TrackKind};
use std::collections::HashMap;

/// A note played live, from a MIDI keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveNote {
    On {
        pitch: u8,
        vel: u8,
    },
    Off {
        pitch: u8,
    },
    /// The sustain pedal (controller 64).
    Sustain {
        val: u8,
    },
}

/// What [`update`] did, for the app to react to (selection, messages).
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// A new region started recording at (track, region).
    Started(usize, usize),
    /// Recording ended: the region that was kept, or none when nothing was played.
    Finished(Option<(usize, usize)>),
    /// Record was on but there is no instrument track to record on.
    NoTrack,
}

/// The recording in progress.
#[derive(Default)]
pub struct Recorder {
    /// The region being recorded: (track, region).
    target: Option<(usize, usize)>,
    /// Song position, in beats, where the region starts.
    start_beat: f64,
    /// Notes still held down: pitch to the index of the note in the region.
    open: HashMap<u8, usize>,
    /// Where the sustain pedal is: a recording starts with this, as Logic does.
    pedal: u8,
    /// With the Cycle on: the length of one pass in beats. Every pass is a take.
    cycle_len: Option<f64>,
    /// The passes of a cycle recording that are already over.
    takes: Vec<Take>,
    /// Song position at the last frame, to see the cycle jump back.
    last_pos: f64,
    /// What was played while not recording, for Capture Recording: (song position, note).
    played: Vec<(f64, LiveNote)>,
    /// The transport was running at the last frame.
    was_playing: bool,
}

impl Recorder {
    /// The region being recorded now (drawn red).
    pub fn target(&self) -> Option<(usize, usize)> {
        self.target
    }

    pub fn active(&self) -> bool {
        self.target.is_some()
    }
}

/// Notes kept for Capture Recording.
const MAX_PLAYED: usize = 20_000;

/// The track to record on: an armed one (R button), else the selected one if it is an
/// instrument, else the first instrument.
fn record_track(p: &Project) -> Option<usize> {
    let is_midi =
        |t: &crate::model::Track| matches!(t.kind, TrackKind::Instrument | TrackKind::ExternalMidi);
    p.tracks
        .iter()
        .position(|t| t.rec && is_midi(t))
        .or_else(|| {
            p.tracks
                .get(p.selected_track)
                .filter(|t| is_midi(t))
                .map(|_| p.selected_track)
        })
        .or_else(|| p.tracks.iter().position(is_midi))
}

/// Call once per frame with the notes played since the last one.
pub fn update(rec: &mut Recorder, p: &mut Project, input: &[LiveNote]) -> Outcome {
    let want = p.recording && p.playing && !p.counting_in;
    remember_played(rec, p, input, want);
    let pedal_before = rec.pedal;
    for ev in input {
        if let LiveNote::Sustain { val } = *ev {
            rec.pedal = val;
        }
    }
    match (rec.target, want) {
        (None, false) => Outcome::Nothing,
        (None, true) => start(rec, p, pedal_before),
        (Some(_), false) => finish(rec, p),
        (Some((t, r)), true) => {
            // The region being recorded has a name that starts with `*`. If it is not there
            // (the track or the region was deleted or moved), the recording is given up rather
            // than written into another region.
            if !is_recording_region(p, t, r) {
                *rec = Recorder::default();
                return Outcome::Finished(None);
            }
            record(rec, p, t, r, input);
            Outcome::Nothing
        }
    }
}

/// Keeps what is played along with the song and not recorded. Starting the song again, or
/// the cycle jumping back, begins a new performance.
fn remember_played(rec: &mut Recorder, p: &Project, input: &[LiveNote], recording: bool) {
    let now = p.position_beats;
    if p.playing {
        let jumped = rec.played.last().is_some_and(|(at, _)| *at > now + 0.25);
        if !rec.was_playing || jumped {
            rec.played.clear();
        }
        if !recording {
            let room = MAX_PLAYED.saturating_sub(rec.played.len());
            rec.played
                .extend(input.iter().take(room).map(|note| (now, *note)));
        }
    }
    rec.was_playing = p.playing;
}

/// Capture Recording: makes a region of what was just played on the record track, starting
/// at the bar of the first note. Returns (track, region), or `None` if nothing was played.
pub fn capture(rec: &mut Recorder, p: &mut Project) -> Option<(usize, usize)> {
    let t = record_track(p)?;
    let played = std::mem::take(&mut rec.played);
    let first = played.iter().find_map(|(at, note)| match note {
        LiveNote::On { .. } => Some(*at),
        _ => None,
    })?;
    let bar = p.beats_to_bar(first).floor();
    let start = p.bar_to_beats(bar);
    let last = played.last().map_or(first, |(at, _)| *at);
    let mut notes: Vec<Note> = Vec::new();
    let mut open: HashMap<u8, usize> = HashMap::new();
    for (at, note) in played {
        let now = at - start;
        match note {
            LiveNote::On { pitch, vel } => {
                if let Some(i) = open.insert(pitch, notes.len()) {
                    close(&mut notes[i], now);
                }
                notes.push(Note {
                    start: now as f32,
                    len: MIN_LEN,
                    pitch,
                    vel,
                });
            }
            LiveNote::Off { pitch } => {
                if let Some(i) = open.remove(&pitch) {
                    close(&mut notes[i], now);
                }
            }
            LiveNote::Sustain { .. } => {}
        }
    }
    for i in open.into_values() {
        close(&mut notes[i], last - start);
    }
    let end = notes.iter().map(|n| n.start + n.len).fold(0.0, f32::max);
    let bars = (p.beats_to_bar(start + f64::from(end)) - bar)
        .ceil()
        .max(1.0);
    let track = &mut p.tracks[t];
    track.regions.push(Region {
        name: track.name.clone(),
        start_bar: bar as f32 + 1.0,
        len_bars: bars as f32,
        notes,
        controls: Vec::new(),
        folder: None,
        generated: None,
        audio: None,
        params: RegionParams::default(),
    });
    let r = track.regions.len() - 1;
    p.selected_track = t;
    p.song_rev += 1;
    Some((t, r))
}

/// Whether region `r` of track `t` is the one being recorded (its name starts with `*`).
fn is_recording_region(p: &Project, t: usize, r: usize) -> bool {
    p.tracks
        .get(t)
        .and_then(|t| t.regions.get(r))
        .is_some_and(|region| region.name.starts_with('*'))
}

fn start(rec: &mut Recorder, p: &mut Project, pedal: u8) -> Outcome {
    let Some(t) = record_track(p) else {
        p.recording = false;
        return Outcome::NoTrack;
    };
    // With the Cycle on, recording from inside it fills the cycle: each pass is a take.
    let cycle = (p.cycle
        && p.cycle_end > p.cycle_start
        && (p.cycle_start..p.cycle_end).contains(&p.position_beats))
    .then_some(p.cycle_end - p.cycle_start);
    let start = if cycle.is_some() {
        p.cycle_start
    } else {
        p.position_beats.max(0.0)
    };
    let name = format!("*{}", p.tracks[t].name);
    let (start_bar, len_bars) = (
        p.beats_to_bar(start) as f32 + 1.0,
        cycle.map_or(0.01, |len| bars_between(p, start, len)),
    );
    p.tracks[t].regions.push(Region {
        name,
        start_bar,
        len_bars,
        notes: Vec::new(),
        // Logic begins every recording with the pedal's state.
        controls: vec![Control {
            start: 0.0,
            num: SUSTAIN,
            val: pedal,
        }],
        folder: None,
        generated: None,
        audio: None,
        params: RegionParams::default(),
    });
    let r = p.tracks[t].regions.len() - 1;
    *rec = Recorder {
        target: Some((t, r)),
        start_beat: start,
        pedal: rec.pedal,
        cycle_len: cycle,
        last_pos: p.position_beats,
        ..Recorder::default()
    };
    p.selected_track = t;
    Outcome::Started(t, r)
}

/// How many bars `len` quarter notes are when they start at `start` (a bar can be longer or
/// shorter than the next when the signature changes).
fn bars_between(p: &Project, start: f64, len: f64) -> f32 {
    (p.beats_to_bar(start + len) - p.beats_to_bar(start)) as f32
}

fn record(rec: &mut Recorder, p: &mut Project, t: usize, r: usize, input: &[LiveNote]) {
    let raw = p.position_beats;
    let region = &mut p.tracks[t].regions[r];
    // The cycle jumped back: the pass is over and the next one is a new take.
    if let Some(len) = rec.cycle_len {
        if raw + 0.25 < rec.last_pos {
            for &i in rec.open.values() {
                close(&mut region.notes[i], len);
            }
            rec.open.clear();
            rec.takes.push(Take {
                notes: std::mem::take(&mut region.notes),
                controls: std::mem::take(&mut region.controls),
            });
            region.controls.push(Control {
                start: 0.0,
                num: SUSTAIN,
                val: rec.pedal,
            });
        }
    }
    rec.last_pos = raw;
    // The playhead can be before the start (the cycle), or past the end of the cycle.
    let mut now = (raw - rec.start_beat).max(0.0);
    if let Some(len) = rec.cycle_len {
        now = now.min(len);
    }
    for ev in input {
        match *ev {
            LiveNote::On { pitch, vel } => {
                if let Some(i) = rec.open.remove(&pitch) {
                    close(&mut region.notes[i], now);
                }
                region.notes.push(Note {
                    start: now as f32,
                    len: MIN_LEN,
                    pitch,
                    vel,
                });
                rec.open.insert(pitch, region.notes.len() - 1);
            }
            LiveNote::Off { pitch } => {
                if let Some(i) = rec.open.remove(&pitch) {
                    close(&mut region.notes[i], now);
                }
            }
            LiveNote::Sustain { val } => region.controls.push(Control {
                start: now as f32,
                num: SUSTAIN,
                val,
            }),
        }
    }
    // Held notes keep growing, and so does the region.
    for &i in rec.open.values() {
        close(&mut region.notes[i], now);
    }
    if rec.cycle_len.is_none() {
        let bars = bars_between(p, rec.start_beat, now).max(0.01);
        p.tracks[t].regions[r].len_bars = bars;
    }
}

/// The sustain pedal's controller number.
pub const SUSTAIN: u8 = 64;

/// The shortest note, in beats (a 64th note).
const MIN_LEN: f32 = 0.0625;

fn close(note: &mut Note, now: f64) {
    note.len = ((now as f32) - note.start).max(MIN_LEN);
}

fn finish(rec: &mut Recorder, p: &mut Project) -> Outcome {
    let Some((t, r)) = rec.target.take() else {
        return Outcome::Nothing;
    };
    let open = std::mem::take(&mut rec.open);
    let cycle_len = rec.cycle_len.take();
    let earlier = std::mem::take(&mut rec.takes);
    p.recording = false;
    let mut now = (p.position_beats - rec.start_beat).max(0.0);
    if let Some(len) = cycle_len {
        now = now.min(len);
    }
    if !is_recording_region(p, t, r) {
        return Outcome::Finished(None);
    }
    let bars_between_len = cycle_len.map_or(0.0, |len| bars_between(p, rec.start_beat, len));
    let region = &mut p.tracks[t].regions[r];
    for i in open.into_values() {
        close(&mut region.notes[i], now);
    }
    if let Some(len) = cycle_len {
        // Every pass is a take; the passes where nothing was played are dropped.
        let mut takes = earlier;
        takes.push(Take {
            notes: std::mem::take(&mut region.notes),
            controls: std::mem::take(&mut region.controls),
        });
        takes.retain(|t| !t.notes.is_empty());
        region.len_bars = bars_between_len;
        region.name = region.name.trim_start_matches('*').to_string();
        match takes.len() {
            0 => {
                p.tracks[t].regions.remove(r);
                return Outcome::Finished(None);
            }
            1 => {
                let take = takes.remove(0);
                region.notes = take.notes;
                region.controls = take.controls;
            }
            _ => {
                let folder = TakeFolder::new(takes, len as f32);
                folder.apply(&mut region.notes, &mut region.controls);
                region.folder = Some(folder);
            }
        }
        p.song_rev += 1;
        return Outcome::Finished(Some((t, r)));
    }
    if region.notes.is_empty() {
        p.tracks[t].regions.remove(r);
        return Outcome::Finished(None);
    }
    // The region ends on a bar line, covering everything that was played.
    let end = region
        .notes
        .iter()
        .map(|n| (n.start + n.len) as f64)
        .chain(region.controls.iter().map(|c| c.start as f64))
        .fold(now, f64::max);
    let end_bars = bars_between(p, rec.start_beat, end);
    let region = &mut p.tracks[t].regions[r];
    region.len_bars = end_bars.ceil().max(1.0);
    region.name = region.name.trim_start_matches('*').to_string();
    p.song_rev += 1;
    Outcome::Finished(Some((t, r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn project() -> Project {
        let mut p = Project::empty();
        p.tracks.truncate(1);
        p.tracks[0].regions.clear();
        p
    }

    fn on(pitch: u8) -> LiveNote {
        LiveNote::On { pitch, vel: 100 }
    }

    fn off(pitch: u8) -> LiveNote {
        LiveNote::Off { pitch }
    }

    #[test]
    fn nothing_happens_unless_record_and_play_are_both_on() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Nothing);
        p.recording = false;
        p.playing = true;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Nothing);
        assert!(p.tracks[0].regions.is_empty());
    }

    #[test]
    fn recording_makes_a_red_region_that_grows_and_keeps_the_notes() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        p.position_beats = 8.0;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Started(0, 0));
        assert_eq!(rec.target(), Some((0, 0)));
        assert!(p.tracks[0].regions[0].name.starts_with('*'));
        assert_eq!(p.tracks[0].regions[0].start_bar, 3.0);

        p.position_beats = 9.0;
        update(&mut rec, &mut p, &[on(60)]);
        p.position_beats = 10.0;
        update(&mut rec, &mut p, &[on(64), off(60)]);
        p.position_beats = 11.5;
        update(&mut rec, &mut p, &[off(64)]);
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.notes.len(), 2);
        assert_eq!((r.notes[0].start, r.notes[0].len), (1.0, 1.0));
        assert_eq!((r.notes[1].start, r.notes[1].len), (2.0, 1.5));
        assert!((r.len_bars - 3.5 / 4.0).abs() < 1e-6, "{}", r.len_bars);

        // Stop: the region turns into a normal one, ending on a bar line.
        p.playing = false;
        let rev = p.song_rev;
        assert_eq!(
            update(&mut rec, &mut p, &[]),
            Outcome::Finished(Some((0, 0)))
        );
        assert!(!rec.active());
        assert!(!p.recording);
        let r = &p.tracks[0].regions[0];
        assert!(!r.name.starts_with('*'));
        assert_eq!(r.len_bars, 1.0);
        assert_eq!(p.song_rev, rev + 1, "the engine song is rebuilt");
    }

    #[test]
    fn held_notes_are_closed_when_recording_stops() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        p.position_beats = 1.0;
        update(&mut rec, &mut p, &[on(60)]);
        p.position_beats = 3.0;
        p.playing = false;
        update(&mut rec, &mut p, &[]);
        let n = &p.tracks[0].regions[0].notes[0];
        assert_eq!((n.start, n.len), (1.0, 2.0));
    }

    #[test]
    fn an_empty_recording_leaves_no_region() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        p.playing = false;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Finished(None));
        assert!(p.tracks[0].regions.is_empty());
    }

    #[test]
    fn record_needs_an_instrument_track() {
        let mut p = Project::empty();
        p.tracks.clear();
        p.recording = true;
        p.playing = true;
        let mut rec = Recorder::default();
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::NoTrack);
        assert!(!p.recording);
    }

    #[test]
    fn a_deleted_region_ends_the_recording_without_a_panic() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        p.tracks[0].regions.clear();
        assert_eq!(update(&mut rec, &mut p, &[on(60)]), Outcome::Finished(None));
        assert!(!rec.active());
    }

    #[test]
    fn the_sustain_pedal_is_recorded_and_a_recording_starts_with_its_state() {
        let mut p = project();
        let mut rec = Recorder::default();
        // The pedal is down before recording starts.
        update(&mut rec, &mut p, &[LiveNote::Sustain { val: 127 }]);
        p.recording = true;
        p.playing = true;
        p.position_beats = 4.0;
        update(&mut rec, &mut p, &[]);
        assert_eq!(
            p.tracks[0].regions[0].controls,
            vec![Control {
                start: 0.0,
                num: SUSTAIN,
                val: 127
            }]
        );
        p.position_beats = 5.0;
        update(&mut rec, &mut p, &[on(60), LiveNote::Sustain { val: 0 }]);
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.controls.len(), 2);
        assert_eq!((r.controls[1].start, r.controls[1].val), (1.0, 0));
        p.playing = false;
        update(&mut rec, &mut p, &[]);
        assert_eq!(p.tracks[0].regions[0].controls.len(), 2);
    }

    #[test]
    fn cycle_recording_makes_a_take_per_pass_and_a_take_folder() {
        let mut p = project();
        p.cycle = true;
        p.cycle_start = 8.0;
        p.cycle_end = 16.0;
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        p.position_beats = 8.0;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Started(0, 0));
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.start_bar, 3.0, "the region fills the cycle");
        assert_eq!(r.len_bars, 2.0);

        // First pass: notes at beats 1 and 3 of the cycle.
        p.position_beats = 9.0;
        update(&mut rec, &mut p, &[on(60)]);
        p.position_beats = 11.0;
        update(&mut rec, &mut p, &[off(60), on(62)]);
        p.position_beats = 15.5;
        update(&mut rec, &mut p, &[off(62)]);
        // The cycle jumps back: second pass with one note at beat 2.
        p.position_beats = 8.1;
        update(&mut rec, &mut p, &[]);
        p.position_beats = 10.0;
        update(&mut rec, &mut p, &[on(64)]);
        p.position_beats = 12.0;
        update(&mut rec, &mut p, &[off(64)]);

        p.playing = false;
        assert_eq!(
            update(&mut rec, &mut p, &[]),
            Outcome::Finished(Some((0, 0)))
        );
        let r = &p.tracks[0].regions[0];
        let f = r.folder.as_ref().expect("two passes make a take folder");
        assert_eq!(f.takes.len(), 2);
        assert_eq!(f.takes[0].notes.len(), 2);
        assert_eq!(f.takes[1].notes.len(), 1);
        assert_eq!(f.comp.len(), 1);
        assert_eq!(f.comp[0].take, 1, "the last take plays to begin with");
        assert_eq!(r.notes.len(), 1);
        assert_eq!(r.len_bars, 2.0);
        assert!(!r.name.starts_with('*'));
    }

    #[test]
    fn one_cycle_pass_is_an_ordinary_region_and_empty_passes_are_dropped() {
        let mut p = project();
        p.cycle = true;
        p.cycle_start = 0.0;
        p.cycle_end = 8.0;
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        p.position_beats = 2.0;
        update(&mut rec, &mut p, &[on(60)]);
        p.position_beats = 3.0;
        update(&mut rec, &mut p, &[off(60)]);
        // An empty second pass, then stop.
        p.position_beats = 0.2;
        update(&mut rec, &mut p, &[]);
        p.position_beats = 1.0;
        p.playing = false;
        update(&mut rec, &mut p, &[]);
        let r = &p.tracks[0].regions[0];
        assert!(r.folder.is_none());
        assert_eq!(r.notes.len(), 1);
    }

    #[test]
    fn a_region_that_moved_is_never_written_into() {
        let mut p = Project::empty();
        p.tracks.truncate(1);
        p.tracks[0].regions.clear();
        let mut other = Track::new("Other", TrackKind::Instrument);
        other.regions.push(Region {
            name: "Mine".into(),
            start_bar: 1.0,
            len_bars: 1.0,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            generated: None,
            audio: None,
            params: RegionParams::default(),
        });
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        // The track with the recording is deleted and another one takes its place.
        p.tracks.remove(0);
        p.tracks.insert(0, other);
        p.position_beats = 2.0;
        assert_eq!(
            update(&mut rec, &mut p, &[on(60)]),
            Outcome::Finished(None),
            "the recording is given up"
        );
        assert!(
            p.tracks[0].regions[0].notes.is_empty(),
            "the note went nowhere"
        );
        assert_eq!(p.tracks[0].regions[0].name, "Mine");
        assert!(!rec.active());
    }

    #[test]
    fn an_armed_instrument_records_whichever_track_is_selected() {
        let mut p = project();
        p.tracks.push(Track::new("Keys", TrackKind::Instrument));
        assert_eq!(
            record_track(&p),
            Some(0),
            "nothing armed: the selected track"
        );
        p.tracks[1].rec = true;
        assert_eq!(record_track(&p), Some(1));
    }

    #[test]
    fn recording_waits_for_the_count_in() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.recording = true;
        p.playing = true;
        p.counting_in = true;
        assert_eq!(update(&mut rec, &mut p, &[]), Outcome::Nothing);
        assert!(!rec.active());
        p.counting_in = false;
        assert!(matches!(
            update(&mut rec, &mut p, &[]),
            Outcome::Started(..)
        ));
    }

    #[test]
    fn capture_makes_a_region_of_what_was_played_along_with_the_song() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.playing = true;
        p.position_beats = 5.0;
        update(&mut rec, &mut p, &[LiveNote::On { pitch: 60, vel: 90 }]);
        p.position_beats = 6.0;
        update(&mut rec, &mut p, &[LiveNote::Off { pitch: 60 }]);
        p.playing = false;
        update(&mut rec, &mut p, &[]);
        let rev = p.song_rev;

        assert_eq!(capture(&mut rec, &mut p), Some((0, 0)));
        let region = &p.tracks[0].regions[0];
        assert_eq!(
            (region.start_bar, region.len_bars),
            (2.0, 1.0),
            "bar 2 of 4/4"
        );
        assert_eq!(region.notes.len(), 1);
        assert_eq!((region.notes[0].start, region.notes[0].len), (1.0, 1.0));
        assert_eq!(region.notes[0].pitch, 60);
        assert_eq!(p.song_rev, rev + 1);
        assert_eq!(capture(&mut rec, &mut p), None, "captured once");
    }

    #[test]
    fn playing_the_song_again_starts_a_new_performance() {
        let mut p = project();
        let mut rec = Recorder::default();
        p.playing = true;
        update(&mut rec, &mut p, &[LiveNote::On { pitch: 60, vel: 90 }]);
        p.playing = false;
        update(&mut rec, &mut p, &[]);
        p.playing = true;
        update(&mut rec, &mut p, &[]);
        assert_eq!(capture(&mut rec, &mut p), None);
    }
}
