//! Trimming regions by dragging their left or right edge (Logic Pro: the edges of a region in
//! the Tracks area).
//!
//! - An audio region keeps its recording: the left edge moves where the region starts playing
//!   in the file (`AudioRegion::offset_secs`) and the right edge how much of it plays
//!   (`AudioRegion::length_secs`). Both can be dragged back out, up to the ends of the file.
//!   The sound stays where it is on the timeline when the left edge moves.
//! - A MIDI region has no hidden content: trimming the left edge moves the notes with it (a note
//!   that crosses the new start begins at the start), trimming the right edge cuts the notes
//!   there. Dragging an edge back out does not bring cut notes back, but the drag itself is
//!   undone by Edit > Undo, and while one drag lasts the region is rebuilt from how it was.
//!
//! Not trimmed: Flex regions (their file is made from the edit), take folders, and Pattern and
//! Drummer regions (their notes are made from settings and the region's length).

use crate::model::{Control, Note, Project, Region};

/// Which edge of the region is dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Left,
    Right,
}

/// An audio region can never play less than this many seconds.
const MIN_AUDIO_SECS: f64 = 0.01;

/// Whether the region's edges can be dragged.
pub(crate) fn trimmable(r: &Region) -> bool {
    r.folder.is_none() && r.generated.is_none() && r.audio.as_ref().is_none_or(|a| a.flex.is_none())
}

/// Smallest region, in quarter notes, at `beat`: a 64th of a bar.
fn min_beats(p: &Project, beat: f64) -> f64 {
    p.bar_len_at(beat) / 64.0
}

/// Moves `edge` of region `r` on track `t` to `to_beat` (quarter notes). The position is held
/// to what the region allows. Returns whether the region changed.
pub(crate) fn trim(p: &mut Project, t: usize, r: usize, edge: Edge, to_beat: f64) -> bool {
    let Some(region) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return false;
    };
    if !trimmable(region) || !to_beat.is_finite() {
        return false;
    }
    let before = (region.start_bar, region.len_bars);
    if region.audio.is_some() {
        trim_audio(p, t, r, edge, to_beat);
    } else {
        trim_midi(p, t, r, edge, to_beat);
    }
    let region = &p.tracks[t].regions[r];
    let changed = (region.start_bar, region.len_bars) != before;
    if changed {
        p.song_rev += 1;
    }
    changed
}

fn trim_midi(p: &mut Project, t: usize, r: usize, edge: Edge, to_beat: f64) {
    let start = p.region_start_beats(&p.tracks[t].regions[r]);
    let end = p.region_end_beats(&p.tracks[t].regions[r]);
    let min = min_beats(p, start);
    let old_start_bar = p.tracks[t].regions[r].start_bar;
    let old_end_bar = old_start_bar + p.tracks[t].regions[r].len_bars;
    match edge {
        Edge::Left => {
            let to = to_beat.clamp(0.0, end - min);
            let bar = p.beats_to_bar(to) as f32 + 1.0;
            let local = (to - start) as f32;
            let region = &mut p.tracks[t].regions[r];
            region.notes = region
                .notes
                .iter()
                .filter(|n| n.start + n.len > local)
                .map(|n| {
                    let from = n.start.max(local);
                    Note {
                        start: from - local,
                        len: n.start + n.len - from,
                        ..*n
                    }
                })
                .collect();
            region.controls = region
                .controls
                .iter()
                .filter(|c| c.start >= local)
                .map(|c| Control {
                    start: c.start - local,
                    ..*c
                })
                .collect();
            region.start_bar = bar;
            region.len_bars = old_end_bar - bar;
        }
        Edge::Right => {
            let to = to_beat.max(start + min);
            let bar = p.beats_to_bar(to) as f32 + 1.0;
            let len = (to - start) as f32;
            let region = &mut p.tracks[t].regions[r];
            region.notes.retain(|n| n.start < len);
            for n in &mut region.notes {
                n.len = n.len.min(len - n.start);
            }
            region.controls.retain(|c| c.start < len);
            region.len_bars = bar - old_start_bar;
        }
    }
}

fn trim_audio(p: &mut Project, t: usize, r: usize, edge: Edge, to_beat: f64) {
    let map = p.tempo_map();
    let region = &p.tracks[t].regions[r];
    let Some(a) = region.audio.as_ref() else {
        return;
    };
    let file_secs = a.file.duration_secs();
    let offset = a.offset_secs.clamp(0.0, file_secs);
    let play = a
        .length_secs
        .unwrap_or(file_secs - offset)
        .clamp(0.0, file_secs - offset);
    let start = p.region_start_beats(region);
    let start_secs = map.seconds_at(start);
    let to_secs = map.seconds_at(to_beat.max(0.0));
    let (new_start, new_offset, new_play) = match edge {
        Edge::Left => {
            // The end stays where it is in time: the file part that played last still does.
            let delta = (to_secs - start_secs).clamp(-offset, (play - MIN_AUDIO_SECS).max(0.0));
            (
                map.beat_at(start_secs + delta),
                offset + delta,
                play - delta,
            )
        }
        Edge::Right => {
            let play = (to_secs - start_secs)
                .clamp(MIN_AUDIO_SECS.min(file_secs - offset), file_secs - offset);
            (start, offset, play)
        }
    };
    let end = map.advance(new_start, new_play);
    let bar = p.beats_to_bar(new_start) as f32 + 1.0;
    let len_bars = (p.beats_to_bar(end) as f32 + 1.0 - bar).max(1.0 / 64.0);
    let region = &mut p.tracks[t].regions[r];
    region.start_bar = bar;
    region.len_bars = len_bars;
    if let Some(a) = region.audio.as_mut() {
        a.offset_secs = new_offset;
        a.length_secs = Some(new_play);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::{AudioRegion, Note};

    fn tone() -> AudioRegion {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prowave-engine/tests/fixtures");
        AudioRegion {
            file: prowave_engine::AudioFile::open(dir.join("tone.caf")).unwrap(),
            peaks: Default::default(),
            flex: None,
            offset_secs: 0.0,
            length_secs: None,
        }
    }

    fn note(start: f32, len: f32) -> Note {
        Note {
            start,
            len,
            pitch: 60,
            vel: 90,
        }
    }

    /// Track 0, region 0: bars 1-5 (16 beats) with notes at 0 (2 long), 3 (1 long) and 8 (1).
    fn rig() -> Project {
        let mut p = Project::demo();
        let region = &mut p.tracks[0].regions[0];
        region.start_bar = 1.0;
        region.len_bars = 4.0;
        region.notes = vec![note(0.0, 2.0), note(3.0, 1.0), note(8.0, 1.0)];
        region.controls.clear();
        p
    }

    #[test]
    fn trimming_the_left_of_a_midi_region_moves_the_notes_with_it() {
        let mut p = rig();
        assert!(trim(&mut p, 0, 0, Edge::Left, 1.0));
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.start_bar, 1.25, "one beat is a quarter of a bar");
        assert_eq!(r.len_bars, 3.75, "the end stays");
        assert_eq!(
            r.notes,
            vec![note(0.0, 1.0), note(2.0, 1.0), note(7.0, 1.0)],
            "the first note is cut to start at the edge"
        );
        // A note ending exactly at the edge is gone.
        let mut p = rig();
        assert!(trim(&mut p, 0, 0, Edge::Left, 2.0));
        assert_eq!(p.tracks[0].regions[0].notes.len(), 2);
    }

    #[test]
    fn trimming_the_right_of_a_midi_region_cuts_the_notes_there() {
        let mut p = rig();
        assert!(trim(&mut p, 0, 0, Edge::Right, 3.5));
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.len_bars, 0.875);
        assert_eq!(r.notes, vec![note(0.0, 2.0), note(3.0, 0.5)]);
        // Extending adds nothing and keeps the start.
        assert!(trim(&mut p, 0, 0, Edge::Right, 20.0));
        let r = &p.tracks[0].regions[0];
        assert_eq!((r.start_bar, r.len_bars), (1.0, 5.0));
        assert_eq!(r.notes.len(), 2);
    }

    #[test]
    fn an_edge_cannot_cross_the_other_one() {
        let mut p = rig();
        trim(&mut p, 0, 0, Edge::Left, 99.0);
        let r = &p.tracks[0].regions[0];
        assert!(r.len_bars >= 1.0 / 64.0 && r.start_bar < 5.0);
        let mut p = rig();
        trim(&mut p, 0, 0, Edge::Right, -5.0);
        assert!(p.tracks[0].regions[0].len_bars >= 1.0 / 64.0);
    }

    #[test]
    fn audio_trims_move_the_part_of_the_file_that_plays() {
        let mut p = rig();
        p.tracks[0].regions[0].audio = Some(tone());
        let file_secs = p.tracks[0].regions[0]
            .audio
            .as_ref()
            .unwrap()
            .file
            .duration_secs();
        let map = p.tempo_map();
        let start = p.region_start_beats(&p.tracks[0].regions[0]);
        let end_secs_before = map.seconds_at(start) + file_secs;
        // Left edge a tenth of the way in: the sound starts later in the file, ends where it did.
        let to = map.beat_at(map.seconds_at(start) + file_secs * 0.1);
        assert!(trim(&mut p, 0, 0, Edge::Left, to));
        let a = p.tracks[0].regions[0].audio.as_ref().unwrap();
        assert!((a.offset_secs - file_secs * 0.1).abs() < 1e-6);
        assert!((a.length_secs.unwrap() - file_secs * 0.9).abs() < 1e-6);
        let r = &p.tracks[0].regions[0];
        let start_now = p.region_start_beats(r);
        let end_secs = map.seconds_at(start_now) + a.length_secs.unwrap();
        assert!(
            (end_secs - end_secs_before).abs() < 1e-4,
            "the end stays in time"
        );
        // Right edge: shorten to half of what is left.
        let half = map.beat_at(map.seconds_at(start_now) + file_secs * 0.45);
        assert!(trim(&mut p, 0, 0, Edge::Right, half));
        let a = p.tracks[0].regions[0].audio.as_ref().unwrap();
        assert!((a.length_secs.unwrap() - file_secs * 0.45).abs() < 1e-6);
        assert!(
            (a.offset_secs - file_secs * 0.1).abs() < 1e-6,
            "the start is untouched"
        );
    }

    #[test]
    fn audio_cannot_be_dragged_past_the_ends_of_its_file() {
        let mut p = rig();
        p.tracks[0].regions[0].audio = Some(tone());
        let file_secs = p.tracks[0].regions[0]
            .audio
            .as_ref()
            .unwrap()
            .file
            .duration_secs();
        trim(&mut p, 0, 0, Edge::Right, 1000.0);
        let a = p.tracks[0].regions[0].audio.as_ref().unwrap();
        assert!(
            (a.length_secs.unwrap() - file_secs).abs() < 1e-9,
            "no longer than the file"
        );
        trim(&mut p, 0, 0, Edge::Left, 0.0);
        let a = p.tracks[0].regions[0].audio.as_ref().unwrap();
        assert_eq!(a.offset_secs, 0.0, "no earlier than the file's start");
        trim(&mut p, 0, 0, Edge::Left, 1000.0);
        let a = p.tracks[0].regions[0].audio.as_ref().unwrap();
        assert!(a.length_secs.unwrap() >= MIN_AUDIO_SECS - 1e-9);
        assert!(a.offset_secs + a.length_secs.unwrap() <= file_secs + 1e-9);
    }

    #[test]
    fn flex_and_generated_regions_are_not_trimmable() {
        let mut p = rig();
        assert!(trimmable(&p.tracks[0].regions[0]));
        p.tracks[0].regions[0].generated = Some(crate::model::Generated::new(
            crate::model::Generator::Pattern(crate::pattern::PatternParams::drums()),
        ));
        let before = p.tracks[0].regions[0].len_bars;
        assert!(!trim(&mut p, 0, 0, Edge::Right, 2.0));
        assert_eq!(p.tracks[0].regions[0].len_bars, before);
    }
}
