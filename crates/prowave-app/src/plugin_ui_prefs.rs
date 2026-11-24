//! How hosted plug-ins are shown: the portable generic sliders or the plug-in's own editor.
//! Kept in `plugin-ui.txt` so the next start opens plug-ins in the same view.
use std::path::{Path, PathBuf};

/// The view a hosted plug-in opens in (Logic Pro: View > Controls or View > Editor).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PluginUiMode {
    /// Normalized egui sliders built from the plug-in's parameter metadata. The default.
    #[default]
    Sliders,
    /// The plug-in's own window, where the host can provide it.
    Editor,
}

impl PluginUiMode {
    /// Reads `mode=sliders` or `mode=editor`; anything else keeps the default.
    pub(crate) fn parse(text: &str) -> Self {
        for line in text.lines() {
            match line.trim().strip_prefix("mode=") {
                Some("editor") => return Self::Editor,
                Some("sliders") => return Self::Sliders,
                _ => {}
            }
        }
        Self::default()
    }

    /// The line written for `self`.
    pub(crate) fn to_text(self) -> String {
        match self {
            Self::Sliders => "mode=sliders\n".to_string(),
            Self::Editor => "mode=editor\n".to_string(),
        }
    }
}

/// The saved choice, or sliders when nothing is saved (or the file cannot be read).
pub(crate) fn load() -> PluginUiMode {
    file().map_or_else(PluginUiMode::default, |f| {
        f.parent().map_or_else(PluginUiMode::default, load_from)
    })
}

/// Saves the choice, best effort. Tests run menu entries; they must not change settings.
pub(crate) fn save(mode: PluginUiMode) {
    if cfg!(test) {
        return;
    }
    let Some(file) = file() else { return };
    if let Some(dir) = file.parent() {
        save_to(dir, mode);
    }
}

/// Reads the choice from `dir`, the config folder used by [`save_to`].
pub(crate) fn load_from(dir: &Path) -> PluginUiMode {
    std::fs::read_to_string(dir.join("plugin-ui.txt")).map_or_else(
        |_| PluginUiMode::default(),
        |text| PluginUiMode::parse(&text),
    )
}

/// Writes `mode` to `plugin-ui.txt` in `dir`, best effort.
pub(crate) fn save_to(dir: &Path, mode: PluginUiMode) {
    let result = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(dir.join("plugin-ui.txt"), mode.to_text()));
    if let Err(e) = result {
        tracing::warn!("could not save the plug-in UI choice: {e}");
    }
}

/// `<config folder>/prowave/plugin-ui.txt`.
fn file() -> Option<PathBuf> {
    Some(crate::recent_files::config_dir()?.join("plugin-ui.txt"))
}

#[cfg(test)]
mod tests {
    use super::{load_from, save_to, PluginUiMode};

    #[test]
    fn sliders_are_the_default() {
        assert_eq!(PluginUiMode::default(), PluginUiMode::Sliders);
    }

    #[test]
    fn saved_mode_round_trips() {
        for mode in [PluginUiMode::Sliders, PluginUiMode::Editor] {
            assert_eq!(PluginUiMode::parse(&PluginUiMode::to_text(mode)), mode);
        }
    }

    #[test]
    fn unknown_or_empty_text_keeps_the_default() {
        assert_eq!(PluginUiMode::parse(""), PluginUiMode::Sliders);
        assert_eq!(
            PluginUiMode::parse("mode=nonsense\n"),
            PluginUiMode::Sliders
        );
    }

    #[test]
    fn load_from_and_save_to_use_the_config_directory() {
        let dir = std::env::temp_dir().join(format!(
            "prowave-plugin-ui-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        save_to(&dir, PluginUiMode::Editor);
        assert_eq!(load_from(&dir), PluginUiMode::Editor);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
