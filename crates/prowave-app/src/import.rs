//! Import audio files as new audio tracks (Logic Pro: File > Import > Audio File, or drag-and-drop).
use crate::model::{AudioRegion, Project, Region, RegionParams, Track, TrackKind, TrackParams};
use prowave_engine::{AudioError, AudioFile};
use std::path::Path;
use std::sync::{Arc, OnceLock};

const PEAK_BINS: usize = 4096;

pub fn is_audio_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "wav" | "wave" | "aif" | "aiff" | "aifc" | "flac" | "mp3" | "caf"
        )
    })
}

/// Wraps an audio file as a region source and computes its waveform overview on a
/// background thread (the region shows a flat line until it is ready).
pub fn audio_region(file: AudioFile, ctx: &egui::Context) -> AudioRegion {
    let peaks = Arc::new(OnceLock::new());
    {
        let (file, peaks, ctx) = (file.clone(), peaks.clone(), ctx.clone());
        std::thread::spawn(move || match file.peaks(PEAK_BINS) {
            Ok(v) => {
                let _ = peaks.set(v);
                ctx.request_repaint();
            }
            Err(e) => tracing::warn!("waveform overview failed for {}: {e}", file.path.display()),
        });
    }
    AudioRegion {
        file,
        peaks,
        flex: None,
    }
}

/// Adds a new audio track with one region starting at the bar under the playhead.
pub fn import_audio(p: &mut Project, path: &Path, ctx: &egui::Context) -> Result<(), AudioError> {
    let file = AudioFile::open(path)?;
    let start_bar = p.beats_to_bar(p.position_beats).floor() + 1.0;
    // The region's length follows the Tempo track from where it starts.
    let map = p.tempo_map();
    let start = p.bar_to_beats(start_bar - 1.0);
    let end = map.advance(start, file.duration_secs());
    let len_bars = p.beats_to_bar(end) - (start_bar - 1.0);
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Audio")
        .to_string();

    p.tracks.push(Track {
        name: name.clone(),
        kind: TrackKind::Audio,
        color: egui::Color32::from_rgb(0x3f, 0x5f, 0xc0),
        mute: false,
        solo: false,
        rec: false,
        monitor: false,
        volume_db: 0.0,
        pan: 0.0,
        inserts: vec![],
        instrument: None,
        regions: vec![Region {
            name,
            start_bar: start_bar as f32,
            len_bars: len_bars as f32,
            notes: vec![],
            controls: Vec::new(),
            folder: None,
            generated: None,
            audio: Some(audio_region(file, ctx)),
            params: RegionParams::default(),
        }],
        params: TrackParams::default(),
        category: None,
        send: crate::model::SendSlot::default(),
        height: 1.0,
        indent: 0,
        folded: false,
        vca: None,
        midi_port: None,
        midi_channel: 1,
        midi_fx: Vec::new(),
    });
    p.selected_track = p.tracks.len() - 1;
    p.song_rev += 1;
    Ok(())
}
