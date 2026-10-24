//! Smart Tempo (the first part of it): finding the tempo of a recorded audio region and, if you
//! ask, making it the project's tempo (Logic: Adapt Project Tempo). The file is read on its own
//! thread, as in Normalize Region Gain, so the window does not freeze on a long recording.
use crate::i18n::{tr, trf};
use crate::model::Project;
use crate::UiState;
use prowave_engine::Analysis;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// An analysis of one region's file in progress.
pub struct Job {
    track: usize,
    region: usize,
    path: PathBuf,
    /// Set the project's tempo to the result, not just say it.
    adapt: bool,
    result: Receiver<Result<Analysis, String>>,
}

/// Starts reading the audio of region `region` of track `track` for its tempo. `adapt` also
/// sets the project's tempo (at the region's start) when it is found.
pub fn start(p: &Project, s: &mut UiState, track: usize, region: usize, adapt: bool) {
    let Some(audio) = p
        .tracks
        .get(track)
        .and_then(|t| t.regions.get(region))
        .and_then(|r| r.audio.as_ref())
    else {
        s.status = Some(tr("Smart Tempo: this region has no recorded audio").into());
        return;
    };
    let jobs = &mut s.tracks.tempo_jobs;
    if jobs.iter().any(|j| (j.track, j.region) == (track, region)) {
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
                adapt,
                result,
            });
            s.status = Some(tr("Smart Tempo: analysing…").into());
        }
        Err(e) => s.status = Some(format!("cannot start the Smart Tempo analysis: {e}")),
    }
}

/// How sure the analysis has to be before its tempo is offered.
const MIN_CONFIDENCE: f64 = 0.2;

/// Applies the analyses that have finished. Call once per frame.
pub fn poll(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
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
                finished.push((job.track, job.region, job.path.clone(), job.adapt, result));
                false
            }
        });
    let mut message = None;
    for (track, region, path, adapt, result) in finished {
        // The project may have changed while the file was being read.
        let Some(r) = p
            .tracks
            .get(track)
            .and_then(|t| t.regions.get(region))
            .filter(|r| r.audio.as_ref().is_some_and(|a| a.file.path == path))
        else {
            continue;
        };
        let start = p.region_start_beats(r);
        message = Some(match result {
            Err(e) => trf("Smart Tempo: cannot read {}: {}", &[&path.display(), &e]),
            Ok(found) => match found.tempo.filter(|t| t.confidence >= MIN_CONFIDENCE) {
                None => tr("Smart Tempo: no steady tempo found").to_string(),
                Some(t) => {
                    let bpm = ((t.bpm * 100.0).round() / 100.0) as f32;
                    if adapt {
                        p.set_tempo_at(start, bpm);
                        trf("Smart Tempo: the project tempo is now {} BPM", &[&bpm])
                    } else {
                        trf("Smart Tempo: this region is at {} BPM", &[&bpm])
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
        assert!((p.tempo - 96.0).abs() < 1.0, "{}", p.tempo);
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
}
