//! The Loop Browser and the All Files browser (the two buttons at the right of the control bar):
//! a folder list; double-click a file to add it to the project.
//!
//! The Loop Browser shows the audio files of a folder of your choice, which it remembers. For
//! Apple Loops (`.caf`) it also reads the tags (tempo, key, category) on a background thread,
//! shows them under the file name and lets the search find them. It does not yet stretch a
//! loop to the project's tempo and key.
use crate::apple_loops::{self, LoopTags};
use crate::i18n::tr;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

/// Width of the pane.
pub const WIDTH: f32 = 280.0;
/// A folder with more entries than this shows only these.
const MAX_ENTRIES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Loops,
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: String,
    folder: bool,
}

/// A background read of the Apple Loops tags of the files in a folder.
struct TagScan {
    results: Receiver<(PathBuf, Option<LoopTags>)>,
    /// Set when the folder on show changes, so the thread stops early.
    cancel: Arc<AtomicBool>,
}

impl TagScan {
    /// Starts reading the tags of `paths`.
    fn start(paths: Vec<PathBuf>) -> Option<Self> {
        let (tx, results) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("loop-tags".into())
            .spawn(move || {
                for path in paths {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let tags = apple_loops::read_file(&path);
                    if tx.send((path, tags)).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self { results, cancel })
    }
}

impl Drop for TagScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct BrowserView {
    /// The pane on show, if any.
    pub open: Option<Tab>,
    dir: Option<PathBuf>,
    /// What `entries` lists.
    listed: Option<(Tab, PathBuf)>,
    entries: Vec<Entry>,
    search: String,
    /// The Apple Loops tags read so far (`None`: the file has none), and the read still running.
    tags: HashMap<PathBuf, Option<LoopTags>>,
    scan: Option<TagScan>,
}

impl BrowserView {
    /// A control bar button: opens its pane, or closes it when it is the one on show.
    pub fn toggle(&mut self, tab: Tab) {
        self.open = (self.open != Some(tab)).then_some(tab);
        self.dir = None;
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn loop_folder_file() -> Option<PathBuf> {
    crate::recent_files::config_dir().map(|d| d.join("loop-folder"))
}

/// Where a pane opens: the remembered loop folder, else the Music folder or the home folder.
fn start_dir(tab: Tab) -> PathBuf {
    let saved = (tab == Tab::Loops)
        .then(loop_folder_file)
        .flatten()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|text| PathBuf::from(text.trim()))
        .filter(|d| d.is_dir());
    saved.unwrap_or_else(|| {
        let home = home().unwrap_or_else(|| PathBuf::from("."));
        let music = home.join("Music");
        if tab == Tab::Loops && music.is_dir() {
            music
        } else {
            home
        }
    })
}

fn wanted(tab: Tab, path: &Path) -> bool {
    crate::import::is_audio_path(path) || tab == Tab::Files && crate::midi_file::is_midi_path(path)
}

/// The folders and the files `tab` can add, folders first, each sorted by name. Hidden entries
/// are left out. A folder that cannot be read lists nothing.
fn list(dir: &Path, tab: Tab) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .flatten()
        .take(MAX_ENTRIES)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let folder = path.is_dir();
            (!name.starts_with('.') && (folder || wanted(tab, &path))).then_some(Entry {
                path,
                name,
                folder,
            })
        })
        .collect();
    entries.sort_by_key(|e| (!e.folder, e.name.to_lowercase()));
    entries
}

fn matches(name: &str, search: &str) -> bool {
    name.to_lowercase().contains(&search.trim().to_lowercase())
}

/// Whether a file shows for `search`: its name, or any of its Apple Loops tags, matches.
fn entry_matches(name: &str, tags: Option<&LoopTags>, search: &str) -> bool {
    matches(name, search) || tags.is_some_and(|t| t.matches(&search.trim().to_lowercase()))
}

pub fn show(ui: &mut egui::Ui, s: &mut crate::UiState) {
    let v = &mut s.browser;
    let Some(tab) = v.open else { return };
    let dir = v.dir.get_or_insert_with(|| start_dir(tab)).clone();
    if v.listed.as_ref() != Some(&(tab, dir.clone())) {
        v.entries = list(&dir, tab);
        v.listed = Some((tab, dir.clone()));
        // Read the tags of the loops of the new folder in the background.
        v.tags.clear();
        let loops = v
            .entries
            .iter()
            .filter(|e| !e.folder && tab == Tab::Loops)
            .map(|e| e.path.clone())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("caf")))
            .collect::<Vec<_>>();
        v.scan = if loops.is_empty() {
            None
        } else {
            TagScan::start(loops)
        };
    }
    if let Some(scan) = &v.scan {
        loop {
            match scan.results.try_recv() {
                Ok((path, tags)) => {
                    v.tags.insert(path, tags);
                }
                Err(TryRecvError::Empty) => {
                    ui.ctx().request_repaint();
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    v.scan = None;
                    break;
                }
            }
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong(match tab {
            Tab::Loops => tr("Loops"),
            Tab::Files => tr("All Files"),
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        if ui
            .add_enabled(dir.parent().is_some(), egui::Button::new(tr("Up")))
            .clicked()
        {
            v.dir = dir.parent().map(Path::to_path_buf);
        }
        if tab == Tab::Loops && ui.button(tr("Choose Folder…")).clicked() {
            if let Some(picked) = rfd::FileDialog::new().pick_folder() {
                if let Some(file) = loop_folder_file() {
                    let saved = file
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&file, picked.to_string_lossy().as_bytes()));
                    if let Err(e) = saved {
                        tracing::warn!("could not save the loop folder: {e}");
                    }
                }
                v.dir = Some(picked);
            }
        }
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut v.search)
                .hint_text(tr("Search"))
                .desired_width(WIDTH - 24.0),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.small(dir.display().to_string());
    });
    ui.separator();
    let mut enter = None;
    let mut add = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for e in v.entries.iter().filter(|e| {
            let tags = v.tags.get(&e.path).and_then(Option::as_ref);
            entry_matches(&e.name, tags, &v.search)
        }) {
            let text = if e.folder {
                format!("▸ {}", e.name)
            } else {
                e.name.clone()
            };
            let row = ui.selectable_label(false, text);
            if e.folder && row.clicked() {
                enter = Some(e.path.clone());
            } else if !e.folder && row.double_clicked() {
                add = Some(e.path.clone());
            }
            if let Some(line) = v
                .tags
                .get(&e.path)
                .and_then(Option::as_ref)
                .map(LoopTags::summary)
                .filter(|line| !line.is_empty())
            {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.weak(egui::RichText::new(line).small());
                });
            }
        }
    });
    if enter.is_some() {
        v.dir = enter;
        v.search.clear();
    }
    s.open_paths.extend(add);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn folders_come_first_and_each_pane_lists_what_it_can_add() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["b.wav", "A.flac", "song.mid", "notes.txt", ".hidden.wav"] {
            std::fs::write(dir.path().join(f), b"").unwrap();
        }
        std::fs::create_dir(dir.path().join("Drums")).unwrap();
        assert_eq!(
            names(&list(dir.path(), Tab::Files)),
            ["Drums", "A.flac", "b.wav", "song.mid"]
        );
        assert_eq!(
            names(&list(dir.path(), Tab::Loops)),
            ["Drums", "A.flac", "b.wav"],
            "loops are audio"
        );
    }

    #[test]
    fn a_missing_folder_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("gone"), Tab::Files).is_empty());
    }

    #[test]
    fn the_buttons_open_and_close_their_pane() {
        let mut v = BrowserView::default();
        v.toggle(Tab::Loops);
        assert_eq!(v.open, Some(Tab::Loops));
        v.toggle(Tab::Files);
        assert_eq!(v.open, Some(Tab::Files), "the other pane takes over");
        v.toggle(Tab::Files);
        assert_eq!(v.open, None);
    }

    #[test]
    fn search_ignores_case_and_spaces() {
        assert!(matches("Funky Drums.wav", " drums "));
        assert!(matches("x.wav", ""));
        assert!(!matches("x.wav", "bass"));
    }

    /// A minimal Apple Loop: a CAF file whose metadata chunk names a category and key.
    fn loop_bytes(category: &str) -> Vec<u8> {
        let mut body = vec![
            0x29, 0x81, 0x92, 0x73, 0xb5, 0xbf, 0x4a, 0xef, 0xb7, 0x8d, 0x62, 0xd1, 0xef, 0x90,
            0xbb, 0x2c,
        ];
        body.extend_from_slice(&2u32.to_be_bytes());
        for text in ["category", category, "key signature", "G"] {
            body.extend_from_slice(text.as_bytes());
            body.push(0);
        }
        let mut file = b"caff\0\x01\0\0uuid".to_vec();
        file.extend_from_slice(&(body.len() as i64).to_be_bytes());
        file.extend(body);
        file
    }

    #[test]
    fn a_file_shows_for_a_search_that_matches_its_name_or_its_tags() {
        let tags = LoopTags {
            category: "Bass".into(),
            key: "G".into(),
            ..LoopTags::default()
        };
        assert!(entry_matches("Funky.caf", Some(&tags), "funk"));
        assert!(entry_matches("Funky.caf", Some(&tags), " BASS "));
        assert!(!entry_matches("Funky.caf", Some(&tags), "drums"));
        assert!(
            !entry_matches("Funky.caf", None, "bass"),
            "no tags, no match"
        );
        assert!(entry_matches("Funky.caf", None, ""));
    }

    #[test]
    fn the_tags_of_the_loops_in_a_folder_are_read_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let bass = dir.path().join("Bass One.caf");
        let plain = dir.path().join("plain.caf");
        std::fs::write(&bass, loop_bytes("Bass")).unwrap();
        std::fs::write(&plain, b"not a caf").unwrap();
        let scan = TagScan::start(vec![bass.clone(), plain.clone()]).unwrap();
        let mut found = HashMap::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while found.len() < 2 && std::time::Instant::now() < deadline {
            match scan.results.try_recv() {
                Ok((path, tags)) => {
                    found.insert(path, tags);
                }
                Err(TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(TryRecvError::Disconnected) => break,
            }
        }
        assert_eq!(
            found[&bass].as_ref().map(|t| t.category.as_str()),
            Some("Bass")
        );
        assert_eq!(found[&plain], None, "a file without the tags has none");
    }

    #[test]
    fn the_loop_browser_draws_a_folder_of_loops_and_finds_them_by_tag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Groove.caf"), loop_bytes("Bass")).unwrap();
        std::fs::write(dir.path().join("Hits.caf"), loop_bytes("Drums")).unwrap();
        let mut s = crate::UiState::new();
        s.browser.open = Some(Tab::Loops);
        s.browser.dir = Some(dir.path().to_path_buf());
        let mut h = crate::ui_test::Harness::new(300.0, 600.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.browser.tags.len() < 2 && std::time::Instant::now() < deadline {
            h.frame(vec![], |ui| show(ui, &mut s));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(s.browser.tags.len(), 2, "both loops were read");
        let drums: Vec<&str> = s
            .browser
            .entries
            .iter()
            .filter(|e| {
                let tags = s.browser.tags.get(&e.path).and_then(Option::as_ref);
                entry_matches(&e.name, tags, "drums")
            })
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(drums, ["Hits.caf"], "found by its category, not its name");
    }
}
