//! Groove Track (Logic: Track > Track Header > Groove Track): one track sets the feel, and the
//! tracks that follow it play their notes with its timing. The groove is how far, on average,
//! the groove track's hits sit from each sixteenth of the beat (ahead, behind, swung); a note
//! of a following track near a sixteenth is moved by that much instead of its own distance.
//!
//! A MIDI groove track gives its notes' starts; an audio one the hits found in its recordings,
//! read in the background whenever its regions or the tempo change. Following tracks are MIDI
//! (audio would need Flex Time quantize to the groove, which is not done yet).
use crate::model::{Project, TrackKind};
use crate::UiState;
use prowave_engine::NoteEvent;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// How far from a sixteenth a hit or note may be and still belong to it, in beats.
const NEAR: f64 = 0.1;

/// The average distance (beats) of the groove from each of the four sixteenths of a beat.
pub type Template = [f64; 4];

/// Makes `t` the groove track (or no track, with `None`).
pub fn set_groove_track(p: &mut Project, t: Option<usize>) {
    for (i, track) in p.tracks.iter_mut().enumerate() {
        track.params.groove = Some(i) == t;
        if Some(i) == t {
            track.params.follow_groove = false;
        }
    }
    p.groove_onsets = Arc::new(Vec::new());
    p.groove_key = 0;
    p.song_rev += 1;
}

/// The groove track's index.
pub fn groove_track(p: &Project) -> Option<usize> {
    p.tracks.iter().position(|t| t.params.groove)
}

/// Where the groove track plays its hits, in beats from the song's start.
fn groove_hits(p: &Project) -> Vec<f64> {
    let Some(i) = groove_track(p) else {
        return Vec::new();
    };
    let t = &p.tracks[i];
    if t.kind == TrackKind::Audio {
        return p.groove_onsets.to_vec();
    }
    t.regions
        .iter()
        .filter(|r| !r.params.mute)
        .flat_map(|r| {
            let origin = p.region_start_beats(r);
            r.notes.iter().map(move |n| origin + f64::from(n.start))
        })
        .collect()
}

/// The groove of the groove track, if there is one with hits.
pub fn template(p: &Project) -> Option<Template> {
    template_of(&groove_hits(p))
}

/// The average distance of `hits` (beats) from each sixteenth of the beat; a sixteenth with
/// no hits near it keeps the grid.
pub fn template_of(hits: &[f64]) -> Option<Template> {
    let mut sum = [0.0f64; 4];
    let mut count = [0usize; 4];
    for x in hits.iter().filter(|x| x.is_finite()) {
        let grid = (x * 4.0).round() / 4.0;
        let d = x - grid;
        if d.abs() <= NEAR {
            let slot = ((grid * 4.0).round() as i64).rem_euclid(4) as usize;
            sum[slot] += d;
            count[slot] += 1;
        }
    }
    if count.iter().all(|c| *c == 0) {
        return None;
    }
    let mut t = [0.0; 4];
    for i in 0..4 {
        if count[i] > 0 {
            t[i] = sum[i] / count[i] as f64;
        }
    }
    Some(t)
}

/// Moves notes near a sixteenth to the groove's place for it.
pub fn apply(notes: &mut [NoteEvent], groove: &Template) {
    for n in notes {
        let grid = (n.start_beat * 4.0).round() / 4.0;
        let d = n.start_beat - grid;
        if d.abs() > NEAR {
            continue;
        }
        let slot = ((grid * 4.0).round() as i64).rem_euclid(4) as usize;
        let shift = groove[slot] - d;
        // Never before the song's start, and a note keeps its length.
        let shift = shift.max(-n.start_beat);
        n.start_beat += shift;
        n.end_beat += shift;
    }
}

/// Reading the audio groove track's recordings for their hits.
#[derive(Default)]
pub struct GrooveState {
    job: Option<(u64, HitsJob)>,
}

/// The hits found (beats), or why the recordings could not be read.
type HitsJob = Receiver<Result<Vec<f64>, String>>;

/// What the audio groove track's hits depend on: its recordings, where they are, the tempo.
fn audio_key(p: &Project, i: usize) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |w: u64| h = (h ^ w).wrapping_mul(0x0000_0100_0000_01b3);
    let map = p.tempo_map();
    for r in &p.tracks[i].regions {
        if let Some(a) = &r.audio {
            for b in a.file.path.to_string_lossy().bytes() {
                mix(u64::from(b));
            }
            mix(map.seconds_at(p.region_start_beats(r)).to_bits());
            mix(u64::from(r.params.mute));
        }
    }
    mix(u64::from(p.tempo.to_bits()));
    for c in &p.tempo_changes {
        mix(c.beat.to_bits());
        mix(u64::from(c.bpm.to_bits()));
    }
    h.max(1)
}

/// Keeps the audio groove track's hits up to date. Call once per frame.
pub fn poll(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    if let Some((key, rx)) = &s.groove.job {
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(TryRecvError::Disconnected) => s.groove.job = None,
            Ok(result) => {
                let key = *key;
                s.groove.job = None;
                match result {
                    Ok(hits) => {
                        p.groove_onsets = Arc::new(hits);
                        p.groove_key = key;
                        p.song_rev += 1;
                    }
                    Err(e) => {
                        // Not tried again until something changes.
                        p.groove_key = key;
                        s.status = Some(crate::i18n::trf("Groove Track: {}", &[&e]));
                    }
                }
            }
        }
    }
    let Some(i) = groove_track(p).filter(|i| p.tracks[*i].kind == TrackKind::Audio) else {
        return;
    };
    let key = audio_key(p, i);
    if key == p.groove_key || s.groove.job.is_some() {
        return;
    }
    let map = p.tempo_map();
    let files: Vec<(prowave_engine::AudioFile, f64)> = p.tracks[i]
        .regions
        .iter()
        .filter(|r| !r.params.mute)
        .filter_map(|r| {
            let a = r.audio.as_ref()?;
            Some((a.file.clone(), map.seconds_at(p.region_start_beats(r))))
        })
        .collect();
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("prowave-groove".into())
        .spawn(move || {
            let mut hits = Vec::new();
            for (file, at) in &files {
                match file.analyze() {
                    Ok(found) => hits.extend(found.onsets.iter().map(|o| map.beat_at(at + o))),
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
            let _ = tx.send(Ok(hits));
        });
    if spawned.is_ok() {
        s.groove.job = Some((key, rx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Note;

    fn note(start: f64) -> NoteEvent {
        NoteEvent {
            start_beat: start,
            end_beat: start + 0.2,
            pitch: 60,
            vel: 100,
            track: 0,
        }
    }

    #[test]
    fn the_groove_is_the_average_distance_from_each_sixteenth() {
        // Swung eighths: the off-beat eighth (slot 2) is late by 0.08, the downbeats on time.
        let hits: Vec<f64> = (0..8)
            .flat_map(|b| [b as f64, b as f64 + 0.5 + 0.08])
            .collect();
        let t = template_of(&hits).unwrap();
        assert!(t[0].abs() < 1e-9);
        assert!((t[2] - 0.08).abs() < 1e-9);
        assert_eq!((t[1], t[3]), (0.0, 0.0), "no hits there: the grid");
        assert!(template_of(&[]).is_none());
        // Hits far from any sixteenth do not count.
        assert!(template_of(&[0.125, 1.375]).is_none());
    }

    #[test]
    fn following_notes_take_the_groove() {
        let t = [0.0, 0.0, 0.08, -0.02];
        let mut notes = vec![note(1.0), note(1.5), note(1.51), note(1.75), note(1.125)];
        apply(&mut notes, &t);
        let starts: Vec<f64> = notes.iter().map(|n| n.start_beat).collect();
        assert!((starts[0] - 1.0).abs() < 1e-9);
        assert!((starts[1] - 1.58).abs() < 1e-9);
        assert!(
            (starts[2] - 1.58).abs() < 1e-9,
            "its own lateness is replaced"
        );
        assert!((starts[3] - 1.73).abs() < 1e-9);
        assert!(
            (starts[4] - 1.125).abs() < 1e-9,
            "a thirty-second between is left"
        );
        assert!(notes
            .iter()
            .all(|n| (n.end_beat - n.start_beat - 0.2).abs() < 1e-9));
        // A note at the very start cannot go before it.
        let mut first = vec![note(0.0)];
        apply(&mut first, &[-0.05, 0.0, 0.0, 0.0]);
        assert_eq!(first[0].start_beat, 0.0);
    }

    #[test]
    fn a_midi_groove_track_moves_the_tracks_that_follow_it() {
        let mut p = Project::demo();
        p.tracks.truncate(2);
        for t in &mut p.tracks {
            t.kind = TrackKind::Instrument;
            t.regions.truncate(1);
            t.regions[0].start_bar = 1.0;
            t.regions[0].params = Default::default();
        }
        // Track 0 swings its off-beats by a twelfth of a beat; track 1 plays straight.
        p.tracks[0].regions[0].notes = (0..8)
            .map(|k| Note {
                start: k as f32 * 0.5 + if k % 2 == 1 { 1.0 / 12.0 } else { 0.0 },
                len: 0.25,
                pitch: 40,
                vel: 100,
            })
            .collect();
        p.tracks[1].regions[0].notes = (0..8)
            .map(|k| Note {
                start: k as f32 * 0.5,
                len: 0.25,
                pitch: 60,
                vel: 100,
            })
            .collect();
        set_groove_track(&mut p, Some(0));
        assert_eq!(groove_track(&p), Some(0));
        p.tracks[1].params.follow_groove = true;
        let played = crate::engine_sync::song_notes_for_test(&p);
        let off: Vec<f64> = played
            .iter()
            .filter(|n| n.pitch == 60)
            .map(|n| n.start_beat - (n.start_beat * 2.0).round() / 2.0)
            .collect();
        assert!(off.iter().any(|d| (d - 1.0 / 12.0).abs() < 1e-3), "{off:?}");
        // Not following: straight again.
        p.tracks[1].params.follow_groove = false;
        let played = crate::engine_sync::song_notes_for_test(&p);
        assert!(played
            .iter()
            .filter(|n| n.pitch == 60)
            .all(|n| (n.start_beat * 2.0 - (n.start_beat * 2.0).round()).abs() < 1e-6));
        // Only one groove track at a time.
        set_groove_track(&mut p, Some(1));
        assert!(!p.tracks[0].params.groove && p.tracks[1].params.groove);
        assert!(!p.tracks[1].params.follow_groove);
    }
}
