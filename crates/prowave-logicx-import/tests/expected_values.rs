//! The values the user entered in the real project (`Projet.logicx`, 120 bpm, 4/4, C major,
//! 44.1 kHz), asserted against what `read_full` decodes.
//!
//! Notes and audio regions are asserted. Volume, pan, mute, solo and the record and monitor
//! flags are not decoded yet, so their tests are ignored until a decoder finds the bytes.
//!
//! Note and region times are in beats here: `FullProject::ticks_per_quarter` (960) ticks make one.

use prowave_logicx_import::{read_full, FullProject, TrackKind};
use std::fs;
use std::path::Path;

const REAL_PROJECT: &str = "/Users/maximerandazzo/Downloads/Projet.logicx";

/// The notes of the region `Inst 1`, as the file holds them: pitch, velocity, start and length in
/// beats from the region start (bar 1 beat 1, tick 0). The user gave pitches (B2, D3, E3) and the
/// rhythm; the velocities are the file's (byte +0x0b of each note), which the user did not state.
const EXPECTED_NOTES: [(u8, u8, f64, f64); 3] = [
    // B2 a sixteenth (one quarter of a beat) after the start, a sixteenth long.
    (59, 80, 0.25, 0.25),
    // D3 half a beat after the start, a sixteenth long.
    (62, 80, 0.5, 0.25),
    // E3 one beat after the start, two beats long (a half note).
    (64, 63, 1.0, 2.0),
];

/// The project copied to a temporary folder: the metadata and `ProjectData` only.
fn copy_project() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let real = Path::new(REAL_PROJECT);
    if !real.join("Alternatives/000/ProjectData").exists() {
        eprintln!("skipping: {REAL_PROJECT} is not on this machine");
        return None;
    }
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
    Some((dir, bundle))
}

fn inst_1(project: &FullProject) -> &prowave_logicx_import::Track {
    project
        .tracks
        .iter()
        .find(|t| t.name == "Inst 1" && t.kind == TrackKind::Instrument)
        .expect("the Inst 1 strip")
}

#[test]
fn inst_1_has_the_three_notes_the_user_played() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let per_beat = u64::from(project.ticks_per_quarter);
    let region = inst_1(&project)
        .midi_regions
        .first()
        .expect("a MIDI region on Inst 1");
    assert_eq!(region.name.as_deref(), Some("Inst 1"));
    assert_eq!(region.notes.len(), EXPECTED_NOTES.len(), "three notes");
    for (note, (pitch, velocity, start, length)) in region.notes.iter().zip(EXPECTED_NOTES) {
        assert_eq!(note.pitch, pitch, "pitch");
        assert_eq!(note.velocity, velocity, "velocity");
        assert_eq!(
            note.start_tick,
            (start * per_beat as f64) as u64,
            "start of pitch {pitch}"
        );
        assert_eq!(
            note.length_ticks,
            (length * per_beat as f64) as u64,
            "length of pitch {pitch}"
        );
    }
}

#[test]
fn the_bounce_is_placed_on_audio_3_one_bar_after_the_start() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let audio_3 = project
        .tracks
        .iter()
        .find(|t| t.name == "Audio 3")
        .expect("Audio 3");
    assert_eq!(audio_3.audio_regions.len(), 1, "one placed region");
    let region = &audio_3.audio_regions[0];
    assert_eq!(region.name, "Inst 1_bip");
    // The placement is 3840 ticks (one bar) after the start, not the one beat of the estimate.
    assert_eq!(region.start_tick, Some(3840));
    assert_eq!(
        region.length_frames,
        Some(88_200),
        "two seconds of the 99 807-frame file"
    );
    let placed_elsewhere = project
        .tracks
        .iter()
        .any(|t| t.name != "Audio 3" && t.audio_regions.iter().any(|r| r.name == "Inst 1_bip"));
    assert!(!placed_elsewhere);
}

#[test]
fn the_bounce_file_matches_its_descriptor() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let file = project.audio_files.first().expect("the audio file");
    assert_eq!(file.name, "Inst 1_bip.wav");
    assert_eq!(file.frames, 99_807);
    assert_eq!(file.sample_rate, 44_100);
    assert_eq!(file.bits_per_sample, 24);
}

#[test]
fn inst_1_region_starts_at_bar_1() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let region = inst_1(&project).midi_regions.first().expect("the region");
    assert_eq!(region.start_tick, Some(0));
    assert_eq!(region.length_ticks, Some(3840), "one bar");
}

#[test]
fn the_bounce_is_an_audio_region_on_the_bounced_file() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let names: Vec<(&str, Option<&str>)> = project
        .audio_regions
        .iter()
        .map(|r| (r.name.as_str(), r.file.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            ("Inst 1_bip", Some("Audio Files/Inst 1_bip.wav")),
            ("Inst 1_bip.1", Some("Audio Files/Inst 1_bip.wav")),
        ]
    );
}

#[test]
fn the_active_tracks_are_the_four_the_metadata_counts() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let active: Vec<&str> = project
        .tracks
        .iter()
        .filter(|t| t.active)
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(active, ["Audio 1", "Audio 2", "Audio 3", "Inst 1"]);
}

/// What the saved file holds for the two moved faders and pans. The user's latest stated values
/// (Inst 1 at -2.2 dB and pan +36, Audio 3 at +4.6 dB and pan -64) are not in this file: the
/// file holds the state the earlier screenshot showed (Inst 1 at +2.0 dB and pan +29), and an
/// Audio 3 pan of -41.
#[test]
fn the_saved_faders_and_pans_are_read() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let track = |name: &str| {
        project
            .tracks
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(track("Audio 1").fader_raw, Some(90), "default fader");
    assert_eq!(track("Audio 1").pan, Some(0.0), "default pan is centre");
    assert_eq!(track("Inst 1").fader_raw, Some(100));
    assert_eq!(track("Inst 1").pan, Some(29.0));
    assert_eq!(track("Audio 3").fader_raw, Some(117));
    assert_eq!(track("Audio 3").pan, Some(-41.0));
}

/// The user's latest value for the Inst 1 fader (-2.2 dB). Ignored: the file does not hold it,
/// and the fader curve to dB is not known, so `volume_db` stays `None`.
#[test]
#[ignore = "the saved file holds a different fader, and the fader curve is not known"]
fn inst_1_volume_is_minus_2_2_db() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    assert_eq!(
        inst_1(&project).volume_db.map(|v| (v * 10.0).round()),
        Some(-22.0)
    );
}

/// The user's latest value for the Audio 3 fader (+4.6 dB). Ignored: as above.
#[test]
#[ignore = "the saved file holds a different fader, and the fader curve is not known"]
fn audio_3_volume_is_plus_4_6_db() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    let audio_3 = project
        .tracks
        .iter()
        .find(|t| t.name == "Audio 3")
        .expect("Audio 3");
    assert_eq!(audio_3.volume_db.map(|v| (v * 10.0).round()), Some(46.0));
}

/// The user's latest pans: Inst 1 +36 and Audio 3 -64. Ignored: the saved file holds +29 and -41.
#[test]
#[ignore = "the saved file holds pans of +29 and -41"]
fn pans_are_plus_36_and_minus_64() {
    let Some((_dir, bundle)) = copy_project() else {
        return;
    };
    let project = read_full(&bundle).expect("read_full");
    assert_eq!(inst_1(&project).pan, Some(36.0));
    let audio_3 = project
        .tracks
        .iter()
        .find(|t| t.name == "Audio 3")
        .expect("Audio 3");
    assert_eq!(audio_3.pan, Some(-64.0));
}
