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
