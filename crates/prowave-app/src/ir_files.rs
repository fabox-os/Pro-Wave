//! Impulse-response files that Space Designer uses (Logic: Load Impulse Response…): each file
//! is read once and shared by every Space Designer that uses it, and its resampled copies (one
//! for each sample rate asked for) are kept too. The file itself is read by
//! `prowave_engine::ir_file`; this module is the cache and the names.
//!
//! A project keeps the file's path, like it does for SoundFonts. A file that is gone leaves the
//! Space Designer playing the built-in response chosen by its IR setting.

use prowave_engine::ir_file::{read_ir_file, IrFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// An impulse-response file a Space Designer uses: a number into this module's table, so an
/// insert stays a small copyable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IrFileId(u32);

impl IrFileId {
    /// The number in the table, for comparing responses.
    pub fn number(self) -> u32 {
        self.0
    }
}

#[derive(Default)]
struct State {
    paths: Vec<PathBuf>,
    /// The file as read (or why it could not be).
    files: HashMap<u32, Result<Arc<IrFile>, String>>,
    /// The file at a sample rate.
    resampled: HashMap<(u32, u32), Arc<Vec<[f32; 2]>>>,
    /// Resampling that has been started in the background and not finished.
    pending: Vec<(u32, u32)>,
    /// Why the file the user chose last could not be used.
    last_error: Option<String>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    let m = STATE.get_or_init(|| Mutex::new(State::default()));
    // A panic while the lock was held leaves plain data behind: carry on with it.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The id for the file at `path` (the same one every time for the same path).
pub fn intern(path: &Path) -> IrFileId {
    let mut s = state();
    if let Some(i) = s.paths.iter().position(|p| p == path) {
        return IrFileId(i as u32);
    }
    s.paths.push(path.to_path_buf());
    IrFileId((s.paths.len() - 1) as u32)
}

/// The path `id` stands for.
pub fn path(id: IrFileId) -> Option<PathBuf> {
    state().paths.get(id.0 as usize).cloned()
}

/// The file's name, for display.
pub fn name(id: IrFileId) -> String {
    path(id)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The file, read now if it was not yet (this reads from disk: not for the audio thread, and
/// not every frame; see [`peek`]). A failure is remembered, so a missing file is looked for once
/// until [`forget`].
pub fn load(id: IrFileId) -> Result<Arc<IrFile>, String> {
    let file = {
        let s = state();
        if let Some(known) = s.files.get(&id.0) {
            return known.clone();
        }
        s.paths.get(id.0 as usize).cloned()
    };
    let result = match file {
        Some(p) => read_ir_file(&p).map(Arc::new),
        None => Err("unknown file".to_string()),
    };
    state().files.insert(id.0, result.clone());
    result
}

/// The file if it has been read already (never reads from disk).
pub fn peek(id: IrFileId) -> Option<Result<Arc<IrFile>, String>> {
    state().files.get(&id.0).cloned()
}

/// Forgets what was read from the file (and its resampled copies), so the next [`load`] reads
/// it again after it was replaced.
pub fn forget(id: IrFileId) {
    let mut s = state();
    s.files.remove(&id.0);
    s.resampled.retain(|(f, _), _| *f != id.0);
}

/// The file's response at `sample_rate` Hz, resampled once and kept. Reads the file if needed,
/// so call it on a background thread. `None` when the file cannot be read.
pub fn resampled(id: IrFileId, sample_rate: u32) -> Option<Arc<Vec<[f32; 2]>>> {
    if let Some(done) = state().resampled.get(&(id.0, sample_rate)) {
        return Some(done.clone());
    }
    let file = load(id).ok()?;
    let frames = Arc::new(file.at_rate(sample_rate));
    state()
        .resampled
        .insert((id.0, sample_rate), frames.clone());
    Some(frames)
}

/// Like [`resampled`] but never blocks: the copy if it exists, else `None` and the work is
/// started on a background thread (for drawing the response each frame).
pub fn resampled_in_background(id: IrFileId, sample_rate: u32) -> Option<Arc<Vec<[f32; 2]>>> {
    {
        let mut s = state();
        if let Some(done) = s.resampled.get(&(id.0, sample_rate)) {
            return Some(done.clone());
        }
        if s.pending.contains(&(id.0, sample_rate)) {
            return None;
        }
        s.pending.push((id.0, sample_rate));
    }
    std::thread::spawn(move || {
        let _ = resampled(id, sample_rate);
        state().pending.retain(|k| *k != (id.0, sample_rate));
    });
    None
}

/// The reason the last file the user chose could not be used (cleared by a good choice).
pub fn last_error() -> Option<String> {
    state().last_error.clone()
}

/// Remembers (or clears) the reason a chosen file could not be used.
pub fn set_last_error(error: Option<String>) {
    state().last_error = error;
}

/// What the Space Designer shows for the file: its name, "missing" when it cannot be read.
pub fn label(id: IrFileId) -> String {
    let name = name(id);
    match peek(id) {
        Some(Err(_)) => format!("{name} · {}", crate::i18n::tr("missing")),
        _ => name,
    }
}

/// The text of a response's file name for the project, and the file for a project's text.
pub fn to_doc(id: IrFileId) -> Option<String> {
    path(id).map(|p| p.to_string_lossy().into_owned())
}

/// The file a project names (nothing for an empty path).
pub fn from_doc(text: &str) -> Option<IrFileId> {
    let t = text.trim();
    (!t.is_empty()).then(|| intern(Path::new(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, frames: &[[f32; 2]]) -> Vec<u8> {
        let data = frames.len() as u32 * 8;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 8).to_le_bytes());
        v.extend_from_slice(&8u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        for f in frames {
            v.extend_from_slice(&f[0].to_le_bytes());
            v.extend_from_slice(&f[1].to_le_bytes());
        }
        v
    }

    #[test]
    fn the_same_path_gets_the_same_id_and_a_project_text_round_trips() {
        let a = intern(Path::new("/irs/hall.wav"));
        assert_eq!(intern(Path::new("/irs/hall.wav")), a);
        assert_ne!(intern(Path::new("/irs/room.wav")), a);
        assert_eq!(name(a), "hall.wav");
        assert_eq!(to_doc(a).as_deref(), Some("/irs/hall.wav"));
        assert_eq!(from_doc("/irs/hall.wav"), Some(a));
        assert_eq!(from_doc("   "), None);
    }

    #[test]
    fn a_file_is_read_once_resampled_once_and_a_missing_one_is_remembered() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("click.wav");
        let mut frames = vec![[0.0f32; 2]; 4_410];
        frames[100] = [1.0, 0.5];
        std::fs::write(&path, wav(44_100, &frames)).expect("write");
        let id = intern(&path);
        assert!(peek(id).is_none(), "nothing is read until asked");
        let file = load(id).expect("read");
        assert_eq!(file.sample_rate, 44_100);
        assert!(Arc::ptr_eq(&file, &load(id).expect("cached")));
        let at48 = resampled(id, 48_000).expect("resampled");
        assert_eq!(at48.len(), 4_800);
        assert!(Arc::ptr_eq(&at48, &resampled(id, 48_000).expect("cached")));
        assert_eq!(label(id), "click.wav");
        // Replaced on disk: forgotten, then read again.
        let mut other = vec![[0.0f32; 2]; 2_205];
        other[10] = [1.0, 1.0];
        std::fs::write(&path, wav(44_100, &other)).expect("write");
        forget(id);
        assert!(peek(id).is_none());
        assert_eq!(load(id).expect("read again").frames.len(), 2_205);

        let gone = intern(&dir.path().join("gone.wav"));
        assert!(load(gone).is_err());
        assert!(matches!(peek(gone), Some(Err(_))), "the failure is kept");
        assert_eq!(label(gone), "gone.wav · missing");
        assert!(resampled(gone, 48_000).is_none());
    }

    #[test]
    fn the_background_copy_arrives_without_blocking_the_caller() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("bg.wav");
        let mut frames = vec![[0.0f32; 2]; 9_600];
        frames[0] = [1.0, 1.0];
        std::fs::write(&path, wav(96_000, &frames)).expect("write");
        let id = intern(&path);
        let first = resampled_in_background(id, 48_000);
        let mut got = first;
        for _ in 0..200 {
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            got = resampled_in_background(id, 48_000);
        }
        assert_eq!(got.map(|f| f.len()), Some(4_800));
    }
}
