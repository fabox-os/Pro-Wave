//! `read_full`: the channel strips, plug-ins and instrument references of `ProjectData`.
//!
//! Most tests build a small bundle in a temporary folder. The tests that read the real Logic
//! project skip themselves when it is not on disk, so the suite runs anywhere.

use prowave_logicx_import::{read_full, Error, PluginFormat, TrackKind};
use std::fs;
use std::path::{Path, PathBuf};

/// The real project studied for the format notes. Read-only: the tests copy the small files
/// they need and never open its media.
const REAL_PROJECT: &str = "/Users/maximerandazzo/Downloads/Projet.logicx";

const MAGIC: [u8; 4] = [0x23, 0x47, 0xC0, 0xAB];

/// The root header: magic, three words, the payload length, and four zero bytes.
fn project_data(payload: &[u8]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&[0xCD, 0x09, 0x03, 0x00]);
    bytes.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&[0x01, 0x00, 0x08, 0x00]);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(payload);
    bytes
}

/// A channel strip record: NUL, a 16-byte name field opening with a space, then the descriptor.
fn strip(name: &str, descriptor: [u8; 8]) -> Vec<u8> {
    let mut field = vec![b' '];
    field.extend_from_slice(name.as_bytes());
    field.resize(16, 0);
    let mut record = vec![0u8];
    record.extend_from_slice(&field);
    record.extend_from_slice(&descriptor);
    record
}

/// An Audio Unit record: the 12-byte name, then the manufacturer, type and subtype codes
/// stored little-endian.
fn au(name: &str, manufacturer: &str, kind: &str, subtype: &str) -> Vec<u8> {
    let mut name_field = name.as_bytes().to_vec();
    name_field.resize(12, 0);
    let mut bytes = name_field;
    for code in [manufacturer, kind, subtype] {
        let mut stored: Vec<u8> = code.bytes().collect();
        stored.reverse();
        bytes.extend_from_slice(&stored);
    }
    bytes
}

/// A class-info property list for the Audio Unit above, with a `FabFilterPluginState` blob.
fn class_info(manufacturer: u32, kind: u32, subtype: u32) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>FabFilterPluginState</key><data>RkZCUwEAAAA=</data>
<key>manufacturer</key><integer>{manufacturer}</integer>
<key>name</key><string>Default Setting</string>
<key>subtype</key><integer>{subtype}</integer>
<key>type</key><integer>{kind}</integer>
<key>version</key><integer>0</integer>
</dict></plist>"#
    )
    .into_bytes()
}

/// A bundle folder with the metadata plists copied or written, and the given `ProjectData`.
struct Bundle {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Bundle {
    fn new(project_data_bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("Song.logicx");
        let alternative = path.join("Alternatives/000");
        fs::create_dir_all(&alternative).expect("the alternative");
        fs::create_dir_all(path.join("Resources")).expect("the resources");
        fs::write(
            alternative.join("MetaData.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>tempo</key><real>120</real>
<key>NumberOfTracks</key><integer>3</integer>
</dict></plist>"#,
        )
        .expect("MetaData.plist");
        fs::write(
            path.join("Resources/ProjectInformation.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>activeAlternative</key><integer>0</integer>
<key>logicVersion</key><string>11.0</string>
</dict></plist>"#,
        )
        .expect("ProjectInformation.plist");
        fs::write(alternative.join("ProjectData"), project_data_bytes).expect("ProjectData");
        Self { _dir: dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// A synthetic project: three strips, one Audio Unit with its class info, and one stock record.
fn synthetic_payload() -> Vec<u8> {
    let mut payload = vec![0u8; 8];
    payload.extend(strip("Audio 1", [0xAB, 0xF7, 0xD3, 0xCF, 0x08, 0, 0, 0]));
    payload.extend(strip("Inst 1", [0x29, 0xF5, 0xF7, 0xCF, 0x08, 0, 0, 0]));
    payload.extend(au("Pro-Q 4", "FabF", "aumf", "FQ4p"));
    // The class info must name the same codes as the descriptor, read as big-endian integers.
    payload.extend(class_info(
        u32::from_be_bytes(*b"FabF"),
        u32::from_be_bytes(*b"aumf"),
        u32::from_be_bytes(*b"FQ4p"),
    ));
    // A stock insert named SmpleDly: flags 0x02 0x02 then the 12-byte name, then GAME.
    payload.extend_from_slice(&[0x02, 0x02]);
    payload.extend_from_slice(b"SmpleDly\0\0\0\0");
    payload.extend_from_slice(b"GAME");
    payload.extend(strip("Bus 1", [0xE9, 0xF5, 0xD7, 0xCD, 0, 0, 0, 0]));
    payload
}

#[test]
fn a_synthetic_bundle_gives_its_strips_and_plug_ins() {
    let bundle = Bundle::new(&project_data(&synthetic_payload()));
    let project = read_full(bundle.path()).expect("read_full");
    assert_eq!(project.info.name, "Song");
    assert_eq!(project.info.tempo, 120.0);

    let names: Vec<&str> = project.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Audio 1", "Inst 1", "Bus 1"]);
    assert_eq!(project.tracks[0].kind, TrackKind::Audio);
    assert!(project.tracks[0].active);
    assert_eq!(project.tracks[1].kind, TrackKind::Instrument);
    assert_eq!(project.tracks[2].kind, TrackKind::Bus);
    assert!(!project.tracks[2].active);

    // The Audio Unit and the stock insert both start after Inst 1, so they belong to it.
    let inst = &project.tracks[1];
    assert_eq!(inst.plugins.len(), 2);
    let fabfilter = &inst.plugins[0];
    assert_eq!(fabfilter.name, "Pro-Q 4");
    assert_eq!(fabfilter.format, PluginFormat::AudioUnit);
    assert_eq!(fabfilter.au_manufacturer, Some(*b"FabF"));
    assert_eq!(fabfilter.au_type, Some(*b"aumf"));
    assert_eq!(fabfilter.au_subtype, Some(*b"FQ4p"));
    let state = fabfilter.state.as_ref().expect("the class info");
    let value: plist::Value = plist::from_bytes(state).expect("the state parses");
    let dict = value.as_dictionary().expect("a dictionary");
    assert!(dict
        .get("FabFilterPluginState")
        .and_then(|v| v.as_data())
        .is_some());
    assert_eq!(inst.plugins[1].name, "SmpleDly");
    assert_eq!(inst.plugins[1].format, PluginFormat::Stock);
    assert!(project.unassigned_plugins.is_empty());
}

#[test]
fn a_real_project_gives_its_three_tracks_and_plug_ins() {
    let real = Path::new(REAL_PROJECT);
    if !real.join("Alternatives/000/ProjectData").exists() {
        eprintln!("skipping: {REAL_PROJECT} is not on this machine");
        return;
    }
    // Copy the metadata and the ProjectData only: the media folder is hundreds of megabytes.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let bundle = dir.path().join("Projet.logicx");
    for relative in [
        "Alternatives/000/MetaData.plist",
        "Alternatives/000/ProjectData",
        "Resources/ProjectInformation.plist",
    ] {
        let target = bundle.join(relative);
        fs::create_dir_all(target.parent().expect("a parent")).expect("the folder");
        fs::copy(real.join(relative), target).expect("the copy");
    }

    let project = read_full(&bundle).expect("read_full");
    assert_eq!(project.info.name, "Projet");
    assert!(project.info.project_data_bytes > 0, "ProjectData is read");

    // The strips, in the order the file stores them.
    let first: Vec<(&str, TrackKind)> = project
        .tracks
        .iter()
        .take(9)
        .map(|t| (t.name.as_str(), t.kind))
        .collect();
    assert_eq!(
        first,
        [
            ("Audio 1", TrackKind::Audio),
            ("Audio 2", TrackKind::Audio),
            ("Audio 3", TrackKind::Audio),
            ("Audio 4", TrackKind::Audio),
            ("Audio 5", TrackKind::Audio),
            ("Audio 6", TrackKind::Audio),
            ("Aux 1", TrackKind::Aux),
            ("Aux 2", TrackKind::Aux),
            ("Inst 1", TrackKind::Instrument),
        ]
    );

    // The metadata counts four tracks, and byte 4 of the descriptor marks exactly four.
    let in_use: Vec<&str> = project
        .tracks
        .iter()
        .filter(|t| t.active)
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(in_use, ["Audio 1", "Audio 2", "Audio 3", "Inst 1"]);

    let inst = project
        .tracks
        .iter()
        .find(|t| t.name == "Inst 1")
        .expect("the Inst 1 strip");
    assert_eq!(inst.instrument_files, ["Classic Bass Finger.exs"]);
    let names: Vec<&str> = inst.plugins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Sampler", "SmpleDly", "Pro-Q 4"]);

    let fabfilter = &inst.plugins[2];
    assert_eq!(fabfilter.format, PluginFormat::AudioUnit);
    assert_eq!(fabfilter.au_manufacturer, Some(*b"FabF"));
    assert_eq!(fabfilter.au_type, Some(*b"aumf"));
    assert_eq!(fabfilter.au_subtype, Some(*b"FQ4p"));
    let state = fabfilter.state.as_ref().expect("the FabFilter state");
    let value: plist::Value = plist::from_bytes(state).expect("the state parses");
    let blob = value
        .as_dictionary()
        .and_then(|d| d.get("FabFilterPluginState"))
        .and_then(|v| v.as_data())
        .expect("the state blob");
    assert!(
        blob.starts_with(b"FFBS"),
        "the FabFilter blob starts with FFBS"
    );

    let klopfgeist = project
        .unassigned_plugins
        .iter()
        .find(|p| p.name == "Klopfgeist")
        .expect("the metronome is unassigned");
    assert_eq!(klopfgeist.format, PluginFormat::Stock);
}

/// Each cut must give an error, and never a panic. The cuts cover the whole file.
#[test]
fn truncated_project_data_is_an_error_at_every_cut() {
    let real = Path::new(REAL_PROJECT).join("Alternatives/000/ProjectData");
    let whole = match fs::read(&real) {
        Ok(bytes) => bytes,
        Err(_) => project_data(&synthetic_payload()),
    };
    let mut cuts: Vec<usize> = (0..=40).collect();
    cuts.extend((41..whole.len()).step_by(211));
    cuts.push(whole.len() - 1);
    for cut in cuts {
        let bundle = Bundle::new(&whole[..cut]);
        let result = read_full(bundle.path());
        assert!(
            result.is_err(),
            "a file cut at {cut} of {} must not read",
            whole.len()
        );
    }
    let bundle = Bundle::new(&whole);
    assert!(read_full(bundle.path()).is_ok(), "the whole file reads");
}

/// Random byte changes never panic. A change may read or error, but not crash.
#[test]
fn mutated_project_data_never_panics() {
    let real = Path::new(REAL_PROJECT).join("Alternatives/000/ProjectData");
    let base = match fs::read(&real) {
        Ok(bytes) => bytes,
        Err(_) => project_data(&synthetic_payload()),
    };
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..120 {
        let mut bytes = base.clone();
        for _ in 0..8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index = (state as usize) % bytes.len();
            bytes[index] = (state >> 32) as u8;
        }
        let bundle = Bundle::new(&bytes);
        let _ = read_full(bundle.path());
    }
}

#[test]
fn a_file_that_is_not_project_data_is_refused() {
    let bundle = Bundle::new(b"not a Logic project at all, just text");
    assert!(matches!(
        read_full(bundle.path()),
        Err(Error::BadProjectData)
    ));
}
