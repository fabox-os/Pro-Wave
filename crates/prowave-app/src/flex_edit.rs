//! Flex Time and Flex Pitch on audio regions (Logic: the Flex mode of a track, Quantize of
//! audio, Flex Pitch's Pitch Correction and "Create MIDI Track from Flex Pitch Data").
//!
//! An edit is kept as data on the region ([`FlexEdit`]): the mode, the Flex markers (a moment
//! of the recording and where it now plays) and the pitch moves. The sound the region plays
//! is made from it in the background (`prowave_engine::warp` and `shift_pitch`) and written to
//! a file in the temporary folder; until it is ready the region plays what it played before.
//! The recording itself is never changed, and the edit is saved with the project and made
//! again when the project opens.
//!
//! The commands that need to know the recording (where the hits are, which notes are sung)
//! read it on a thread too and apply their result when it comes back, if the region is still
//! there.
use crate::i18n::{tr, trf};
use crate::model::{AudioRegion, Note, Project, Region, Track, TrackKind};
use crate::UiState;
use prowave_engine::{AudioFile, FlexMode, PitchShift, TempoMap, WarpPoint};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// The Flex edit of an audio region.
#[derive(Clone, Debug)]
pub struct FlexEdit {
    pub mode: FlexMode,
    /// Flex markers: `src` seconds into the recording plays at `dst` seconds from the start of
    /// the region.
    pub markers: Vec<WarpPoint>,
    /// Flex Pitch: stretches of the result (seconds from the region's start) moved in pitch.
    pub pitch: Vec<PitchShift>,
    /// The recording the edit starts from (the region's `file` is the result).
    pub original: AudioFile,
    /// The [`FlexEdit::fingerprint`] the region's `file` was made for (0: not made yet).
    pub rendered: u64,
}

impl FlexEdit {
    pub fn new(original: AudioFile, mode: FlexMode) -> Self {
        Self {
            mode,
            markers: Vec::new(),
            pitch: Vec::new(),
            original,
            rendered: 0,
        }
    }

    /// A number that changes with anything that changes the sound.
    pub fn fingerprint(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |w: u64| h = (h ^ w).wrapping_mul(0x0000_0100_0000_01b3);
        for b in self.original.path.to_string_lossy().bytes() {
            mix(u64::from(b));
        }
        for b in self.mode.name().bytes() {
            mix(u64::from(b));
        }
        for m in &self.markers {
            mix(m.src.to_bits());
            mix(m.dst.to_bits());
        }
        mix(0xfeed);
        for p in &self.pitch {
            mix(p.start.to_bits());
            mix(p.end.to_bits());
            mix(u64::from(p.semitones.to_bits()));
        }
        h.max(1)
    }

    /// Nothing is moved: the region plays the recording itself.
    pub fn is_neutral(&self) -> bool {
        self.markers.is_empty() && self.pitch.is_empty()
    }

    /// Where moment `src` of the recording plays, in seconds from the region's start.
    pub fn place_of(&self, src: f64) -> f64 {
        let mut prev = WarpPoint { src: 0.0, dst: 0.0 };
        for m in &self.markers {
            if src < m.src {
                let span = m.src - prev.src;
                return if span > 0.0 {
                    prev.dst + (src - prev.src) * (m.dst - prev.dst) / span
                } else {
                    m.dst
                };
            }
            prev = *m;
        }
        prev.dst + (src - prev.src)
    }
}

/// What a command does once the recording has been read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Flex markers on the hits, moved to the nearest step of `grid` beats.
    Quantize { grid: f64 },
    /// Flex markers on the beats, moved to the project's beats: the region follows the
    /// project's tempo (Smart Tempo: Adapt Region Tempo).
    FollowProjectTempo,
    /// Flex Pitch: every note moved to the nearest note of the project's key.
    CorrectPitch,
    /// A new software instrument track with the notes Flex Pitch hears.
    MidiFromPitch,
}

/// What the background work found.
enum Outcome {
    Markers {
        markers: Vec<WarpPoint>,
        /// The length of the result, in seconds.
        length: f64,
    },
    Pitch(Vec<PitchShift>),
    Notes(Vec<prowave_engine::DetectedNote>),
}

struct Task {
    track: usize,
    region: usize,
    /// The recording it was started for, to check it is still the same region.
    path: PathBuf,
    command: Command,
    result: Receiver<Result<Outcome, String>>,
}

struct Render {
    fingerprint: u64,
    result: Receiver<Result<AudioFile, String>>,
}

/// The Flex work running in the background.
#[derive(Default)]
pub struct FlexState {
    tasks: Vec<Task>,
    renders: Vec<Render>,
    /// Regions whose edits are to be removed (it needs the context, which the menus lack).
    removals: Vec<(usize, usize)>,
}

impl FlexState {
    /// Whether anything is still being worked on.
    pub fn busy(&self) -> bool {
        !self.tasks.is_empty() || !self.renders.is_empty() || !self.removals.is_empty()
    }

    /// Removes the Flex edits of region `r` of track `t` on the next [`poll`].
    pub fn remove_later(&mut self, t: usize, r: usize) {
        self.removals.push((t, r));
    }
}

fn audio_of(p: &Project, t: usize, r: usize) -> Option<&AudioRegion> {
    p.tracks.get(t)?.regions.get(r)?.audio.as_ref()
}

/// The recording under region `r`: the original of its Flex edit, or its file.
fn recording_of(a: &AudioRegion) -> AudioFile {
    a.flex
        .as_ref()
        .map_or_else(|| a.file.clone(), |f| f.original.clone())
}

/// The name of a Flex mode, translated.
pub fn mode_label(mode: FlexMode) -> &'static str {
    match mode {
        FlexMode::Slicing => tr("Slicing"),
        FlexMode::Rhythmic => tr("Rhythmic"),
        FlexMode::Monophonic => tr("Monophonic"),
        FlexMode::Polyphonic => tr("Polyphonic"),
        FlexMode::Speed => tr("Speed"),
        FlexMode::Tempophone => tr("Tempophone"),
    }
}

/// Sets the Flex mode of an audio region (turning Flex on for it).
pub fn set_mode(p: &mut Project, t: usize, r: usize, mode: FlexMode) {
    let Some(a) = p
        .tracks
        .get_mut(t)
        .and_then(|t| t.regions.get_mut(r))
        .and_then(|r| r.audio.as_mut())
    else {
        return;
    };
    match &mut a.flex {
        Some(f) => f.mode = mode,
        None => a.flex = Some(FlexEdit::new(a.file.clone(), mode)),
    }
}

/// Removes the Flex edits of a region: it plays its recording again.
pub fn remove(p: &mut Project, t: usize, r: usize, ctx: &egui::Context) {
    let Some(a) = p
        .tracks
        .get_mut(t)
        .and_then(|t| t.regions.get_mut(r))
        .and_then(|r| r.audio.as_mut())
    else {
        return;
    };
    if let Some(f) = a.flex.take() {
        *a = crate::import::audio_region(f.original, ctx);
        p.song_rev += 1;
    }
}

/// Starts a command on region `r` of track `t`.
pub fn start(p: &Project, s: &mut UiState, t: usize, r: usize, command: Command) {
    let Some(a) = audio_of(p, t, r) else {
        s.status = Some(tr("Flex: this region has no recorded audio").into());
        return;
    };
    if s.flex
        .tasks
        .iter()
        .any(|j| (j.track, j.region, j.command) == (t, r, command))
    {
        return;
    }
    let file = recording_of(a);
    let edit = a.flex.clone();
    let path = file.path.clone();
    let map = p.tempo_map();
    let origin = p.region_start_beats(&p.tracks[t].regions[r]);
    let fifths = crate::score::fifths(p.key);
    let (tx, result) = channel();
    let spawned = std::thread::Builder::new()
        .name("prowave-flex".into())
        .spawn(move || {
            let _ = tx.send(work(&file, edit.as_ref(), &map, origin, fifths, command));
        });
    match spawned {
        Ok(_) => {
            s.flex.tasks.push(Task {
                track: t,
                region: r,
                path,
                command,
                result,
            });
            s.status = Some(tr("Flex: analysing…").into());
        }
        Err(e) => s.status = Some(format!("cannot start the Flex analysis: {e}")),
    }
}

/// Flex markers that put the moments `src` of the recording at `dst`, kept in order (a
/// marker that would go back in time, or land on another, is left out).
pub fn markers_from(pairs: impl IntoIterator<Item = (f64, f64)>) -> Vec<WarpPoint> {
    let mut out: Vec<WarpPoint> = Vec::new();
    for (src, dst) in pairs {
        if !(src.is_finite() && dst.is_finite() && src > 0.0 && dst > 0.0) {
            continue;
        }
        if out
            .last()
            .is_some_and(|m| src <= m.src + 0.002 || dst <= m.dst + 0.002)
        {
            continue;
        }
        out.push(WarpPoint { src, dst });
    }
    out
}

/// The markers that make the beats `beats` (seconds into the recording) land on the project's
/// beats, from the beat nearest the first one on. Also the result's length.
pub fn follow_tempo(
    beats: &[f64],
    duration: f64,
    map: &TempoMap,
    origin: f64,
) -> (Vec<WarpPoint>, f64) {
    let Some(first) = beats.first() else {
        return (Vec::new(), duration);
    };
    let s0 = map.seconds_at(origin);
    let k0 = (map.beat_at(s0 + first) - origin).round().max(0.0);
    let markers = markers_from(
        beats
            .iter()
            .enumerate()
            .map(|(k, b)| (*b, map.seconds_at(origin + k0 + k as f64) - s0)),
    );
    let length = match markers.last() {
        Some(m) => m.dst + (duration - m.src).max(0.0),
        None => duration,
    };
    (markers, length)
}

/// Flex markers that put `beats` (seconds into the recording, as the Smart Tempo Editor has
/// them) on the project's beats, set on region `r` of track `t` at once.
pub fn follow_beats(p: &mut Project, t: usize, r: usize, beats: &[f64]) {
    let Some(a) = audio_of(p, t, r) else { return };
    let duration = recording_of(a).duration_secs();
    let origin = p.region_start_beats(&p.tracks[t].regions[r]);
    let map = p.tempo_map();
    // The editor's beats are of the file the region plays; with an edit already there they are
    // taken back to the recording.
    let edit = a.flex.clone();
    let to_src = |x: f64| match &edit {
        Some(e) if !e.markers.is_empty() => {
            let mut prev = WarpPoint { src: 0.0, dst: 0.0 };
            for m in &e.markers {
                if x < m.dst {
                    let span = m.dst - prev.dst;
                    return if span > 0.0 {
                        prev.src + (x - prev.dst) * (m.src - prev.src) / span
                    } else {
                        m.src
                    };
                }
                prev = *m;
            }
            prev.src + (x - prev.dst)
        }
        _ => x,
    };
    let src: Vec<f64> = beats.iter().map(|b| to_src(*b)).collect();
    let (markers, length) = follow_tempo(&src, duration, &map, origin);
    let mode = flex_mode_of(p, t, r);
    set_mode(p, t, r, mode);
    let end = map.beat_at(map.seconds_at(origin) + length);
    let bar_end = p.beats_to_bar(end);
    let region = &mut p.tracks[t].regions[r];
    if let Some(f) = region.audio.as_mut().and_then(|a| a.flex.as_mut()) {
        f.markers = markers;
    }
    region.len_bars = (bar_end as f32 - (region.start_bar - 1.0)).max(1.0 / 64.0);
    p.song_rev += 1;
}

/// The nearest MIDI note to `pitch` that is in the key with `fifths` sharps or flats.
pub fn nearest_in_key(pitch: f32, fifths: i8) -> f32 {
    const NATURAL: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
    let alt = crate::score::alterations(fifths);
    let pcs: Vec<i32> = (0..7)
        .map(|l| (NATURAL[l] + i32::from(alt[l])).rem_euclid(12))
        .collect();
    let base = pitch.round() as i32;
    (-6..=6)
        .map(|d| base + d)
        .filter(|m| pcs.contains(&m.rem_euclid(12)))
        .min_by(|a, b| {
            (*a as f32 - pitch)
                .abs()
                .total_cmp(&(*b as f32 - pitch).abs())
        })
        .unwrap_or(base) as f32
}

fn work(
    file: &AudioFile,
    edit: Option<&FlexEdit>,
    map: &TempoMap,
    origin: f64,
    fifths: i8,
    command: Command,
) -> Result<Outcome, String> {
    let found = file.analyze().map_err(|e| e.to_string())?;
    let place = |src: f64| edit.map_or(src, |e| e.place_of(src));
    match command {
        Command::Quantize { grid } => {
            let grid = if grid.is_finite() && grid > 0.0 {
                grid
            } else {
                0.25
            };
            let s0 = map.seconds_at(origin);
            let markers = markers_from(found.onsets.iter().map(|t| {
                let beat = map.beat_at(s0 + t) - origin;
                let q = (beat / grid).round() * grid;
                (*t, map.seconds_at(origin + q.max(0.0)) - s0)
            }));
            let length = match markers.last() {
                Some(m) => m.dst + (file.duration_secs() - m.src).max(0.0),
                None => file.duration_secs(),
            };
            if markers.is_empty() {
                return Err(tr("no hits found to quantize").into());
            }
            Ok(Outcome::Markers { markers, length })
        }
        Command::FollowProjectTempo => {
            if found.beats.len() < 2 {
                return Err(tr("no steady beat found").into());
            }
            let (markers, length) = follow_tempo(&found.beats, file.duration_secs(), map, origin);
            Ok(Outcome::Markers { markers, length })
        }
        Command::CorrectPitch | Command::MidiFromPitch => {
            let stereo = file.read_stereo().map_err(|e| e.to_string())?;
            let mono: Vec<f32> = stereo.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect();
            let notes = prowave_engine::detect_notes(&mono, file.sample_rate, &found.onsets);
            if notes.is_empty() {
                return Err(tr("no notes with a clear pitch found").into());
            }
            // Into the time of the result (after any Flex Time markers).
            let notes: Vec<_> = notes
                .into_iter()
                .map(|n| prowave_engine::DetectedNote {
                    start: place(n.start),
                    end: place(n.end),
                    ..n
                })
                .collect();
            if command == Command::MidiFromPitch {
                return Ok(Outcome::Notes(notes));
            }
            Ok(Outcome::Pitch(
                notes
                    .iter()
                    .filter_map(|n| {
                        let semitones = nearest_in_key(n.pitch, fifths) - n.pitch;
                        (semitones.abs() > 0.05).then_some(PitchShift {
                            start: n.start,
                            end: n.end,
                            semitones,
                        })
                    })
                    .collect(),
            ))
        }
    }
}

/// Applies the commands that finished and the sounds that are ready, and starts making the
/// sound of edits that changed. Call once per frame.
pub fn poll(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    for (t, r) in std::mem::take(&mut s.flex.removals) {
        remove(p, t, r, ctx);
    }
    poll_tasks(p, s);
    start_renders(p, s);
    poll_renders(ctx, p, s);
    if s.flex.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn poll_tasks(p: &mut Project, s: &mut UiState) {
    if s.flex.tasks.is_empty() {
        return;
    }
    let mut finished = Vec::new();
    s.flex.tasks.retain(|job| match job.result.try_recv() {
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => false,
        Ok(result) => {
            finished.push((job.track, job.region, job.path.clone(), job.command, result));
            false
        }
    });
    for (t, r, path, _command, result) in finished {
        let same = audio_of(p, t, r).is_some_and(|a| recording_of(a).path == path);
        if !same {
            continue;
        }
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                s.status = Some(trf("Flex: {}", &[&e]));
                continue;
            }
        };
        s.status = None;
        match outcome {
            Outcome::Markers { markers, length } => {
                let mode = flex_mode_of(p, t, r);
                set_mode(p, t, r, mode);
                let origin = p.region_start_beats(&p.tracks[t].regions[r]);
                let map = p.tempo_map();
                let end = map.beat_at(map.seconds_at(origin) + length);
                let bar_end = p.beats_to_bar(end);
                let region = &mut p.tracks[t].regions[r];
                if let Some(f) = region.audio.as_mut().and_then(|a| a.flex.as_mut()) {
                    f.markers = markers;
                }
                // The region covers the whole result.
                region.len_bars = (bar_end as f32 - (region.start_bar - 1.0)).max(1.0 / 64.0);
                s.status = Some(tr("Flex: markers set").into());
            }
            Outcome::Pitch(shifts) => {
                let mode = flex_mode_of(p, t, r);
                set_mode(p, t, r, mode);
                let n = shifts.len();
                if let Some(f) = p.tracks[t].regions[r]
                    .audio
                    .as_mut()
                    .and_then(|a| a.flex.as_mut())
                {
                    f.pitch = shifts;
                }
                s.status = Some(trf("Flex Pitch: {} notes corrected", &[&n]));
            }
            Outcome::Notes(notes) => {
                let made = midi_track_from(p, t, r, &notes);
                s.status = Some(trf("Flex Pitch: {} notes on a new track", &[&made]));
            }
        }
        p.song_rev += 1;
    }
}

/// The region's Flex mode, or the one that suits its track (Monophonic for most recordings).
fn flex_mode_of(p: &Project, t: usize, r: usize) -> FlexMode {
    audio_of(p, t, r)
        .and_then(|a| a.flex.as_ref())
        .map_or(FlexMode::Monophonic, |f| f.mode)
}

/// Adds a software instrument track under track `t` with a region at the place of region `r`
/// holding `notes`. Returns how many notes it has.
fn midi_track_from(
    p: &mut Project,
    t: usize,
    r: usize,
    notes: &[prowave_engine::DetectedNote],
) -> usize {
    let src = &p.tracks[t].regions[r];
    let origin = p.region_start_beats(src);
    let map = p.tempo_map();
    let s0 = map.seconds_at(origin);
    let beats = |sec: f64| (map.beat_at(s0 + sec) - origin) as f32;
    let midi: Vec<Note> = notes
        .iter()
        .map(|n| {
            let start = beats(n.start).max(0.0);
            Note {
                start,
                len: (beats(n.end) - start).max(1.0 / 32.0),
                pitch: n.pitch.round().clamp(0.0, 127.0) as u8,
                vel: (40.0 + 87.0 * n.level.clamp(0.0, 1.0)).round() as u8,
            }
        })
        .collect();
    let region = Region {
        name: format!("{} (Flex Pitch)", src.name),
        start_bar: src.start_bar,
        len_bars: src.len_bars,
        notes: midi,
        controls: Vec::new(),
        folder: None,
        audio: None,
        params: crate::model::RegionParams::default(),
        generated: None,
    };
    let mut track = Track::new(
        format!("{} (Flex Pitch)", p.tracks[t].name),
        TrackKind::Instrument,
    );
    let made = region.notes.len();
    track.regions.push(region);
    p.tracks.insert(t + 1, track);
    made
}

/// The folder the sounds of Flex edits are written to.
fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("prowave-flex")
}

fn start_renders(p: &mut Project, s: &mut UiState) {
    for track in &mut p.tracks {
        for region in &mut track.regions {
            let Some(a) = region.audio.as_mut() else {
                continue;
            };
            let Some(f) = a.flex.as_mut() else { continue };
            let fp = f.fingerprint();
            if f.rendered == fp || s.flex.renders.iter().any(|r| r.fingerprint == fp) {
                continue;
            }
            if f.is_neutral() {
                // Nothing moved: the recording itself plays.
                if a.file.path != f.original.path {
                    a.file = f.original.clone();
                    p.song_rev += 1;
                }
                if let Some(f) = a.flex.as_mut() {
                    f.rendered = fp;
                }
                continue;
            }
            let edit = f.clone();
            let (tx, result) = channel();
            let spawned = std::thread::Builder::new()
                .name("prowave-flex-render".into())
                .spawn(move || {
                    let _ = tx.send(render(&edit, fp));
                });
            if spawned.is_ok() {
                s.flex.renders.push(Render {
                    fingerprint: fp,
                    result,
                });
            }
        }
    }
}

/// Makes the sound of an edit (or finds it already made).
fn render(edit: &FlexEdit, fingerprint: u64) -> Result<AudioFile, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{fingerprint:016x}.wav"));
    if let Ok(f) = AudioFile::open(&path) {
        return Ok(f);
    }
    let sr = edit.original.sample_rate;
    let input = edit.original.read_stereo().map_err(|e| e.to_string())?;
    let timed = if edit.markers.is_empty() {
        input
    } else {
        prowave_engine::warp(&input, sr, &edit.markers, edit.mode)
    };
    let pitched = if edit.pitch.is_empty() {
        timed
    } else {
        // Pitch moves keep the time; Polyphonic material keeps its longer window.
        let mode = if edit.mode == FlexMode::Polyphonic {
            FlexMode::Polyphonic
        } else {
            FlexMode::Monophonic
        };
        prowave_engine::shift_pitch(&timed, sr, &edit.pitch, mode)
    };
    prowave_engine::write_wav(&path, &pitched, sr).map_err(|e| e.to_string())?;
    AudioFile::open(&path).map_err(|e| e.to_string())
}

fn poll_renders(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    let mut done = Vec::new();
    s.flex.renders.retain(|job| match job.result.try_recv() {
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => false,
        Ok(result) => {
            done.push((job.fingerprint, result));
            false
        }
    });
    for (fp, result) in done {
        let file = match result {
            Ok(f) => f,
            Err(e) => {
                s.status = Some(trf("Flex: cannot make the sound: {}", &[&e]));
                continue;
            }
        };
        for track in &mut p.tracks {
            for region in &mut track.regions {
                let Some(a) = region.audio.as_mut() else {
                    continue;
                };
                let Some(mut f) = a.flex.take().filter(|f| f.fingerprint() == fp) else {
                    continue;
                };
                f.rendered = fp;
                *a = crate::import::audio_region(file.clone(), ctx);
                a.flex = Some(f);
                p.song_rev += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo 16-bit WAV with a click at each of `times` (seconds).
    pub(crate) fn write_clicks(path: &std::path::Path, times: &[f64], seconds: f64) {
        const RATE: u32 = 44_100;
        let frames = (seconds * f64::from(RATE)) as usize;
        let mut v = vec![0i16; frames];
        for t in times {
            let a = (t * f64::from(RATE)) as usize;
            for k in 0..300 {
                if a + k < frames {
                    v[a + k] = if k % 2 == 0 { 20_000 } else { -20_000 };
                }
            }
        }
        let data_len = (frames * 4) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&RATE.to_le_bytes());
        b.extend_from_slice(&(RATE * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in v {
            b.extend_from_slice(&s.to_le_bytes());
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// A stereo 16-bit WAV of sine notes: (start, end, MIDI pitch).
    pub(crate) fn write_notes(path: &std::path::Path, notes: &[(f64, f64, f32)], seconds: f64) {
        const RATE: u32 = 44_100;
        let frames = (seconds * f64::from(RATE)) as usize;
        let mut v = vec![0i16; frames];
        for (a, b, m) in notes {
            let hz = 440.0 * 2f64.powf((f64::from(*m) - 69.0) / 12.0);
            let (from, to) = (
                (a * f64::from(RATE)) as usize,
                ((b * f64::from(RATE)) as usize).min(frames),
            );
            for (i, s) in v.iter_mut().enumerate().take(to).skip(from) {
                let t = i as f64 / f64::from(RATE);
                *s = ((std::f64::consts::TAU * hz * t).sin() * 12_000.0) as i16;
            }
        }
        let data_len = (frames * 4) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&RATE.to_le_bytes());
        b.extend_from_slice(&(RATE * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in v {
            b.extend_from_slice(&s.to_le_bytes());
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    fn project_with(path: &std::path::Path) -> (Project, usize) {
        let mut p = Project::empty();
        p.tempo = 120.0;
        crate::import::import_audio(&mut p, path, &egui::Context::default()).unwrap();
        // The region at bar 1, so seconds and beats are easy: half a second a beat.
        let t = p.tracks.len() - 1;
        p.tracks[t].regions[0].start_bar = 1.0;
        (p, t)
    }

    /// Polls until nothing is running.
    fn settle(p: &mut Project, s: &mut UiState) {
        let ctx = egui::Context::default();
        let began = std::time::Instant::now();
        loop {
            poll(&ctx, p, s);
            if !s.flex.busy() {
                break;
            }
            assert!(began.elapsed().as_secs() < 60, "Flex never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn markers_stay_in_order() {
        let m = markers_from([
            (1.0, 1.0),
            (0.5, 2.0),
            (2.0, 0.5),
            (3.0, 3.0),
            (f64::NAN, 4.0),
        ]);
        assert_eq!(
            m.iter().map(|m| (m.src, m.dst)).collect::<Vec<_>>(),
            [(1.0, 1.0), (3.0, 3.0)]
        );
        let edit = FlexEdit {
            markers: m,
            ..FlexEdit::new(
                AudioFile {
                    path: "x.wav".into(),
                    sample_rate: 44_100,
                    frames: 1,
                    channels: 2,
                },
                FlexMode::Rhythmic,
            )
        };
        assert_eq!(edit.place_of(0.5), 0.5);
        assert_eq!(edit.place_of(2.0), 2.0);
        assert_eq!(edit.place_of(4.0), 4.0);
        let mut other = edit.clone();
        other.mode = FlexMode::Slicing;
        assert_ne!(edit.fingerprint(), other.fingerprint());
    }

    #[test]
    fn the_key_decides_where_a_note_goes() {
        assert_eq!(nearest_in_key(60.3, 0), 60.0);
        assert_eq!(nearest_in_key(61.2, 0), 62.0, "C# is not in C major");
        assert_eq!(nearest_in_key(66.0, 1), 66.0, "F# is in G major");
        assert_eq!(nearest_in_key(65.8, 1), 66.0);
        assert_eq!(nearest_in_key(70.0, -1), 70.0, "Bb is in F major");
    }

    #[test]
    fn quantize_moves_the_hits_to_the_grid_and_the_region_plays_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("drums.wav");
        // At 120 BPM the beats are half a second apart: two hits are late, one early.
        write_clicks(&wav, &[0.5, 1.04, 1.47, 2.0, 2.53], 3.5);
        let (mut p, t) = project_with(&wav);
        let original = p.tracks[t].regions[0]
            .audio
            .as_ref()
            .unwrap()
            .file
            .path
            .clone();
        let mut s = UiState::new();
        set_mode(&mut p, t, 0, FlexMode::Slicing);
        start(&p, &mut s, t, 0, Command::Quantize { grid: 1.0 });
        settle(&mut p, &mut s);
        let a = p.tracks[t].regions[0].audio.as_ref().unwrap();
        let f = a.flex.as_ref().unwrap();
        assert_eq!(f.mode, FlexMode::Slicing);
        assert!(f.markers.len() >= 4, "{:?}", f.markers);
        for m in &f.markers {
            assert!(
                ((m.dst * 2.0) - (m.dst * 2.0).round()).abs() < 0.01,
                "{m:?}"
            );
        }
        // The region now plays a new file; the recording is untouched.
        assert_ne!(a.file.path, original);
        assert_eq!(f.original.path, original);
        assert_eq!(f.rendered, f.fingerprint());
        let found = a.file.analyze().unwrap();
        for o in &found.onsets {
            let off = (o * 2.0 - (o * 2.0).round()).abs() / 2.0;
            assert!(off < 0.012, "a hit at {o}");
        }
        // Removing the edit plays the recording again.
        remove(&mut p, t, 0, &egui::Context::default());
        let a = p.tracks[t].regions[0].audio.as_ref().unwrap();
        assert!(a.flex.is_none());
        assert_eq!(a.file.path, original);
    }

    #[test]
    fn pitch_is_corrected_to_the_key_and_made_into_midi() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("vocal.wav");
        // A slightly sharp E (64.3), then a C# (61) that is not in C major.
        write_notes(&wav, &[(0.0, 0.8, 64.3), (1.0, 1.8, 61.0)], 2.0);
        let (mut p, t) = project_with(&wav);
        p.key = "Cmaj";
        let mut s = UiState::new();
        start(&p, &mut s, t, 0, Command::CorrectPitch);
        settle(&mut p, &mut s);
        let f = p.tracks[t].regions[0]
            .audio
            .as_ref()
            .unwrap()
            .flex
            .clone()
            .unwrap();
        assert_eq!(f.pitch.len(), 2, "{:?}", f.pitch);
        assert!((f.pitch[0].semitones + 0.3).abs() < 0.1, "{:?}", f.pitch);
        assert!(
            (f.pitch[1].semitones.abs() - 1.0).abs() < 0.1,
            "C# goes to C or D: {:?}",
            f.pitch
        );
        // The made sound has the new pitches.
        let a = p.tracks[t].regions[0].audio.as_ref().unwrap();
        let stereo = a.file.read_stereo().unwrap();
        let mono: Vec<f32> = stereo.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect();
        let notes = prowave_engine::detect_notes(&mono, 44_100, &[1.0]);
        assert!((notes[0].pitch - 64.0).abs() < 0.15, "{notes:?}");

        // Create MIDI: a new track under it with the two notes.
        let tracks = p.tracks.len();
        start(&p, &mut s, t, 0, Command::MidiFromPitch);
        settle(&mut p, &mut s);
        assert_eq!(p.tracks.len(), tracks + 1);
        let midi = &p.tracks[t + 1];
        assert_eq!(midi.kind, TrackKind::Instrument);
        let n = &midi.regions[0].notes;
        assert_eq!(n.len(), 2, "{n:?}");
        assert_eq!(n[0].pitch, 64);
        assert!(
            (n[1].start - 2.0).abs() < 0.1,
            "second note on beat 3: {n:?}"
        );
    }

    #[test]
    fn a_region_can_follow_the_project_tempo() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("loop.wav");
        // A loop at 100 BPM in a project at 120.
        let times: Vec<f64> = (0..16).map(|k| 0.3 + k as f64 * 0.6).collect();
        write_clicks(&wav, &times, 10.0);
        let (mut p, t) = project_with(&wav);
        let mut s = UiState::new();
        start(&p, &mut s, t, 0, Command::FollowProjectTempo);
        settle(&mut p, &mut s);
        let a = p.tracks[t].regions[0].audio.as_ref().unwrap();
        let found = a.file.analyze().unwrap();
        let tempo = found.tempo.unwrap();
        assert!((tempo.bpm - 120.0).abs() < 1.5, "{}", tempo.bpm);
        // The region got shorter with the faster tempo.
        assert!(
            p.tracks[t].regions[0].len_bars < 4.6,
            "{}",
            p.tracks[t].regions[0].len_bars
        );
    }

    #[test]
    fn a_region_without_audio_and_a_gone_region_change_nothing() {
        let mut p = Project::demo();
        let mut s = UiState::new();
        start(&p, &mut s, 0, 0, Command::CorrectPitch);
        assert!(!s.flex.busy());
        assert_eq!(
            s.status.as_deref(),
            Some("Flex: this region has no recorded audio")
        );
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("x.wav");
        write_clicks(&wav, &[0.5, 1.0], 2.0);
        let (mut q, t) = project_with(&wav);
        start(&q, &mut s, t, 0, Command::Quantize { grid: 1.0 });
        q.tracks[t].regions.clear();
        settle(&mut q, &mut s);
        assert!(q.tracks[t].regions.is_empty());
        settle(&mut p, &mut s);
    }
}
