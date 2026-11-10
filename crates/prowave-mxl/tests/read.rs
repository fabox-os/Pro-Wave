//! Built in-memory .mxl bundles: container root, XML fallback, and damaged archives.

use std::io::{Cursor, Write};

use prowave_mxl::{read, Error};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const CONTAINER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0"><rootfiles><rootfile full-path="score.xml" media-type="application/vnd.recordare.musicxml+xml"/></rootfiles></container>"#;
const SCORE: &str = r#"<score-partwise><part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list></score-partwise>"#;

fn bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(data).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

#[test]
fn reads_the_score_named_by_the_container() {
    let bytes = bundle(&[
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("score.xml", SCORE.as_bytes()),
        ("other.xml", b"<not-the-score/>"),
    ]);
    assert_eq!(read(&bytes).unwrap(), SCORE);
}

#[test]
fn falls_back_to_the_first_xml_without_a_container() {
    let bytes = bundle(&[("music/etude.xml", SCORE.as_bytes())]);
    assert_eq!(read(&bytes).unwrap(), SCORE);
}

#[test]
fn refuses_missing_roots_bad_archives_and_bad_text() {
    let missing = bundle(&[
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("elsewhere.xml", SCORE.as_bytes()),
    ]);
    assert!(matches!(read(&missing), Err(Error::Zip(_) | Error::NoRoot)));

    let no_xml = bundle(&[("readme.txt", b"hello")]);
    assert_eq!(read(&no_xml), Err(Error::NoRoot));

    assert!(read(b"this is not a zip archive").is_err());

    let not_utf8 = bundle(&[
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("score.xml", &[0xFF, 0xFE, 0x00, 0x41]),
    ]);
    assert_eq!(read(&not_utf8), Err(Error::NotUtf8));
}
