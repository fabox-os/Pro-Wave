//! Hydrogen drum kits as they are downloaded: a `.h2drumkit` file, which is a (usually gzip
//! compressed) tar archive holding one folder with `drumkit.xml` and the kit's samples.
//!
//! Pro Wave plays a kit from its `drumkit.xml` and the samples next to it, so an archive is
//! unpacked once into the program's own folder (`<config folder>/prowave/drumkits/<name>`) and
//! the kit is then used from there. Nothing outside that folder is ever written: names that
//! leave it, links and odd file types are skipped, and the size of an archive is capped,
//! because the file comes from the internet.
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// The most files and the most bytes one kit may unpack to.
const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 1 << 30;

/// Whether `path` is a Hydrogen kit archive, by its extension.
pub fn is_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("h2drumkit"))
}

/// Where unpacked kits live.
pub fn kits_dir() -> Option<PathBuf> {
    Some(crate::recent_files::config_dir()?.join("drumkits"))
}

/// Unpacks the archive into `<into>/<its name>` (unless that was done before) and returns the
/// path of the kit's `drumkit.xml`.
pub fn unpack(archive: &Path, into: &Path) -> Result<PathBuf, String> {
    let stem = archive
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && !s.contains(['/', '\\']))
        .ok_or_else(|| "the kit's file name is not usable".to_string())?;
    let dest = into.join(stem);
    if let Some(found) = find_kit(&dest) {
        return Ok(found);
    }
    let part = into.join(format!("{stem}.part"));
    let _ = std::fs::remove_dir_all(&part);
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let result = extract(archive, &part).and_then(|()| {
        find_kit(&part).ok_or_else(|| "the archive holds no drumkit.xml".to_string())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&part);
        return Err(format!("{}: {e}", archive.display()));
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    find_kit(&dest).ok_or_else(|| "the archive holds no drumkit.xml".to_string())
}

/// The `drumkit.xml` in `dir` or in one folder below it (where Hydrogen archives keep it).
fn find_kit(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("drumkit.xml");
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("drumkit.xml"))
        .find(|p| p.is_file())
}

fn extract(archive: &Path, dest: &Path) -> Result<(), String> {
    let mut file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 2];
    let gzip = file.read_exact(&mut magic).is_ok() && magic == [0x1f, 0x8b];
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    if gzip {
        unpack_tar(tar::Archive::new(flate2::read::GzDecoder::new(file)), dest)
    } else {
        unpack_tar(tar::Archive::new(file), dest)
    }
}

fn unpack_tar<R: Read>(mut tar: tar::Archive<R>, dest: &Path) -> Result<(), String> {
    let (mut files, mut bytes) = (0usize, 0u64);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            continue; // links, devices and the like are not part of a kit
        }
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        // Only plain names: no `..`, no root, no drive.
        let mut target = dest.to_path_buf();
        let mut any = false;
        for part in path.components() {
            match part {
                Component::Normal(name) => {
                    target.push(name);
                    any = true;
                }
                Component::CurDir => {}
                _ => {
                    return Err(format!(
                        "the archive has an unsafe name: {}",
                        path.display()
                    ))
                }
            }
        }
        if !any {
            continue;
        }
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        files += 1;
        bytes = bytes.saturating_add(entry.size());
        if files > MAX_FILES || bytes > MAX_BYTES {
            return Err("the archive is too large".into());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry.by_ref().take(MAX_BYTES), &mut out).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prowave-h2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A gzip tar with `files` (name, bytes); names are written as given, even unsafe ones.
    fn archive(path: &Path, files: &[(&str, &[u8])]) {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            let raw = header.as_old_mut();
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            tar.append(&header, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn a_kit_archive_is_unpacked_and_its_drumkit_xml_found() {
        let dir = temp("ok");
        let file = dir.join("Room Kit.h2drumkit");
        archive(
            &file,
            &[
                ("Room Kit/drumkit.xml", b"<drumkit_info/>"),
                ("Room Kit/kick.wav", b"RIFF"),
            ],
        );
        let into = dir.join("kits");
        let xml = unpack(&file, &into).unwrap();
        assert!(xml.ends_with("drumkit.xml") && xml.is_file());
        assert!(xml.with_file_name("kick.wav").is_file());
        // A second time uses what is there.
        assert_eq!(unpack(&file, &into).unwrap(), xml);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_that_leave_the_folder_are_refused() {
        let dir = temp("evil");
        let file = dir.join("evil.h2drumkit");
        archive(
            &file,
            &[("../outside.txt", b"x"), ("k/drumkit.xml", b"<a/>")],
        );
        let into = dir.join("kits");
        assert!(unpack(&file, &into).is_err());
        assert!(!dir.join("outside.txt").exists());
        assert!(!into.join("evil").exists(), "nothing half unpacked is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_without_a_kit_is_an_error() {
        let dir = temp("empty");
        let file = dir.join("none.h2drumkit");
        archive(&file, &[("a/readme.txt", b"hi")]);
        assert!(unpack(&file, &dir.join("kits")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_h2drumkit_files_are_archives() {
        assert!(is_archive(Path::new("/k/Room.h2drumkit")));
        assert!(is_archive(Path::new("/k/Room.H2DRUMKIT")));
        assert!(!is_archive(Path::new("/k/drumkit.xml")));
    }
}
