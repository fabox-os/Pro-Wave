//! The audio output the user chose in Project Settings > Audio (Logic Pro: Settings > Audio):
//! the output device and the I/O buffer size. Kept in `audio.txt`, next to the recent
//! projects, so the next start opens the same output.
use std::path::PathBuf;

/// The smallest and largest buffer size a saved preference may ask for, in frames.
pub const MIN_BUFFER: u32 = 16;
pub const MAX_BUFFER: u32 = 8192;

/// What the user asked for. `None` means the system's default output device, and the device's
/// own default buffer size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioPrefs {
    /// The output device's id (see `prowave_engine::OutputDevice::id`).
    pub device: Option<String>,
    /// The I/O buffer size in frames.
    pub buffer: Option<u32>,
}

impl AudioPrefs {
    /// The saved choice, or the defaults when nothing is saved (or the file cannot be read).
    pub fn load() -> Self {
        let text = file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    /// Best effort: a choice that cannot be saved is not worth interrupting the user for.
    pub fn save(&self) {
        let Some(file) = file() else { return };
        let result = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file, self.to_text()));
        if let Err(e) = result {
            tracing::warn!("could not save the audio output choice: {e}");
        }
    }

    /// Reads `device=<id>` and `buffer=<frames>` lines. Other lines, and a number that is not
    /// one, are ignored; a buffer size is kept within [`MIN_BUFFER`] and [`MAX_BUFFER`].
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(id) = line.strip_prefix("device=") {
                if !id.is_empty() {
                    prefs.device = Some(id.to_string());
                }
            } else if let Some(n) = line.strip_prefix("buffer=") {
                if let Ok(n) = n.trim().parse::<u32>() {
                    prefs.buffer = Some(n.clamp(MIN_BUFFER, MAX_BUFFER));
                }
            }
        }
        prefs
    }

    /// The file's text: one line per setting that is not the default.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        if let Some(id) = self
            .device
            .as_deref()
            .filter(|id| !id.contains(['\n', '\r']))
        {
            text.push_str(&format!("device={id}\n"));
        }
        if let Some(n) = self.buffer {
            text.push_str(&format!("buffer={n}\n"));
        }
        text
    }

    /// The same choice with another output device. The buffer size is kept when `device` can
    /// take it (`range` is the device's buffer range, `None` when it does not say), else it
    /// goes back to the device's default.
    pub fn with_device(&self, device: Option<String>, range: Option<(u32, u32)>) -> Self {
        let buffer = self
            .buffer
            .filter(|n| prowave_engine::buffer_choices(range).contains(n));
        Self { device, buffer }
    }

    /// The same output with another buffer size (`None`: the device's default).
    pub fn with_buffer(&self, buffer: Option<u32>) -> Self {
        Self {
            device: self.device.clone(),
            buffer,
        }
    }
}

/// `<config folder>/prowave/audio.txt`.
fn file() -> Option<PathBuf> {
    Some(crate::recent_files::config_dir()?.join("audio.txt"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn prefs(device: Option<&str>, buffer: Option<u32>) -> AudioPrefs {
        AudioPrefs {
            device: device.map(str::to_string),
            buffer,
        }
    }

    #[test]
    fn a_saved_choice_reads_back_the_same() {
        for p in [
            prefs(None, None),
            prefs(Some("coreaudio:BuiltInSpeakers"), None),
            prefs(None, Some(128)),
            prefs(Some("wasapi:{0.0.0.00000000}.{abc}|x=y"), Some(32)),
        ] {
            assert_eq!(AudioPrefs::parse(&p.to_text()), p);
        }
    }

    #[test]
    fn junk_lines_and_bad_numbers_are_ignored() {
        let text = "hello\n\
                    device=Speakers\n\
                    buffer=abc\n\
                    buffer=\n\
                    colour=blue\n\
                    =nothing\n\
                    device=\n";
        assert_eq!(AudioPrefs::parse(text), prefs(Some("Speakers"), None));
        assert_eq!(AudioPrefs::parse(""), prefs(None, None));
    }

    #[test]
    fn the_buffer_is_kept_within_the_saved_limits() {
        assert_eq!(AudioPrefs::parse("buffer=1").buffer, Some(MIN_BUFFER));
        assert_eq!(AudioPrefs::parse("buffer=999999").buffer, Some(MAX_BUFFER));
        assert_eq!(AudioPrefs::parse("buffer=99999999999999").buffer, None);
        assert_eq!(AudioPrefs::parse("buffer=64\r\n").buffer, Some(64));
    }

    #[test]
    fn a_new_device_keeps_the_buffer_only_when_it_supports_it() {
        let p = prefs(None, Some(256));
        assert_eq!(
            p.with_device(Some("b".into()), Some((32, 1024))).buffer,
            Some(256)
        );
        assert_eq!(
            p.with_device(Some("b".into()), Some((32, 128))).buffer,
            None,
            "256 is not in 32..=128"
        );
        assert_eq!(p.with_device(None, None).buffer, Some(256), "unknown range");
    }

    #[test]
    fn a_new_buffer_keeps_the_device() {
        let p = prefs(Some("a"), None).with_buffer(Some(512));
        assert_eq!(p, prefs(Some("a"), Some(512)));
        assert_eq!(p.with_buffer(None), prefs(Some("a"), None));
    }
}
