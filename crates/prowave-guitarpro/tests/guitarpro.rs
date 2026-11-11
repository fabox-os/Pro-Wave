//! Reading a Guitar Pro 7/8 score: metadata, bars, beats, rhythms and notes.

use std::io::{Cursor, Write};

use prowave_guitarpro::{read, Error, MasterBar, Score, TimeSignature};
use zip::write::SimpleFileOptions;

const GPIF: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<GPIF>
<GPVersion>8.1.4</GPVersion>
<Score>
<Title><![CDATA[A Title]]></Title>
<SubTitle><![CDATA[Sub]]></SubTitle>
<Artist><![CDATA[An Artist]]></Artist>
<Album><![CDATA[An Album]]></Album>
<Words><![CDATA[W]]></Words>
<Music><![CDATA[M]]></Music>
<Copyright><![CDATA[C]]></Copyright>
<Tabber><![CDATA[T]]></Tabber>
<Instructions><![CDATA[I]]></Instructions>
</Score>
<MasterTrack>
<Tracks>0</Tracks>
<Automations>
<Automation><Type>Tempo</Type><Bar>0</Bar><Position>0</Position><Value>95 2</Value></Automation>
</Automations>
</MasterTrack>
<Tracks>
<Track id="0">
<Name><![CDATA[Guitar]]></Name>
<ShortName><![CDATA[Gtr]]></ShortName>
<InstrumentSet><Name><![CDATA[Electric Guitar]]></Name></InstrumentSet>
</Track>
</Tracks>
<MasterBars>
<MasterBar><Key><AccidentalCount>-2</AccidentalCount><Mode>Minor</Mode></Key><Time>3/4</Time><Bars>0</Bars></MasterBar>
<MasterBar><Key><AccidentalCount>0</AccidentalCount><Mode>Major</Mode></Key><Time>4/4</Time><Bars>1</Bars></MasterBar>
</MasterBars>
<Bars>
<Bar id="0"><Voices>0 -1 -1 -1</Voices></Bar>
<Bar id="1"><Voices>1 -1 -1 -1</Voices></Bar>
</Bars>
<Voices>
<Voice id="0"><Beats>0 1</Beats></Voice>
<Voice id="1"><Beats>2</Beats></Voice>
</Voices>
<Beats>
<Beat id="0"><Dynamic>MF</Dynamic><Rhythm ref="0" /><Notes>0 1</Notes></Beat>
<Beat id="1"><Rhythm ref="2" /></Beat>
<Beat id="2"><Rhythm ref="1" /><Notes>2</Notes></Beat>
</Beats>
<Notes>
<Note id="0"><Properties>
<Property name="Midi"><Number>64</Number></Property>
<Property name="String"><String>1</String></Property>
<Property name="Fret"><Fret>3</Fret></Property>
</Properties></Note>
<Note id="1"><LetRing /><Properties>
<Property name="Midi"><Number>67</Number></Property>
<Property name="ConcertPitch"><Pitch><Step>G</Step><Accidental>#</Accidental><Octave>4</Octave></Pitch></Property>
<Property name="String"><String>2</String></Property>
</Properties></Note>
<Note id="2"><Properties>
<Property name="ConcertPitch"><Pitch><Step>E</Step><Accidental>b</Accidental><Octave>5</Octave></Pitch></Property>
</Properties></Note>
</Notes>
<Rhythms>
<Rhythm id="0"><NoteValue>Quarter</NoteValue></Rhythm>
<Rhythm id="1"><NoteValue>Eighth</NoteValue><AugmentationDot count="1" /></Rhythm>
<Rhythm id="2"><NoteValue>Eighth</NoteValue><PrimaryTuplet num="3" den="2" /></Rhythm>
</Rhythms>
</GPIF>"#;

fn song() -> Score {
    prowave_guitarpro::parse(GPIF).expect("the score parses")
}

#[test]
fn reads_metadata_tempo_and_master_bars() {
    let song = song();
    assert_eq!(song.gp_version, "8.1.4");
    assert_eq!(song.title, "A Title");
    assert_eq!(song.subtitle, "Sub");
    assert_eq!(song.artist, "An Artist");
    assert_eq!(song.album, "An Album");
    assert_eq!(song.words, "W");
    assert_eq!(song.music, "M");
    assert_eq!(song.copyright, "C");
    assert_eq!(song.tabber, "T");
    assert_eq!(song.instructions, "I");
    assert_eq!(song.tempo, 95.0);
    assert_eq!(
        song.master_bars,
        vec![
            MasterBar {
                time_signature: TimeSignature {
                    beats: 3,
                    beat_type: 4
                },
                key: prowave_guitarpro::KeySignature {
                    accidentals: -2,
                    major: false
                },
            },
            MasterBar {
                time_signature: TimeSignature {
                    beats: 4,
                    beat_type: 4
                },
                key: prowave_guitarpro::KeySignature {
                    accidentals: 0,
                    major: true
                },
            },
        ]
    );
}

#[test]
fn reads_bars_beats_rhythms_and_notes() {
    let song = song();
    assert_eq!(song.tracks.len(), 1);
    let track = &song.tracks[0];
    assert_eq!(track.id, 0);
    assert_eq!(track.name, "Guitar");
    assert_eq!(track.short_name, "Gtr");
    assert_eq!(track.instrument, "Electric Guitar");
    assert_eq!(track.bars.len(), 2, "one bar per master bar");

    let first = &track.bars[0];
    assert_eq!(first.beats.len(), 2);
    let first_beat = &first.beats[0];
    assert_eq!(first_beat.voice, 0);
    assert_eq!(first_beat.position, 0.0);
    assert_eq!(first_beat.duration, 1.0, "a quarter note");
    assert_eq!(first_beat.notes.len(), 2);
    assert_eq!(first_beat.notes[0].midi, 64);
    assert_eq!(first_beat.notes[0].string, Some(1));
    assert_eq!(first_beat.notes[0].fret, Some(3));
    assert!(!first_beat.notes[0].let_ring);
    assert_eq!(first_beat.notes[1].midi, 67);
    assert!(first_beat.notes[1].let_ring);
    assert_eq!(first_beat.notes[1].fret, None);

    let second_beat = &first.beats[1];
    assert_eq!(second_beat.position, 1.0);
    assert!(
        (second_beat.duration - 1.0 / 3.0).abs() < 1e-9,
        "an eighth-note triplet is a third of a quarter"
    );
    assert!(second_beat.notes.is_empty());

    let second = &track.bars[1];
    assert_eq!(second.beats.len(), 1);
    let dotted = &second.beats[0];
    assert_eq!(dotted.duration, 0.75, "a dotted eighth note");
    assert_eq!(dotted.notes.len(), 1);
    assert_eq!(
        dotted.notes[0].midi, 75,
        "E flat 5 from the concert pitch, when Midi is absent"
    );
    assert_eq!(dotted.notes[0].string, None);

    assert_eq!(track.note_count(), 3);
    assert_eq!(song.note_count(), 3);
}

#[test]
fn reads_a_gp_archive_from_its_bytes() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer
        .start_file("Content/score.gpif", options)
        .expect("start entry");
    writer.write_all(GPIF.as_bytes()).expect("write entry");
    let bytes = writer.finish().expect("finish zip").into_inner();
    let song = read(&bytes).expect("the archive reads");
    assert_eq!(song.title, "A Title");
    assert_eq!(song.tracks[0].name, "Guitar");
}

#[test]
fn refuses_other_files_and_broken_documents() {
    assert!(matches!(read(b"not a zip"), Err(Error::NotGuitarPro)));
    assert!(matches!(read(b"BCFZ0123"), Err(Error::GpxContainer)));
    assert!(matches!(
        prowave_guitarpro::parse("<score-partwise/>"),
        Err(Error::Xml(_))
    ));
    assert!(matches!(
        prowave_guitarpro::parse("<GPIF><Score>"),
        Err(Error::Xml(_))
    ));

    // A ZIP without the score entry is not a score.
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("Content/other.xml", SimpleFileOptions::default())
        .expect("start entry");
    writer.write_all(b"<x/>").expect("write entry");
    let bytes = writer.finish().expect("finish zip").into_inner();
    assert!(matches!(read(&bytes), Err(Error::MissingScore)));
}

#[test]
fn refuses_more_master_bars_than_the_object_cap() {
    // One more master bar than `MAX_OBJECTS` (500 000) allows.
    let mut xml = String::from("<GPIF><MasterBars>");
    for _ in 0..=500_000 {
        xml.push_str("<MasterBar></MasterBar>");
    }
    xml.push_str("</MasterBars></GPIF>");
    assert!(matches!(
        prowave_guitarpro::parse(&xml),
        Err(Error::TooLarge)
    ));
}

#[test]
fn refuses_scores_whose_bar_cells_exceed_the_cap() {
    // 1 001 tracks × 1 000 master bars is 1 001 000 cells, over the 1 000 000 cap, although
    // each count is well inside the object cap.
    let mut xml = String::from("<GPIF><Tracks>");
    for id in 0..1_001 {
        xml.push_str(&format!("<Track id=\"{id}\"></Track>"));
    }
    xml.push_str("</Tracks><MasterBars>");
    for _ in 0..1_000 {
        xml.push_str("<MasterBar></MasterBar>");
    }
    xml.push_str("</MasterBars></GPIF>");
    assert!(matches!(
        prowave_guitarpro::parse(&xml),
        Err(Error::TooLarge)
    ));
}

#[test]
fn refuses_an_archive_whose_score_is_too_large() {
    // 65 MiB of whitespace after the root: it compresses to almost nothing, but the
    // decompressed score is over the 64 MiB cap.
    let mut score = b"<GPIF>".to_vec();
    score.resize(65 * 1024 * 1024, b' ');
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("Content/score.gpif", SimpleFileOptions::default())
        .expect("start entry");
    writer.write_all(&score).expect("write entry");
    let bytes = writer.finish().expect("finish zip").into_inner();
    assert!(matches!(read(&bytes), Err(Error::TooLarge)));
}

#[test]
fn bounds_the_decompressed_score_when_the_archive_lies_about_its_size() {
    // Same oversized score, but the central directory declares 100 bytes. The reader must not
    // trust the declared size: the decompressed output is capped as it is read.
    let mut score = b"<GPIF>".to_vec();
    score.resize(65 * 1024 * 1024, b' ');
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("Content/score.gpif", SimpleFileOptions::default())
        .expect("start entry");
    writer.write_all(&score).expect("write entry");
    let mut bytes = writer.finish().expect("finish zip").into_inner();
    // Patch the central directory header's uncompressed size (offset 24, little-endian u32).
    let central = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x01\x02")
        .expect("a central directory header");
    bytes[central + 24..central + 28].copy_from_slice(&100u32.to_le_bytes());
    // The `zip` crate rejects output longer than the declared size, so the read fails before
    // the reader's own cap; either way no score comes back.
    assert!(read(&bytes).is_err());
}

/// One track, one bar, one voice. `voice_beats` is the voice's beat list and `beat_notes` the
/// notes of beat 0. Both can name the same object many times, which is what the budgets bound.
fn amplified_score(voice_beats: &str, beat_notes: &str) -> String {
    format!(
        "<GPIF>\
<Tracks><Track id=\"0\"><Name>T</Name></Track></Tracks>\
<MasterBars><MasterBar><Bars>0</Bars></MasterBar></MasterBars>\
<Bars><Bar id=\"0\"><Voices>0 -1 -1 -1</Voices></Bar></Bars>\
<Voices><Voice id=\"0\"><Beats>{voice_beats}</Beats></Voice></Voices>\
<Beats><Beat id=\"0\"><Rhythm ref=\"0\" /><Notes>{beat_notes}</Notes></Beat></Beats>\
<Notes><Note id=\"0\"><Properties><Property name=\"Midi\"><Number>60</Number></Property></Properties></Note></Notes>\
<Rhythms><Rhythm id=\"0\"><NoteValue>Quarter</NoteValue></Rhythm></Rhythms>\
</GPIF>"
    )
}

#[test]
fn refuses_a_voice_that_repeats_a_beat_past_the_beat_budget() {
    // 2 000 001 references to one beat: one more than `MAX_TAB_BEATS` allows.
    let voice_beats = "0 ".repeat(2_000_001);
    let xml = amplified_score(&voice_beats, "");
    assert!(matches!(
        prowave_guitarpro::parse(&xml),
        Err(Error::TooLarge)
    ));
}

#[test]
fn refuses_a_beat_that_repeats_a_note_past_the_note_budget() {
    // 4 000 001 references to one note: one more than `MAX_TAB_NOTES` allows.
    let beat_notes = "0 ".repeat(4_000_001);
    let xml = amplified_score("0", &beat_notes);
    assert!(matches!(
        prowave_guitarpro::parse(&xml),
        Err(Error::TooLarge)
    ));
}

#[test]
fn reads_a_score_inside_the_budgets() {
    // A single reference each is far inside both budgets and must still read.
    let song = prowave_guitarpro::parse(&amplified_score("0", "0")).expect("the score parses");
    assert_eq!(song.note_count(), 1);
    assert_eq!(song.tracks[0].bars[0].beats.len(), 1);
}

#[test]
fn entity_references_do_not_truncate_text() {
    // quick-xml reports `&amp;` and `&lt;` as separate events between the text fragments;
    // each element's text must be put back together before it is applied.
    let xml = GPIF
        .replace(
            "<Title><![CDATA[A Title]]></Title>",
            "<Title>Rock &amp; Roll &lt;live&gt;</Title>",
        )
        .replace(
            "<Tabber><![CDATA[T]]></Tabber>",
            "<Tabber>Tabber &amp; Co</Tabber>",
        )
        .replace(
            "<Name><![CDATA[Guitar]]></Name>",
            "<Name>Acoustic &amp; Steel</Name>",
        )
        .replace(
            "<Name><![CDATA[Electric Guitar]]></Name>",
            "<Name>Electric &lt;Set&gt;</Name>",
        )
        // CDATA and text next to each other, and a numeric character reference.
        .replace(
            "<Artist><![CDATA[An Artist]]></Artist>",
            "<Artist><![CDATA[An]]> &amp; <![CDATA[Artist]]></Artist>",
        )
        .replace(
            "<Album><![CDATA[An Album]]></Album>",
            "<Album>Vol &#49;</Album>",
        );
    let score = prowave_guitarpro::parse(&xml).expect("the escaped score parses");
    assert_eq!(score.title, "Rock & Roll <live>");
    assert_eq!(score.tabber, "Tabber & Co");
    assert_eq!(score.artist, "An & Artist");
    assert_eq!(score.album, "Vol 1");
    assert_eq!(score.tracks[0].name, "Acoustic & Steel");
    assert_eq!(score.tracks[0].instrument, "Electric <Set>");
    assert!(score.warnings.is_empty());
}

#[test]
fn an_unknown_note_value_is_an_error() {
    let xml = GPIF.replace(
        "<NoteValue>Quarter</NoteValue>",
        "<NoteValue>Sixteenth</NoteValue>",
    );
    assert!(matches!(prowave_guitarpro::parse(&xml), Err(Error::Xml(_))));
}

#[test]
fn a_beat_without_a_rhythm_is_reported_and_read_as_a_quarter() {
    let xml = GPIF.replace(
        "<Beat id=\"1\"><Rhythm ref=\"2\" /></Beat>",
        "<Beat id=\"1\"></Beat>",
    );
    let score = prowave_guitarpro::parse(&xml).expect("the beat is still readable");
    assert!(
        score
            .warnings
            .iter()
            .any(|warning| warning.contains("no rhythm")),
        "{:?}",
        score.warnings
    );
    assert_eq!(score.tracks[0].bars[0].beats[1].duration, 1.0);
}
