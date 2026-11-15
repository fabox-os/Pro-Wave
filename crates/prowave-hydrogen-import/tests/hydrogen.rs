//! Reading the two Hydrogen session layouts.

use prowave_hydrogen_import::{read, Error, Song};

const V1_2: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<song>
 <version>1.2.0</version>
 <bpm>115</bpm>
 <name>Demo Song</name>
 <author>Someone</author>
 <license>CC0</license>
 <drumkit_name>GMRockKit</drumkit_name>
 <componentList>
  <drumkitComponent><id>0</id><name>Main</name></drumkitComponent>
 </componentList>
 <instrumentList>
  <instrument>
   <id>0</id><name>Kick</name><drumkit>GMRockKit</drumkit>
   <midiOutNote>36</midiOutNote><volume>0.8</volume><pan>0.1</pan>
   <gain>1</gain><isMuted>false</isMuted>
  </instrument>
  <instrument>
   <id>1</id><name>Snare</name><drumkit>GMRockKit</drumkit>
   <midiOutNote>38</midiOutNote>
  </instrument>
 </instrumentList>
 <patternList>
  <pattern>
   <name>main 1-1</name><size>192</size><denominator>4</denominator>
   <noteList>
    <note>
     <position>0</position><leadlag>0</leadlag><velocity>0.8</velocity>
     <pan>0</pan><pitch>0</pitch><probability>1</probability>
     <length>-1</length><instrument>0</instrument><note_off>false</note_off>
    </note>
    <note>
     <position>48</position><velocity>0.5</velocity><length>24</length>
     <instrument>1</instrument><note_off>true</note_off>
    </note>
   </noteList>
  </pattern>
  <pattern>
   <name>main 1-2</name><size>96</size><denominator>8</denominator>
   <noteList></noteList>
  </pattern>
 </patternList>
 <patternSequence>
  <group><patternID>main 1-1</patternID></group>
  <group><patternID>main 1-2</patternID></group>
  <group><patternID>missing</patternID></group>
 </patternSequence>
 <ladspa>
  <fx><name>no plugin</name><enabled>false</enabled></fx>
 </ladspa>
</song>"#;

const V0_9: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<hydrogen_song_file>
 <song>
  <version>0.9.4</version>
  <bpm>140</bpm>
  <patternList>
   <pattern id="1">
    <name>One</name><size>48</size>
    <noteList>
     <note><position>12</position><velocity>1</velocity><instrument>0</instrument></note>
    </noteList>
   </pattern>
   <pattern id="2"><name>Two</name><size>48</size><noteList></noteList></pattern>
  </patternList>
  <patternSequence>
   <patternID>1</patternID><patternID>2</patternID><patternID>1</patternID>
  </patternSequence>
 </song>
</hydrogen_song_file>"#;

#[test]
fn reads_a_1_2_session() {
    let song = prowave_hydrogen_import::parse(V1_2).expect("the song parses");
    assert_eq!(song.version, "1.2.0");
    assert_eq!(song.name, "Demo Song");
    assert_eq!(song.author, "Someone");
    assert_eq!(song.license, "CC0");
    assert_eq!(song.tempo, 115.0);
    assert_eq!(song.drumkit, "GMRockKit");

    assert_eq!(song.instruments.len(), 2);
    let kick = &song.instruments[0];
    assert_eq!(kick.id, 0);
    assert_eq!(kick.name, "Kick");
    assert_eq!(kick.midi_note, Some(36));
    assert_eq!(kick.volume, 0.8);
    assert_eq!(kick.pan, 0.1);
    assert!(!kick.muted);
    assert_eq!(song.instruments[1].midi_note, Some(38));

    assert_eq!(song.patterns.len(), 2);
    let first = &song.patterns[0];
    assert_eq!(first.name, "main 1-1");
    assert_eq!(first.size, 192);
    assert_eq!(first.denominator, 4);
    assert_eq!(first.notes.len(), 2);
    assert_eq!(first.notes[0].position, 0);
    assert_eq!(first.notes[0].velocity, 0.8);
    assert_eq!(first.notes[0].length, -1);
    assert_eq!(first.notes[0].instrument, 0);
    assert!(!first.notes[0].note_off);
    assert_eq!(first.notes[1].position, 48);
    assert_eq!(first.notes[1].length, 24);
    assert!(first.notes[1].note_off);
    assert_eq!(song.patterns[1].size, 96);
    assert_eq!(song.patterns[1].denominator, 8);
    assert!(song.patterns[1].notes.is_empty());

    assert_eq!(song.sequence, vec![0, 1], "the missing name is skipped");
    assert_eq!(song.note_count(), 2);
    assert_eq!(
        song.pattern_at(1).map(|pattern| pattern.name.as_str()),
        Some("main 1-2")
    );
    assert!(song.pattern_at(2).is_none());
}

#[test]
fn reads_an_older_session_with_pattern_ids() {
    let song = prowave_hydrogen_import::parse(V0_9).expect("the song parses");
    assert_eq!(song.version, "0.9.4");
    assert_eq!(song.tempo, 140.0);
    assert_eq!(song.drumkit, "");
    assert_eq!(song.patterns[0].name, "One");
    assert_eq!(song.patterns[0].notes[0].position, 12);
    assert_eq!(song.sequence, vec![0, 1, 0]);
}

#[test]
fn missing_values_default_to_hydrogens() {
    let song = prowave_hydrogen_import::parse(
        "<song><patternList><pattern><noteList></noteList></pattern></patternList></song>",
    )
    .expect("the song parses");
    assert_eq!(song.tempo, 120.0);
    assert_eq!(song.name, "");
    let pattern = &song.patterns[0];
    assert_eq!(pattern.size, 192);
    assert_eq!(pattern.denominator, 4);
    assert!(pattern.notes.is_empty());
    assert!(song.sequence.is_empty());
    assert!(song.instruments.is_empty());
}

#[test]
fn refuses_other_documents_and_bad_input() {
    assert!(matches!(
        prowave_hydrogen_import::parse("<score-partwise/>"),
        Err(Error::Xml(_))
    ));
    assert!(matches!(
        prowave_hydrogen_import::parse("<song><bpm>120</bpm>"),
        Err(Error::Xml(_))
    ));
    assert!(matches!(read(&[0xff, 0xfe]), Err(Error::Encoding)));
}

#[test]
fn a_song_may_be_read_from_its_bytes() {
    let song: Song = read(V1_2.as_bytes()).expect("the song parses");
    assert_eq!(song.name, "Demo Song");
}
