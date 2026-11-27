//! Built in-memory .mxl bundles: container root, XML fallback, and damaged archives.

use std::io::{Cursor, Write};

use prowave_mxl::{read, read_path, Error};
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
fn a_malformed_container_is_reported_instead_of_falling_back() {
    let bytes = bundle(&[
        ("META-INF/container.xml", b"<container><rootfiles></wrong>"),
        ("score.xml", SCORE.as_bytes()),
    ]);
    assert!(matches!(read(&bytes), Err(Error::Container(_))));
}

#[test]
fn the_first_xml_is_chosen_in_order_and_meta_inf_in_any_case_is_skipped() {
    // The archive's order does not matter: the reader sorts the names.
    let ordered = bundle(&[("b.xml", b"<b/>"), ("a.xml", b"<a/>")]);
    assert_eq!(read(&ordered).unwrap(), "<a/>");
    let lower_case = bundle(&[
        ("meta-inf/notes.xml", b"<notes/>"),
        ("score.xml", SCORE.as_bytes()),
    ]);
    assert_eq!(read(&lower_case).unwrap(), SCORE);
}

#[test]
fn too_many_entries_are_refused() {
    // One more entry than the reader accepts.
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..=4096 {
        writer
            .start_file(format!("entry-{index}.xml"), SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(b"<x/>").expect("write entry");
    }
    let bytes = writer.finish().expect("finish zip").into_inner();
    assert_eq!(read(&bytes), Err(Error::TooLarge));
}

#[test]
fn an_oversized_container_is_refused_before_it_is_read() {
    // One byte over the one MiB a container may hold.
    let container = vec![b' '; (1 << 20) + 1];
    let bytes = bundle(&[("META-INF/container.xml", &container)]);
    assert_eq!(read(&bytes), Err(Error::TooLarge));
}

#[test]
fn an_oversized_file_is_refused_before_it_is_read() {
    let path = std::env::temp_dir().join(format!("prowave-mxl-too-big-{}.mxl", std::process::id()));
    let file = std::fs::File::create(&path).expect("make the file");
    // A sparse file: one byte over the 64 MiB the reader accepts, with nothing written.
    file.set_len((64 << 20) + 1).expect("size the file");
    drop(file);
    let result = read_path(&path);
    let _ = std::fs::remove_file(&path);
    assert_eq!(result, Err(Error::TooLarge));
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
