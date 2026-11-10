//! Compressed MusicXML (`.mxl`) bundles: the ZIP wrapper around a MusicXML score.
//!
//! An `.mxl` is a ZIP archive. `META-INF/container.xml` names the root score with a
//! `<rootfile full-path="…">` element; files without a container fall back to the first `.xml`
//! entry outside `META-INF`. [`read`] returns the score XML as text, ready for
//! `prowave-musicxml::parse`.
//!
//! The archive is untrusted input: entry count and uncompressed sizes are capped, and entries
//! are read through a bounded reader. MusicXML in UTF-16 is refused (the text must be UTF-8);
//! converting it is a follow-up.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Read};
use std::path::Path;

use quick_xml::{Reader, XmlVersion};
use zip::ZipArchive;

/// The most entries an `.mxl` may have.
const MAX_ENTRIES: usize = 4096;
/// The most bytes `META-INF/container.xml` may hold.
const MAX_CONTAINER: u64 = 1 << 20;
/// The most bytes the score XML may hold (32 MiB).
const MAX_XML: u64 = 32 << 20;

/// Why a bundle could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Zip(String),
    Io(String),
    TooLarge,
    NoRoot,
    NotUtf8,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Zip(message) => write!(f, "not a valid .mxl archive: {message}"),
            Error::Io(message) => write!(f, "cannot read the .mxl entry: {message}"),
            Error::TooLarge => f.write_str("the .mxl archive is larger than the reader accepts"),
            Error::NoRoot => f.write_str("the .mxl archive names no MusicXML score"),
            Error::NotUtf8 => f.write_str("the MusicXML score is not UTF-8 text"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads the MusicXML score from an `.mxl` archive.
pub fn read(bytes: &[u8]) -> Result<String, Error> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|error| Error::Zip(error.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::TooLarge);
    }
    let root = match container_root(&mut archive)? {
        Some(root) => root,
        None => first_xml(&mut archive)?,
    };
    let entry = archive
        .by_name(&root)
        .map_err(|error| Error::Zip(format!("{root}: {error}")))?;
    if entry.size() > MAX_XML {
        return Err(Error::TooLarge);
    }
    let mut xml = String::new();
    entry
        .take(MAX_XML + 1)
        .read_to_string(&mut xml)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                Error::NotUtf8
            } else {
                Error::Io(error.to_string())
            }
        })?;
    if xml.len() as u64 > MAX_XML {
        return Err(Error::TooLarge);
    }
    Ok(xml)
}

/// Reads the MusicXML score from an `.mxl` file on disk.
pub fn read_path(path: impl AsRef<Path>) -> Result<String, Error> {
    read(&std::fs::read(path).map_err(|error| Error::Io(error.to_string()))?)
}

/// The `rootfile full-path` in `META-INF/container.xml`, if the container exists.
fn container_root(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Option<String>, Error> {
    let entry = match archive.by_name("META-INF/container.xml") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(Error::Zip(error.to_string())),
    };
    if entry.size() > MAX_CONTAINER {
        return Err(Error::TooLarge);
    }
    let mut text = String::new();
    entry
        .take(MAX_CONTAINER + 1)
        .read_to_string(&mut text)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                Error::NotUtf8
            } else {
                Error::Io(error.to_string())
            }
        })?;
    Ok(rootfile(&text))
}

/// The first `rootfile full-path` attribute in a container document.
fn rootfile(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(
                quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element),
            ) => {
                let name = element.name();
                let local = name
                    .as_ref()
                    .rsplit_once(':')
                    .map_or(name.as_ref(), |(_, local)| local);
                if local != "rootfile" {
                    continue;
                }
                for attribute in element.attributes().flatten() {
                    if attribute.key.as_ref() == "full-path" {
                        return attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .ok()
                            .map(|value| value.into_owned());
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// The first XML entry outside `META-INF`, for archives without a container.
fn first_xml(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<String, Error> {
    let names: Vec<String> = archive
        .file_names()
        .filter_map(Result::ok)
        .filter(|name| {
            !name.starts_with("META-INF/")
                && !name.ends_with('/')
                && name.to_ascii_lowercase().ends_with(".xml")
        })
        .map(|name| name.into_owned())
        .collect();
    names.into_iter().next().ok_or(Error::NoRoot)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CONTAINER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="score.xml" media-type="application/vnd.recordare.musicxml+xml"/>
  </rootfiles>
</container>"#;

    #[test]
    fn finds_the_rootfile_in_the_container() {
        assert_eq!(rootfile(CONTAINER).as_deref(), Some("score.xml"));
        assert_eq!(
            rootfile("<m:container xmlns:m=\"urn:test\"><m:rootfiles><m:rootfile full-path=\"music/etude.xml\"/></m:rootfiles></m:container>").as_deref(),
            Some("music/etude.xml")
        );
    }
}
