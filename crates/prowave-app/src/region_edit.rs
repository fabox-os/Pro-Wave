//! Edit > Cut, Copy, Paste, Split at Playhead and Join on the selected regions (Logic Pro: Edit
//! menu of the Tracks area). Select All is in `tracks_area.rs`, with the selection itself.
//!
//! Logic behaviour followed here:
//! - Copy and Cut take every selected region; Paste puts them on the selected track at the
//!   playhead, keeping the distances between them.
//! - Split at Playhead cuts every selected region that the playhead is inside of. The right
//!   piece is what gets selected.
//!   - MIDI notes that cross the cut end there.
//!   - An audio region's right piece plays on from the cut in its file (the engine starts it that
//!     many frames in), so the two pieces sound like the one region.
//!   - A Pattern or Drummer region's pieces keep its settings and the notes they had, cut like
//!     MIDI notes are. Changing the settings then makes each piece's notes again from its start.
//!   - A take folder becomes two folders: each take is cut, and the comp too.
//! - Join merges the selected regions of one track into one region that spans them all.
//!   - MIDI regions keep their notes where they were. Pattern and Drummer settings stay when all
//!     of them share the same settings and play on one after another; otherwise the joined region
//!     is plain MIDI.
//!   - Audio regions join when they are of one file and play on from one another in it.
//!   - Take folders join when they have the same number of takes and touch.
//! - Flex Time and Flex Pitch regions are neither split nor joined: the edit is made from the whole
//!   recording, which a piece does not play.

use crate::model::{AudioRegion, Control, Generated, Note, Project, Region, TakeFolder, TrackKind};
use crate::tracks_area::TracksView;

/// The regions the clipboard holds (Edit > Copy / Cut), earliest first.
pub(crate) type RegionClip = Vec<Region>;

/// A MIDI region with its own notes: the only kind that can be quantized.
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

/// Edit > Paste Replace: puts the clipboard at the playhead like Paste, after removing the
/// regions on the selected track that the pasted span covers.
pub(crate) fn paste_replace(p: &mut Project, v: &mut TracksView, clip: &RegionClip) {
    if !can_paste(p, clip) {
        return;
    }
    // How long the clipboard is, from its earliest region.
    let base = clip.iter().map(|r| r.start_bar).fold(f32::MAX, f32::min);
    let span = clip
        .iter()
        .map(|r| r.start_bar + r.len_bars - base)
        .fold(0.0f32, f32::max);
    let from = p.beats_to_bar(p.position_beats) as f32 + 1.0;
    let to = from + span;
    let Some(track) = p.tracks.get_mut(p.selected_track) else {
        return;
    };
    track
        .regions
        .retain(|r| r.start_bar + r.len_bars <= from + 1e-6 || r.start_bar >= to - 1e-6);
    paste_at_playhead(p, v, clip);
}

/// Smallest piece (in quarter notes) a split may leave on either side.
const MIN_PIECE: f64 = 1e-6;

/// How far apart (in quarter notes) the edges of two regions may be and still touch. Bars are
/// stored as `f32`, so two edges that meet are not exactly equal.
const TOUCH: f64 = 1e-3;

/// Whether a region can be cut and joined: all but a Flex Time or Flex Pitch audio region.
fn editable(r: &Region) -> bool {
    r.audio.as_ref().is_none_or(|a| a.flex.is_none())
}

/// Cuts MIDI notes and controllers `at` beats into two pieces. The left piece keeps what starts
/// before the cut (a note that crosses it ends there). The right one keeps what starts at or
/// after it, moved to its own start. Returns the left notes and controls, then the right ones.
pub(crate) fn cut_notes(
    notes: &[Note],
    controls: &[Control],
    at: f32,
) -> (Vec<Note>, Vec<Control>, Vec<Note>, Vec<Control>) {
    let left_notes = notes
        .iter()
        .filter(|n| n.start < at)
        .map(|n| Note {
            len: n.len.min(at - n.start),
            ..*n
        })
        .collect();
    let right_notes = notes
        .iter()
        .filter(|n| n.start >= at)
        .map(|n| Note {
            start: n.start - at,
            ..*n
        })
        .collect();
    let left_controls = controls.iter().filter(|c| c.start < at).copied().collect();
    let right_controls = controls
        .iter()
        .filter(|c| c.start >= at)
        .map(|c| Control {
            start: c.start - at,
            ..*c
        })
        .collect();
    (left_notes, left_controls, right_notes, right_controls)
}

/// How many frames of an audio region's part play before the beat `at`. The sound runs in real
/// time, so the frames follow the tempo map. `None` when either side of the cut would have no
/// frames.
fn cut_frames(p: &Project, region: &Region, a: &AudioRegion, at: f64) -> Option<u64> {
    let map = p.tempo_map();
    let secs = map.seconds_at(at) - map.seconds_at(p.region_start_beats(region));
    let before = (secs * f64::from(a.file.sample_rate)).round();
    let (_, len) = a.part();
    ((1.0..=(len as f64 - 1.0)).contains(&before)).then_some(before as u64)
}

/// Whether the playhead is inside region `r` of track `t`, away from its edges, and the region
/// can be cut there.
fn can_split_at(p: &Project, t: usize, r: usize) -> bool {
    let Some(region) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return false;
    };
    let at = p.position_beats;
    editable(region)
        && at > p.region_start_beats(region) + MIN_PIECE
        && at < p.region_end_beats(region) - MIN_PIECE
        && region
            .audio
            .as_ref()
            .is_none_or(|a| cut_frames(p, region, a, at).is_some())
}

/// Whether Split at Playhead does anything to the selection.
pub(crate) fn can_split(p: &Project, v: &TracksView) -> bool {
    v.selection()
        .into_iter()
        .any(|(t, r)| can_split_at(p, t, r))
}

/// Cuts region `r` of track `t` in two at beat `at`. The left piece stays at `r`; the right one
/// (appended to the track) plays on from the cut. Returns the right piece's index, or `None`
/// when the cut would leave an audio piece with no frames (nothing is changed then).
fn split_region(p: &mut Project, t: usize, r: usize, at: f64) -> Option<usize> {
    let region = p.tracks.get(t)?.regions.get(r)?.clone();
    let local = (at - p.region_start_beats(&region)) as f32;
    let cut_bar = p.beats_to_bar(at) as f32 + 1.0;
    let end_bar = region.start_bar + region.len_bars;
    let mut left = region.clone();
    let mut right = region.clone();
    left.len_bars = cut_bar - region.start_bar;
    right.start_bar = cut_bar;
    right.len_bars = end_bar - cut_bar;

    if let Some(folder) = &region.folder {
        let (lf, rf) = folder.split(local);
        lf.apply(&mut left.notes, &mut left.controls);
        rf.apply(&mut right.notes, &mut right.controls);
        left.folder = Some(lf);
        right.folder = Some(rf);
    } else {
        let (ln, lc, rn, rc) = cut_notes(&region.notes, &region.controls, local);
        left.notes = ln;
        left.controls = lc;
        right.notes = rn;
        right.controls = rc;
    }

    if let Some(a) = &region.audio {
        let before = cut_frames(p, &region, a, at)?;
        let (start, total) = a.part();
        // A forward region plays its part from the start of it; a reversed one from its end, so
        // its left piece holds the later frames of the file and its right piece the earlier.
        let (left_start, right_start) = if region.params.reverse {
            (start + total - before, start)
        } else {
            (start, start + before)
        };
        left.audio = Some(AudioRegion {
            start_frame: left_start,
            len_frames: Some(before),
            ..a.clone()
        });
        right.audio = Some(AudioRegion {
            start_frame: right_start,
            len_frames: Some(total - before),
            ..a.clone()
        });
        // The cut is not faded: the left piece keeps the fade in and the right one the fade out.
        left.params.fade_out = 0.0;
        right.params.fade_in = 0.0;
    }

    // A generated piece keeps its settings. Its notes are the cut ones, so they are marked as
    // made for the piece's own bars: making them again would start the settings over here.
    if left.generated.is_some() {
        let key = p.generated_key(&left);
        if let Some(g) = left.generated.as_mut() {
            g.made_for = key;
        }
    }
    if right.generated.is_some() {
        let key = p.generated_key(&right);
        if let Some(g) = right.generated.as_mut() {
            g.made_for = key;
        }
    }

    let track = &mut p.tracks[t];
    track.regions[r] = left;
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

/// Whether two regions' edges meet on the timeline: `b` starts where `a` ends.
fn touches(p: &Project, a: &Region, b: &Region) -> bool {
    (p.region_end_beats(a) - p.region_start_beats(b)).abs() < TOUCH
}

/// The number of takes of a take folder region.
fn takes_of(r: &Region) -> Option<usize> {
    r.folder.as_ref().map(|f| f.takes.len())
}

/// Whether audio region `b` plays on from where `a` stops: the same file with the same edits
/// (the fades aside), next to `a` on the timeline, and on the next frames of the file. A
/// reversed pair runs the other way through the file, so `b` then holds the frames before `a`'s.
fn audio_follows(p: &Project, a: &Region, b: &Region) -> bool {
    let (Some(x), Some(y)) = (a.audio.as_ref(), b.audio.as_ref()) else {
        return false;
    };
    let (mut ea, mut eb) = (a.params, b.params);
    (ea.fade_in, ea.fade_out, eb.fade_in, eb.fade_out) = (0.0, 0.0, 0.0, 0.0);
    let (xs, xl) = x.part();
    let (ys, yl) = y.part();
    let frames_follow = if a.params.reverse {
        ys + yl == xs
    } else {
        xs + xl == ys
    };
    x.file.path == y.file.path && ea == eb && touches(p, a, b) && frames_follow
}

/// Whether the regions (earliest first) join into one: all MIDI regions (plain or made from
/// settings), all audio regions that play on from one another, or all take folders with the same
/// number of takes that touch. Flex edited audio never joins.
fn joinable(p: &Project, pieces: &[&Region]) -> bool {
    let Some(first) = pieces.first() else {
        return false;
    };
    if !pieces.iter().all(|r| editable(r)) {
        return false;
    }
    if first.audio.is_some() {
        pieces.windows(2).all(|w| audio_follows(p, w[0], w[1]))
    } else if first.folder.is_some() {
        pieces.iter().all(|r| r.folder.is_some())
            && pieces
                .windows(2)
                .all(|w| touches(p, w[0], w[1]) && takes_of(w[0]) == takes_of(w[1]))
    } else {
        pieces
            .iter()
            .all(|r| r.audio.is_none() && r.folder.is_none())
    }
}

/// The groups Join merges: per track, the indexes of its selected regions, earliest first. A
/// group needs two or more regions, and they must be joinable.
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
            if rs.len() < 2 || !rs.iter().all(|&r| regions.get(r).is_some()) {
                return None;
            }
            rs.sort_by(|&a, &b| {
                p.region_start_beats(&regions[a])
                    .total_cmp(&p.region_start_beats(&regions[b]))
            });
            let pieces: Vec<&Region> = rs.iter().map(|&r| &regions[r]).collect();
            joinable(p, &pieces).then_some((t, rs))
        })
        .collect()
}

/// Whether Join does anything to the selection.
pub(crate) fn can_join(p: &Project, v: &TracksView) -> bool {
    !join_groups(p, v).is_empty()
}

/// The settings a joined MIDI region keeps. When every piece is made from the same settings and
/// they play on one after another, the joined region keeps them too, and its notes (the pieces'
/// own) are not made again. Otherwise it has none, and its notes are plain.
fn joined_settings(p: &Project, pieces: &[&Region], joined: &Region) -> Option<Generated> {
    let first = pieces.first()?.generated.as_ref()?;
    let same = pieces.iter().all(|r| {
        r.generated
            .as_ref()
            .is_some_and(|g| g.source == first.source)
    });
    let next = pieces.windows(2).all(|w| touches(p, w[0], w[1]));
    (same && next).then(|| {
        let mut g = Generated::new(first.source.clone());
        g.made_for = p.generated_key(joined);
        g
    })
}

/// The region that joins the regions `rs` (indexes on track `t`, earliest first). It takes the
/// earliest region's name and settings, with the notes, audio or takes of all of them. `None`
/// when the regions cannot be joined.
fn join_pieces(p: &Project, t: usize, rs: &[usize]) -> Option<Region> {
    let regions = &p.tracks.get(t)?.regions;
    let pieces: Vec<&Region> = rs.iter().map(|&r| regions.get(r)).collect::<Option<_>>()?;
    let first = *pieces.first()?;
    let base = p.region_start_beats(first);
    let end = pieces
        .iter()
        .map(|r| p.region_end_beats(r))
        .fold(f64::MIN, f64::max);
    let mut joined = first.clone();
    joined.start_bar = p.beats_to_bar(base) as f32 + 1.0;
    joined.len_bars = (p.beats_to_bar(end) - p.beats_to_bar(base)) as f32;

    if let Some(a) = &first.audio {
        // The joined region plays from the earliest frame of the pieces to the latest.
        let (start, stop) = pieces
            .iter()
            .filter_map(|r| r.audio.as_ref())
            .map(|x| {
                let (s, l) = x.part();
                (s, s + l)
            })
            .fold((u64::MAX, 0), |(lo, hi), (s, e)| (lo.min(s), hi.max(e)));
        let frames = a.file.frames;
        joined.audio = Some(AudioRegion {
            start_frame: start,
            // The whole file is kept as it is when no part of it is cut off, as an unsplit
            // region keeps it.
            len_frames: (start != 0 || stop != frames).then_some(stop - start),
            ..a.clone()
        });
        // The fade in is the first piece's, the fade out the last one's (the cuts are not faded).
        joined.params.fade_out = pieces.last()?.params.fade_out;
    } else if let Some(folder) = &first.folder {
        let mut joined_folder = folder.clone();
        for r in pieces.iter().skip(1) {
            joined_folder = TakeFolder::joined(&joined_folder, r.folder.as_ref()?)?;
        }
        joined_folder.apply(&mut joined.notes, &mut joined.controls);
        joined.folder = Some(joined_folder);
    } else {
        joined.notes.clear();
        joined.controls.clear();
        for r in &pieces {
            // Each region's notes move on by how far it starts after the first one.
            let off = (p.region_start_beats(r) - base) as f32;
            joined.notes.extend(r.notes.iter().map(|n| Note {
                start: n.start + off,
                ..*n
            }));
            joined.controls.extend(r.controls.iter().map(|c| Control {
                start: c.start + off,
                ..*c
            }));
        }
        joined.generated = joined_settings(p, &pieces, &joined);
    }
    Some(joined)
}

/// Join: merges each group of selected regions into one region on its track. The joined region
/// is selected.
pub(crate) fn join_selected(p: &mut Project, v: &mut TracksView) {
    let groups = join_groups(p, v);
    let mut made = Vec::new();
    for (t, rs) in groups {
        let Some(joined) = join_pieces(p, t, &rs) else {
            continue;
        };
        let track = &mut p.tracks[t];
        // Remove the joined regions (highest index first), then put the joined one where the
        // first of them was.
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

/// Functions > Move to Playhead: the earliest selected region starts at the playhead, the others
/// keep their distances from it (each stays on its track).
pub(crate) fn move_to_playhead(p: &mut Project, v: &TracksView) {
    let sel = v.selection();
    let base = sel
        .iter()
        .filter_map(|&(t, r)| p.tracks.get(t)?.regions.get(r))
        .map(|reg| reg.start_bar)
        .fold(f32::MAX, f32::min);
    if sel.is_empty() || base == f32::MAX {
        return;
    }
    let playhead_bar = p.beats_to_bar(p.position_beats) as f32 + 1.0;
    for (t, r) in sel {
        if let Some(reg) = p.tracks.get_mut(t).and_then(|tr| tr.regions.get_mut(r)) {
            reg.start_bar = playhead_bar + (reg.start_bar - base);
        }
    }
    p.song_rev += 1;
}

/// Grid used by Functions > Quantize when the region's Quantize setting is Off: 1/16 note.
pub const DEFAULT_QUANTIZE_GRID: f32 = 0.25;

/// Whether Quantize does anything to the selection: a selected region with notes of its own.
pub(crate) fn can_quantize(p: &Project, v: &TracksView) -> bool {
    v.selection().into_iter().any(|(t, r)| {
        p.tracks
            .get(t)
            .and_then(|tr| tr.regions.get(r))
            .is_some_and(|reg| plain_midi(reg) && !reg.notes.is_empty())
    })
}

/// Functions > Quantize: moves the notes of every selected MIDI region onto the region's
/// Quantize grid (1/16 when it is Off). Destructive, like Logic's Functions > Quantize.
pub(crate) fn quantize_selected(p: &mut Project, v: &TracksView) {
    let mut changed = false;
    for (t, r) in v.selection() {
        let Some(reg) = p.tracks.get_mut(t).and_then(|tr| tr.regions.get_mut(r)) else {
            continue;
        };
        if !plain_midi(reg) || reg.notes.is_empty() {
            continue;
        }
        let grid = reg
            .params
            .quantize
            .filter(|g| *g > 0.0)
            .unwrap_or(DEFAULT_QUANTIZE_GRID);
        let all: Vec<usize> = (0..reg.notes.len()).collect();
        crate::note_edit::quantize(&mut reg.notes, &all, grid, 1.0, 0.0);
        changed = true;
    }
    if changed {
        p.song_rev += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioRegion, Generator, Note, Take, TakeFolder};
    use crate::pattern::{DrumStyle, DrummerParams};

    fn test_audio() -> AudioRegion {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prowave-engine/tests/fixtures");
        AudioRegion::whole(
            prowave_engine::AudioFile::open(dir.join("tone.caf")).unwrap(),
            Default::default(),
        )
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

    /// The beat half way through the recording of region 0 of track 0.
    fn middle_of_recording(p: &Project) -> f64 {
        let region = &p.tracks[0].regions[0];
        let map = p.tempo_map();
        let secs = region
            .audio
            .as_ref()
            .map_or(0.0, |a| a.file.duration_secs());
        map.beat_at(map.seconds_at(p.region_start_beats(region)) + secs / 2.0)
    }

    fn drummer_settings() -> Option<Generated> {
        Some(Generated::new(Generator::Drummer(DrummerParams::new(
            DrumStyle::Rock,
        ))))
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
    fn an_audio_region_splits_into_two_pieces_that_play_the_file_on() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].audio = Some(test_audio());
        let total = p.tracks[0].regions[0].audio.as_ref().unwrap().file.frames;
        p.position_beats = middle_of_recording(&p);
        assert!(can_split(&p, &v));
        split_at_playhead(&mut p, &mut v);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 2);
        let left = regions[0].audio.as_ref().unwrap().part();
        let right = regions[1].audio.as_ref().unwrap().part();
        assert_eq!(left.0, 0, "the left piece starts at the file's start");
        assert_eq!(
            right.0,
            left.0 + left.1,
            "the right piece picks up where it ends"
        );
        assert_eq!(left.1 + right.1, total, "together they play the whole file");
        assert!(left.1 > 0 && right.1 > 0);
        assert_eq!(v.selected, Some((0, 1)));
    }

    #[test]
    fn a_reversed_audio_region_puts_the_later_frames_on_its_left_piece() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].audio = Some(test_audio());
        p.tracks[0].regions[0].params.reverse = true;
        let total = p.tracks[0].regions[0].audio.as_ref().unwrap().file.frames;
        p.position_beats = middle_of_recording(&p);
        split_at_playhead(&mut p, &mut v);
        let regions = &p.tracks[0].regions;
        let left = regions[0].audio.as_ref().unwrap().part();
        let right = regions[1].audio.as_ref().unwrap().part();
        assert_eq!(
            left.0 + left.1,
            total,
            "the left piece ends at the file's end"
        );
        assert_eq!(right.0, 0, "the right piece holds the start of the file");
        assert_eq!(right.1, total - left.1);
    }

    #[test]
    fn a_split_audio_region_round_trip_gives_the_original_region_back() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].audio = Some(test_audio());
        let before = p.tracks[0].regions[0].clone();
        p.position_beats = middle_of_recording(&p);
        split_at_playhead(&mut p, &mut v);
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(can_join(&p, &v));
        join_selected(&mut p, &mut v);
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 1, "one region is left");
        let joined = &regions[0];
        assert!((joined.start_bar - before.start_bar).abs() < 1e-4);
        assert!((joined.len_bars - before.len_bars).abs() < 1e-3);
        let (a, b) = (
            joined.audio.as_ref().unwrap(),
            before.audio.as_ref().unwrap(),
        );
        assert_eq!((a.start_frame, a.len_frames), (b.start_frame, b.len_frames));
        assert!(
            joined.params == before.params,
            "the region's edits come back"
        );
    }

    #[test]
    fn audio_pieces_join_only_when_they_play_on_from_each_other() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].audio = Some(test_audio());
        p.position_beats = middle_of_recording(&p);
        split_at_playhead(&mut p, &mut v);
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(can_join(&p, &v));
        p.tracks[0].regions[1].start_bar += 1.0;
        assert!(!can_join(&p, &v), "a gap on the timeline");
        p.tracks[0].regions[1].start_bar -= 1.0;
        p.tracks[0].regions[1].audio.as_mut().unwrap().start_frame += 1;
        assert!(!can_join(&p, &v), "not the next frames of the file");
        p.tracks[0].regions[1].audio.as_mut().unwrap().start_frame -= 1;
        p.tracks[0].regions[1].audio.as_mut().unwrap().file.path = "other.caf".into();
        assert!(!can_join(&p, &v), "another file");
    }

    #[test]
    fn a_flex_edited_region_is_not_split() {
        let (mut p, v) = rig();
        let mut a = test_audio();
        a.flex = Some(crate::flex_edit::FlexEdit::new(
            a.file.clone(),
            prowave_engine::FlexMode::Rhythmic,
        ));
        p.tracks[0].regions[0].audio = Some(a);
        p.position_beats = middle_of_recording(&p);
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
    fn paste_replace_removes_what_the_clipboard_covers() {
        let (mut p, mut v) = rig();
        v.select_many(&[(0, 0)]);
        let mut clip = Vec::new();
        copy_selected(&p, &v, &mut clip);
        p.position_beats = 4.0; // bar 2
        let before = p.tracks[0].regions.len();
        paste_replace(&mut p, &mut v, &clip);
        // The old region overlapped the pasted span and is gone; the pasted one is there.
        assert_eq!(p.tracks[0].regions.len(), before, "one out, one in");
        assert_eq!(p.tracks[0].regions[0].start_bar, 2.0);
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
    fn join_needs_two_midi_regions_on_one_track() {
        let (mut p, mut v) = rig();
        assert!(!can_join(&p, &v), "one region");
        second_region(&mut p, 5.0);
        p.tracks[0].regions[1].generated = None;
        p.tracks[0].regions[1].audio = Some(test_audio());
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(!can_join(&p, &v), "an audio region is in it");
    }

    #[test]
    fn a_generated_region_splits_into_two_that_keep_settings_and_their_notes() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].generated = drummer_settings();
        p.position_beats = 4.0; // the note at 3 crosses the cut
        split_at_playhead(&mut p, &mut v);
        let (left, right) = (&p.tracks[0].regions[0], &p.tracks[0].regions[1]);
        assert_eq!(left.notes.len(), 2);
        assert_eq!(right.notes.len(), 1);
        assert!((left.notes[1].len - 1.0).abs() < 1e-6, "cut like MIDI");
        let (lg, rg) = (left.generated.as_ref(), right.generated.as_ref());
        assert_eq!(
            lg.map(|g| &g.source),
            rg.map(|g| &g.source),
            "same settings"
        );
        assert_eq!(
            rg.map(|g| g.made_for),
            Some(p.generated_key(right)),
            "the pieces are not made again"
        );
    }

    #[test]
    fn a_generated_round_trip_keeps_the_settings_and_the_notes() {
        let (mut p, mut v) = rig();
        let original = p.tracks[0].regions[0].clone();
        p.tracks[0].regions[0].generated = drummer_settings();
        p.position_beats = 2.0; // no note crosses this cut
        split_at_playhead(&mut p, &mut v);
        v.select_many(&[(0, 0), (0, 1)]);
        join_selected(&mut p, &mut v);
        let joined = &p.tracks[0].regions[0];
        assert_eq!(joined.notes, original.notes);
        assert_eq!(
            joined.generated.as_ref().map(|g| g.source.clone()),
            drummer_settings().map(|g| g.source)
        );
    }

    #[test]
    fn generated_pieces_of_other_settings_join_as_plain_notes() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].generated = drummer_settings();
        p.position_beats = 2.0;
        split_at_playhead(&mut p, &mut v);
        let mut other = DrummerParams::new(DrumStyle::Rock);
        other.seed = 7;
        p.tracks[0].regions[1].generated = Some(Generated::new(Generator::Drummer(other)));
        v.select_many(&[(0, 0), (0, 1)]);
        join_selected(&mut p, &mut v);
        assert!(
            p.tracks[0].regions[0].generated.is_none(),
            "plain notes now"
        );
        assert_eq!(p.tracks[0].regions[0].notes.len(), 3);
    }

    #[test]
    fn a_take_folder_splits_and_joins_back_to_the_same_folder() {
        let (mut p, mut v) = rig();
        let take = |starts: &[f32]| Take {
            notes: starts.iter().map(|&s| note(s, 1.0)).collect(),
            controls: Vec::new(),
        };
        let mut folder = TakeFolder::new(vec![take(&[0.0, 3.0]), take(&[2.0])], 16.0);
        folder.swipe(0, 4.0, 12.0);
        let region = &mut p.tracks[0].regions[0];
        folder.apply(&mut region.notes, &mut region.controls);
        region.folder = Some(folder.clone());
        p.position_beats = 6.0;
        split_at_playhead(&mut p, &mut v);
        assert_eq!(p.tracks[0].regions.len(), 2);
        assert!(p.tracks[0].regions.iter().all(|r| r.folder.is_some()));
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(can_join(&p, &v));
        join_selected(&mut p, &mut v);
        let joined = &p.tracks[0].regions[0];
        assert_eq!(joined.folder.as_ref(), Some(&folder));
        assert_eq!(
            joined.notes,
            folder.flatten().0,
            "the comp plays what it did"
        );
    }

    #[test]
    fn take_folders_with_other_take_counts_do_not_join() {
        let (mut p, mut v) = rig();
        let take = |s: f32| Take {
            notes: vec![note(s, 1.0)],
            controls: Vec::new(),
        };
        p.tracks[0].regions[0].folder = Some(TakeFolder::new(vec![take(0.0), take(1.0)], 16.0));
        second_region(&mut p, 5.0);
        p.tracks[0].regions[1].folder = Some(TakeFolder::new(vec![take(0.0)], 8.0));
        v.select_many(&[(0, 0), (0, 1)]);
        assert!(!can_join(&p, &v));
    }

    #[test]
    fn move_to_playhead_keeps_the_distances_between_the_regions() {
        let (mut p, mut v) = rig();
        second_region(&mut p, 3.0); // two bars after the first
        v.select_many(&[(0, 0), (0, 1)]);
        p.position_beats = 32.0; // bar 9
        move_to_playhead(&mut p, &v);
        let starts: Vec<f32> = p.tracks[0].regions.iter().map(|r| r.start_bar).collect();
        assert_eq!(starts, vec![9.0, 11.0]);
    }

    #[test]
    fn move_to_playhead_does_nothing_without_a_selection() {
        let (mut p, mut v) = rig();
        v.clear_selection();
        p.position_beats = 32.0;
        move_to_playhead(&mut p, &v);
        assert_eq!(p.tracks[0].regions[0].start_bar, 1.0);
    }

    #[test]
    fn quantize_snaps_the_notes_of_selected_regions_to_the_grid() {
        let (mut p, mut v) = rig();
        // Notes at 0, 3 and 8 are on the 1/16 grid already; move one off it.
        p.tracks[0].regions[0].notes[1].start = 3.1;
        v.select_only(0, 0);
        assert!(can_quantize(&p, &v));
        quantize_selected(&mut p, &v);
        assert!((p.tracks[0].regions[0].notes[1].start - 3.0).abs() < 1e-5);
    }

    #[test]
    fn quantize_uses_the_regions_grid_and_skips_generated_regions() {
        let (mut p, mut v) = rig();
        p.tracks[0].regions[0].notes = vec![note(0.3, 1.0)];
        p.tracks[0].regions[0].params.quantize = Some(1.0); // whole beats
        v.select_only(0, 0);
        quantize_selected(&mut p, &v);
        assert_eq!(p.tracks[0].regions[0].notes[0].start, 0.0);
        // A generated region's notes are made from its settings, so they are left alone.
        p.tracks[0].regions[0].generated = drummer_settings();
        assert!(!can_quantize(&p, &v));
    }
}
