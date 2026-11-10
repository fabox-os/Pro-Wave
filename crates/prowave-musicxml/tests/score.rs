//! A score with two parts, attributes, a chord and a rest; and malformed input.

use prowave_musicxml::parse;

const SCORE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="4.0">
  <work><work-title>Etude</work-title></work>
  <part-list>
    <score-part id="P1"><part-name>Piano</part-name></score-part>
    <score-part id="P2"><part-name>Cello</part-name></score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>2</divisions>
        <key><fifths>-1</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
      </attributes>
      <direction><sound tempo="96"/></direction>
      <note><pitch><step>F</step><alter>1</alter><octave>4</octave></pitch><duration>2</duration><voice>1</voice><staff>1</staff></note>
      <note><chord/><pitch><step>A</step><octave>4</octave></pitch><duration>2</duration></note>
      <note><rest/><duration>4</duration></note>
    </measure>
  </part>
  <part id="P2">
    <measure number="1">
      <note><pitch><step>C</step><octave>3</octave></pitch><duration>4</duration></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn reads_parts_measures_notes_and_attributes() {
    let score = parse(SCORE).expect("the score should parse");
    assert_eq!(score.title.as_deref(), Some("Etude"));
    assert_eq!(score.parts.len(), 2);
    assert_eq!(score.parts[0].id, "P1");
    assert_eq!(score.parts[0].name, "Piano");
    assert_eq!(score.parts[1].name, "Cello");
    assert_eq!(score.note_count(), 4);

    let measure = &score.parts[0].measures[0];
    assert_eq!(measure.number, "1");
    assert_eq!(measure.tempo, Some(96.0));
    let attributes = measure
        .attributes
        .expect("the first measure has attributes");
    assert_eq!(attributes.divisions, Some(2));
    assert_eq!(attributes.fifths, Some(-1));
    assert_eq!(attributes.beats, Some(4));
    assert_eq!(attributes.beat_type, Some(4));

    assert_eq!(measure.notes.len(), 3);
    let first = &measure.notes[0];
    let pitch = first.pitch.expect("first note has a pitch");
    assert_eq!(pitch.step, 'F');
    assert_eq!(pitch.alter, 1);
    assert_eq!(pitch.octave, 4);
    assert_eq!(pitch.midi(), 66, "F sharp above middle C");
    assert_eq!(first.duration, 2);
    assert_eq!(first.voice, Some(1));
    assert_eq!(first.staff, Some(1));
    assert!(!first.chord && !first.rest);

    assert!(
        measure.notes[1].chord,
        "the second note is part of the chord"
    );
    assert_eq!(measure.notes[1].pitch.expect("chord note").midi(), 69);
    assert!(measure.notes[2].rest);
    assert_eq!(measure.notes[2].duration, 4);

    let cello = &score.parts[1].measures[0].notes[0];
    assert_eq!(cello.pitch.expect("cello note").midi(), 48);
}

#[test]
fn malformed_xml_is_an_error() {
    assert!(parse("<score-partwise><part id=\"P1\"></wrong></score-partwise>").is_err());
    assert!(parse("not xml at all").is_err());
}

#[test]
fn an_unpitched_note_keeps_its_duration() {
    let score = parse(
        r#"<score-partwise><part-list><score-part id="P1"><part-name>Drum</part-name></score-part></part-list>
        <part id="P1"><measure number="1"><note><unpitched><display-step>C</display-step></unpitched><duration>1</duration></note></measure></part></score-partwise>"#,
    )
    .unwrap();
    let note = &score.parts[0].measures[0].notes[0];
    assert_eq!(note.pitch, None);
    assert_eq!(note.duration, 1);
    assert!(!note.rest);
}
