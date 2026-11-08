//! SoundFont (`.sf2`) files and Hydrogen drum kits (`drumkit.xml` and its samples) that the
//! project's instrument tracks use. Each file is read and parsed once and shared by every track
//! that plays it; the engine plays the presets (see `prowave_engine::SfPlayer`).

use prowave_engine::{hydrogen, SoundFontBank, MAX_SOUNDFONT_BYTES};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

type Cache = Mutex<HashMap<String, Arc<SoundFontBank>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The instrument at `path`, read and parsed on first use: a SoundFont (`.sf2`), or a Hydrogen
/// drum kit (`drumkit.xml`), which is built into a SoundFont from its samples. Errors (a
/// missing or unreadable file, a file that is neither, or one larger than
/// [`MAX_SOUNDFONT_BYTES`]) are returned, not cached, so a fixed file is picked up the next time.
pub fn load(path: &str) -> Result<Arc<SoundFontBank>, String> {
    let Ok(mut map) = cache().lock() else {
        return Err("the SoundFont cache is unavailable".into());
    };
    if let Some(bank) = map.get(path) {
        return Ok(Arc::clone(bank));
    }
    let file = Path::new(path);
    let bytes = if hydrogen::is_drumkit(file) {
        hydrogen::soundfont_bytes(file).map_err(|e| format!("{path}: {e}"))?
    } else {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > MAX_SOUNDFONT_BYTES as u64 {
            return Err(format!(
                "{path}: larger than {} MB",
                MAX_SOUNDFONT_BYTES >> 20
            ));
        }
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))?
    };
    if bytes.len() > MAX_SOUNDFONT_BYTES {
        return Err(format!(
            "{path}: larger than {} MB",
            MAX_SOUNDFONT_BYTES >> 20
        ));
    }
    let bank = Arc::new(SoundFontBank::load(&bytes).map_err(|e| format!("{path}: {e}"))?);
    map.insert(path.to_string(), Arc::clone(&bank));
    Ok(bank)
}

/// The SoundFont at `path` if it is already loaded (never reads the file).
pub fn loaded(path: &str) -> Option<Arc<SoundFontBank>> {
    cache().lock().ok()?.get(path).map(Arc::clone)
}

/// Forgets the file at `path`, so the next [`load`] reads it again (after it was replaced).
pub fn forget(path: &str) {
    if let Ok(mut map) = cache().lock() {
        map.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let path = "/nonexistent/prowave-test-missing.sf2";
        assert!(load(path).is_err());
        assert!(loaded(path).is_none());
    }

    #[test]
    fn a_file_that_is_not_a_soundfont_is_an_error() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("not-a-font.sf2");
        assert!(std::fs::write(&path, b"this is not a SoundFont").is_ok());
        let Some(path) = path.to_str() else {
            panic!("a UTF-8 path");
        };
        assert!(load(path).is_err());
        assert!(loaded(path).is_none());
    }

    /// A 16-bit mono PCM WAV holding a 220 Hz sine for 0.1 second at 44.1 kHz.
    fn wav() -> Vec<u8> {
        let rate = 44_100u32;
        let mut data = Vec::new();
        for i in 0..4_410 {
            let t = i as f32 / rate as f32;
            let value = (f32::sin(std::f32::consts::TAU * 220.0 * t) * 12_000.0) as i16;
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

    #[test]
    fn a_hydrogen_drum_kit_loads_as_one_drum_preset() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        if let Err(e) = std::fs::write(dir.path().join("kick.wav"), wav()) {
            panic!("kick.wav: {e}");
        }
        let xml = br#"<drumkit>
            <drumkit_info><name>Test Kit</name></drumkit_info>
            <instrumentList><instrument>
              <name>Kick</name>
              <layer><filename>kick.wav</filename></layer>
              <midi_out_note>36</midi_out_note>
            </instrument></instrumentList>
          </drumkit>"#;
        if let Err(e) = std::fs::write(dir.path().join("drumkit.xml"), xml) {
            panic!("drumkit.xml: {e}");
        }
        let path = dir.path().join("drumkit.xml");
        let Some(path) = path.to_str() else {
            panic!("a UTF-8 path");
        };
        let bank = load(path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bank.presets().len(), 1, "one preset for the kit");
        let Some(preset) = bank.presets().first() else {
            panic!("a preset");
        };
        assert_eq!(preset.name, "Test Kit");
        assert_eq!(preset.bank, 128, "a drum kit");
        assert!(loaded(path).is_some());
        forget(path);
        assert!(loaded(path).is_none());
    }

    /// The General MIDI SoundFont named by `PROWAVE_TEST_SF2`, if set (it is not in the repo).
    #[test]
    fn a_soundfont_is_loaded_once_and_then_shared() {
        let Ok(path) = std::env::var("PROWAVE_TEST_SF2") else {
            return;
        };
        let Ok(first) = load(&path) else {
            panic!("load {path}");
        };
        let Ok(second) = load(&path) else {
            panic!("load again");
        };
        assert!(Arc::ptr_eq(&first, &second), "one copy in memory");
        assert!(loaded(&path).is_some());
        forget(&path);
        assert!(loaded(&path).is_none());
    }
}
