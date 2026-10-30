//! The Loop Browser and the All Files browser (the two buttons at the right of the control bar):
//! a folder list; double-click a file to add it to the project.
//!
//! The Loop Browser shows the audio files of a folder of your choice, which it remembers. It
//! does not read Apple Loops tags, and loops in ALAC or AAC do not open yet.
use crate::i18n::tr;
use std::path::{Path, PathBuf};

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

#[derive(Default)]
pub struct BrowserView {
    /// The pane on show, if any.
    pub open: Option<Tab>,
    dir: Option<PathBuf>,
    /// What `entries` lists.
    listed: Option<(Tab, PathBuf)>,
    entries: Vec<Entry>,
    search: String,
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

pub fn show(ui: &mut egui::Ui, s: &mut crate::UiState) {
    let v = &mut s.browser;
    let Some(tab) = v.open else { return };
    let dir = v.dir.get_or_insert_with(|| start_dir(tab)).clone();
    if v.listed.as_ref() != Some(&(tab, dir.clone())) {
        v.entries = list(&dir, tab);
        v.listed = Some((tab, dir.clone()));
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
        for e in v.entries.iter().filter(|e| matches(&e.name, &v.search)) {
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
}
