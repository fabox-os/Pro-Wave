//! SFZ reading: regions and the settings they inherit, note names, loops, and the limits and
//! path checks that untrusted instrument files need.

use prowave_sfz::{
    load, parse, parse_bytes, AmpEnvelope, Error, LoopMode, MAX_FILE_BYTES, MAX_REGIONS,
};
use std::path::Path;

/// A global setting, a master setting, a group and two regions that inherit and override them.
#[test]
fn regions_inherit_global_master_and_group_settings() {
    let instrument = parse(
        r#"
        <control>
        default_path=samples/

        <global>
        volume=-3
        ampeg_release=1.5

        <master>
        pan=-100

        <group>
        lokey=36 hikey=48

        <region> sample=piano_c2.wav pitch_keycenter=36
        <region> sample=piano_c3.wav pitch_keycenter=48 lovel=64 hivel=127
        "#,
    )
    .expect("the instrument should parse");

    assert_eq!(instrument.regions.len(), 2);
    let low = &instrument.regions[0];
    assert_eq!(low.sample, "samples/piano_c2.wav");
    assert_eq!((low.key_low, low.key_high), (36, 48));
    assert_eq!(low.root_key, 36);
    assert_eq!((low.vel_low, low.vel_high), (0, 127));
    assert_eq!(low.volume_db, -3.0);
    assert_eq!(low.pan_percent, -100.0);
    assert_eq!(low.amp.release, 1.5);

    let high = &instrument.regions[1];
    assert_eq!(high.sample, "samples/piano_c3.wav");
    assert_eq!(high.root_key, 48);
    assert_eq!((high.vel_low, high.vel_high), (64, 127));
    assert_eq!(high.volume_db, -3.0, "inherited through the group");
}

#[test]
fn note_names_and_defaults() {
    let instrument = parse("<region> sample=x.wav lokey=C2 hikey=C4").expect("parse");
    let region = &instrument.regions[0];
    assert_eq!((region.key_low, region.key_high), (36, 60), "C2..C4");
    assert_eq!(region.root_key, 60, "SFZ's default pitch key centre");
    assert_eq!((region.vel_low, region.vel_high), (0, 127));
    assert_eq!(region.loop_mode, LoopMode::NoLoop);
    assert_eq!(region.volume_db, 0.0);
    assert_eq!(region.pan_percent, 0.0);
    assert_eq!(region.transpose, 0);
    assert_eq!(region.tune_cents, 0.0);
    assert_eq!(region.amp, AmpEnvelope::default());
    assert_eq!(AmpEnvelope::default().sustain, 1.0);

    // Accidentals, middle C, and the ends of the MIDI range.
    for (text, midi) in [
        ("c#2", 37),
        ("Db3", 49),
        ("A4", 69),
        ("C-1", 0),
        ("G9", 127),
        ("60", 60),
    ] {
        let instrument =
            parse(&format!("<region> sample=x.wav key={text}")).expect("a single region");
        let region = &instrument.regions[0];
        assert_eq!((region.key_low, region.key_high), (midi, midi), "{text}");
        assert_eq!(region.root_key, midi, "key also sets the root: {text}");
    }
}

#[test]
fn loop_settings_are_read() {
    let instrument = parse(
        "<region> sample=loop.wav loop_mode=loop_continuous loop_start=1000 loop_end=2000 key=60",
    )
    .expect("parse");
    let region = &instrument.regions[0];
    assert_eq!(region.loop_mode, LoopMode::Continuous);
    assert_eq!(region.loop_start, Some(1000));
    assert_eq!(region.loop_end, Some(2000));
    assert_eq!((region.key_low, region.key_high), (60, 60));

    let one_shot = parse("<region> sample=x.wav loop_mode=one_shot").expect("parse");
    assert_eq!(one_shot.regions[0].loop_mode, LoopMode::OneShot);
    let sustain = parse("<region> sample=x.wav loop_mode=loop_sustain").expect("parse");
    assert_eq!(sustain.regions[0].loop_mode, LoopMode::Sustain);
    let no_loop = parse("<region> sample=x.wav loop_mode=no_loop").expect("parse");
    assert_eq!(no_loop.regions[0].loop_mode, LoopMode::NoLoop);
}

#[test]
fn sample_paths_that_leave_the_instrument_are_rejected() {
    for sample in [
        "../../etc/passwd",
        "samples/../../secret.wav",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "\\\\server\\share\\x.wav",
        "..",
    ] {
        let text = format!("<control> default_path=samples/ <region> sample={sample}");
        match parse(&text) {
            Err(Error::BadSamplePath { .. }) => {}
            other => panic!("{sample} should be rejected, got {other:?}"),
        }
    }

    // Ordinary relative paths and SFZ's built-in generators are fine.
    for sample in ["kick.wav", "a/b/c.wav", "*sine", "*noise"] {
        let text = format!("<region> sample={sample}");
        let instrument = parse(&text).expect("a safe sample path");
        assert_eq!(instrument.regions[0].sample, sample);
    }
}

#[test]
fn default_path_is_applied_and_checked() {
    let instrument = parse("<control> default_path=Kit/ <region> sample=kick.wav").expect("parse");
    assert_eq!(instrument.regions[0].sample, "Kit/kick.wav");

    let escaped = parse("<control> default_path=../ <region> sample=kick.wav");
    assert!(matches!(escaped, Err(Error::BadSamplePath { .. })));

    // A sample that is already generated must not be prefixed.
    let generated = parse("<control> default_path=Kit/ <region> sample=*sine").expect("parse");
    assert_eq!(generated.regions[0].sample, "*sine");
}

#[test]
fn files_larger_than_the_limit_are_refused() {
    let Ok(dir) = tempfile::tempdir() else {
        panic!("a temporary directory");
    };
    let path = dir.path().join("huge.sfz");
    let huge = vec![b' '; MAX_FILE_BYTES + 1];
    if let Err(e) = std::fs::write(&path, &huge) {
        panic!("writing the oversized instrument: {e}");
    }
    assert!(matches!(
        load(&path),
        Err(Error::TooLarge {
            limit: MAX_FILE_BYTES
        })
    ));
    assert!(matches!(
        parse_bytes(&huge),
        Err(Error::TooLarge {
            limit: MAX_FILE_BYTES
        })
    ));
}

#[test]
fn the_region_count_is_capped() {
    let mut text = String::with_capacity((MAX_REGIONS + 1) * 24);
    for _ in 0..=MAX_REGIONS {
        text.push_str("<region> sample=a.wav\n");
    }
    assert!(matches!(
        parse(&text),
        Err(Error::TooManyRegions { limit: MAX_REGIONS })
    ));
}

#[test]
fn malformed_input_never_panics() {
    assert!(matches!(
        parse("<region> sample=\"unterminated"),
        Err(Error::UnterminatedQuote)
    ));

    // Unknown headers are ignored, and a bad number falls back to the default.
    let instrument = parse("<<< >>> <region> sample=x.wav volume=abc").expect("parse");
    assert_eq!(instrument.regions.len(), 1);
    assert_eq!(instrument.regions[0].volume_db, 0.0);

    let empty = parse("").expect("an empty instrument");
    assert!(empty.regions.is_empty());

    // `#include` is not followed (a file could name any path); the line is ignored.
    let with_include = parse("#include \"other.sfz\"\n<region> sample=x.wav").expect("parse");
    assert_eq!(with_include.regions.len(), 1);
    assert_eq!(with_include.regions[0].sample, "x.wav");
}

#[test]
fn quoted_values_keep_spaces_and_comments_are_ignored() {
    let instrument = parse(
        r#"
        <region> sample="my samples/kick 1.wav" // the comment is not a value
        volume=-6
        "#,
    )
    .expect("parse");
    assert_eq!(instrument.regions[0].sample, "my samples/kick 1.wav");
    assert_eq!(instrument.regions[0].volume_db, -6.0);
}

#[test]
fn regions_without_a_sample_are_skipped() {
    let instrument = parse("<region> lokey=36\n<region> sample=*noise").expect("parse");
    assert_eq!(instrument.regions.len(), 1);
    assert_eq!(instrument.regions[0].sample, "*noise");
}

#[test]
fn out_of_range_values_are_clamped() {
    let instrument = parse(
        "<region> sample=x.wav lokey=-5 hikey=999 lovel=200 hivel=-3 \
         tune=99999 volume=999 pan=-500 ampeg_sustain=150 ampeg_attack=-1 transpose=999",
    )
    .expect("parse");
    let region = &instrument.regions[0];
    assert_eq!((region.key_low, region.key_high), (0, 127));
    assert_eq!((region.vel_low, region.vel_high), (0, 127));
    assert_eq!(region.tune_cents, 100.0);
    assert_eq!(region.volume_db, 6.0);
    assert_eq!(region.pan_percent, -100.0);
    assert_eq!(region.amp.sustain, 1.0);
    assert_eq!(region.amp.attack, 0.0);
    assert_eq!(region.transpose, 127);
}

#[test]
fn reads_a_file_and_refuses_invalid_utf8() {
    let Ok(dir) = tempfile::tempdir() else {
        panic!("a temporary directory");
    };
    let good = dir.path().join("good.sfz");
    if let Err(e) = std::fs::write(&good, "<region> sample=kick.wav key=36") {
        panic!("writing the instrument: {e}");
    }
    let instrument = load(Path::new(&good)).expect("the instrument should load");
    assert_eq!(instrument.regions[0].sample, "kick.wav");

    let bad = dir.path().join("bad.sfz");
    if let Err(e) = std::fs::write(&bad, [0xff, 0xfe, b'<', b'r', b'e']) {
        panic!("writing the invalid instrument: {e}");
    }
    assert!(matches!(load(Path::new(&bad)), Err(Error::NotUtf8)));
}

#[test]
fn a_region_plays_its_key_and_velocity_range() {
    let instrument = parse("<region> sample=x.wav key=60 lovel=64 hivel=127").expect("parse");
    let region = &instrument.regions[0];
    assert!(region.plays(60, 100));
    assert!(!region.plays(59, 100), "below the key range");
    assert!(!region.plays(60, 63), "below the velocity range");
}
