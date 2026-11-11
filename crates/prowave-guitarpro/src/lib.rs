//! Guitar Pro 7/8 `.gp` tablature reading.
//!
//! A `.gp` file is a ZIP whose `Content/score.gpif` is the score as XML: metadata, the tempo
//! and time-signature track, and global tables of bars, voices, beats, notes and rhythms that
//! the tracks reference by id. This reader turns those tables into one [`Track`] per instrument,
//! with every bar in song order and every beat positioned in quarter notes, so the notes can be
//! placed on a Pro Wave score or piano roll.
//!
//! Logic Pro parity: Logic's Score editor reads MusicXML, not Guitar Pro files, so this crate is
//! not a Logic import path. It supplies the Guitar Pro half of issue #71's notation request for
//! Pro Wave's own notation window. Guitar Pro 6 `.gpx` files use a BCFZ container rather than a
//! ZIP and are refused with a message asking for a `.gp` export; `.gp3`/`.gp4`/`.gp5` (binary
//! formats) are not read either.
//!
//! Files are untrusted input: the reader caps the archive size, the decompressed score, the XML
//! events, the objects, the (track, master bar) cells, and the beats and note references built
//! from them, and never panics on malformed XML.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// The XML score inside a `.gp` archive.
const SCORE_ENTRY: &str = "Content/score.gpif";
/// The largest `score.gpif` the reader accepts.
const MAX_GPIF_BYTES: u64 = 64 * 1024 * 1024;
/// The largest `.gp` archive `read_path` loads into memory.
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EVENTS: usize = 20_000_000;
const MAX_OBJECTS: usize = 500_000;
/// The most (track, master bar) cells a score may have. The reader builds one bar per cell,
/// so this bounds the output size however the file's objects are arranged.
const MAX_BAR_CELLS: usize = 1_000_000;
/// The most beats the built score may hold. A voice's beat list can name the same beat many
/// times, and every reference becomes a beat in each bar that uses that voice, so the output is
/// bounded here rather than by the file's object counts.
const MAX_TAB_BEATS: usize = 2_000_000;
/// The most note references the built score may hold, counted the same way as beats.
const MAX_TAB_NOTES: usize = 4_000_000;

/// A Guitar Pro score.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Score {
    /// The Guitar Pro version that wrote the file (`GPVersion`, e.g. `8.1.4`).
    pub gp_version: String,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub album: String,
    pub words: String,
    pub music: String,
    pub copyright: String,
    pub tabber: String,
    pub instructions: String,
    /// The first tempo in beats per minute, 120 when the file does not say.
    pub tempo: f64,
    pub master_bars: Vec<MasterBar>,
    pub tracks: Vec<Track>,
}

impl Score {
    /// How many notes every track holds together.
    pub fn note_count(&self) -> usize {
        self.tracks
            .iter()
            .map(|track| {
                track
                    .bars
                    .iter()
                    .flat_map(|bar| bar.beats.iter())
                    .map(|beat| beat.notes.len())
                    .sum::<usize>()
            })
            .sum()
    }
}

/// One bar of the score's master track (the bar line, not a track's notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterBar {
    pub time_signature: TimeSignature,
    pub key: KeySignature,
}

impl Default for MasterBar {
    fn default() -> Self {
        Self {
            time_signature: TimeSignature {
                beats: 4,
                beat_type: 4,
            },
            key: KeySignature {
                accidentals: 0,
                major: true,
            },
        }
    }
}

/// A time signature, numerator over denominator (4/4, 6/8, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSignature {
    pub beats: u8,
    pub beat_type: u8,
}

/// A key signature: sharps (positive) or flats (negative), and major or minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySignature {
    pub accidentals: i8,
    pub major: bool,
}

/// One instrument's staff, with its bars in song order.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The track's id in the file.
    pub id: u32,
    pub name: String,
    pub short_name: String,
    /// The instrument set's name (`InstrumentSet/Name`), e.g. `Flute`.
    pub instrument: String,
    /// One bar per master bar, in order. A bar with no notes has no beats.
    pub bars: Vec<TabBar>,
}

impl Track {
    /// How many notes the track holds.
    pub fn note_count(&self) -> usize {
        self.bars
            .iter()
            .flat_map(|bar| bar.beats.iter())
            .map(|beat| beat.notes.len())
            .sum()
    }
}

/// One bar of a track.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TabBar {
    /// Every beat in the bar, ordered by voice and then time.
    pub beats: Vec<TabBeat>,
}

/// One beat (a rhythm and the notes played together on it).
#[derive(Clone, Debug, PartialEq)]
pub struct TabBeat {
    /// Which voice the beat belongs to (0 to 3).
    pub voice: u8,
    /// Where the beat starts, in quarter notes from the bar line.
    pub position: f64,
    /// How long the beat lasts, in quarter notes (tuplets and dots included).
    pub duration: f64,
    pub notes: Vec<TabNote>,
}

/// One note of a beat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabNote {
    /// The MIDI note (the sounding pitch, not the written one).
    pub midi: u8,
    /// The guitar string, when the note is on a tablature staff.
    pub string: Option<u8>,
    /// The fret, when the note is on a tablature staff.
    pub fret: Option<u8>,
    /// Whether the note is let ring.
    pub let_ring: bool,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    /// The bytes are not a ZIP (and so not a `.gp` file).
    NotGuitarPro,
    /// A Guitar Pro 6 `.gpx`, whose BCFZ container this reader does not unpack.
    GpxContainer,
    /// The archive has no `Content/score.gpif`.
    MissingScore,
    TooLarge,
    Xml(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(message) => write!(f, "cannot read the Guitar Pro file: {message}"),
            Error::NotGuitarPro => f.write_str("the file is not a Guitar Pro 7/8 (.gp) archive"),
            Error::GpxContainer => f.write_str(
                "Guitar Pro 6 (.gpx) files use a BCFZ container; export the score as .gp",
            ),
            Error::MissingScore => f.write_str("the archive has no Content/score.gpif"),
            Error::TooLarge => {
                f.write_str("the Guitar Pro score is larger than the reader accepts")
            }
            Error::Xml(message) => write!(f, "Guitar Pro score error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads a `.gp` file from disk.
pub fn read_path(path: impl AsRef<Path>) -> Result<Score, Error> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?
        .len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(Error::TooLarge);
    }
    let bytes =
        std::fs::read(path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    read(&bytes)
}

/// Reads a `.gp` archive from its bytes.
pub fn read(bytes: &[u8]) -> Result<Score, Error> {
    if bytes.starts_with(b"BCFS") || bytes.starts_with(b"BCFZ") {
        return Err(Error::GpxContainer);
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| Error::NotGuitarPro)?;
    let mut entry = archive
        .by_name(SCORE_ENTRY)
        .map_err(|_| Error::MissingScore)?;
    // The declared size is checked first, but the archive can lie about it, so the decompressed
    // output is also read through a limit of one byte past the cap and checked afterwards.
    if entry.size() > MAX_GPIF_BYTES {
        return Err(Error::TooLarge);
    }
    let mut xml = String::new();
    entry
        .by_ref()
        .take(MAX_GPIF_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(|error| Error::Io(error.to_string()))?;
    if xml.len() as u64 > MAX_GPIF_BYTES {
        return Err(Error::TooLarge);
    }
    parse(&xml)
}

/// Reads the `score.gpif` XML.
pub fn parse(xml: &str) -> Result<Score, Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut state = State::default();
    let mut events = 0usize;
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
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if !saw_root {
                    if name != "GPIF" {
                        return Err(Error::Xml(format!("expected a GPIF root, found `{name}`")));
                    }
                    saw_root = true;
                }
                state.start(name, &element)?;
                state.stack.push(name.to_string());
            }
            Event::Empty(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if !saw_root && name == "GPIF" {
                    saw_root = true;
                }
                state.empty(name, &element);
            }
            Event::Text(text) => state.text(&text.xml10_content()),
            Event::CData(text) => state.text(&text.xml10_content()),
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                state.end(name)?;
                state.stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err(Error::Xml("not a Guitar Pro score".to_string()));
    }
    if !state.stack.is_empty() {
        return Err(Error::Xml(
            "the document ends before its elements close".to_string(),
        ));
    }
    // `into_score` builds one bar per track and master bar, so refuse the score before that.
    if state.tracks.len().saturating_mul(state.master_bars.len()) > MAX_BAR_CELLS {
        return Err(Error::TooLarge);
    }
    state.into_score()
}

#[derive(Default)]
struct RawNote {
    midi: Option<u8>,
    string: Option<u8>,
    fret: Option<u8>,
    let_ring: bool,
    step: Option<char>,
    accidental: String,
    octave: Option<i8>,
}

#[derive(Default)]
struct RawBar {
    voices: Vec<i32>,
}

#[derive(Default)]
struct RawVoice {
    beats: Vec<u32>,
}

#[derive(Default)]
struct RawBeat {
    rhythm: Option<u32>,
    notes: Vec<u32>,
}

#[derive(Default)]
struct RawRhythm {
    note_value: String,
    dots: u8,
    tuplet_num: u32,
    tuplet_den: u32,
}

#[derive(Default)]
struct RawMasterBar {
    time: TimeSignature,
    key: KeySignature,
    /// The bar id in each track, in the master track's track order.
    bars: Vec<u32>,
}

#[derive(Default)]
struct RawTrack {
    id: u32,
    name: String,
    short_name: String,
    instrument: String,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            beats: 4,
            beat_type: 4,
        }
    }
}

impl Default for KeySignature {
    fn default() -> Self {
        Self {
            accidentals: 0,
            major: true,
        }
    }
}

#[derive(Default)]
struct State {
    stack: Vec<String>,
    gp_version: String,
    title: String,
    subtitle: String,
    artist: String,
    album: String,
    words: String,
    music: String,
    copyright: String,
    tabber: String,
    instructions: String,
    tempo: Option<f64>,
    in_score: bool,
    in_master_track: bool,
    master_order: Vec<u32>,
    in_master_bar: bool,
    master_bar: RawMasterBar,
    in_automation: bool,
    automation_type: String,
    in_track: bool,
    track_id: Option<u32>,
    track: RawTrack,
    in_bar: bool,
    bar_id: Option<u32>,
    bar: RawBar,
    in_voice: bool,
    voice_id: Option<u32>,
    voice: RawVoice,
    in_beat: bool,
    beat_id: Option<u32>,
    beat: RawBeat,
    in_note: bool,
    note_id: Option<u32>,
    note: RawNote,
    note_property: Option<String>,
    in_rhythm: bool,
    rhythm_id: Option<u32>,
    rhythm: RawRhythm,
    tracks: Vec<RawTrack>,
    master_bars: Vec<RawMasterBar>,
    bars: Vec<Option<RawBar>>,
    voices: Vec<Option<RawVoice>>,
    beats: Vec<Option<RawBeat>>,
    notes: Vec<Option<RawNote>>,
    rhythms: Vec<Option<RawRhythm>>,
}

impl State {
    fn start(&mut self, name: &str, element: &BytesStart<'_>) -> Result<(), Error> {
        // `start` runs before the element is pushed, so the stack's top is its parent.
        let parent = self.stack.last().map(String::as_str);
        match name {
            "Score" if parent == Some("GPIF") => self.in_score = true,
            "MasterTrack" if parent == Some("GPIF") => self.in_master_track = true,
            "MasterBar" if parent == Some("MasterBars") => {
                self.in_master_bar = true;
                self.master_bar = RawMasterBar::default();
            }
            "Automation" if parent == Some("Automations") && self.in_master_track => {
                self.in_automation = true;
                self.automation_type.clear();
            }
            "Track" if parent == Some("Tracks") => {
                self.in_track = true;
                self.track_id = attribute_parse(element, "id");
                self.track = RawTrack::default();
            }
            "Bar" if parent == Some("Bars") => {
                self.in_bar = true;
                self.bar_id = attribute_parse(element, "id");
                self.bar = RawBar::default();
            }
            "Voice" if parent == Some("Voices") => {
                self.in_voice = true;
                self.voice_id = attribute_parse(element, "id");
                self.voice = RawVoice::default();
            }
            "Beat" if parent == Some("Beats") => {
                self.in_beat = true;
                self.beat_id = attribute_parse(element, "id");
                self.beat = RawBeat::default();
            }
            "Note" if parent == Some("Notes") => {
                self.in_note = true;
                self.note_id = attribute_parse(element, "id");
                self.note = RawNote::default();
                self.note_property = None;
            }
            "Rhythm" if parent == Some("Rhythms") => {
                self.in_rhythm = true;
                self.rhythm_id = attribute_parse(element, "id");
                self.rhythm = RawRhythm::default();
            }
            "Property" if self.in_note => {
                self.note_property = attribute(element, "name");
            }
            _ => {}
        }
        Ok(())
    }

    fn empty(&mut self, name: &str, element: &BytesStart<'_>) {
        if self.in_beat && name == "Rhythm" {
            self.beat.rhythm = attribute_parse(element, "ref");
        } else if self.in_rhythm && name == "AugmentationDot" {
            self.rhythm.dots = attribute_parse(element, "count").unwrap_or(1);
        } else if self.in_rhythm && name == "PrimaryTuplet" {
            self.rhythm.tuplet_num = attribute_parse(element, "num").unwrap_or(0);
            self.rhythm.tuplet_den = attribute_parse(element, "den").unwrap_or(0);
        } else if self.in_note && name == "LetRing" {
            self.note.let_ring = true;
        }
    }

    fn text(&mut self, raw: &str) {
        let text = raw.trim();
        if text.is_empty() {
            return;
        }
        let name = self.stack.last().map(String::as_str).unwrap_or("");
        // The top of the stack is the element the text belongs to; its parent is one below.
        let parent = self
            .stack
            .len()
            .checked_sub(2)
            .and_then(|index| self.stack.get(index))
            .map(String::as_str);
        if self.in_note {
            match (self.note_property.as_deref(), name) {
                (Some("Midi"), "Number") => self.note.midi = text.parse().ok(),
                (Some("Fret"), "Fret") => self.note.fret = text.parse().ok(),
                (Some("String"), "String") => self.note.string = text.parse().ok(),
                (Some("ConcertPitch"), "Step") => self.note.step = text.chars().next(),
                (Some("ConcertPitch"), "Accidental") => {
                    self.note.accidental = text.to_string();
                }
                (Some("ConcertPitch"), "Octave") => self.note.octave = text.parse().ok(),
                _ => {}
            }
        } else if self.in_beat {
            if name == "Notes" {
                self.beat.notes = parse_ids(text);
            }
        } else if self.in_voice {
            if name == "Beats" {
                self.voice.beats = parse_ids(text);
            }
        } else if self.in_bar {
            if name == "Voices" {
                self.bar.voices = text
                    .split_whitespace()
                    .filter_map(|value| value.parse().ok())
                    .collect();
            }
        } else if self.in_rhythm {
            if name == "NoteValue" {
                self.rhythm.note_value = text.to_string();
            }
        } else if self.in_master_bar {
            match name {
                "Time" => {
                    if let Some(time) = parse_time(text) {
                        self.master_bar.time = time;
                    }
                }
                "AccidentalCount" => {
                    self.master_bar.key.accidentals = text.parse().unwrap_or(0);
                }
                "Mode" => self.master_bar.key.major = text == "Major",
                "Bars" if parent == Some("MasterBar") => {
                    self.master_bar.bars = parse_ids(text);
                }
                _ => {}
            }
        } else if self.in_automation {
            match name {
                "Type" => self.automation_type = text.to_string(),
                "Value" if self.automation_type == "Tempo" && self.tempo.is_none() => {
                    if let Some(bpm) = text.split_whitespace().next() {
                        self.tempo = bpm.parse().ok();
                    }
                }
                _ => {}
            }
        } else if self.in_track {
            match (parent, name) {
                (Some("Track"), "Name") => self.track.name = text.to_string(),
                (Some("Track"), "ShortName") => self.track.short_name = text.to_string(),
                (Some("InstrumentSet"), "Name") => self.track.instrument = text.to_string(),
                _ => {}
            }
        } else if self.in_master_track {
            if name == "Tracks" && parent == Some("MasterTrack") {
                self.master_order = parse_ids(text);
            }
        } else if self.in_score {
            match name {
                "Title" => self.title = text.to_string(),
                "SubTitle" => self.subtitle = text.to_string(),
                "Artist" => self.artist = text.to_string(),
                "Album" => self.album = text.to_string(),
                "Words" => self.words = text.to_string(),
                "Music" => self.music = text.to_string(),
                "Copyright" => self.copyright = text.to_string(),
                "Tabber" => self.tabber = text.to_string(),
                "Instructions" => self.instructions = text.to_string(),
                _ => {}
            }
        } else if parent == Some("GPIF") && name == "GPVersion" {
            self.gp_version = text.to_string();
        }
    }

    fn end(&mut self, name: &str) -> Result<(), Error> {
        match name {
            "Score" => self.in_score = false,
            "MasterTrack" => self.in_master_track = false,
            "MasterBar" if self.in_master_bar => {
                self.in_master_bar = false;
                if self.master_bars.len() >= MAX_OBJECTS {
                    return Err(Error::TooLarge);
                }
                self.master_bars.push(std::mem::take(&mut self.master_bar));
            }
            "Automation" if self.in_automation => {
                self.in_automation = false;
                self.automation_type.clear();
            }
            "Track" if self.in_track => {
                self.in_track = false;
                let mut track = std::mem::take(&mut self.track);
                track.id = self.track_id.take().unwrap_or(self.tracks.len() as u32);
                if self.tracks.len() >= MAX_OBJECTS {
                    return Err(Error::TooLarge);
                }
                self.tracks.push(track);
            }
            "Bar" if self.in_bar => {
                self.in_bar = false;
                put(
                    &mut self.bars,
                    self.bar_id.take(),
                    std::mem::take(&mut self.bar),
                )?;
            }
            "Voice" if self.in_voice => {
                self.in_voice = false;
                put(
                    &mut self.voices,
                    self.voice_id.take(),
                    std::mem::take(&mut self.voice),
                )?;
            }
            "Beat" if self.in_beat => {
                self.in_beat = false;
                put(
                    &mut self.beats,
                    self.beat_id.take(),
                    std::mem::take(&mut self.beat),
                )?;
            }
            "Note" if self.in_note => {
                self.in_note = false;
                self.note_property = None;
                put(
                    &mut self.notes,
                    self.note_id.take(),
                    std::mem::take(&mut self.note),
                )?;
            }
            "Rhythm" if self.in_rhythm => {
                self.in_rhythm = false;
                put(
                    &mut self.rhythms,
                    self.rhythm_id.take(),
                    std::mem::take(&mut self.rhythm),
                )?;
            }
            "Property" => self.note_property = None,
            _ => {}
        }
        Ok(())
    }

    fn into_score(self) -> Result<Score, Error> {
        let by_id: HashMap<u32, usize> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (track.id, index))
            .collect();
        let mut order: Vec<usize> = self
            .master_order
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        let mut seen = vec![false; self.tracks.len()];
        for &index in &order {
            if let Some(seen) = seen.get_mut(index) {
                *seen = true;
            }
        }
        order.extend(
            (0..self.tracks.len()).filter(|index| !seen.get(*index).copied().unwrap_or(true)),
        );

        let mut tracks = Vec::with_capacity(order.len());
        let mut beat_count = 0usize;
        let mut note_count = 0usize;
        for (slot, &track_index) in order.iter().enumerate() {
            let Some(raw) = self.tracks.get(track_index) else {
                continue;
            };
            let mut bars = Vec::with_capacity(self.master_bars.len());
            for master_bar in &self.master_bars {
                let mut bar = TabBar::default();
                let Some(&bar_id) = master_bar.bars.get(slot) else {
                    bars.push(bar);
                    continue;
                };
                let Some(Some(raw_bar)) = self.bars.get(bar_id as usize) else {
                    bars.push(bar);
                    continue;
                };
                for (voice_slot, &voice_id) in raw_bar.voices.iter().enumerate() {
                    if voice_id < 0 {
                        continue;
                    }
                    let Some(Some(raw_voice)) = self.voices.get(voice_id as usize) else {
                        continue;
                    };
                    let mut position = 0.0;
                    for &beat_id in &raw_voice.beats {
                        let Some(Some(raw_beat)) = self.beats.get(beat_id as usize) else {
                            continue;
                        };
                        // Check before allocating: the note count is the reference count, which
                        // is an upper bound on the notes this beat yields.
                        if beat_count >= MAX_TAB_BEATS
                            || note_count.saturating_add(raw_beat.notes.len()) > MAX_TAB_NOTES
                        {
                            return Err(Error::TooLarge);
                        }
                        beat_count += 1;
                        note_count += raw_beat.notes.len();
                        let duration = raw_beat
                            .rhythm
                            .and_then(|id| self.rhythms.get(id as usize))
                            .and_then(Option::as_ref)
                            .map(quarter_notes)
                            .unwrap_or(1.0);
                        let notes = raw_beat
                            .notes
                            .iter()
                            .filter_map(|id| self.notes.get(*id as usize))
                            .filter_map(Option::as_ref)
                            .map(|note| TabNote {
                                midi: note.midi.unwrap_or_else(|| note_pitch(note)),
                                string: note.string,
                                fret: note.fret,
                                let_ring: note.let_ring,
                            })
                            .collect();
                        bar.beats.push(TabBeat {
                            voice: u8::try_from(voice_slot).unwrap_or(0),
                            position,
                            duration,
                            notes,
                        });
                        position += duration;
                    }
                }
                bars.push(bar);
            }
            tracks.push(Track {
                id: raw.id,
                name: raw.name.clone(),
                short_name: raw.short_name.clone(),
                instrument: raw.instrument.clone(),
                bars,
            });
        }

        Ok(Score {
            gp_version: self.gp_version,
            title: self.title,
            subtitle: self.subtitle,
            artist: self.artist,
            album: self.album,
            words: self.words,
            music: self.music,
            copyright: self.copyright,
            tabber: self.tabber,
            instructions: self.instructions,
            tempo: self.tempo.unwrap_or(120.0),
            master_bars: self
                .master_bars
                .iter()
                .map(|master_bar| MasterBar {
                    time_signature: master_bar.time,
                    key: master_bar.key,
                })
                .collect(),
            tracks,
        })
    }
}

/// The length of a rhythm in quarter notes.
fn quarter_notes(rhythm: &RawRhythm) -> f64 {
    let mut beats = match rhythm.note_value.as_str() {
        "Whole" => 4.0,
        "Half" => 2.0,
        "Quarter" => 1.0,
        "Eighth" => 0.5,
        "16th" => 0.25,
        "32nd" => 0.125,
        "64th" => 0.0625,
        "128th" => 0.03125,
        _ => 1.0,
    };
    if rhythm.dots > 0 {
        let divisor = f64::from(1u32 << u32::from(rhythm.dots.min(4)));
        beats *= 2.0 - 1.0 / divisor;
    }
    if rhythm.tuplet_num > 0 && rhythm.tuplet_den > 0 {
        beats *= f64::from(rhythm.tuplet_den) / f64::from(rhythm.tuplet_num);
    }
    beats.max(0.0)
}

/// The MIDI note of a note without a `<Midi>` property, from its concert pitch.
fn note_pitch(note: &RawNote) -> u8 {
    let Some(step) = note.step else {
        return 60;
    };
    let semitone = match step.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return 60,
    };
    let alter: i32 = note
        .accidental
        .chars()
        .map(|character| match character {
            '#' => 1,
            'b' | 'B' => -1,
            _ => 0,
        })
        .sum();
    let octave = i32::from(note.octave.unwrap_or(4));
    u8::try_from((octave + 1) * 12 + semitone + alter)
        .ok()
        .filter(|midi| *midi <= 127)
        .unwrap_or(60)
}

fn parse_ids(text: &str) -> Vec<u32> {
    text.split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect()
}

fn parse_time(text: &str) -> Option<TimeSignature> {
    let (beats, beat_type) = text.split_once('/')?;
    Some(TimeSignature {
        beats: beats.trim().parse().ok()?,
        beat_type: beat_type.trim().parse().ok()?,
    })
}

/// Stores an object in a table indexed by its file id.
fn put<T>(table: &mut Vec<Option<T>>, id: Option<u32>, value: T) -> Result<(), Error> {
    let index = id.map_or(table.len(), |id| id as usize);
    if index >= MAX_OBJECTS {
        return Err(Error::TooLarge);
    }
    if table.len() <= index {
        table.resize_with(index + 1, || None);
    }
    table[index] = Some(value);
    Ok(())
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
