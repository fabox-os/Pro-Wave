//! GarageBand `.band` projects, read-only: what is inside one, before any import.
//!
//! A `.band` is a bundle (a folder) on macOS and a zip on iPad and iPhone. Two families exist
//! (see `GARAGEBAND-FORMAT-SPEC.md` in the repository): GarageBand 10.x, whose arrangement is
//! the binary `Alternatives/<n>/ProjectData` with the same chunk format as Logic Pro, and the
//! older and iOS form, whose arrangement is an NSKeyedArchiver archive inside an XML
//! `projectData`.
//!
//! This crate only looks: [`detect`] names the family, [`summary`] reads `MetaData.plist`
//! (tempo, time signature, sample rate) and lists the audio files. It never decodes media,
//! never writes anything, and treats the bundle as untrusted input.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The most `MetaData.plist` accepted, in bytes.
const MAX_PLIST_BYTES: u64 = 4 * 1024 * 1024;
/// The most entries (files and folders) a bundle may hold.
const MAX_ENTRIES: usize = 200_000;
/// The most bytes a zip may claim to unpack to (32 GiB).
const MAX_UNPACKED_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Which family a project belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// GarageBand 10.x: the arrangement is a binary `ProjectData` (Logic's chunk format).
    GarageBand10,
    /// GarageBand 6 and earlier, and GarageBand for iOS: an XML `projectData` archive.
    Legacy,
}

/// A GarageBand project found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bundle {
    /// A `.band` folder (the macOS form).
    Folder { root: PathBuf, family: Family },
    /// A `.band` zip (the iPad and iPhone export).
    Zip { path: PathBuf, family: Family },
}

impl Bundle {
    /// Which family the project belongs to.
    pub fn family(&self) -> Family {
        match self {
            Bundle::Folder { family, .. } | Bundle::Zip { family, .. } => *family,
        }
    }
}

/// What is inside a project: its name, its tempo and time signature, its media, and anything
/// the reader could not use.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// The project's name: the bundle's file name without `.band`.
    pub name: String,
    pub family: Family,
    /// Beats per minute, when `MetaData.plist` says it.
    pub tempo: Option<f64>,
    /// The time signature, when `MetaData.plist` says it.
    pub signature: Option<(u8, u8)>,
    /// The project's sample rate, when `MetaData.plist` says it.
    pub sample_rate: Option<u32>,
    /// The audio files, sorted by path.
    pub media: Vec<MediaFile>,
    /// Things worth telling the user (missing metadata, entries that were skipped).
    pub warnings: Vec<String>,
}

/// One audio file of the bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFile {
    /// Path below the bundle root, with `/` separators.
    pub path: String,
    pub bytes: u64,
}

/// Finds out what `path` is: a `.band` folder or zip, and which family it belongs to.
pub fn detect(path: &Path) -> Result<Bundle, String> {
    if path.is_dir() {
        let family = folder_family(path)?;
        return Ok(Bundle::Folder {
            root: path.to_path_buf(),
            family,
        });
    }
    if !path.is_file() {
        return Err(format!("{}: no such file or folder", path.display()));
    }
    let family = zip_family(path)?;
    Ok(Bundle::Zip {
        path: path.to_path_buf(),
        family,
    })
}

/// Reads what a project says about itself (see [`Summary`]). Never decodes media.
pub fn summary(bundle: &Bundle) -> Result<Summary, String> {
    match bundle {
        Bundle::Folder { root, family } => folder_summary(root, *family),
        Bundle::Zip { path, family } => zip_summary(path, *family),
    }
}

// --- Folders (the macOS form) -------------------------------------------------------------

fn folder_family(root: &Path) -> Result<Family, String> {
    if alternatives(root).is_some() {
        return Ok(Family::GarageBand10);
    }
    if root.join("projectData").is_file() {
        return Ok(Family::Legacy);
    }
    Err(format!(
        "{}: not a GarageBand project (no ProjectData or projectData)",
        root.display()
    ))
}

/// The first `Alternatives/<n>/ProjectData` folder of a GarageBand 10 bundle.
fn alternatives(root: &Path) -> Option<PathBuf> {
    let alternatives = root.join("Alternatives");
    let entries = std::fs::read_dir(&alternatives).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.is_dir() && dir.join("ProjectData").is_file() {
            return Some(dir);
        }
    }
    None
}

fn folder_summary(root: &Path, family: Family) -> Result<Summary, String> {
    let mut warnings = Vec::new();
    let (tempo, signature, sample_rate) = match family {
        Family::GarageBand10 => match metadata_of_folder(root) {
            Some(value) => metadata(&value),
            None => {
                warnings.push("no MetaData.plist: the tempo is unknown".to_string());
                (None, None, None)
            }
        },
        Family::Legacy => {
            warnings.push("the arrangement is an archive Pro Wave cannot read yet".to_string());
            (None, None, None)
        }
    };
    let media = media_of_folder(root, &mut warnings);
    Ok(Summary {
        name: bundle_name(root),
        family,
        tempo,
        signature,
        sample_rate,
        media,
        warnings,
    })
}

/// `MetaData.plist` of a GarageBand 10 bundle: in its alternative, or at the root.
fn metadata_of_folder(root: &Path) -> Option<plist::Value> {
    let mut candidates = Vec::new();
    if let Some(dir) = alternatives(root) {
        candidates.push(dir.join("MetaData.plist"));
    }
    candidates.push(root.join("MetaData.plist"));
    for path in candidates {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_PLIST_BYTES {
            continue;
        }
        if let Ok(value) = plist::Value::from_file(&path) {
            return Some(value);
        }
    }
    None
}

/// The files below `root/Media`, sorted by path.
fn media_of_folder(root: &Path, warnings: &mut Vec<String>) -> Vec<MediaFile> {
    let mut out = Vec::new();
    let media = root.join("Media");
    let mut stack = vec![media.clone()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                warnings.push(format!(
                    "more than {MAX_ENTRIES} entries: the rest is ignored"
                ));
                stack.clear();
                break;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_symlink() {
                warnings.push(format!("{}: a link, skipped", relative(&path, root)));
                continue;
            }
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            out.push(MediaFile {
                path: relative(&path, root),
                bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

// --- Zips (the iPad and iPhone export) ----------------------------------------------------

fn zip_family(path: &Path) -> Result<Family, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|_| format!("{}: not a GarageBand zip", path.display()))?;
    let mut legacy = false;
    let mut modern = false;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else {
            continue;
        };
        let Ok(name) = entry.name() else {
            continue;
        };
        let name = name.replace('\\', "/");
        if name == "projectData" {
            legacy = true;
        } else if is_alternative_project_data(&name) {
            modern = true;
        }
    }
    if modern {
        Ok(Family::GarageBand10)
    } else if legacy {
        Ok(Family::Legacy)
    } else {
        Err(format!(
            "{}: not a GarageBand project (no ProjectData or projectData)",
            path.display()
        ))
    }
}

/// Whether `name` is an `Alternatives/<n>/ProjectData` entry.
fn is_alternative_project_data(name: &str) -> bool {
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("Alternatives"), Some(_), Some("ProjectData"))
    ) && parts.next().is_none()
}

fn zip_summary(path: &Path, family: Family) -> Result<Summary, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    if zip.len() > MAX_ENTRIES {
        return Err(format!(
            "{}: more than {MAX_ENTRIES} entries in the zip",
            path.display()
        ));
    }
    let mut warnings = Vec::new();
    let mut media = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else {
            warnings.push(format!("entry {i}: unreadable, skipped"));
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Ok(name) = entry.name() else {
            warnings.push(format!("entry {i}: no name, skipped"));
            continue;
        };
        let name = name.replace('\\', "/");
        if !safe_name(&name) {
            warnings.push(format!("{name}: outside the project, skipped"));
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(format!("{}: the zip unpacks to too much", path.display()));
        }
        if name.starts_with("Media/") || name.starts_with("media/") {
            media.push(MediaFile {
                path: name,
                bytes: entry.size(),
            });
        }
    }
    media.sort_by(|a, b| a.path.cmp(&b.path));

    let (tempo, signature, sample_rate) = match family {
        Family::GarageBand10 => match metadata_of_zip(path) {
            Some(value) => metadata(&value),
            None => {
                warnings.push("no MetaData.plist: the tempo is unknown".to_string());
                (None, None, None)
            }
        },
        Family::Legacy => {
            warnings.push("the arrangement is an archive Pro Wave cannot read yet".to_string());
            (None, None, None)
        }
    };
    Ok(Summary {
        name: bundle_name(path),
        family,
        tempo,
        signature,
        sample_rate,
        media,
        warnings,
    })
}

/// `MetaData.plist` of a GarageBand 10 zip, read into memory.
fn metadata_of_zip(path: &Path) -> Option<plist::Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut candidates = Vec::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else {
            continue;
        };
        let Ok(name) = entry.name() else {
            continue;
        };
        let name = name.replace('\\', "/");
        if is_alternative_metadata(&name) {
            candidates.push(name);
        }
    }
    candidates.sort();
    for name in candidates {
        let Ok(mut entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_PLIST_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if entry
            .by_ref()
            .take(MAX_PLIST_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(bytes)) {
            return Some(value);
        }
    }
    None
}

/// Whether `name` is an `Alternatives/<n>/MetaData.plist` entry.
fn is_alternative_metadata(name: &str) -> bool {
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("Alternatives"), Some(_), Some("MetaData.plist"))
    ) && parts.next().is_none()
}

/// Whether a zip entry's name stays inside the project.
fn safe_name(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && !name.starts_with('/')
}

// --- Shared -------------------------------------------------------------------------------

/// The project name: the bundle's file name without its `.band` (and `.zip`) extensions.
fn bundle_name(path: &Path) -> String {
    let mut name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    for suffix in [".zip", ".band"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            name = stem.to_string();
        }
    }
    if name.is_empty() {
        "GarageBand Project".to_string()
    } else {
        name
    }
}

/// `path` below `root`, with `/` separators.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Tempo, time signature and sample rate from `MetaData.plist`, as far as it says them.
fn metadata(value: &plist::Value) -> (Option<f64>, Option<(u8, u8)>, Option<u32>) {
    let mut dicts: Vec<&plist::Dictionary> = Vec::new();
    if let Some(root) = value.as_dictionary() {
        dicts.push(root);
        for key in ["Song", "song", "Project", "project"] {
            if let Some(inner) = root.get(key).and_then(plist::Value::as_dictionary) {
                dicts.push(inner);
            }
        }
    }
    let mut tempo = None;
    let mut signature = None;
    let mut sample_rate = None;
    for dict in dicts {
        if tempo.is_none() {
            tempo = entry(dict, "tempo")
                .and_then(number)
                .filter(|bpm| (20.0..=400.0).contains(bpm));
        }
        if signature.is_none() {
            signature = entry(dict, "timeSignature").and_then(time_signature);
        }
        if sample_rate.is_none() {
            sample_rate = entry(dict, "sampleRate")
                .and_then(number)
                .filter(|rate| (8_000.0..=384_000.0).contains(rate))
                .map(|rate| rate as u32);
        }
    }
    (tempo, signature, sample_rate)
}

/// A key of `dict`, ignoring case (`MetaData.plist` has used both spellings).
fn entry<'a>(dict: &'a plist::Dictionary, key: &str) -> Option<&'a plist::Value> {
    if let Some(value) = dict.get(key) {
        return Some(value);
    }
    dict.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

/// A number as a real (plists store integers and reals differently).
fn number(value: &plist::Value) -> Option<f64> {
    match value {
        plist::Value::Real(f) => f.is_finite().then_some(*f),
        plist::Value::Integer(i) => i
            .as_signed()
            .map(|n| n as f64)
            .or_else(|| i.as_unsigned().map(|n| n as f64)),
        _ => None,
    }
}

/// A time signature as `"6/8"` or the array `[6, 8]`.
fn time_signature(value: &plist::Value) -> Option<(u8, u8)> {
    let pair = match value {
        plist::Value::String(text) => {
            let (num, den) = text.split_once('/')?;
            (
                num.trim().parse::<u8>().ok()?,
                den.trim().parse::<u8>().ok()?,
            )
        }
        plist::Value::Array(items) => {
            let num = items.first().and_then(number)?;
            let den = items.get(1).and_then(number)?;
            (num as u8, den as u8)
        }
        _ => return None,
    };
    let (num, den) = pair;
    ((1..=64).contains(&num) && (1..=64).contains(&den) && den.is_power_of_two()).then_some(pair)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    /// An XML `MetaData.plist` with the three values the summary reads.
    fn write_metadata(path: &Path, tempo: &str, signature: &str, sample_rate: &str) {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>tempo</key>{tempo}
<key>timeSignature</key>{signature}
<key>sampleRate</key>{sample_rate}
</dict></plist>"#
        );
        std::fs::write(path, xml).unwrap();
    }

    /// A GarageBand 10 folder named `My Song.band` with one audio file.
    fn garageband_10(dir: &Path) -> PathBuf {
        let band = dir.join("My Song.band");
        let alternative = band.join("Alternatives").join("000");
        std::fs::create_dir_all(&alternative).unwrap();
        std::fs::write(alternative.join("ProjectData"), [0x23, 0x47, 0xC0, 0xAB]).unwrap();
        write_metadata(
            &alternative.join("MetaData.plist"),
            "<real>120.0</real>",
            "<string>6/8</string>",
            "<integer>44100</integer>",
        );
        let media = band.join("Media").join("Audio Files");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("Take 1.wav"), [0u8; 10]).unwrap();
        band
    }

    /// Writes a zip with `entries` (name, bytes).
    fn zip_at(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    const LEGACY_PROJECT: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>archive</key><data>AAECAwQ=</data></dict></plist>"#;

    #[test]
    fn a_garageband_10_folder_gives_its_name_tempo_and_media() {
        let dir = tempfile::tempdir().unwrap();
        let band = garageband_10(dir.path());
        let bundle = detect(&band).unwrap();
        assert_eq!(bundle.family(), Family::GarageBand10);
        let s = summary(&bundle).unwrap();
        assert_eq!(s.name, "My Song");
        assert_eq!(s.tempo, Some(120.0));
        assert_eq!(s.signature, Some((6, 8)));
        assert_eq!(s.sample_rate, Some(44_100));
        assert_eq!(
            s.media,
            [MediaFile {
                path: "Media/Audio Files/Take 1.wav".to_string(),
                bytes: 10,
            }]
        );
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn a_garageband_10_zip_reads_its_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A Song.band");
        zip_at(
            &path,
            &[
                ("Alternatives/000/ProjectData", &[0x23, 0x47, 0xC0, 0xAB]),
                ("Alternatives/000/MetaData.plist", br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Tempo</key><integer>96</integer>
<key>TimeSignature</key><array><integer>3</integer><integer>4</integer></array></dict></plist>"#),
                ("Media/Audio Files/a.caf", &[0u8; 3]),
            ],
        );
        let bundle = detect(&path).unwrap();
        assert_eq!(bundle.family(), Family::GarageBand10);
        let s = summary(&bundle).unwrap();
        assert_eq!(s.name, "A Song");
        assert_eq!(s.tempo, Some(96.0));
        assert_eq!(s.signature, Some((3, 4)));
        assert_eq!(s.sample_rate, None);
        assert_eq!(s.media.len(), 1);
        assert_eq!(s.media[0].path, "Media/Audio Files/a.caf");
    }

    #[test]
    fn an_ios_zip_is_a_legacy_project_and_its_media_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iOS Song.band.zip");
        zip_at(
            &path,
            &[
                ("projectData", LEGACY_PROJECT),
                ("Media/Take.aif", &[0u8; 5]),
            ],
        );
        let bundle = detect(&path).unwrap();
        assert_eq!(bundle.family(), Family::Legacy);
        let s = summary(&bundle).unwrap();
        assert_eq!(s.name, "iOS Song");
        assert_eq!(s.tempo, None);
        assert_eq!(s.media.len(), 1);
        assert_eq!(s.media[0].path, "Media/Take.aif");
        assert!(
            s.warnings.iter().any(|w| w.contains("archive")),
            "{:?}",
            s.warnings
        );
    }

    #[test]
    fn a_legacy_folder_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let band = dir.path().join("Old.band");
        std::fs::create_dir_all(band.join("Media")).unwrap();
        std::fs::write(band.join("projectData"), LEGACY_PROJECT).unwrap();
        std::fs::write(band.join("Media").join("a.aif"), [0u8; 2]).unwrap();
        let bundle = detect(&band).unwrap();
        assert_eq!(bundle.family(), Family::Legacy);
        let s = summary(&bundle).unwrap();
        assert_eq!(s.name, "Old");
        assert_eq!(s.media.len(), 1);
    }

    #[test]
    fn zip_entries_outside_the_project_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Evil.band");
        zip_at(
            &path,
            &[
                ("projectData", LEGACY_PROJECT),
                ("Media/ok.wav", &[0u8; 1]),
                ("../evil.wav", &[0u8; 1]),
                ("/absolute.wav", &[0u8; 1]),
            ],
        );
        let s = summary(&detect(&path).unwrap()).unwrap();
        let names: Vec<&str> = s.media.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(names, ["Media/ok.wav"]);
        assert!(
            s.warnings.iter().filter(|w| w.contains("outside")).count() >= 2,
            "{:?}",
            s.warnings
        );
    }

    #[test]
    fn a_folder_without_project_data_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = detect(dir.path()).unwrap_err();
        assert!(error.contains("not a GarageBand project"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(detect(&path).is_err());
    }

    #[test]
    fn missing_or_odd_metadata_is_reported_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let band = dir.path().join("No Metadata.band");
        let alternative = band.join("Alternatives").join("000");
        std::fs::create_dir_all(&alternative).unwrap();
        std::fs::write(alternative.join("ProjectData"), [0u8; 4]).unwrap();
        let s = summary(&detect(&band).unwrap()).unwrap();
        assert_eq!((s.tempo, s.signature, s.sample_rate), (None, None, None));
        assert!(
            s.warnings.iter().any(|w| w.contains("MetaData")),
            "{:?}",
            s.warnings
        );

        // Wild values are refused rather than shown.
        write_metadata(
            &alternative.join("MetaData.plist"),
            "<real>1e9</real>",
            "<string>7/5</string>",
            "<integer>1</integer>",
        );
        let s = summary(&detect(&band).unwrap()).unwrap();
        assert_eq!((s.tempo, s.signature, s.sample_rate), (None, None, None));
    }
}
