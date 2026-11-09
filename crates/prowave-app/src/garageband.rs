//! Importing a GarageBand project (issue #35): its audio files become audio tracks.
//!
//! The project's arrangement — MIDI regions and where each audio region sits — is not read
//! yet; `GARAGEBAND-FORMAT-SPEC.md` has the plan. What works today is the part that needs no
//! arrangement: the media of a macOS `.band` bundle, one track each, placed at the playhead
//! like File > Import > Audio File.

use crate::model::Project;
use std::path::Path;

/// Adds an audio track for each audio file of the GarageBand project at `path`, and returns how
/// many were added. The project's own tempo and signature are left alone, as when importing
/// tracks from another project in Logic.
pub fn import(p: &mut Project, path: &Path, ctx: &egui::Context) -> Result<usize, String> {
    let bundle = prowave_garageband_import::detect(path)?;
    let summary = prowave_garageband_import::summary(&bundle)?;
    let prowave_garageband_import::Bundle::Folder { root, .. } = &bundle else {
        return Err(
            "an iOS .band zip cannot be imported yet: unzip it and pick the folder".to_string(),
        );
    };
    let mut imported = 0;
    for media in &summary.media {
        let file = root.join(&media.path);
        if !crate::import::is_audio_path(&file) {
            continue;
        }
        crate::import::import_audio(p, &file, ctx).map_err(|e| format!("{}: {e}", media.path))?;
        imported += 1;
    }
    if imported == 0 {
        return Err("the GarageBand project has no audio files".to_string());
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A 0.1 s, 44.1 kHz, 16-bit mono WAV holding a 440 Hz tone.
    fn wav() -> Vec<u8> {
        let rate = 44_100u32;
        let mut data = Vec::new();
        for i in 0..4_410 {
            let t = i as f32 / rate as f32;
            let value = (f32::sin(std::f32::consts::TAU * 440.0 * t) * 12_000.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// A GarageBand 10 bundle folder with one audio file.
    fn band(dir: &Path) -> PathBuf {
        let band = dir.join("Song.band");
        let alternative = band.join("Alternatives").join("000");
        std::fs::create_dir_all(&alternative).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(alternative.join("ProjectData"), [0x23, 0x47, 0xC0, 0xAB])
            .unwrap_or_else(|e| panic!("{e}"));
        let media = band.join("Media").join("Audio Files");
        std::fs::create_dir_all(&media).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(media.join("Take 1.wav"), wav()).unwrap_or_else(|e| panic!("{e}"));
        band
    }

    #[test]
    fn a_band_adds_a_track_for_its_audio_files() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = band(dir.path());
        assert!(prowave_garageband_import::detect(&path).is_ok());
        let ctx = egui::Context::default();
        let mut project = Project::empty();
        let before = project.tracks.len();
        let added = import(&mut project, &path, &ctx).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(added, 1);
        assert_eq!(project.tracks.len(), before + 1);
        let track = project.tracks.last().unwrap_or_else(|| panic!("a track"));
        assert_eq!(track.kind, crate::model::TrackKind::Audio);
        assert_eq!(track.regions.len(), 1, "one audio region");
        assert!(track.regions[0].audio.is_some());
    }

    #[test]
    fn a_folder_that_is_not_a_project_is_an_error() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        assert!(prowave_garageband_import::detect(dir.path()).is_err());
        let ctx = egui::Context::default();
        let mut project = Project::empty();
        assert!(import(&mut project, dir.path(), &ctx).is_err());
    }

    #[test]
    fn an_ios_zip_says_what_to_do_instead() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("Phone Song.band.zip");
        let file = std::fs::File::create(&path).unwrap_or_else(|e| panic!("{e}"));
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write;
        zip.start_file("projectData", zip::write::SimpleFileOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        zip.write_all(b"<plist version=\"1.0\"><dict/></plist>")
            .unwrap_or_else(|e| panic!("{e}"));
        zip.finish().unwrap_or_else(|e| panic!("{e}"));
        assert!(prowave_garageband_import::detect(&path).is_ok());
        let ctx = egui::Context::default();
        let mut project = Project::empty();
        let error = import(&mut project, &path, &ctx).unwrap_err();
        assert!(error.contains("zip"), "{error}");
    }
}
