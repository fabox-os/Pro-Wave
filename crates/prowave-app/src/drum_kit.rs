//! The Drum Machine's kit as a track keeps it: the settings of its sixteen pads and, for a
//! sample pad, the file it plays. Building the engine's drum machine from it reads the sample
//! files, so it runs on a background thread (see `engine_sync.rs`).

use prowave_engine::drum_machine::{default_kit, DrumMachine, PadKind, PadSettings, PADS};
use prowave_engine::sample_file::{read_sample, SampleData};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The longest sample a drum pad takes, in seconds (a drum hit, not a loop).
pub const MAX_PAD_SECONDS: f32 = 20.0;

/// One pad: its settings, and the sample file of a sample pad.
#[derive(Clone, Debug, PartialEq)]
pub struct PadUse {
    pub settings: PadSettings,
    pub sample: Option<PathBuf>,
}

/// A track's Drum Machine: sixteen pads on the keys 36 to 51.
#[derive(Clone, Debug, PartialEq)]
pub struct DrumsUse {
    pub pads: [PadUse; PADS],
}

impl Default for DrumsUse {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumsUse {
    /// The starting kit: the General MIDI layout of synthesized drums.
    pub fn new() -> Self {
        let kit = default_kit();
        Self {
            pads: std::array::from_fn(|i| PadUse {
                settings: kit[i],
                sample: None,
            }),
        }
    }

    /// The name a pad shows: the sample's file name for a sample pad, else its drum.
    pub fn pad_name(&self, pad: usize) -> String {
        match self.pads.get(pad) {
            Some(PadUse {
                settings,
                sample: Some(path),
            }) if settings.kind == PadKind::Sample => path.file_name().map_or_else(
                || PadKind::Sample.name().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            Some(p) => p.settings.kind.name().to_string(),
            None => String::new(),
        }
    }

    /// The name a pad shows, translated (a sample pad shows its file's name).
    pub fn pad_label(&self, pad: usize) -> String {
        match self.pads.get(pad) {
            Some(PadUse {
                settings,
                sample: Some(_),
            }) if settings.kind == PadKind::Sample => self.pad_name(pad),
            Some(p) => crate::i18n::tr(p.settings.kind.name()).to_string(),
            None => String::new(),
        }
    }

    /// The pads that play a sample, with their files: what has to be read before the kit can be
    /// swapped in. Two kits with the same list differ only in settings, which move live.
    pub fn sample_pads(&self) -> Vec<(usize, PathBuf)> {
        self.pads
            .iter()
            .enumerate()
            .filter(|(_, p)| p.settings.kind == PadKind::Sample)
            .filter_map(|(i, p)| p.sample.clone().map(|s| (i, s)))
            .collect()
    }
}

/// The engine's drum machine for `kit`: every sample pad's file is read (a pad whose file cannot
/// be read is silent, with the reason kept for [`error`]).
pub fn build_machine(kit: &DrumsUse, sample_rate: f32) -> DrumMachine {
    let mut samples: [Option<Arc<SampleData>>; PADS] = std::array::from_fn(|_| None);
    for (pad, path) in kit.sample_pads() {
        match read_sample(&path) {
            Ok(s) if s.seconds() <= MAX_PAD_SECONDS => {
                set_error(&path, None);
                samples[pad] = Some(Arc::new(s));
            }
            Ok(_) => set_error(
                &path,
                Some(format!(
                    "{}: longer than {MAX_PAD_SECONDS} seconds",
                    path.display()
                )),
            ),
            Err(e) => {
                tracing::warn!("drum machine: {e}");
                set_error(&path, Some(e));
            }
        }
    }
    DrumMachine::new(kit.pads.clone().map(|p| p.settings), samples, sample_rate)
}

// ---------------------------------------------------------------------------------------------
// What went wrong, for the window.

fn errors() -> MutexGuard<'static, HashMap<PathBuf, String>> {
    static ERRORS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    ERRORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn set_error(path: &Path, error: Option<String>) {
    let mut map = errors();
    match error {
        Some(e) => {
            map.insert(path.to_path_buf(), e);
        }
        None => {
            map.remove(path);
        }
    }
}

/// Why the sample at `path` could not be used the last time a kit was built.
pub fn error(path: &Path) -> Option<String> {
    errors().get(path).cloned()
}

/// Checks that `path` is a sample a pad can take (readable audio, not longer than
/// [`MAX_PAD_SECONDS`]) and returns why not. Reads the file: call it when the user picks one.
pub fn check_sample(path: &Path) -> Result<(), String> {
    let s = read_sample(path)?;
    if s.seconds() > MAX_PAD_SECONDS {
        return Err(format!(
            "{}: longer than {MAX_PAD_SECONDS} seconds",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frames: usize) -> Vec<u8> {
        let data = frames as u32 * 4;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&48_000u32.to_le_bytes());
        v.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            v.extend_from_slice(&(if i < 300 { 0.7f32 } else { 0.2 }).to_le_bytes());
        }
        v
    }

    #[test]
    fn the_starting_kit_is_the_general_midi_layout_named_after_its_drums() {
        let kit = DrumsUse::new();
        assert_eq!(kit.pads.len(), 16);
        assert_eq!(kit.pad_name(0), "Kick");
        assert_eq!(kit.pad_name(6), "Closed Hi-Hat");
        assert_eq!(kit.pad_name(10), "Open Hi-Hat");
        assert_eq!(kit.pad_name(99), "");
        assert!(kit.sample_pads().is_empty());
        assert_eq!(DrumsUse::default(), kit);
    }

    #[test]
    fn a_sample_pad_is_named_after_its_file_and_listed_and_a_kit_with_it_builds_and_plays() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("snap.wav");
        std::fs::write(&file, wav(9_600)).expect("write");
        let mut kit = DrumsUse::new();
        kit.pads[1].settings = PadKind::Sample.settings();
        kit.pads[1].sample = Some(file.clone());
        assert_eq!(kit.pad_name(1), "snap.wav");
        assert_eq!(kit.sample_pads(), vec![(1, file.clone())]);
        // A sample pad with no file keeps the name of its kind and is not listed.
        kit.pads[2].settings = PadKind::Sample.settings();
        assert_eq!(kit.pad_name(2), "Sample");
        assert_eq!(kit.sample_pads().len(), 1);
        let mut machine = build_machine(&kit, 48_000.0);
        machine.note_on(37, 127);
        let x: Vec<f32> = (0..4_800).map(|_| machine.render().0).collect();
        assert!(x.iter().any(|v| v.abs() > 0.1), "the sample pad sounds");
        machine.note_on(38, 127);
        assert_eq!(
            machine.active_voices(),
            1,
            "a sample pad with no file is silent"
        );
        assert!(error(&file).is_none());
        assert!(check_sample(&file).is_ok());
    }

    #[test]
    fn a_missing_a_broken_or_a_too_long_sample_is_reported_and_the_pad_is_silent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let gone = dir.path().join("gone.wav");
        let junk = dir.path().join("junk.wav");
        std::fs::write(&junk, b"nope").expect("write");
        let long = dir.path().join("long.wav");
        std::fs::write(&long, wav(48_000 * 25)).expect("write");
        let mut kit = DrumsUse::new();
        for (i, p) in [&gone, &junk, &long].into_iter().enumerate() {
            kit.pads[i].settings = PadKind::Sample.settings();
            kit.pads[i].sample = Some(p.clone());
        }
        let mut machine = build_machine(&kit, 48_000.0);
        for key in [36u8, 37, 38] {
            machine.note_on(key, 127);
        }
        assert_eq!(machine.active_voices(), 0);
        for p in [&gone, &junk, &long] {
            assert!(error(p).is_some(), "{}", p.display());
        }
        assert!(check_sample(&gone).is_err());
        assert!(check_sample(&long).unwrap_err().contains("longer"));
    }
}
