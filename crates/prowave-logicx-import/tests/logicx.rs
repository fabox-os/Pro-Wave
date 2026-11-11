//! `.logicx` bundle reading: package and folder projects, the active alternative, the metadata
//! plists, and the size and path checks untrusted project files need.

use prowave_logicx_import::{read, Error, MAX_PLIST_BYTES, MAX_PROJECT_DATA_BYTES};
use std::fs;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 4] = [0x23, 0x47, 0xC0, 0xAB];

const HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
"#;

fn metadata(tempo: &str, key: &str, time: &str, rate: &str, channels: &str) -> String {
    format!(
        r#"{HEADER}<plist version="1.0"><dict>
<key>tempo</key><real>{tempo}</real>
<key>keySignature</key><string>{key}</string>
<key>timeSignature</key><string>{time}</string>
<key>sampleRate</key><real>{rate}</real>
<key>channelCount</key><integer>{channels}</integer>
</dict></plist>"#
    )
}

fn info(active: u32, version: &str) -> String {
    format!(
        r#"{HEADER}<plist version="1.0"><dict>
<key>activeAlternative</key><integer>{active}</integer>
<key>logicVersion</key><string>{version}</string>
</dict></plist>"#
    )
}

/// A temporary `.logicx` package (or folder project) with a default alternative 0.
struct Fixture {
    /// Keeps the temporary directory alive; the name is prefixed because only the Unix symlink
    /// test reads it, and Windows builds would otherwise see a dead field.
    _dir: tempfile::TempDir,
    bundle: PathBuf,
}

impl Fixture {
    fn package() -> Self {
        Self::under("Song.logicx")
    }

    fn folder_project() -> Self {
        Self::under("My Song/My Song.logicx")
    }

    fn under(relative: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let bundle = dir.path().join(relative);
        let fixture = Self { _dir: dir, bundle };
        fixture.write_metadata(0, &metadata("96", "C Major", "4/4", "44100", "2"));
        fixture.write_info(&info(0, "11.0"));
        fixture.write_project_data(0, &[MAGIC, [0; 4]].concat());
        fixture
    }

    fn path(&self) -> &Path {
        &self.bundle
    }

    #[cfg(unix)]
    fn dir(&self) -> &Path {
        self._dir.path()
    }

    fn alternative(&self, index: u32) -> PathBuf {
        let path = self.bundle.join(format!("Alternatives/{index:03}"));
        fs::create_dir_all(&path).expect("the alternative directory");
        path
    }

    fn write_metadata(&self, index: u32, xml: &str) {
        fs::write(self.alternative(index).join("MetaData.plist"), xml).expect("MetaData.plist");
    }

    fn write_info(&self, xml: &str) {
        let resources = self.bundle.join("Resources");
        fs::create_dir_all(&resources).expect("the Resources directory");
        fs::write(resources.join("ProjectInformation.plist"), xml)
            .expect("ProjectInformation.plist");
    }

    fn write_project_data(&self, index: u32, bytes: &[u8]) {
        fs::write(self.alternative(index).join("ProjectData"), bytes).expect("ProjectData");
    }
}

#[test]
fn reads_metadata_from_a_package() {
    let fixture = Fixture::package();
    let project = read(fixture.path()).expect("read");
    assert_eq!(project.name, "Song");
    assert_eq!(project.tempo, 96.0);
    assert_eq!(project.key_signature.as_deref(), Some("C Major"));
    assert_eq!(project.time_signature.as_deref(), Some("4/4"));
    assert_eq!(project.sample_rate, Some(44_100.0));
    assert_eq!(project.channels, Some(2));
    assert_eq!(project.alternative, 0);
    assert_eq!(project.logic_version.as_deref(), Some("11.0"));
    assert_eq!(project.project_data_bytes, 8);
}

#[test]
fn reads_a_folder_project() {
    let fixture = Fixture::folder_project();
    let project = read(fixture.path().parent().expect("the project folder")).expect("read");
    assert_eq!(project.name, "My Song");
    assert_eq!(project.tempo, 96.0);
}

#[test]
fn uses_the_active_alternative() {
    let fixture = Fixture::package();
    fixture.write_metadata(0, &metadata("96", "C Major", "4/4", "44100", "2"));
    fixture.write_metadata(1, &metadata("128", "A Minor", "3/4", "48000", "2"));
    fixture.write_info(&info(1, "11.1"));
    fixture.write_project_data(1, &[MAGIC, [0; 4]].concat());

    let project = read(fixture.path()).expect("read");
    assert_eq!(project.alternative, 1);
    assert_eq!(project.tempo, 128.0);
    assert_eq!(project.key_signature.as_deref(), Some("A Minor"));
    assert_eq!(project.time_signature.as_deref(), Some("3/4"));
    assert_eq!(project.sample_rate, Some(48_000.0));
    assert_eq!(project.logic_version.as_deref(), Some("11.1"));
}

#[test]
fn numbers_stored_as_strings_are_accepted() {
    let fixture = Fixture::package();
    fixture.write_metadata(
        0,
        &format!(
            r#"{HEADER}<plist version="1.0"><dict>
<key>tempo</key><string>90</string>
<key>sampleRate</key><string>96000</string>
<key>channelCount</key><string>1</string>
</dict></plist>"#
        ),
    );
    let project = read(fixture.path()).expect("read");
    assert_eq!(project.tempo, 90.0);
    assert_eq!(project.sample_rate, Some(96_000.0));
    assert_eq!(project.channels, Some(1));
    assert_eq!(project.key_signature, None);
}

#[test]
fn a_missing_metadata_plist_is_a_missing_file_error() {
    let fixture = Fixture::package();
    fs::remove_file(fixture.path().join("Alternatives/000/MetaData.plist"))
        .expect("remove the metadata");
    assert!(matches!(read(fixture.path()), Err(Error::Missing { .. })));
}

#[test]
fn project_data_without_the_magic_is_refused() {
    let fixture = Fixture::package();
    fixture.write_project_data(0, b"not a Logic project");
    assert!(matches!(read(fixture.path()), Err(Error::BadProjectData)));
}

#[test]
fn oversized_plists_and_project_data_are_refused() {
    let fixture = Fixture::package();
    let metadata = fixture.path().join("Alternatives/000/MetaData.plist");
    let file = fs::File::create(&metadata).expect("create the plist");
    file.set_len(MAX_PLIST_BYTES as u64 + 1)
        .expect("make the plist too large");
    assert!(matches!(read(fixture.path()), Err(Error::TooLarge { .. })));

    let fixture = Fixture::package();
    let project_data = fixture.path().join("Alternatives/000/ProjectData");
    let file = fs::File::create(&project_data).expect("create ProjectData");
    file.set_len(MAX_PROJECT_DATA_BYTES as u64 + 1)
        .expect("make ProjectData too large");
    assert!(matches!(read(fixture.path()), Err(Error::TooLarge { .. })));
}

#[test]
fn a_file_that_is_not_a_bundle_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = dir.path().join("Song.logicx");
    fs::write(&file, b"not a bundle").expect("write the file");
    assert!(matches!(read(&file), Err(Error::NotABundle)));
}

#[cfg(unix)]
#[test]
fn a_symlinked_metadata_file_outside_the_bundle_is_refused() {
    let fixture = Fixture::package();
    let outside = fixture.dir().join("outside.plist");
    fs::write(&outside, metadata("96", "C Major", "4/4", "44100", "2")).expect("the outside plist");
    let inside = fixture.path().join("Alternatives/000/MetaData.plist");
    fs::remove_file(&inside).expect("remove the metadata");
    std::os::unix::fs::symlink(&outside, &inside).expect("the symlink");
    assert!(matches!(
        read(fixture.path()),
        Err(Error::OutsideBundle { .. })
    ));
}
