//! Smart Tempo (Logic: Smart Tempo and its editor): finding the tempo of recorded audio and
//! making the project follow it, with the tempo changes inside a recording, from one region or
//! from all the audio of the project together (multitrack). The Smart Tempo Editor shows the
//! beats found over the waveform and lets you fix what the analysis got wrong (twice or half
//! the tempo, which beat is the downbeat) before applying it. Files are read on their own
//! threads, so the window does not freeze on a long recording.
//!
//! Adapting the project's tempo keeps audio where it is in time: audio regions are moved to
//! the bars that now fall under them, as in Logic, so the recordings still play together.
use crate::i18n::{tr, trf};
use crate::model::{Project, TempoPoint, MAX_BPM, MIN_BPM};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Sense, Stroke};
use prowave_engine::{Analysis, AudioFile, STRENGTH_RATE};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// What to do with an analysis when it comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// Say the tempo.
    Tell,
    /// Make the project follow the region.
    Adapt,
    /// Show it in the Smart Tempo Editor.
    Editor,
}

/// An analysis of one region's file in progress.
pub struct Job {
    track: usize,
    region: usize,
    path: PathBuf,
    used: Use,
    result: Receiver<Result<Analysis, String>>,
}

/// The analysis of all audio regions together, in progress.
pub struct AllJob {
    result: Receiver<Result<Analysis, String>>,
}

/// The Smart Tempo Editor.
#[derive(Default)]
pub struct Editor {
    /// The region shown, and the file it was analysed from.
    target: Option<(usize, usize, PathBuf)>,
    /// The beats found, in seconds into the file.
    beats: Vec<f64>,
    confidence: f64,
    /// 1: the beats as found; 2: twice as many (the tempo was read as half); -2: every other.
    scale: i8,
    /// Which beat is the first downbeat (the beats before it are a pickup).
    downbeat: usize,
    /// Whether the analysis is still running.
    waiting: bool,
}

impl Editor {
    /// The beats as edited: doubled or halved, from the downbeat on.
    pub fn edited_beats(&self) -> Vec<f64> {
        let base: Vec<f64> = match self.scale {
            2 => {
                let mut v = Vec::with_capacity(self.beats.len() * 2);
                for w in self.beats.windows(2) {
                    v.push(w[0]);
                    v.push((w[0] + w[1]) / 2.0);
                }
                v.extend(self.beats.last());
                v
            }
            -2 => self.beats.iter().step_by(2).copied().collect(),
            _ => self.beats.clone(),
        };
        base.into_iter().skip(self.downbeat).collect()
    }

    /// The tempo of the edited beats (from their median spacing).
    pub fn tempo(&self) -> Option<f64> {
        let beats = self.edited_beats();
        let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(f64::total_cmp);
        gaps.get(gaps.len() / 2)
            .filter(|g| **g > 0.0)
            .map(|g| 60.0 / g)
    }
}

/// Starts reading the audio of region `region` of track `track` for its tempo. `adapt` also
/// makes the project follow it when it is found.
pub fn start(p: &Project, s: &mut UiState, track: usize, region: usize, adapt: bool) {
    let used = if adapt { Use::Adapt } else { Use::Tell };
    start_with(p, s, track, region, used);
}

/// Opens the Smart Tempo Editor on region `region` of track `track`.
pub fn open_editor(p: &Project, s: &mut UiState, track: usize, region: usize) {
    let Some(a) = audio_of(p, track, region) else {
        s.status = Some(tr("Smart Tempo: this region has no recorded audio").into());
        return;
    };
    s.tracks.tempo_editor = Editor {
        target: Some((track, region, a.file.path.clone())),
        scale: 1,
        waiting: true,
        ..Editor::default()
    };
    start_with(p, s, track, region, Use::Editor);
}

fn audio_of(p: &Project, t: usize, r: usize) -> Option<&crate::model::AudioRegion> {
    p.tracks.get(t)?.regions.get(r)?.audio.as_ref()
}

fn start_with(p: &Project, s: &mut UiState, track: usize, region: usize, used: Use) {
    let Some(audio) = audio_of(p, track, region) else {
        s.status = Some(tr("Smart Tempo: this region has no recorded audio").into());
        return;
    };
    let jobs = &mut s.tracks.tempo_jobs;
    if jobs
        .iter()
        .any(|j| (j.track, j.region, j.used) == (track, region, used))
    {
        return;
    }
    let file = audio.file.clone();
    let path = file.path.clone();
    let (tx, result) = channel();
    let spawned = std::thread::Builder::new()
        .name("prowave-smart-tempo".into())
        .spawn(move || {
            let _ = tx.send(file.analyze().map_err(|e| e.to_string()));
        });
    match spawned {
        Ok(_) => {
            jobs.push(Job {
                track,
                region,
                path,
                used,
                result,
            });
            s.status = Some(tr("Smart Tempo: analysing…").into());
        }
        Err(e) => s.status = Some(format!("cannot start the Smart Tempo analysis: {e}")),
    }
}

/// The longest stretch of the song analysed together, in seconds.
const MAX_SONG_SECONDS: f64 = 1800.0;

/// Starts the multitrack analysis: every audio region of the project, heard together.
pub fn start_all(p: &Project, s: &mut UiState) {
    if s.tracks.tempo_all.is_some() {
        return;
    }
    let map = p.tempo_map();
    let files: Vec<(AudioFile, f64)> = p
        .tracks
        .iter()
        .filter(|t| !t.mute)
        .flat_map(|t| t.regions.iter())
        .filter(|r| !r.params.mute)
        .filter_map(|r| {
            let a = r.audio.as_ref()?;
            Some((a.file.clone(), map.seconds_at(p.region_start_beats(r))))
        })
        .collect();
    if files.is_empty() {
        s.status = Some(tr("Smart Tempo: the project has no audio regions").into());
        return;
    }
    let (tx, result) = channel();
    let spawned = std::thread::Builder::new()
        .name("prowave-smart-tempo-all".into())
        .spawn(move || {
            let _ = tx.send(analyze_together(&files));
        });
    if spawned.is_ok() {
        s.tracks.tempo_all = Some(AllJob { result });
        s.status = Some(tr("Smart Tempo: analysing all audio…").into());
    }
}

/// The onset strengths of the files added at their places in the song (seconds) and analysed
/// as one: the beats are then in seconds from the start of the song.
fn analyze_together(files: &[(AudioFile, f64)]) -> Result<Analysis, String> {
    let mut sum: Vec<f32> = Vec::new();
    let limit = (MAX_SONG_SECONDS * STRENGTH_RATE) as usize;
    // Regions of the same file (a loop repeated) are read once.
    let mut read: std::collections::HashMap<&std::path::Path, Analysis> = Default::default();
    for (file, at) in files {
        if !read.contains_key(file.path.as_path()) {
            let found = file.analyze().map_err(|e| e.to_string())?;
            read.insert(file.path.as_path(), found);
        }
        let found = &read[file.path.as_path()];
        let offset = (at.max(0.0) * STRENGTH_RATE).round() as usize;
        if offset >= limit {
            continue;
        }
        let end = (offset + found.strength.len()).min(limit);
        if sum.len() < end {
            sum.resize(end, 0.0);
        }
        for (i, v) in found.strength.iter().take(end - offset).enumerate() {
            sum[offset + i] += v;
        }
    }
    Ok(Analysis::from_strength(sum))
}

/// How sure the analysis has to be before its tempo is offered.
const MIN_CONFIDENCE: f64 = 0.2;

/// Makes the project's tempo follow `beats` (seconds from the start of the song): the first
/// beat lands on the nearest bar line, each bar after it gets the tempo of its beats, and the
/// audio regions stay where they are in time. Returns the tempo of the first bar.
pub fn fit_tempo(p: &mut Project, beats: &[f64]) -> Option<f32> {
    let mut beats: Vec<f64> = beats
        .iter()
        .copied()
        .filter(|b| b.is_finite() && *b >= 0.0)
        .collect();
    if beats.len() < 2 {
        return None;
    }
    let old = p.tempo_map();
    // Where the audio is now, in seconds.
    let mut places: Vec<(usize, usize, f64, f64)> = Vec::new();
    for (t, track) in p.tracks.iter().enumerate() {
        for (r, region) in track.regions.iter().enumerate() {
            if region.audio.is_some() {
                places.push((
                    t,
                    r,
                    old.seconds_at(p.region_start_beats(region)),
                    old.seconds_at(p.region_end_beats(region)),
                ));
            }
        }
    }
    let t0 = beats[0];
    // The bar line nearest the first beat (not the song's start unless the beat is there).
    let mut bar = p.beats_to_bar(old.beat_at(t0)).round();
    if bar < 1.0 && t0 > 0.02 {
        bar = 1.0;
    }
    // A pickup from a whole beat before the bar line, as late as can be, at a tempo inside the
    // limits, brings the bar line to the first beat.
    let pickup = |kb: f64| -> Option<(f64, f32)> {
        if kb <= 0.0 {
            return Some((0.0, 0.0));
        }
        let mut kp = kb - 1.0;
        while kp >= 0.0 {
            let gap = t0 - old.seconds_at(kp);
            if gap > 0.0 {
                let bpm = (60.0 * (kb - kp) / gap) as f32;
                if bpm <= MAX_BPM {
                    return (bpm >= MIN_BPM).then_some((kp, bpm));
                }
            }
            kp -= 1.0;
        }
        None
    };
    let found = (0..4)
        .map(|k| p.bar_to_beats(bar + f64::from(k)))
        .find_map(|kb| pickup(kb).map(|x| (kb, x)));
    // When no pickup fits (the first beat is too near the song's start), all the audio moves
    // later together, by less than a bar, so that the first beat is on the next bar line.
    let mut shift = 0.0;
    let (kb, (kp, pickup_bpm)) = match found {
        Some(x) => x,
        None => {
            let kb = p.bar_to_beats(p.beats_to_bar(old.beat_at(t0)).ceil().max(1.0));
            shift = old.seconds_at(kb) - t0;
            for b in &mut beats {
                *b += shift;
            }
            (kb, (kb, 0.0))
        }
    };
    let has_pickup = found.is_some() && kb > 0.0;
    let num = usize::from(
        p.signature_of_bar(p.beats_to_bar(kb).round() as u32)
            .0
            .max(1),
    );
    let round = |bpm: f32| ((bpm * 100.0).round() / 100.0).clamp(MIN_BPM, MAX_BPM);
    // The tempo of each bar from its beats.
    let mut bars: Vec<(f64, f32)> = Vec::new();
    let mut i = 0;
    while i + 1 < beats.len() {
        let end = (i + num).min(beats.len() - 1);
        let span = beats[end] - beats[i];
        if span > 0.0 {
            bars.push((
                kb + i as f64,
                round((60.0 * (end - i) as f64 / span) as f32),
            ));
        }
        i += num;
    }
    // A steady recording gets one tempo.
    let mut sorted: Vec<f32> = bars.iter().map(|b| b.1).collect();
    sorted.sort_by(f32::total_cmp);
    let median = *sorted.get(sorted.len() / 2)?;
    if bars.iter().all(|b| (b.1 - median).abs() <= 0.5) {
        bars = vec![(kb, median)];
    }
    // The new tempo: what was before the pickup, the pickup, then the bars.
    let mut changes: Vec<TempoPoint> = p
        .tempo_changes
        .iter()
        .copied()
        .filter(|c| c.beat < kp - 1e-6)
        .collect();
    let mut points = Vec::new();
    if has_pickup {
        points.push((kp, round(pickup_bpm)));
    }
    points.extend(bars.iter().copied());
    for (beat, bpm) in points {
        if beat <= 1e-6 {
            p.tempo = bpm;
            p.tempo_ramp = false;
        } else if changes
            .last()
            .map_or((p.tempo - bpm).abs() > 0.05, |c| (c.bpm - bpm).abs() > 0.05)
        {
            changes.push(TempoPoint {
                beat,
                bpm,
                ramp: false,
            });
        }
    }
    p.tempo_changes = changes;
    // The audio back where it was in time.
    let new = p.tempo_map();
    for (t, r, start, end) in places {
        let (start, end) = (start + shift, end + shift);
        let start_bar = p.beats_to_bar(new.beat_at(start)) as f32 + 1.0;
        let end_bar = p.beats_to_bar(new.beat_at(end)) as f32 + 1.0;
        let region = &mut p.tracks[t].regions[r];
        region.start_bar = start_bar;
        region.len_bars = (end_bar - start_bar).max(1.0 / 64.0);
    }
    p.song_rev += 1;
    Some(bars[0].1)
}

/// Applies the analyses that have finished. Call once per frame.
pub fn poll(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    poll_all(p, s);
    if s.tracks.tempo_jobs.is_empty() {
        return;
    }
    let mut finished = Vec::new();
    s.tracks
        .tempo_jobs
        .retain(|job| match job.result.try_recv() {
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
            Ok(result) => {
                finished.push((job.track, job.region, job.path.clone(), job.used, result));
                false
            }
        });
    let mut message = None;
    for (track, region, path, used, result) in finished {
        // The project may have changed while the file was being read.
        let Some(r) = p
            .tracks
            .get(track)
            .and_then(|t| t.regions.get(region))
            .filter(|r| r.audio.as_ref().is_some_and(|a| a.file.path == path))
        else {
            continue;
        };
        let s0 = p.tempo_map().seconds_at(p.region_start_beats(r));
        if used == Use::Editor {
            let e = &mut s.tracks.tempo_editor;
            if e.target.as_ref() == Some(&(track, region, path.clone())) {
                e.waiting = false;
                if let Ok(found) = &result {
                    e.beats = found.beats.clone();
                    e.confidence = found.tempo.map_or(0.0, |t| t.confidence);
                }
            }
            message = match result {
                Err(e) => Some(trf(
                    "Smart Tempo: cannot read {}: {}",
                    &[&path.display(), &e],
                )),
                Ok(_) => None,
            };
            continue;
        }
        message = Some(match result {
            Err(e) => trf("Smart Tempo: cannot read {}: {}", &[&path.display(), &e]),
            Ok(found) => match found.tempo.filter(|t| t.confidence >= MIN_CONFIDENCE) {
                None => tr("Smart Tempo: no steady tempo found").to_string(),
                Some(t) if used == Use::Tell => {
                    let bpm = ((t.bpm * 100.0).round() / 100.0) as f32;
                    trf("Smart Tempo: this region is at {} BPM", &[&bpm])
                }
                Some(_) => {
                    let beats: Vec<f64> = found.beats.iter().map(|b| s0 + b).collect();
                    match fit_tempo(p, &beats) {
                        Some(bpm) => trf("Smart Tempo: the project tempo is now {} BPM", &[&bpm]),
                        None => tr("Smart Tempo: no steady tempo found").to_string(),
                    }
                }
            },
        });
    }
    if s.tracks.tempo_jobs.is_empty() {
        s.status = message;
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn poll_all(p: &mut Project, s: &mut UiState) {
    let Some(job) = &s.tracks.tempo_all else {
        return;
    };
    let result = match job.result.try_recv() {
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
            s.tracks.tempo_all = None;
            return;
        }
        Ok(r) => r,
    };
    s.tracks.tempo_all = None;
    s.status = Some(match result {
        Err(e) => trf("Smart Tempo: {}", &[&e]),
        Ok(found) => match found.tempo.filter(|t| t.confidence >= MIN_CONFIDENCE) {
            None => tr("Smart Tempo: no steady tempo found").to_string(),
            Some(_) => match fit_tempo(p, &found.beats) {
                Some(bpm) => trf("Smart Tempo: the project tempo is now {} BPM", &[&bpm]),
                None => tr("Smart Tempo: no steady tempo found").to_string(),
            },
        },
    });
}

/// The Smart Tempo Editor window, when it is open.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    let Some((t, r, path)) = s.tracks.tempo_editor.target.clone() else {
        return;
    };
    // The region went away: the editor closes. It plays another file now (a Flex edit was
    // made): the editor reads that one.
    let Some(a) = audio_of(p, t, r) else {
        s.tracks.tempo_editor = Editor::default();
        return;
    };
    if a.file.path != path {
        open_editor(p, s, t, r);
        return;
    }
    let secs = a.file.duration_secs().max(1e-3);
    let peaks = a.peaks.get().cloned();
    let name = p.tracks[t].regions[r].name.clone();
    let num = usize::from(p.sig.0.max(1));
    let mut open = true;
    let mut action = None;
    let e = &mut s.tracks.tempo_editor;
    egui::Window::new(tr("Smart Tempo Editor"))
        .id(egui::Id::unique("smart_tempo_editor"))
        .open(&mut open)
        .default_size(vec2(680.0, 300.0))
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong(&name);
                ui.separator();
                if e.waiting {
                    ui.spinner();
                    ui.label(tr("Analysing…"));
                } else if let Some(bpm) = e.tempo() {
                    ui.label(trf("Tempo: {} BPM", &[&format!("{bpm:.2}")]));
                    ui.label(trf(
                        "(sure: {}%)",
                        &[&((e.confidence * 100.0).round() as i64)],
                    ));
                } else {
                    ui.label(tr("No beats found"));
                }
            });
            let ready = !e.waiting && e.beats.len() >= 2;
            ui.horizontal(|ui| {
                ui.add_enabled_ui(ready, |ui| {
                    if ui
                        .button("×2")
                        .on_hover_text(tr("Twice the tempo"))
                        .clicked()
                    {
                        e.scale = if e.scale == -2 { 1 } else { 2 };
                        e.downbeat = 0;
                    }
                    if ui
                        .button("÷2")
                        .on_hover_text(tr("Half the tempo"))
                        .clicked()
                    {
                        e.scale = if e.scale == 2 { 1 } else { -2 };
                        e.downbeat = 0;
                    }
                    ui.separator();
                    ui.label(tr("Downbeat:"));
                    if ui.button("◀").clicked() {
                        e.downbeat = e.downbeat.saturating_sub(1);
                    }
                    if ui.button("▶").clicked() && e.edited_beats().len() > 2 {
                        e.downbeat += 1;
                    }
                });
            });
            // The waveform with the beats over it; every bar's first beat is lit.
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::hover());
            let g = ui.painter_at(rect);
            g.rect_filled(rect, 4.0, Color32::from_rgb(0x22, 0x22, 0x26));
            if let Some(peaks) = &peaks {
                let mid = rect.center().y;
                let h = rect.height() * 0.45;
                let w = rect.width().max(1.0) as usize;
                for x in 0..w {
                    if let Some((lo, hi)) = peaks.get(x * peaks.len() / w) {
                        let xx = rect.left() + x as f32;
                        g.line_segment(
                            [pos2(xx, mid - hi * h), pos2(xx, mid - lo * h)],
                            Stroke::new(1.0, Color32::from_rgb(0x6a, 0x9c, 0xe8)),
                        );
                    }
                }
            }
            for (k, b) in e.edited_beats().iter().enumerate() {
                let x = rect.left() + (*b / secs) as f32 * rect.width();
                let bar = k % num == 0;
                let colour = if bar {
                    Color32::from_rgb(0xff, 0xd0, 0x40)
                } else {
                    Color32::from_white_alpha(70)
                };
                g.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.bottom())],
                    Stroke::new(if bar { 1.5 } else { 1.0 }, colour),
                );
                if bar {
                    g.text(
                        pos2(x + 3.0, rect.top() + 2.0),
                        Align2::LEFT_TOP,
                        format!("{}", k / num + 1),
                        FontId::proportional(10.0),
                        colour,
                    );
                }
            }
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let ready = ready && e.edited_beats().len() >= 2;
                ui.add_enabled_ui(ready, |ui| {
                    if ui
                        .button(tr("Adapt Project Tempo"))
                        .on_hover_text(tr("The project follows the beats of this region"))
                        .clicked()
                    {
                        action = Some(1);
                    }
                    if ui
                        .button(tr("Region Follows Project Tempo"))
                        .on_hover_text(tr(
                            "Flex Time moves the beats of this region onto the project's",
                        ))
                        .clicked()
                    {
                        action = Some(2);
                    }
                });
                if ui
                    .button(tr("Analyse All Audio Regions"))
                    .on_hover_text(tr("Multitrack: the tempo of all the audio heard together"))
                    .clicked()
                {
                    action = Some(3);
                }
            });
        });
    let beats = s.tracks.tempo_editor.edited_beats();
    match action {
        Some(1) => {
            let s0 = p
                .tempo_map()
                .seconds_at(p.region_start_beats(&p.tracks[t].regions[r]));
            let abs: Vec<f64> = beats.iter().map(|b| s0 + b).collect();
            s.status = Some(match fit_tempo(p, &abs) {
                Some(bpm) => trf("Smart Tempo: the project tempo is now {} BPM", &[&bpm]),
                None => tr("Smart Tempo: no steady tempo found").to_string(),
            });
            // The region moved in bars; it is the same region.
            if let Some(a) = audio_of(p, t, r) {
                s.tracks.tempo_editor.target = Some((t, r, a.file.path.clone()));
            }
        }
        Some(2) => {
            crate::flex_edit::follow_beats(p, t, r, &beats);
            s.status = Some(tr("Flex: markers set").into());
            s.tracks.tempo_editor = Editor::default();
        }
        Some(3) => start_all(p, s),
        _ => {}
    }
    if !open {
        s.tracks.tempo_editor = Editor::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo 16-bit WAV with a noise burst on every beat of `bpm`.
    fn write_clicks(path: &std::path::Path, bpm: f64, seconds: f64) {
        const RATE: u32 = 44_100;
        let frames = (seconds * f64::from(RATE)) as usize;
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
        let beat = (60.0 / bpm * f64::from(RATE)) as usize;
        let mut seed = 1u32;
        for f in 0..frames {
            let into = (f + beat - 9000) % beat;
            let v = if into < 500 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (seed >> 16) as i32 - 32_768;
                (n as f32 * 0.5 * (-(into as f32) / 120.0).exp()) as i16
            } else {
                0
            };
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    fn wait(p: &mut Project, s: &mut UiState) {
        let ctx = egui::Context::default();
        let began = std::time::Instant::now();
        while !s.tracks.tempo_jobs.is_empty() {
            assert!(
                began.elapsed().as_secs() < 20,
                "the analysis never finished"
            );
            poll(&ctx, p, s);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn project_with(bpm: f64) -> (Project, tempfile::TempDir, usize) {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("loop.wav");
        write_clicks(&wav, bpm, 12.0);
        let mut p = Project::empty();
        crate::import::import_audio(&mut p, &wav, &egui::Context::default()).unwrap();
        let t = p.tracks.len() - 1;
        (p, dir, t)
    }

    #[test]
    fn detect_says_the_tempo_and_leaves_the_project_alone() {
        let (mut p, _dir, t) = project_with(132.0);
        let before = p.tempo;
        let mut s = UiState::new();
        start(&p, &mut s, t, 0, false);
        assert_eq!(s.tracks.tempo_jobs.len(), 1);
        assert_eq!(s.status.as_deref(), Some("Smart Tempo: analysing…"));
        start(&p, &mut s, t, 0, false);
        assert_eq!(s.tracks.tempo_jobs.len(), 1, "one analysis per region");
        wait(&mut p, &mut s);
        assert_eq!(p.tempo, before);
        let said = s.status.clone().unwrap();
        assert!(
            said.starts_with("Smart Tempo: this region is at 13") && said.ends_with(" BPM"),
            "{said}"
        );
    }

    #[test]
    fn adapt_sets_the_project_tempo() {
        let (mut p, _dir, t) = project_with(96.0);
        let mut s = UiState::new();
        start(&p, &mut s, t, 0, true);
        wait(&mut p, &mut s);
        // The pickup aside, the bars play at the loop's tempo.
        let at = p.tempo_map().tempo_at(8.0);
        assert!((at - 96.0).abs() < 1.0, "{at}");
        assert!(s
            .status
            .as_deref()
            .is_some_and(|m| m.starts_with("Smart Tempo: the project tempo is now ")));
    }

    #[test]
    fn silence_regions_without_audio_and_gone_regions_are_handled() {
        // No tempo in silence.
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("quiet.wav");
        write_clicks(&wav, 120.0, 6.0);
        let mut bytes = std::fs::read(&wav).unwrap();
        bytes[44..].fill(0);
        std::fs::write(&wav, bytes).unwrap();
        let mut p = Project::empty();
        crate::import::import_audio(&mut p, &wav, &egui::Context::default()).unwrap();
        let (t, mut s) = (p.tracks.len() - 1, UiState::new());
        let before = p.tempo;
        start(&p, &mut s, t, 0, true);
        wait(&mut p, &mut s);
        assert_eq!(p.tempo, before);
        assert_eq!(
            s.status.as_deref(),
            Some("Smart Tempo: no steady tempo found")
        );
        // A MIDI region has nothing to analyse.
        let mut s = UiState::new();
        start(&Project::demo(), &mut s, 0, 0, true);
        assert!(s.tracks.tempo_jobs.is_empty());
        assert_eq!(
            s.status.as_deref(),
            Some("Smart Tempo: this region has no recorded audio")
        );
        // The region is deleted while the file is read: nothing changes.
        let (mut p, _dir, t) = project_with(110.0);
        let (mut s, before) = (UiState::new(), p.tempo);
        start(&p, &mut s, t, 0, true);
        p.tracks[t].regions.clear();
        wait(&mut p, &mut s);
        assert_eq!(p.tempo, before);
    }

    #[test]
    fn fitting_follows_a_tempo_change_and_keeps_the_audio_in_time() {
        let (mut p, _dir, t) = project_with(120.0);
        let map = p.tempo_map();
        let start_secs = map.seconds_at(p.region_start_beats(&p.tracks[t].regions[0]));
        // Eight beats at 100 BPM, then eight at 140, starting 0.3 s into the song.
        let mut beats = vec![0.3];
        for k in 1..16 {
            let gap = if k <= 8 { 0.6 } else { 60.0 / 140.0 };
            beats.push(beats[k - 1] + gap);
        }
        let first = fit_tempo(&mut p, &beats).expect("a tempo");
        assert!((first - 100.0).abs() < 0.5, "{first}");
        let map = p.tempo_map();
        // Every beat is on a whole beat of the project now.
        let k0 = map.beat_at(beats[0]);
        for (k, b) in beats.iter().enumerate() {
            let at = map.beat_at(*b);
            assert!(
                (at - (k0 + k as f64)).abs() < 0.02,
                "beat {k}: {at} vs {}",
                k0 + k as f64
            );
        }
        assert!(
            (k0 - k0.round()).abs() < 0.02
                && (p.beats_to_bar(k0) - p.beats_to_bar(k0).round()).abs() < 0.02
        );
        // The second half plays at 140.
        assert!((map.tempo_at(k0 + 12.0) - 140.0).abs() < 0.5);
        // The audio region starts at the same second as before.
        let now = map.seconds_at(p.region_start_beats(&p.tracks[t].regions[0]));
        assert!((now - start_secs).abs() < 0.002, "{now} vs {start_secs}");
        assert!(fit_tempo(&mut p, &[1.0]).is_none());
        assert!(fit_tempo(&mut p, &[f64::NAN, 2.0]).is_none());
    }

    #[test]
    fn all_audio_regions_are_analysed_together() {
        let (mut p, _dir, _) = project_with(120.0);
        let (q, _dir2, t2) = project_with(120.0);
        // A second copy of the loop on its own track, half a beat later: the beats of both
        // together are still at 120 (and the count of beats grows, not the tempo).
        let mut track = q.tracks[t2].clone();
        track.regions[0].start_bar += 0.125;
        p.tracks.push(track);
        let mut s = UiState::new();
        start_all(&p, &mut s);
        assert!(s.tracks.tempo_all.is_some());
        let began = std::time::Instant::now();
        while s.tracks.tempo_all.is_some() {
            assert!(began.elapsed().as_secs() < 30);
            poll(&egui::Context::default(), &mut p, &mut s);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let said = s.status.clone().unwrap_or_default();
        assert!(
            said.starts_with("Smart Tempo: the project tempo is now"),
            "{said}"
        );
        // No audio at all: said so.
        let mut s = UiState::new();
        start_all(&Project::empty(), &mut s);
        assert!(s.tracks.tempo_all.is_none());
        assert_eq!(
            s.status.as_deref(),
            Some("Smart Tempo: the project has no audio regions")
        );
    }

    #[test]
    fn the_editor_doubles_halves_and_moves_the_downbeat() {
        let mut e = Editor {
            beats: vec![0.0, 0.5, 1.0, 1.5, 2.0],
            scale: 1,
            ..Editor::default()
        };
        assert_eq!(e.tempo().map(|t| t.round()), Some(120.0));
        e.scale = 2;
        assert_eq!(e.edited_beats().len(), 9);
        assert_eq!(e.tempo().map(|t| t.round()), Some(240.0));
        e.scale = -2;
        assert_eq!(e.edited_beats(), [0.0, 1.0, 2.0]);
        e.scale = 1;
        e.downbeat = 2;
        assert_eq!(e.edited_beats(), [1.0, 1.5, 2.0]);
        assert_eq!(Editor::default().tempo(), None);
    }

    #[test]
    fn the_editor_opens_on_a_region_and_draws() {
        let (mut p, _dir, t) = project_with(120.0);
        let mut s = UiState::new();
        open_editor(&p, &mut s, t, 0);
        assert!(s.tracks.tempo_editor.waiting);
        wait(&mut p, &mut s);
        let e = &s.tracks.tempo_editor;
        assert!(!e.waiting && e.beats.len() > 10, "{}", e.beats.len());
        let mut h = crate::ui_test::Harness::new(1000.0, 700.0);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        }
        assert!(s.tracks.tempo_editor.target.is_some(), "still open");
        // The region goes away: the editor closes.
        p.tracks[t].regions.clear();
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        assert!(s.tracks.tempo_editor.target.is_none());
    }
}
