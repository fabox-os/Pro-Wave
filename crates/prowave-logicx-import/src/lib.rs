//! Reading Logic Pro `.logicx` project bundles: the bundle layout, the metadata plists and the
//! ProjectData envelope.
//!
//! A `.logicx` is a macOS bundle (a directory) that Logic can save as a package or inside a
//! plain project folder. It holds one or more alternatives under `Alternatives/NNN/`; each has
//! a `MetaData.plist` with the tempo, key, time signature, sample rate and channel count, and
//! the undocumented `ProjectData` binary. `Resources/ProjectInformation.plist` names the active
//! alternative and the Logic version. This reader resolves both save shapes, follows the active
//! alternative, and reads the two plists.
//!
//! `ProjectData` is a chunk-based binary with mixed endianness and no index; reading the
//! musical content (tracks, regions, plug-ins and Session Player state) is the next step.
//! For now the reader validates its four-byte signature (`23 47 C0 AB`) and size, and reports
//! the size so an importer can budget for the scan. The full layout is described in
//! `LOGICX-FORMAT-SPEC.md`.
//!
//! A project is untrusted input: both plists and `ProjectData` are capped before they are read,
//! and every file is canonicalised and must stay inside the bundle, so a symlink in a downloaded
//! project cannot point the reader at another file.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use plist::{Dictionary, Value};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest plist the reader accepts (8 MiB). Logic's own are kilobytes.
pub const MAX_PLIST_BYTES: usize = 8 * 1024 * 1024;

/// The largest `ProjectData` the reader accepts (64 MiB). Real projects are 1-50 MB.
pub const MAX_PROJECT_DATA_BYTES: usize = 64 * 1024 * 1024;

/// The first four bytes of a Logic `ProjectData` file.
const PROJECT_DATA_MAGIC: [u8; 4] = [0x23, 0x47, 0xC0, 0xAB];

/// Logic's tempo when the plist does not name one.
const DEFAULT_TEMPO: f64 = 120.0;

/// What a `.logicx` bundle says about its project.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    /// The bundle name without the `.logicx` extension.
    pub name: String,
    /// Quarter notes per minute (120 when `MetaData.plist` does not say).
    pub tempo: f64,
    /// The key signature as Logic writes it (for example `C Major`), when present.
    pub key_signature: Option<String>,
    /// The time signature as Logic writes it (for example `4/4`), when present.
    pub time_signature: Option<String>,
    /// Sample rate in Hz, when present.
    pub sample_rate: Option<f64>,
    /// Output channel count, when present.
    pub channels: Option<u32>,
    /// The active alternative's number (`Alternatives/NNN`).
    pub alternative: u32,
    /// The Logic version that wrote the project, when present.
    pub logic_version: Option<String>,
    /// The size of the active alternative's `ProjectData` file.
    pub project_data_bytes: u64,
}

/// Why a project could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The path is not a `.logicx` bundle, nor a folder holding exactly one.
    NotABundle,
    /// A file the layout requires is absent.
    Missing { path: String },
    /// A file is larger than the reader accepts.
    TooLarge { path: String, limit: usize },
    /// A plist could not be parsed, or is not a dictionary.
    Plist { path: String, message: String },
    /// `ProjectData` does not begin with Logic's signature.
    BadProjectData,
    /// A file the layout names resolves outside the bundle (through a symlink).
    OutsideBundle { path: String },
    /// The file system returned an error.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotABundle => f.write_str("the path is not a Logic project bundle"),
            Self::Missing { path } => write!(f, "missing {path}"),
            Self::TooLarge { path, limit } => write!(f, "{path} is larger than {limit} bytes"),
            Self::Plist { path, message } => write!(f, "{path}: {message}"),
            Self::BadProjectData => f.write_str("ProjectData does not have Logic's signature"),
            Self::OutsideBundle { path } => write!(f, "{path} resolves outside the bundle"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads a Logic project from a `.logicx` package, or from a plain folder holding one.
pub fn read(path: &Path) -> Result<Project, Error> {
    let bundle = locate_bundle(path)?;
    let name = bundle
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let (alternative, logic_version) = project_information(&bundle)?;
    let alternative_dir = bundle
        .join("Alternatives")
        .join(format!("{alternative:03}"));
    let metadata = read_plist(&bundle, &alternative_dir.join("MetaData.plist"))?;
    let project_data_bytes = check_project_data(&bundle, &alternative_dir.join("ProjectData"))?;
    Ok(Project {
        name,
        tempo: number(&metadata, "tempo").unwrap_or(DEFAULT_TEMPO),
        key_signature: string(&metadata, "keySignature"),
        time_signature: string(&metadata, "timeSignature"),
        sample_rate: number(&metadata, "sampleRate"),
        channels: integer(&metadata, "channelCount"),
        alternative,
        logic_version,
        project_data_bytes,
    })
}

/// The bundle directory: either the path itself, or the single `.logicx` inside a folder.
fn locate_bundle(path: &Path) -> Result<PathBuf, Error> {
    fn has_alternatives(path: &Path) -> bool {
        path.join("Alternatives").is_dir()
    }
    if !path.is_dir() {
        return Err(Error::NotABundle);
    }
    if has_alternatives(path) {
        return Ok(path.to_path_buf());
    }
    let entries =
        std::fs::read_dir(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    let mut found = None;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        let candidate = entry.path();
        if candidate.is_dir()
            && candidate.extension().and_then(|e| e.to_str()) == Some("logicx")
            && has_alternatives(&candidate)
        {
            if found.is_some() {
                // More than one bundle: the folder does not name one project.
                return Err(Error::NotABundle);
            }
            found = Some(candidate);
        }
    }
    found.ok_or(Error::NotABundle)
}

/// The active alternative and the Logic version, from `Resources/ProjectInformation.plist`.
fn project_information(bundle: &Path) -> Result<(u32, Option<String>), Error> {
    let path = bundle.join("Resources/ProjectInformation.plist");
    if !path.exists() {
        return Ok((0, None));
    }
    let info = read_plist(bundle, &path)?;
    Ok((
        integer(&info, "activeAlternative").unwrap_or(0),
        string(&info, "logicVersion"),
    ))
}

/// Reads a plist inside the bundle, with the size cap applied before the file is read.
fn read_plist(bundle: &Path, path: &Path) -> Result<Dictionary, Error> {
    let shown = path.display().to_string();
    if !path.exists() {
        return Err(Error::Missing { path: shown });
    }
    ensure_inside(bundle, path)?;
    let metadata = std::fs::metadata(path).map_err(|e| Error::Io(format!("{shown}: {e}")))?;
    if metadata.len() > MAX_PLIST_BYTES as u64 {
        return Err(Error::TooLarge {
            path: shown,
            limit: MAX_PLIST_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(format!("{shown}: {e}")))?;
    let value: Value = plist::from_bytes(&bytes).map_err(|e| Error::Plist {
        path: shown.clone(),
        message: e.to_string(),
    })?;
    match value {
        Value::Dictionary(dictionary) => Ok(dictionary),
        _ => Err(Error::Plist {
            path: shown,
            message: "the plist is not a dictionary".to_string(),
        }),
    }
}

/// Checks the `ProjectData` envelope: inside the bundle, within the cap, and with the magic.
fn check_project_data(bundle: &Path, path: &Path) -> Result<u64, Error> {
    let shown = path.display().to_string();
    if !path.exists() {
        return Err(Error::Missing { path: shown });
    }
    ensure_inside(bundle, path)?;
    let metadata = std::fs::metadata(path).map_err(|e| Error::Io(format!("{shown}: {e}")))?;
    if metadata.len() > MAX_PROJECT_DATA_BYTES as u64 {
        return Err(Error::TooLarge {
            path: shown,
            limit: MAX_PROJECT_DATA_BYTES,
        });
    }
    if metadata.len() < PROJECT_DATA_MAGIC.len() as u64 {
        return Err(Error::BadProjectData);
    }
    let mut magic = [0u8; PROJECT_DATA_MAGIC.len()];
    let mut file = std::fs::File::open(path).map_err(|e| Error::Io(format!("{shown}: {e}")))?;
    file.read_exact(&mut magic)
        .map_err(|e| Error::Io(format!("{shown}: {e}")))?;
    if magic != PROJECT_DATA_MAGIC {
        return Err(Error::BadProjectData);
    }
    Ok(metadata.len())
}

/// Canonicalises both paths and requires the file to stay inside the bundle.
fn ensure_inside(bundle: &Path, path: &Path) -> Result<(), Error> {
    let root = std::fs::canonicalize(bundle)
        .map_err(|e| Error::Io(format!("{}: {e}", bundle.display())))?;
    let target =
        std::fs::canonicalize(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    if !target.starts_with(&root) {
        return Err(Error::OutsideBundle {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

/// A plist value by key, ignoring case (Apple's keys are not consistent between versions).
fn lookup<'a>(dictionary: &'a Dictionary, key: &str) -> Option<&'a Value> {
    dictionary
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

/// A number that Logic may store as a real, an integer or a string.
fn number(dictionary: &Dictionary, key: &str) -> Option<f64> {
    match lookup(dictionary, key)? {
        Value::Real(value) => Some(*value),
        Value::Integer(value) => value
            .as_signed()
            .map(|value| value as f64)
            .or_else(|| value.as_unsigned().map(|value| value as f64)),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn integer(dictionary: &Dictionary, key: &str) -> Option<u32> {
    let value = number(dictionary, key)?;
    (0.0..=u32::MAX as f64)
        .contains(&value)
        .then_some(value.round() as u32)
}

fn string(dictionary: &Dictionary, key: &str) -> Option<String> {
    match lookup(dictionary, key)? {
        Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_accept_reals_integers_and_strings() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("a".into(), Value::Real(96.5));
        dictionary.insert("b".into(), Value::Integer(44_100i64.into()));
        dictionary.insert("c".into(), Value::String("48000".into()));
        dictionary.insert("d".into(), Value::String("not a number".into()));
        assert_eq!(number(&dictionary, "a"), Some(96.5));
        assert_eq!(number(&dictionary, "b"), Some(44_100.0));
        assert_eq!(number(&dictionary, "c"), Some(48_000.0));
        assert_eq!(number(&dictionary, "d"), None);
        assert_eq!(integer(&dictionary, "B"), Some(44_100));
    }

    #[test]
    fn keys_are_matched_without_case() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Template".into(), Value::String("Song".into()));
        assert_eq!(string(&dictionary, "template").as_deref(), Some("Song"));
    }
}
