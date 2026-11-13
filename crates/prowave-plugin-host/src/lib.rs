//! Hosted VST3, Audio Unit and CLAP plug-ins for Pro Wave.
//!
//! Plug-in code never runs in the DAW's process: each plug-in chain lives in a helper process
//! ([`helper_path`]), so a crashed or hung plug-in cannot take the app with it. The app scans
//! once, keeps the resulting [`Catalog`], and asks it to [`Catalog::instantiate`] a plug-in. The
//! returned [`Hosted`] has a control side (parameters, state, editor) that stays on the UI
//! thread and a realtime [`prowave_engine::HostedEffect`] that moves into the audio graph.
//!
//! Hosting is available on macOS 15+ (Apple Silicon) and Windows x64. Elsewhere the same API
//! exists but every operation reports [`Error`] `unsupported`, so the rest of the app (and its
//! CI builds for Linux) does not need special cases.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::fmt;
use std::path::PathBuf;

#[cfg(plugin_host)]
mod supported;
#[cfg(plugin_host)]
pub use supported::{available, scan, Catalog, Control, Hosted, HostedInstrument};
#[cfg(not(plugin_host))]
mod unsupported;
#[cfg(not(plugin_host))]
pub use unsupported::{available, scan, Catalog, Control, Hosted, HostedInstrument};

/// The block size hosted effects exchange with their helper, in samples. The engine adapter
/// adds one such block of latency.
pub const DEFAULT_QUANTUM: usize = 128;

/// The plug-in formats this host understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Vst3,
    AudioUnit,
    Clap,
}

impl Format {
    /// The name shown in the plug-in browser.
    pub fn label(self) -> &'static str {
        match self {
            Format::Vst3 => "VST3",
            Format::AudioUnit => "Audio Unit",
            Format::Clap => "CLAP",
        }
    }

    /// The short badge.
    pub fn badge(self) -> &'static str {
        match self {
            Format::Vst3 => "VST3",
            Format::AudioUnit => "AU",
            Format::Clap => "CLAP",
        }
    }

    /// The stable prefix of a [`PluginDescriptor::key`].
    pub fn key(self) -> &'static str {
        match self {
            Format::Vst3 => "vst3",
            Format::AudioUnit => "au",
            Format::Clap => "clap",
        }
    }

    /// Every format, in the order the browser lists them.
    pub const ALL: [Format; 3] = [Format::Vst3, Format::AudioUnit, Format::Clap];
}

/// An effect or an instrument, as the format classifies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Effect,
    Instrument,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Effect => "Effect",
            Kind::Instrument => "Instrument",
        }
    }
}

/// One installed plug-in class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDescriptor {
    /// Stable key: the format and the class ID (`"vst3:…"`). Saved with a project.
    pub key: String,
    pub format: Format,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub kind: Kind,
    pub categories: Vec<String>,
    /// The bundle on disk, when the format has one (Audio Units live in the system registry).
    pub path: Option<String>,
}

/// One plug-in parameter, with its current value when the plug-in reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// The format's parameter ID (VST3/CLAP ID, Audio Unit address).
    pub id: u64,
    pub title: String,
    pub units: String,
    /// Zero for a continuous parameter; otherwise the number of intervals.
    pub steps: u32,
    pub discrete: bool,
    /// The native default, normalized to 0..=1.
    pub default: Option<f64>,
    /// The current value, normalized to 0..=1.
    pub value: Option<f64>,
}

/// Something the host could not do, with the native reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// A platform with no plug-in hosting.
    pub fn unsupported() -> Self {
        Self(format!(
            "plug-in hosting needs macOS on Apple Silicon or Windows x64; this build is for {}",
            std::env::consts::OS
        ))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Where the helper executable that runs plug-ins lives.
///
/// Checked in order: `PROWAVE_PLUGIN_HELPER`, the app binary's folder (and its `Helpers`
/// subfolder on macOS), and the parent of a test binary's `deps` folder.
pub fn helper_path() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "prowave-plugin-helper.exe"
    } else {
        "prowave-plugin-helper"
    };
    if let Some(path) = std::env::var_os("PROWAVE_PLUGIN_HELPER").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidates = [
        dir.join(name),
        dir.join("Helpers").join(name),
        dir.parent().map_or_else(PathBuf::new, |p| p.join(name)),
        dir.parent()
            .and_then(|p| p.parent())
            .map_or_else(PathBuf::new, |p| p.join(name)),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

/// The JSON scan cache the helper keeps beside the user's Pro Wave settings.
pub fn default_cache_path() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join("Pro Wave")
        .join("plugins.json")
}
