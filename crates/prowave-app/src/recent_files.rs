//! File > Open Recent: the projects last opened or saved, newest first. Kept in a plain text
//! file (one path per line) in the user's config folder, so it survives restarts.
use std::path::{Path, PathBuf};

/// How many projects the menu remembers.
const MAX: usize = 10;

/// `<config folder>/prowave/recent.txt`.
fn default_file() -> Option<PathBuf> {
    Some(config_dir()?.join("recent.txt"))
}

/// `<config folder>/prowave`: %APPDATA% on Windows, ~/Library/Application Support on macOS,
/// $XDG_CONFIG_HOME or ~/.config elsewhere. `None` when no home folder is known.
pub(crate) fn config_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        var("APPDATA")?
    } else if cfg!(target_os = "macos") {
        var("HOME")?.join("Library").join("Application Support")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))?
    };
    Some(base.join("prowave"))
}

/// The recent-projects list. `Recent::default()` is an in-memory list that never touches disk
/// (used by tests); [`Recent::load`] is the real, persistent one.
#[derive(Default)]
pub struct Recent {
    paths: Vec<PathBuf>,
    file: Option<PathBuf>,
}

impl Recent {
    /// Reads the saved list, leaving out projects that no longer exist.
    pub fn load() -> Self {
        Self::from_file(default_file())
    }

    fn from_file(file: Option<PathBuf>) -> Self {
        let text = file
            .as_deref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .unwrap_or_default();
        let paths = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .take(MAX)
            .collect();
        Self { paths, file }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Moves `path` to the top (adding it if new) and saves.
    pub fn add(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(MAX);
        self.save();
    }

    /// Drops `path`, e.g. after it turned out to be unreadable.
    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.save();
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.save();
    }

    /// Best effort: a list that cannot be saved is not worth interrupting the user for.
    fn save(&self) {
        let Some(file) = &self.file else { return };
        let text: String = self
            .paths
            .iter()
            .filter_map(|p| p.to_str())
            .map(|p| format!("{p}\n"))
            .collect();
        let result = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(file, text));
        if let Err(e) = result {
            tracing::warn!("could not save the recent projects list: {e}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn newest_first_without_duplicates_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recent::default();
        let files: Vec<PathBuf> = (0..12)
            .map(|i| touch(dir.path(), &format!("p{i}.prowave")))
            .collect();
        for f in &files {
            r.add(f);
        }
        assert_eq!(r.paths().len(), MAX);
        assert_eq!(r.paths()[0], files[11]);
        r.add(&files[5]);
        assert_eq!(r.paths()[0], files[5]);
        assert_eq!(r.paths().iter().filter(|p| **p == files[5]).count(), 1);
        r.remove(&files[5]);
        assert!(!r.paths().contains(&files[5]));
        r.clear();
        assert!(r.paths().is_empty());
    }

    #[test]
    fn survives_a_restart_and_forgets_deleted_projects() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("config").join("prowave").join("recent.txt");
        let (a, b) = (
            touch(dir.path(), "a.prowave"),
            touch(dir.path(), "b.prowave"),
        );
        let mut r = Recent::from_file(Some(list.clone()));
        r.add(&a);
        r.add(&b);

        let reloaded = Recent::from_file(Some(list.clone()));
        assert_eq!(reloaded.paths(), [b.clone(), a.clone()]);

        std::fs::remove_file(&b).unwrap();
        assert_eq!(Recent::from_file(Some(list)).paths(), [a]);
    }

    #[test]
    fn a_missing_or_unwritable_list_is_just_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Recent::from_file(Some(dir.path().join("nope.txt")))
            .paths()
            .is_empty());
        // The "file" is really a folder, so saving fails; that must not panic.
        let mut r = Recent::from_file(Some(dir.path().to_path_buf()));
        r.add(&touch(dir.path(), "a.prowave"));
        assert_eq!(r.paths().len(), 1);
    }
}
