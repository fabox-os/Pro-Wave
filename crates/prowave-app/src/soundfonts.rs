//! SoundFont (`.sf2`) files the project's instrument tracks use. Each file is read and parsed
//! once and shared by every track that plays it; the engine plays the presets (see
//! `prowave_engine::SfPlayer`).

use prowave_engine::{SoundFontBank, MAX_SOUNDFONT_BYTES};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type Cache = Mutex<HashMap<String, Arc<SoundFontBank>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The SoundFont at `path`, read and parsed on first use. Errors (a missing or unreadable
/// file, a file that is not a SoundFont, or one larger than [`MAX_SOUNDFONT_BYTES`]) are
/// returned, not cached, so a fixed file is picked up the next time.
pub fn load(path: &str) -> Result<Arc<SoundFontBank>, String> {
    let Ok(mut map) = cache().lock() else {
        return Err("the SoundFont cache is unavailable".into());
    };
    if let Some(bank) = map.get(path) {
        return Ok(Arc::clone(bank));
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > MAX_SOUNDFONT_BYTES as u64 {
        return Err(format!(
            "{path}: larger than {} MB",
            MAX_SOUNDFONT_BYTES >> 20
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
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
