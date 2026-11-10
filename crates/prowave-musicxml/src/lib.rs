//! MusicXML score reading: the import half of Pro Wave's Score export.
//!
//! The reader takes an uncompressed MusicXML document (`score-partwise` or `score-timewise`'s
//! partwise form) and returns the parts, measures, notes and the attributes an import needs:
//! divisions, key signature, time signature and tempo. It is deliberately a small, lossless
//! subset of MusicXML rather than a full notation model: articulations, slurs, grace notes and
//! layout are ignored, while pitch, duration, voice, staff, chords and rests are kept.
//!
//! MusicXML files are untrusted input: the reader caps the event, part, measure and note counts
//! and never panics on malformed XML. Compressed `.mxl` archives must be unzipped first.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

const MAX_EVENTS: usize = 2_000_000;
const MAX_PARTS: usize = 256;
const MAX_MEASURES: usize = 100_000;
const MAX_NOTES: usize = 1_000_000;

/// A score read from MusicXML.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Score {
    pub title: Option<String>,
    pub parts: Vec<Part>,
}

impl Score {
    /// How many notes the whole score holds, chords counted once.
    pub fn note_count(&self) -> usize {
        self.parts
            .iter()
            .flat_map(|part| part.measures.iter())
            .map(|measure| measure.notes.len())
            .sum()
    }
}

/// One part (`<part id="P1">`) with its measures and name.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Part {
    pub id: String,
    pub name: String,
    pub measures: Vec<Measure>,
}

/// One measure: its number, the attributes that change from it, tempo, and notes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Measure {
    pub number: String,
    pub attributes: Option<Attributes>,
    /// Quarter notes per minute, when a `sound tempo` says so.
    pub tempo: Option<f64>,
    pub notes: Vec<Note>,
}

/// MusicXML `<attributes>`: the notation grid and key/time signature of a measure.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Attributes {
    /// Divisions per quarter note; durations are in these units.
    pub divisions: Option<u16>,
    /// Key signature as a number of sharps (positive) or flats (negative).
    pub fifths: Option<i8>,
    pub beats: Option<u8>,
    pub beat_type: Option<u8>,
}

/// A written pitch. `alter` is -2 to +2 (double flat to double sharp).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pitch {
    pub step: char,
    pub alter: i8,
    pub octave: i8,
}

impl Pitch {
    /// The MIDI note for this pitch (C4 is 60), clamped to 0..=127.
    pub fn midi(self) -> u8 {
        let semitone = match self.step.to_ascii_uppercase() {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => 0,
        };
        let note = (i32::from(self.octave) + 1) * 12 + semitone + i32::from(self.alter);
        note.clamp(0, 127) as u8
    }
}

/// One `<note>`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Note {
    /// `None` for a rest (or an unpitched note).
    pub pitch: Option<Pitch>,
    /// Length in divisions (see [`Attributes::divisions`]).
    pub duration: u32,
    pub voice: Option<u8>,
    pub staff: Option<u8>,
    /// Part of a chord: it sounds together with the previous note.
    pub chord: bool,
    pub rest: bool,
}

/// Why a document could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Xml(String),
    TooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Xml(message) => write!(f, "MusicXML error: {message}"),
            Error::TooLarge => write!(f, "MusicXML document is larger than the reader accepts"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads an uncompressed MusicXML document.
pub fn parse(xml: &str) -> Result<Score, Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut score = Score::default();
    let mut stack: Vec<String> = Vec::new();
    let mut part_names: HashMap<String, String> = HashMap::new();
    let mut score_part: Option<String> = None;
    let mut part_name = String::new();
    let mut current_part: Option<usize> = None;
    let mut current_measure: Option<usize> = None;
    let mut attributes = Attributes::default();
    let mut in_note = false;
    let mut note = Note::default();
    let mut events = 0usize;
    let mut notes = 0usize;
    let mut saw_root = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| Error::Xml(error.to_string()))?;
        events += 1;
        if events > MAX_EVENTS {
            return Err(Error::TooLarge);
        }
        match event {
            Event::Start(element) => {
                let name = local_name(element.name().as_ref()).to_string();
                if !saw_root {
                    if name != "score-partwise" {
                        return Err(Error::Xml(format!(
                            "expected a score-partwise root, found `{name}`"
                        )));
                    }
                    saw_root = true;
                }
                match name.as_str() {
                    "score-part" => {
                        score_part = attribute(&element, "id");
                        part_name.clear();
                    }
                    "part" => {
                        let id = attribute(&element, "id").unwrap_or_default();
                        let name = part_names.get(&id).cloned().unwrap_or_else(|| id.clone());
                        if score.parts.len() >= MAX_PARTS {
                            return Err(Error::TooLarge);
                        }
                        score.parts.push(Part {
                            id,
                            name,
                            measures: Vec::new(),
                        });
                        current_part = Some(score.parts.len() - 1);
                        current_measure = None;
                    }
                    "measure" => {
                        if let Some(part) = current_part {
                            let number = attribute(&element, "number").unwrap_or_default();
                            let part = &mut score.parts[part];
                            if part.measures.len() >= MAX_MEASURES {
                                return Err(Error::TooLarge);
                            }
                            part.measures.push(Measure {
                                number,
                                ..Measure::default()
                            });
                            current_measure = Some(part.measures.len() - 1);
                        }
                    }
                    "attributes" => attributes = Attributes::default(),
                    "note" => {
                        in_note = true;
                        note = Note::default();
                    }
                    "rest" if in_note => note.rest = true,
                    "chord" if in_note => note.chord = true,
                    "sound" => set_tempo(&mut score, current_part, current_measure, &element),
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    "rest" if in_note => note.rest = true,
                    "chord" if in_note => note.chord = true,
                    "sound" => set_tempo(&mut score, current_part, current_measure, &element),
                    _ => {}
                }
            }
            Event::Text(text) => {
                let text = text.xml10_content();
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                match stack.last().map(String::as_str) {
                    Some("work-title" | "movement-title") => {
                        score.title = Some(text.to_string());
                    }
                    Some("part-name") => part_name = text.to_string(),
                    Some("divisions") => attributes.divisions = text.parse().ok(),
                    Some("fifths") => attributes.fifths = text.parse().ok(),
                    Some("beats") => attributes.beats = text.parse().ok(),
                    Some("beat-type") => attributes.beat_type = text.parse().ok(),
                    Some("step") if in_note => {
                        if let Some(pitch) = note.pitch.as_mut() {
                            pitch.step = text.chars().next().unwrap_or('C');
                        } else {
                            note.pitch = Some(Pitch {
                                step: text.chars().next().unwrap_or('C'),
                                ..Pitch::default()
                            });
                        }
                    }
                    Some("alter") if in_note => {
                        if let Some(pitch) = note.pitch.as_mut() {
                            pitch.alter = text.parse().unwrap_or(0);
                        }
                    }
                    Some("octave") if in_note => {
                        if let Some(pitch) = note.pitch.as_mut() {
                            pitch.octave = text.parse().unwrap_or(4);
                        }
                    }
                    Some("duration") if in_note => note.duration = text.parse().unwrap_or(0),
                    Some("voice") if in_note => note.voice = text.parse().ok(),
                    Some("staff") if in_note => note.staff = text.parse().ok(),
                    _ => {}
                }
            }
            Event::End(element) => {
                let name = local_name(element.name().as_ref()).to_string();
                match name.as_str() {
                    "attributes" => {
                        if let (Some(part), Some(measure)) = (current_part, current_measure) {
                            if let Some(measure) = score.parts[part].measures.get_mut(measure) {
                                measure.attributes = Some(attributes);
                            }
                        }
                    }
                    "note" if in_note => {
                        in_note = false;
                        notes += 1;
                        if notes > MAX_NOTES {
                            return Err(Error::TooLarge);
                        }
                        if let (Some(part), Some(measure)) = (current_part, current_measure) {
                            if let Some(measure) = score.parts[part].measures.get_mut(measure) {
                                measure.notes.push(std::mem::take(&mut note));
                            }
                        }
                    }
                    "part" => {
                        current_part = None;
                        current_measure = None;
                    }
                    "part-name" => {
                        if let Some(id) = score_part.take() {
                            if !part_name.is_empty() {
                                part_names.insert(id, part_name.clone());
                            }
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::Eof => {
                if !saw_root {
                    return Err(Error::Xml("not a MusicXML score".to_string()));
                }
                break;
            }
            _ => {}
        }
    }

    // A part that began before its name was known (invalid order) still gets the name.
    for part in &mut score.parts {
        if part.name == part.id {
            if let Some(name) = part_names.get(&part.id) {
                part.name = name.clone();
            }
        }
    }
    Ok(score)
}

fn set_tempo(
    score: &mut Score,
    part: Option<usize>,
    measure: Option<usize>,
    element: &BytesStart<'_>,
) {
    let Some(tempo) = attribute_parse::<f64>(element, "tempo") else {
        return;
    };
    if !tempo.is_finite() || tempo <= 0.0 {
        return;
    }
    if let (Some(part), Some(measure)) = (part, measure) {
        if let Some(measure) = score.parts[part].measures.get_mut(measure) {
            measure.tempo = Some(tempo);
        }
    }
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| attribute.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(|value| value.into_owned())
}

fn attribute_parse<T: std::str::FromStr>(element: &BytesStart<'_>, key: &str) -> Option<T> {
    attribute(element, key)?.trim().parse().ok()
}

/// The local part of a possibly namespaced element name.
fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn pitch_to_midi() {
        assert_eq!(
            Pitch {
                step: 'C',
                alter: 0,
                octave: 4
            }
            .midi(),
            60
        );
        assert_eq!(
            Pitch {
                step: 'F',
                alter: 1,
                octave: 4
            }
            .midi(),
            66
        );
        assert_eq!(
            Pitch {
                step: 'B',
                alter: -1,
                octave: 3
            }
            .midi(),
            58
        );
    }

    #[test]
    fn namespaced_elements_are_read() {
        let score = parse(
            r#"<m:score-partwise xmlns:m="urn:test"><m:part-list><m:score-part id="P1"><m:part-name>Piano</m:part-name></m:score-part></m:part-list><m:part id="P1"><m:measure number="1"><m:note><m:pitch><m:step>C</m:step><m:octave>4</m:octave></m:pitch><m:duration>1</m:duration></m:note></m:measure></m:part></m:score-partwise>"#,
        )
        .unwrap();
        assert_eq!(score.parts[0].name, "Piano");
        assert_eq!(
            score.parts[0].measures[0].notes[0].pitch.unwrap().midi(),
            60
        );
    }
}
