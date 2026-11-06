//! Edit > Cut, Copy, Paste, Split at Playhead and Join on the selected regions (Logic Pro: Edit
//! menu of the Tracks area). Select All is in `tracks_area.rs`, with the selection itself.
//!
//! Logic behaviour followed here:
//! - Copy and Cut take every selected region; Paste puts them on the selected track at the
//!   playhead, keeping the distances between them.
//! - Split at Playhead cuts every selected MIDI region that the playhead is inside of. A note
//!   that crosses the cut ends there, and the right piece is what gets selected.
//! - Join merges the selected MIDI regions of one track into one region that spans them all, with
//!   each region's notes kept where they were. The joined region takes the earliest region's name
//!   and settings.
//!
//! Not done: audio regions cannot be split or joined yet (the engine has no start offset into a
//! file), and Pattern, Drummer and take-folder regions are not, because their notes are made from
//! settings or takes.

use crate::model::{Control, Note, Project, Region, TrackKind};
use crate::tracks_area::TracksView;

/// The regions the clipboard holds (Edit > Copy / Cut), earliest first.
pub(crate) type RegionClip = Vec<Region>;

/// A MIDI region with its own notes: the only kind that can be split or joined.
fn plain_midi(r: &Region) -> bool {
    r.audio.is_none() && r.folder.is_none() && r.generated.is_none()
}

/// Copies every selected region to the clipboard. Returns whether there was one.
pub(crate) fn copy_selected(p: &Project, v: &TracksView, clip: &mut RegionClip) -> bool {
    let mut picked: Vec<Region> = v
        .selection()
        .into_iter()
        .filter_map(|(t, r)| p.tracks.get(t)?.regions.get(r).cloned())
        .collect();
    if picked.is_empty() {
        return false;
    }
    picked.sort_by(|a, b| a.start_bar.total_cmp(&b.start_bar));
    *clip = picked;
    true
}

/// Copies the selected regions and removes them from their tracks.
pub(crate) fn cut_selected(p: &mut Project, v: &mut TracksView, clip: &mut RegionClip) {
    if copy_selected(p, v, clip) {
        crate::tracks_area::delete_selected(p, v);
    }
}

/// Whether the clipboard regions fit the selected track: audio regions go on audio tracks, the
/// others on tracks that play MIDI.
pub(crate) fn can_paste(p: &Project, clip: &RegionClip) -> bool {
    let Some(track) = p.tracks.get(p.selected_track) else {
        return false;
    };
    !clip.is_empty()
        && track.kind.has_regions()
        && clip
            .iter()
            .all(|r| r.audio.is_some() == (track.kind == TrackKind::Audio))
}

/// Puts copies of the clipboard regions on the selected track, the earliest at the playhead,
/// and selects them.
pub(crate) fn paste_at_playhead(p: &mut Project, v: &mut TracksView, clip: &RegionClip) {
    if !can_paste(p, clip) {
        return;
    }
    let base = clip.iter().map(|r| r.start_bar).fold(f32::MAX, f32::min);
    let playhead_bar = p.beats_to_bar(p.position_beats) as f32 + 1.0;
    let t = p.selected_track;
    let Some(track) = p.tracks.get_mut(t) else {
        return;
    };
    let mut made = Vec::new();
    for r in clip {
        track.regions.push(Region {
            start_bar: playhead_bar + (r.start_bar - base),
            ..r.clone()
        });
        made.push((t, track.regions.len() - 1));
    }
    v.select_many(&made);
    p.song_rev += 1;
}

/// Smallest piece (in quarter notes) a split may leave on either side.
const MIN_PIECE: f64 = 1e-6;

/// Whether the playhead is inside region `r` of track `t`, away from its edges, and the region
/// can be cut there.
fn can_split_at(p: &Project, t: usize, r: usize) -> bool {
    let Some(region) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return false;
    };
    plain_midi(region)
        && p.position_beats > p.region_start_beats(region) + MIN_PIECE
        && p.position_beats < p.region_end_beats(region) - MIN_PIECE
}

/// Whether Split at Playhead does anything to the selection.
pub(crate) fn can_split(p: &Project, v: &TracksView) -> bool {
    v.selection()
        .into_iter()
        .any(|(t, r)| can_split_at(p, t, r))
}

/// Cuts region `r` of track `t` in two at `at`. The left piece keeps what starts before the cut,
/// the right one (appended to the track) what starts after it. Returns the right piece's index.
fn split_region(p: &mut Project, t: usize, r: usize, at: f64) -> Option<usize> {
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
        .map(|n| Note {
            start: n.start - local,
            ..*n
        })
        .collect();
    right.controls = region
        .controls
        .iter()
        .filter(|c| c.start >= local)
        .map(|c| Control {
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
    Some(track.regions.len() - 1)
}

/// Split at Playhead: every selected region the playhead is inside of is cut. The right pieces
/// become the selection.
pub(crate) fn split_at_playhead(p: &mut Project, v: &mut TracksView) {
    let at = p.position_beats;
    let mut targets: Vec<(usize, usize)> = v
        .selection()
        .into_iter()
        .filter(|&(t, r)| can_split_at(p, t, r))
        .collect();
    // Highest index first on each track: a cut appends its right piece, so lower indexes stay
    // valid.
    targets.sort_unstable();
    targets.reverse();
    let mut made = Vec::new();
    for (t, r) in targets {
        if let Some(right) = split_region(p, t, r, at) {
            made.push((t, right));
        }
    }
    if !made.is_empty() {
        v.select_many(&made);
        p.song_rev += 1;
    }
}

/// The groups Join merges: per track, the indexes of its selected regions, earliest first. A
/// group needs two or more regions, all of them plain MIDI regions.
fn join_groups(p: &Project, v: &TracksView) -> Vec<(usize, Vec<usize>)> {
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for (t, r) in v.selection() {
        match groups.iter_mut().find(|(gt, _)| *gt == t) {
            Some((_, rs)) => rs.push(r),
            None => groups.push((t, vec![r])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(t, mut rs)| {
            let regions = &p.tracks.get(t)?.regions;
            if rs.len() < 2 || !rs.iter().all(|&r| regions.get(r).is_some_and(plain_midi)) {
                return None;
            }
            rs.sort_by(|&a, &b| {
                p.region_start_beats(&regions[a])
                    .total_cmp(&p.region_start_beats(&regions[b]))
            });
            Some((t, rs))
        })
        .collect()
}

/// Whether Join does anything to the selection.
pub(crate) fn can_join(p: &Project, v: &TracksView) -> bool {
    !join_groups(p, v).is_empty()
}

/// Join: merges each group of selected regions into one region on its track. The joined region
/// is selected.
pub(crate) fn join_selected(p: &mut Project, v: &mut TracksView) {
    let groups = join_groups(p, v);
    let mut made = Vec::new();
    for (t, rs) in groups {
        let regions = &p.tracks[t].regions;
        let base = p.region_start_beats(&regions[rs[0]]);
        let end = rs
            .iter()
            .map(|&r| p.region_end_beats(&regions[r]))
            .fold(f64::MIN, f64::max);
        // Shift every region's notes and controls by where that region starts.
        let mut joined = regions[rs[0]].clone();
        joined.notes.clear();
        joined.controls.clear();
        for &r in &rs {
            let reg = &regions[r];
            let off = (p.region_start_beats(reg) - base) as f32;
            joined.notes.extend(reg.notes.iter().map(|n| Note {
                start: n.start + off,
                ..*n
            }));
            joined.controls.extend(reg.controls.iter().map(|c| Control {
                start: c.start + off,
                ..*c
            }));
        }
        joined.start_bar = p.beats_to_bar(base) as f32 + 1.0;
        joined.len_bars = (p.beats_to_bar(end) - p.beats_to_bar(base)) as f32;

        // Remove the joined regions (highest index first), then put the joined one where the
        // first of them was.
        let track = &mut p.tracks[t];
        let mut sorted = rs.clone();
        sorted.sort_unstable();
        for &r in sorted.iter().rev() {
            track.regions.remove(r);
        }
        let at = sorted[0];
        track.regions.insert(at, joined);
        made.push((t, at));
    }
    if !made.is_empty() {
        v.select_many(&made);
        p.song_rev += 1;
    }
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

    fn note(start: f32, len: f32) -> Note {
        Note {
            start,
            len,
            pitch: 60,
            vel: 90,
        }
    }

    /// The demo project with one region on track 0: 4 bars with notes at beats 0, 3 and 8.
    fn rig() -> (Project, TracksView) {
        let mut p = Project::demo();
        let region = &mut p.tracks[0].regions[0];
        region.start_bar = 1.0;
        region.len_bars = 4.0;
        region.notes = vec![note(0.0, 1.0), note(3.0, 3.0), note(8.0, 1.0)];
        region.controls.clear();
        p.tracks[0].regions.truncate(1);
        let mut v = TracksView::default();
        v.select_only(0, 0);
        p.selected_track = 0;
        (p, v)
    }

    /// Adds a second region on track 0 at `bar` with the same length and one note.
    fn second_region(p: &mut Project, bar: f32) {
        let mut r = p.tracks[0].regions[0].clone();
        r.start_bar = bar;
        r.len_bars = 2.0;
        r.notes = vec![note(1.0, 1.0)];
        p.tracks[0].regions.push(r);
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
    fn split_needs_the_playhead_inside_a_selected_region() {
        let (mut p, v) = rig();
        for at in [0.0, 16.0, 40.0] {
            p.position_beats = at;
            assert!(!can_split(&p, &v), "at {at}");
        }
    }

    #[test]
    fn split_cuts_every_selected_region_under_the_playhead() {
        let (mut p, mut v) = rig();
        second_region(&mut p, 1.0); // also under bar 1
        v.select_many(&[(0, 0), (0, 1)]);
        p.position_beats = 2.0;
        split_at_playhead(&mut p, &mut v);
        assert_eq!(p.tracks[0].regions.len(), 4, "both regions were cut");
        assert_eq!(v.selection().len(), 2, "the two right pieces are selected");
    }

    #[test]
    fn audio_regions_are_not_split() {
        let (mut p, v) = rig();
        p.position_beats = 4.0;
        p.tracks[0].regions[0].audio = Some(test_audio());
        assert!(!can_split(&p, &v));
    }

    #[test]
    fn copy_paste_puts_the_regions_at_the_playhead_keeping_their_spacing() {
        let (mut p, mut v) = rig();
        second_region(&mut p, 3.0); // two bars after the first
        v.select_many(&[(0, 0), (0, 1)]);
        let mut clip = Vec::new();
        assert!(!can_paste(&p, &clip));
        assert!(copy_selected(&p, &v, &mut clip));
        assert_eq!(clip.len(), 2);
        p.position_beats = 16.0; // bar 5
        paste_at_playhead(&mut p, &mut v, &clip);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 4);
        assert_eq!(regions[2].start_bar, 5.0);
        assert_eq!(regions[3].start_bar, 7.0, "the same two bars apart");
        assert_eq!(v.selection().len(), 2);
    }

    #[test]
    fn cut_removes_the_regions_and_paste_brings_them_back() {
        let (mut p, mut v) = rig();
        let mut clip = Vec::new();
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
        assert!(!can_paste(&p, &vec![audio]));
    }

    #[test]
    fn join_merges_two_regions_and_keeps_each_notes_place() {
        let (mut p, mut v) = rig();
        second_region(&mut p, 5.0); // starts at bar 5, after the first (bars 1-4)
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(can_join(&p, &v));
        join_selected(&mut p, &mut v);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 1, "one region is left");
        let joined = &regions[0];
        assert_eq!(
            (joined.start_bar, joined.len_bars),
            (1.0, 6.0),
            "bars 1 to 6"
        );
        // The first region's notes at 0, 3 and 8; the second's note at 1, moved on by the 16 beats
        // before bar 5, to 17.
        let mut starts: Vec<f32> = joined.notes.iter().map(|n| n.start).collect();
        starts.sort_by(f32::total_cmp);
        assert_eq!(starts, vec![0.0, 3.0, 8.0, 17.0]);
        assert_eq!(v.selected, Some((0, 0)));
    }

    #[test]
    fn join_needs_two_plain_midi_regions_on_one_track() {
        let (mut p, mut v) = rig();
        assert!(!can_join(&p, &v), "one region");
        second_region(&mut p, 5.0);
        p.tracks[0].regions[1].generated = None;
        p.tracks[0].regions[1].audio = Some(test_audio());
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(!can_join(&p, &v), "an audio region is in it");
    }
}
