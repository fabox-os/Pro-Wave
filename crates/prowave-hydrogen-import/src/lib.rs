//! Hydrogen `.h2song` session reading.
//!
//! A Hydrogen song is an XML document naming the drum kit, the song tempo, the kit's
//! instruments, its patterns, and the order the patterns play in. Pro Wave already plays a
//! Hydrogen drum kit (`prowave-engine`'s `hydrogen` module); this reader supplies the other half
//! of issue #81, the session: which instruments exist, what every pattern plays, and the song
//! sequence.
//!
//! Logic Pro parity: Logic does not import `.h2song` files. The reader serves Pro Wave's own
//! drum track: it turns a Hydrogen session's patterns into MIDI notes (one instrument per note,
//! as Hydrogen does), so a song made in Hydrogen can be arranged here. Logic's Drummer and
//! Ultrabeat are only the arrangement reference.
//!
//! The reader accepts the newer `1.2` layout (`<patternList>`/`<patternSequence>` with patterns
//! named by the sequence) and the older layout (`<pattern id="…">` with numeric `<patternID>`).
//! It is deliberately a model of the musical content: mixer settings beyond instrument volume,
//! pan and mute, and the audio engine's settings, are not kept.
//!
//! Hydrogen files are untrusted input: the reader caps the event and object counts and never
//! panics on malformed XML.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// The largest `.h2song` the reader accepts.
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_EVENTS: usize = 20_000_000;
const MAX_INSTRUMENTS: usize = 1024;
const MAX_PATTERNS: usize = 4096;
const MAX_NOTES_PER_PATTERN: usize = 100_000;
const MAX_TOTAL_NOTES: usize = 1_000_000;
const MAX_SEQUENCE: usize = 1_000_000;

/// A Hydrogen song as its `.h2song` describes it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Song {
    /// The Hydrogen version that wrote the file (`<version>`).
    pub version: String,
    pub name: String,
    pub author: String,
    pub license: String,
    /// Beats per minute (`<bpm>`), 120 when the file does not say.
    pub tempo: f64,
    /// The drum kit's name, from `<drumkit_name>` (or the first instrument's `<drumkit>`).
    pub drumkit: String,
    pub instruments: Vec<Instrument>,
    pub patterns: Vec<Pattern>,
    /// Indices into [`Song::patterns`], in playback order. Sequence entries that name no
    /// pattern are skipped.
    pub sequence: Vec<usize>,
}

impl Song {
    /// How many notes every pattern holds together.
    pub fn note_count(&self) -> usize {
        self.patterns
            .iter()
            .map(|pattern| pattern.notes.len())
            .sum()
    }

    /// The pattern played at song position `step`, when the sequence reaches there.
    pub fn pattern_at(&self, step: usize) -> Option<&Pattern> {
        self.sequence
            .get(step)
            .and_then(|&index| self.patterns.get(index))
    }
}

/// One kit instrument (a Hydrogen `<instrument>` in the song's `instrumentList`).
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    /// The index patterns use in a note's `<instrument>`.
    pub id: u32,
    pub name: String,
    /// The drum kit the instrument's samples come from, when the song names one.
    pub drumkit: String,
    /// The MIDI note the kit plays it on, when it has one.
    pub midi_note: Option<u8>,
    pub volume: f32,
    pub pan: f32,
    pub gain: f32,
    pub muted: bool,
}

impl Default for Instrument {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            drumkit: String::new(),
            midi_note: None,
            volume: 1.0,
            pan: 0.0,
            gain: 1.0,
            muted: false,
        }
    }
}

/// One pattern: a loop of notes on a grid of [`Pattern::size`] ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub name: String,
    /// The grid resolution in ticks (`<size>`), 192 when the file does not say.
    pub size: u32,
    /// The note value a whole pattern represents (`<denominator>`), 4 when absent.
    pub denominator: u32,
    pub notes: Vec<Note>,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            name: String::new(),
            size: 192,
            denominator: 4,
            notes: Vec::new(),
        }
    }
}

/// One note in a pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// The tick within the pattern at which the note starts.
    pub position: u32,
    /// 0 to 1.
    pub velocity: f32,
    /// Length in ticks; -1 means the instrument's own length.
    pub length: i32,
    /// The [`Instrument::id`] that plays the note.
    pub instrument: u32,
    /// A pitch offset (the kit sample is transposed, not a different MIDI note).
    pub pitch: f32,
    pub pan: f32,
    /// The chance (0 to 1) the note plays, for Hydrogen's random patterns.
    pub probability: f32,
    /// Timing offset in ticks, positive or negative.
    pub lead_lag: f32,
    /// Whether Hydrogen marks this as a note-off that stops a longer note.
    pub note_off: bool,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            position: 0,
            velocity: 1.0,
            length: -1,
            instrument: 0,
            pitch: 0.0,
            pan: 0.0,
            probability: 1.0,
            lead_lag: 0.0,
            note_off: false,
        }
    }
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    Encoding,
    TooLarge,
    Xml(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(message) => write!(f, "cannot read the Hydrogen song: {message}"),
            Error::Encoding => f.write_str("the Hydrogen song is not UTF-8"),
            Error::TooLarge => f.write_str("the Hydrogen song is larger than the reader accepts"),
            Error::Xml(message) => write!(f, "Hydrogen song error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads a `.h2song` file from disk.
pub fn read_path(path: impl AsRef<Path>) -> Result<Song, Error> {
    let path = path.as_ref();
    // The size is checked before the file is read, so an oversized one is refused without
    // being loaded into memory.
    let len = std::fs::metadata(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?
        .len();
    if len > MAX_BYTES as u64 {
        return Err(Error::TooLarge);
    }
    let bytes =
        std::fs::read(path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    read(&bytes)
}

/// Reads a `.h2song` document from its bytes.
pub fn read(bytes: &[u8]) -> Result<Song, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    let xml = std::str::from_utf8(bytes).map_err(|_| Error::Encoding)?;
    parse(xml)
}

/// Reads a `.h2song` document from text.
pub fn parse(xml: &str) -> Result<Song, Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut song = Song {
        tempo: 120.0,
        ..Song::default()
    };
    let mut stack: Vec<String> = Vec::new();
    let mut saw_root = false;
    let mut in_instrument = false;
    let mut instrument = Instrument::default();
    let mut instrument_id: Option<u32> = None;
    let mut in_pattern = false;
    let mut pattern = Pattern::default();
    let mut pattern_id: Option<String> = None;
    let mut pattern_ids: Vec<Option<String>> = Vec::new();
    let mut in_note = false;
    let mut note = Note::default();
    let mut sequence_refs: Vec<String> = Vec::new();
    let mut total_notes = 0usize;
    let mut events = 0usize;

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
                    if name != "song" && name != "hydrogen_song_file" {
                        return Err(Error::Xml(format!("expected a song root, found `{name}`")));
                    }
                    saw_root = true;
                }
                let parent = stack.last().map(String::as_str);
                match name.as_str() {
                    "instrument" if parent == Some("instrumentList") => {
                        if song.instruments.len() >= MAX_INSTRUMENTS {
                            return Err(Error::TooLarge);
                        }
                        in_instrument = true;
                        instrument = Instrument::default();
                        instrument_id = None;
                    }
                    "pattern" if parent == Some("patternList") || parent == Some("patterns") => {
                        if song.patterns.len() >= MAX_PATTERNS {
                            return Err(Error::TooLarge);
                        }
                        in_pattern = true;
                        pattern = Pattern::default();
                        pattern_id = attribute(&element, "id");
                    }
                    "note" if in_pattern => {
                        if pattern.notes.len() >= MAX_NOTES_PER_PATTERN {
                            return Err(Error::TooLarge);
                        }
                        in_note = true;
                        note = Note::default();
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                // Empty fields carry no value; only the root check matters here.
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if !saw_root && name == "song" {
                    saw_root = true;
                }
            }
            Event::Text(text) => {
                let text = text.xml10_content();
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let name = stack.last().map(String::as_str).unwrap_or("");
                // Song metadata are the `<song>` element's own children. Later sections such as
                // `<ladspa><fx><name>` and `<componentList>` have `<name>` elements too.
                let parent = stack
                    .len()
                    .checked_sub(2)
                    .and_then(|index| stack.get(index))
                    .map(String::as_str);
                if name == "patternID" {
                    if sequence_refs.len() >= MAX_SEQUENCE {
                        return Err(Error::TooLarge);
                    }
                    sequence_refs.push(text.to_string());
                } else if in_note {
                    match name {
                        "position" => note.position = value_or(text, 0),
                        "velocity" => note.velocity = value_or(text, 1.0),
                        "length" => note.length = value_or(text, -1),
                        "instrument" => note.instrument = value_or(text, 0),
                        "pitch" => note.pitch = value_or(text, 0.0),
                        "pan" => note.pan = value_or(text, 0.0),
                        "probability" => note.probability = value_or(text, 1.0),
                        "leadlag" => note.lead_lag = value_or(text, 0.0),
                        "note_off" => note.note_off = parse_bool(text),
                        _ => {}
                    }
                } else if in_pattern {
                    match name {
                        "name" => pattern.name = text.to_string(),
                        "size" => pattern.size = value_or(text, 192),
                        "denominator" => pattern.denominator = value_or(text, 4),
                        "id" => pattern_id = Some(text.to_string()),
                        _ => {}
                    }
                } else if in_instrument {
                    match name {
                        "id" => instrument_id = text.parse().ok(),
                        "name" => instrument.name = text.to_string(),
                        "drumkit" => instrument.drumkit = text.to_string(),
                        "midiOutNote" => {
                            instrument.midi_note = text
                                .parse::<i32>()
                                .ok()
                                .filter(|n| (0..=127).contains(n))
                                .map(|n| u8::try_from(n).unwrap_or(0));
                        }
                        "volume" => instrument.volume = value_or(text, 1.0),
                        "pan" => instrument.pan = value_or(text, 0.0),
                        "gain" => instrument.gain = value_or(text, 1.0),
                        "isMuted" => instrument.muted = parse_bool(text),
                        _ => {}
                    }
                } else if parent == Some("song") {
                    match name {
                        "version" => song.version = text.to_string(),
                        "name" => song.name = text.to_string(),
                        "author" => song.author = text.to_string(),
                        "license" => song.license = text.to_string(),
                        "bpm" => song.tempo = value_or(text, 120.0),
                        "drumkit_name" => song.drumkit = text.to_string(),
                        "drumkit" if song.drumkit.is_empty() => {
                            song.drumkit = text.to_string();
                        }
                        _ => {}
                    }
                }
            }
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    "note" if in_note => {
                        in_note = false;
                        total_notes += 1;
                        if total_notes > MAX_TOTAL_NOTES {
                            return Err(Error::TooLarge);
                        }
                        pattern.notes.push(std::mem::take(&mut note));
                    }
                    "pattern" if in_pattern => {
                        in_pattern = false;
                        pattern_ids.push(pattern_id.take());
                        song.patterns.push(std::mem::take(&mut pattern));
                    }
                    "instrument" if in_instrument => {
                        in_instrument = false;
                        instrument.id = instrument_id
                            .take()
                            .unwrap_or(song.instruments.len() as u32);
                        if song.drumkit.is_empty() && !instrument.drumkit.is_empty() {
                            song.drumkit = instrument.drumkit.clone();
                        }
                        song.instruments.push(std::mem::take(&mut instrument));
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err(Error::Xml("not a Hydrogen song".to_string()));
    }
    if !stack.is_empty() {
        return Err(Error::Xml(
            "the document ends before its elements close".to_string(),
        ));
    }

    song.sequence = resolve_sequence(&song.patterns, &pattern_ids, &sequence_refs);
    Ok(song)
}

/// Turns the sequence's pattern references into indices. A reference first matches a pattern's
/// `id`, then its name, then (for older files) its 1-based position.
fn resolve_sequence(
    patterns: &[Pattern],
    pattern_ids: &[Option<String>],
    references: &[String],
) -> Vec<usize> {
    let by_id: HashMap<&str, usize> = pattern_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| id.as_deref().map(|id| (id, index)))
        .collect();
    let by_name: HashMap<&str, usize> = patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| (pattern.name.as_str(), index))
        .collect();
    let mut sequence = Vec::with_capacity(references.len());
    for reference in references {
        let index = by_id
            .get(reference.as_str())
            .copied()
            .or_else(|| by_name.get(reference.as_str()).copied())
            .or_else(|| {
                reference
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|number| (1..=patterns.len()).contains(number))
                    .map(|number| number - 1)
            });
        if let Some(index) = index {
            sequence.push(index);
        }
    }
    sequence
}

fn value_or<T: std::str::FromStr>(text: &str, default: T) -> T {
    text.trim().parse().unwrap_or(default)
}

fn parse_bool(text: &str) -> bool {
    matches!(text.trim(), "true" | "TRUE" | "1")
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| attribute.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(|value| value.into_owned())
}

/// The local part of a possibly namespaced element name.
fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_references_resolve_by_id_name_and_position() {
        let patterns = vec![
            Pattern {
                name: "first".to_string(),
                ..Pattern::default()
            },
            Pattern {
                name: "second".to_string(),
                ..Pattern::default()
            },
        ];
        let ids = vec![Some("7".to_string()), None];
        let resolved = resolve_sequence(
            &patterns,
            &ids,
            &["7".to_string(), "second".to_string(), "1".to_string()],
        );
        assert_eq!(resolved, vec![0, 1, 0]);
        assert!(resolve_sequence(&patterns, &ids, &["nope".to_string()]).is_empty());
    }

    #[test]
    fn an_oversized_file_is_refused_without_being_read() {
        let path = std::env::temp_dir().join(format!(
            "prowave-hydrogen-too-big-{}.h2song",
            std::process::id()
        ));
        let Ok(file) = std::fs::File::create(&path) else {
            panic!("cannot make {}", path.display());
        };
        // A sparse file: longer than the reader accepts, with nothing written.
        assert!(file.set_len(MAX_BYTES as u64 + 1).is_ok());
        drop(file);
        let result = read_path(&path);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(result, Err(Error::TooLarge)));
    }
}
