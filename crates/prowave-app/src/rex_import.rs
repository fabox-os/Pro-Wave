//! REX2 loop import: File > Import > REX Loop…
//!
//! REX slices are ranges of one waveform, so the file is decoded to a temporary float WAV and
//! each slice becomes an audio region referencing a different part of it. The WAV lives in the
//! system temporary directory for the life of the project, like a decoded sample in Logic's
//! audio bin. All decoding and file I/O happen here, never on the audio thread.

use crate::model::{Project, Region, RegionParams};
use std::path::Path;

/// Whether a dropped or command-line file is a REX loop.
pub fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rx2") || e.eq_ignore_ascii_case("rex"))
}

/// The highest bar a slice can be placed at.
const MAX_BAR: f64 = 9999.0;
/// REX stores its slice grid in ticks per 4/4 bar; 15360 ticks is one bar.
const REX_PPQ: u64 = 15_360;

/// Reads a `.rx2`/`.rex` loop and adds one audio track with a region per slice. Returns a status
/// line describing what was imported.
pub fn import(p: &mut Project, path: &Path, ctx: &egui::Context) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let rex = prowave_rex::RexFile::parse(&bytes)?;
    if rex.frame_count() == 0 {
        return Err("the REX loop has no audio".into());
    }
    // A slice table whose slices are all muted or empty would otherwise import nothing audible.
    // Refuse it before the project changes.
    if !rex.slices.is_empty() && (0..rex.slices.len()).all(|i| playable_range(&rex, i).is_none()) {
        return Err("every slice in the REX loop is muted or empty".into());
    }
    let bpm = rex
        .tempo
        .or(rex.original_tempo)
        .unwrap_or(f64::from(p.tempo));
    if p.tracks.iter().all(|t| t.regions.is_empty()) && bpm.is_finite() && bpm > 0.0 {
        p.tempo = bpm.clamp(
            f64::from(crate::model::MIN_BPM),
            f64::from(crate::model::MAX_BPM),
        ) as f32;
        p.tempo_changes.clear();
    }

    let waveform = stereo_samples(&rex);
    let wav = temporary_wav(path);
    prowave_engine::write_wav(&wav, &waveform, rex.sample_rate)
        .map_err(|e| format!("cannot decode the REX loop: {e}"))?;
    let file = prowave_engine::AudioFile::open(&wav)
        .map_err(|e| format!("cannot read the decoded REX loop: {e}"))?;
    let bps = f64::from(p.tempo) / 60.0;

    let ti = p.add_track(crate::model::TrackKind::Audio);
    p.tracks[ti].name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("REX Loop")
        .to_string();
    let mut made = 0;
    for i in 0..rex.slices.len() {
        let Some((start, end)) = playable_range(&rex, i) else {
            continue;
        };
        let start_beat = rex
            .slice_beat(i, REX_PPQ)
            .unwrap_or_else(|| (start as f64 / rex.sample_rate.max(1) as f64) * bps);
        let end_beat = rex
            .slice_beat(i + 1, REX_PPQ)
            .filter(|e| *e > start_beat)
            .unwrap_or_else(|| {
                start_beat + (end - start) as f64 / rex.sample_rate.max(1) as f64 * bps
            });
        let start_bar0 = p
            .beats_to_bar(start_beat + 1e-9)
            .floor()
            .clamp(0.0, MAX_BAR - 1.0);
        let len_bars = (p.beats_to_bar(end_beat - 1e-9) - start_bar0)
            .ceil()
            .clamp(1.0, MAX_BAR - start_bar0);
        let mut audio = crate::import::audio_region(file.clone(), ctx);
        audio.start_frame = start as u64;
        audio.len_frames = Some((end - start) as u64);
        p.tracks[ti].regions.push(Region {
            name: format!("Slice {}", i + 1),
            start_bar: start_bar0 as f32 + 1.0,
            len_bars: len_bars as f32,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            audio: Some(audio),
            params: RegionParams::default(),
            generated: None,
        });
        made += 1;
    }
    if made == 0 {
        // A loop with no slice table at all still plays as one region, as GarageBand does with a
        // REX file whose slices were lost. Muted slices never reach here: they are not replaced
        // by the whole loop.
        let mut audio = crate::import::audio_region(file, ctx);
        audio.len_frames = Some(rex.frame_count() as u64);
        let start_bar = p.beats_to_bar(p.position_beats).floor() as f32 + 1.0;
        let len_bars = p.beats_to_bar(rex.duration_seconds() * bps).ceil().max(1.0) as f32;
        p.tracks[ti].regions.push(Region {
            name: "Loop".into(),
            start_bar,
            len_bars,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            audio: Some(audio),
            params: RegionParams::default(),
            generated: None,
        });
        made = 1;
    }
    p.song_rev += 1;
    let mut status = format!("imported {made} REX slices from {}", path.display());
    if !rex.warnings.is_empty() {
        status.push_str(" (");
        status.push_str(&rex.warnings.join("; "));
        status.push(')');
    }
    Ok(status)
}

/// The sample range of slice `index` when it should play: not muted, not empty, and inside the
/// loop. Anything else yields `None`, so it makes no region.
fn playable_range(rex: &prowave_rex::RexFile, index: usize) -> Option<(usize, usize)> {
    let slice = rex.slices.get(index)?;
    if slice.muted || slice.length == 0 {
        return None;
    }
    let (start, end) = rex.slice_range(index)?;
    (end > start).then_some((start, end))
}

/// REX stores interleaved 1- or 2-channel audio; the engine's WAV writer always takes stereo.
fn stereo_samples(rex: &prowave_rex::RexFile) -> Vec<f32> {
    if rex.channels == 1 {
        let mut stereo = Vec::with_capacity(rex.frame_count() * 2);
        for frame in 0..rex.frame_count() {
            let s = rex.sample(frame, 0);
            stereo.push(s);
            stereo.push(s);
        }
        stereo
    } else {
        rex.to_f32()
    }
}

/// A stable, unique path next to the project's other decoded audio. The file is intentionally
/// kept after import because the regions keep a path-backed `AudioFile`.
fn temporary_wav(source: &Path) -> std::path::PathBuf {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rex")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect::<String>();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "prowave-rex-{}-{nanos}-{stem}.wav",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rex_loop_becomes_slice_regions() {
        let data = prowave_rex::write::RexData::new(48_000, 2, 120.0, vec![0; 16]);
        let data = prowave_rex::write::RexData {
            slices: vec![(0, 4), (4, 4)],
            ..data
        };
        let bytes = prowave_rex::write::write(&data).expect("write a test loop");
        let dir = std::env::temp_dir().join(format!("prowave-rex-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("loop.rx2");
        std::fs::write(&path, bytes).expect("write the test loop");

        let mut p = Project::empty();
        let status = import(&mut p, &path, &egui::Context::default()).expect("import the loop");
        let _ = std::fs::remove_file(&path);

        assert!(status.contains("2 REX slices"));
        assert_eq!(p.tempo, 120.0);
        let track = p.tracks.last().expect("the loop track is appended");
        assert_eq!(track.regions.len(), 2);
        assert_eq!(track.regions[0].name, "Slice 1");
        let audio = track.regions[1].audio.as_ref().expect("slice audio");
        assert_eq!(audio.start_frame, 4);
        assert_eq!(audio.len_frames, Some(4));
    }

    #[test]
    fn muted_slices_make_no_region() {
        let data = prowave_rex::write::RexData::new(48_000, 2, 120.0, vec![0; 16]);
        let data = prowave_rex::write::RexData {
            slices: vec![(0, 4), (4, 4)],
            ..data
        };
        let bytes = prowave_rex::write::write(&data).expect("write a test loop");
        let mut rex = prowave_rex::RexFile::parse(&bytes).expect("parse the test loop");

        assert!(playable_range(&rex, 0).is_some());
        assert!(playable_range(&rex, 1).is_some());

        rex.slices[0].muted = true;
        assert_eq!(playable_range(&rex, 0), None, "a muted slice is not played");
        assert!(
            playable_range(&rex, 1).is_some(),
            "the other slice still is"
        );

        rex.slices[1].muted = true;
        assert!(
            rex.slices
                .iter()
                .enumerate()
                .all(|(i, _)| playable_range(&rex, i).is_none()),
            "with every slice muted nothing is playable, so the import is refused"
        );
    }

    /// Sets the mute flag on the `which`-th `SLCE` chunk: the flags are byte 10 of its payload,
    /// after the 4-byte start and 4-byte length. The writer never sets flags itself.
    fn mute_slice(bytes: &mut [u8], which: usize) {
        let positions: Vec<usize> = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"SLCE")
            .map(|(i, _)| i)
            .collect();
        let at = positions[which] + 8 + 10;
        bytes[at] |= 0b0000_0001;
    }

    fn two_slice_loop() -> Vec<u8> {
        let data = prowave_rex::write::RexData::new(48_000, 2, 120.0, vec![0; 16]);
        let data = prowave_rex::write::RexData {
            slices: vec![(0, 4), (4, 4)],
            ..data
        };
        prowave_rex::write::write(&data).expect("write a test loop")
    }

    #[test]
    fn an_all_muted_loop_is_refused_and_the_project_is_untouched() {
        let mut bytes = two_slice_loop();
        mute_slice(&mut bytes, 0);
        mute_slice(&mut bytes, 1);
        assert!(
            prowave_rex::RexFile::parse(&bytes)
                .expect("parse the muted loop")
                .slices
                .iter()
                .all(|s| s.muted),
            "the test really mutes both slices"
        );
        let dir = std::env::temp_dir().join(format!("prowave-rex-mute-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("all_muted.rx2");
        std::fs::write(&path, &bytes).expect("write the muted loop");

        let mut p = Project::empty();
        let tracks = p.tracks.len();
        let tempo = p.tempo;
        let result = import(&mut p, &path, &egui::Context::default());
        let _ = std::fs::remove_file(&path);

        assert!(result.is_err(), "an all-muted loop is refused");
        assert_eq!(p.tracks.len(), tracks, "no track is added");
        assert_eq!(p.tempo, tempo, "the tempo is not changed");
    }

    #[test]
    fn a_partly_muted_loop_keeps_only_the_audible_slices() {
        let mut bytes = two_slice_loop();
        mute_slice(&mut bytes, 0);
        let dir = std::env::temp_dir().join(format!("prowave-rex-mute-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("one_muted.rx2");
        std::fs::write(&path, &bytes).expect("write the loop");

        let mut p = Project::empty();
        let status = import(&mut p, &path, &egui::Context::default()).expect("import the loop");
        let _ = std::fs::remove_file(&path);

        assert!(status.contains("1 REX slices"), "status: {status}");
        let track = p.tracks.last().expect("the loop track is appended");
        assert_eq!(track.regions.len(), 1);
        assert_eq!(
            track.regions[0].name, "Slice 2",
            "the unmuted slice is the one kept"
        );
    }
}
