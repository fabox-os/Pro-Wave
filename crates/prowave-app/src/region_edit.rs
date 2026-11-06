//! Edit > Cut, Copy, Paste and Split at Playhead on the selected region (Logic Pro:
//! Edit menu of the Tracks area).
//!
//! Logic behaviour followed here: Paste puts the copied region on the selected track at the
//! playhead; Split at Playhead cuts the selected region in two where the playhead is, and a
//! note that crosses the cut is shortened to end there.
//!
//! Not done: audio regions cannot be split yet (the engine has no start offset into the file
//! for a region), and Pattern, Drummer and take-folder regions are not split because their
//! notes are made from settings or takes. The menu keeps those entries greyed.

use crate::model::{Project, Region, TrackKind};
use crate::tracks_area::TracksView;

/// The region the clipboard holds (Edit > Copy / Cut).
pub(crate) type RegionClip = Option<Region>;

/// Copies the selected region to the clipboard. Returns whether there was one.
pub(crate) fn copy_selected(p: &Project, v: &TracksView, clip: &mut RegionClip) -> bool {
    let Some((t, r)) = v.selected else {
        return false;
    };
    match p.tracks.get(t).and_then(|tr| tr.regions.get(r)) {
        Some(region) => {
            *clip = Some(region.clone());
            true
        }
        None => false,
    }
}

/// Copies the selected region and removes it from its track.
pub(crate) fn cut_selected(p: &mut Project, v: &mut TracksView, clip: &mut RegionClip) {
    if copy_selected(p, v, clip) {
        crate::tracks_area::delete_selected(p, v);
    }
}

/// Whether the clipboard region fits the selected track: audio regions go on audio tracks,
/// the others on tracks that play MIDI.
pub(crate) fn can_paste(p: &Project, clip: &RegionClip) -> bool {
    let (Some(region), Some(track)) = (clip, p.tracks.get(p.selected_track)) else {
        return false;
    };
    track.kind.has_regions() && region.audio.is_some() == (track.kind == TrackKind::Audio)
}

/// Puts a copy of the clipboard region on the selected track at the playhead and selects it.
pub(crate) fn paste_at_playhead(p: &mut Project, v: &mut TracksView, clip: &RegionClip) {
    if !can_paste(p, clip) {
        return;
    }
    let Some(region) = clip.clone() else { return };
    let start_bar = p.beats_to_bar(p.position_beats) as f32 + 1.0;
    let t = p.selected_track;
    let Some(track) = p.tracks.get_mut(t) else {
        return;
    };
    track.regions.push(Region {
        start_bar,
        ..region
    });
    v.selected = Some((t, track.regions.len() - 1));
    p.song_rev += 1;
}

/// Smallest piece (in quarter notes) a split may leave on either side.
const MIN_PIECE: f64 = 1e-6;

/// Whether the selected region can be cut at the playhead: a MIDI region that the playhead is
/// inside of.
pub(crate) fn can_split(p: &Project, v: &TracksView) -> bool {
    let Some((t, r)) = v.selected else {
        return false;
    };
    let Some(region) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return false;
    };
    region.audio.is_none()
        && region.folder.is_none()
        && region.generated.is_none()
        && p.position_beats > p.region_start_beats(region) + MIN_PIECE
        && p.position_beats < p.region_end_beats(region) - MIN_PIECE
}

/// Cuts the selected region in two at the playhead. The right piece becomes the selection.
pub(crate) fn split_at_playhead(p: &mut Project, v: &mut TracksView) {
    if !can_split(p, v) {
        return;
    }
    let Some((t, r)) = v.selected else { return };
    let at = p.position_beats;
    let cut_bar = p.beats_to_bar(at) as f32 + 1.0;
    let local = (at - p.region_start_beats(&p.tracks[t].regions[r])) as f32;

    let region = &mut p.tracks[t].regions[r];
    let end_bar = region.start_bar + region.len_bars;

    // The right piece: what starts at or after the cut, moved to the piece's own start.
    let mut right = region.clone();
    right.start_bar = cut_bar;
    right.len_bars = end_bar - cut_bar;
    right.notes = region
        .notes
        .iter()
        .filter(|n| n.start >= local)
        .map(|n| crate::model::Note {
            start: n.start - local,
            ..*n
        })
        .collect();
    right.controls = region
        .controls
        .iter()
        .filter(|c| c.start >= local)
        .map(|c| crate::model::Control {
            start: c.start - local,
            ..*c
        })
        .collect();

    // The left piece keeps what starts before the cut; a note that crosses it ends there.
    region.len_bars = cut_bar - region.start_bar;
    region.notes.retain(|n| n.start < local);
    for n in &mut region.notes {
        n.len = n.len.min(local - n.start);
    }
    region.controls.retain(|c| c.start < local);

    let track = &mut p.tracks[t];
    track.regions.push(right);
    v.selected = Some((t, track.regions.len() - 1));
    p.song_rev += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioRegion, Note};

    fn test_audio() -> AudioRegion {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prowave-engine/tests/fixtures");
        AudioRegion {
            file: prowave_engine::AudioFile::open(dir.join("tone.caf")).unwrap(),
            peaks: Default::default(),
            flex: None,
        }
    }

    /// The demo project with one region on track 0: 4 bars with notes at beats 0, 4 and 8.
    fn rig() -> (Project, TracksView) {
        let mut p = Project::demo();
        let note = |start: f32, len: f32| Note {
            start,
            len,
            pitch: 60,
            vel: 90,
        };
        let region = &mut p.tracks[0].regions[0];
        region.start_bar = 1.0;
        region.len_bars = 4.0;
        region.notes = vec![note(0.0, 1.0), note(3.0, 3.0), note(8.0, 1.0)];
        region.controls.clear();
        p.tracks[0].regions.truncate(1);
        let mut v = TracksView::default();
        v.selected = Some((0, 0));
        p.selected_track = 0;
        (p, v)
    }

    #[test]
    fn split_cuts_the_region_and_shortens_the_note_that_crosses() {
        let (mut p, mut v) = rig();
        p.position_beats = 4.0; // bar 2 in 4/4
        assert!(can_split(&p, &v));
        split_at_playhead(&mut p, &mut v);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 2);
        let (left, right) = (&regions[0], &regions[1]);
        assert_eq!((left.start_bar, left.len_bars), (1.0, 1.0));
        assert_eq!((right.start_bar, right.len_bars), (2.0, 3.0));
        // Notes at 0 and 3 stay left; the one at 3 (length 3) ends at the cut.
        assert_eq!(left.notes.len(), 2);
        assert!((left.notes[1].len - 1.0).abs() < 1e-6);
        // The note at 8 moves right and starts 4 beats into the right piece.
        assert_eq!(right.notes.len(), 1);
        assert!((right.notes[0].start - 4.0).abs() < 1e-6);
        assert_eq!(v.selected, Some((0, 1)));
    }

    #[test]
    fn split_needs_the_playhead_inside_the_region() {
        let (mut p, v) = rig();
        for at in [0.0, 16.0, 40.0] {
            p.position_beats = at;
            assert!(!can_split(&p, &v), "at {at}");
        }
    }

    #[test]
    fn audio_regions_are_not_split() {
        let (mut p, v) = rig();
        p.position_beats = 4.0;
        p.tracks[0].regions[0].audio = Some(test_audio());
        assert!(!can_split(&p, &v));
    }

    #[test]
    fn copy_paste_puts_the_region_at_the_playhead() {
        let (mut p, mut v) = rig();
        let mut clip = None;
        assert!(!can_paste(&p, &clip));
        assert!(copy_selected(&p, &v, &mut clip));
        p.position_beats = 16.0; // bar 5
        paste_at_playhead(&mut p, &mut v, &clip);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[1].start_bar, 5.0);
        assert_eq!(regions[1].notes.len(), regions[0].notes.len());
        assert_eq!(v.selected, Some((0, 1)));
    }

    #[test]
    fn cut_removes_the_region_and_paste_brings_it_back() {
        let (mut p, mut v) = rig();
        let mut clip = None;
        cut_selected(&mut p, &mut v, &mut clip);
        assert!(p.tracks[0].regions.is_empty() && v.selected.is_none());
        paste_at_playhead(&mut p, &mut v, &clip);
        assert_eq!(p.tracks[0].regions.len(), 1);
    }

    #[test]
    fn an_audio_region_does_not_paste_onto_a_midi_track() {
        let (p, _) = rig();
        let mut audio = p.tracks[0].regions[0].clone();
        audio.audio = Some(test_audio());
        assert!(!can_paste(&p, &Some(audio)));
    }
}
