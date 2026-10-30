//! ProWave project file (`.prowave`), Logic Pro equivalent: the `.logicx` project package.
//!
//! A `.prowave` file is a zip archive:
//!
//! ```text
//! project.json      the arrangement (tracks, regions, notes, mixer, transport), see ProjectDoc
//! media/…           audio files used by audio regions, copied in when the project is saved
//! ```
//!
//! The roadmap mentions rkyv for `project.json`; this first version uses plain JSON (serde) so
//! the file stays human-readable and diffable. Every struct uses `#[serde(default)]`, so older
//! files keep loading when fields are added; files from a newer format version are rejected.
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Bumped when the document changes in a way older readers cannot handle.
pub const FORMAT_VERSION: u32 = 1;
pub const PROJECT_JSON: &str = "project.json";
pub const MEDIA_DIR: &str = "media/";
pub const EXTENSION: &str = "prowave";
/// Largest `project.json` accepted when loading (guards against zip bombs).
pub const MAX_JSON_BYTES: u64 = 64 << 20;
/// Most media data extracted from one project (guards against zip bombs filling the disk).
pub const MAX_MEDIA_BYTES: u64 = 16 << 30;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrackKindDoc {
    #[default]
    Instrument,
    Audio,
    Drummer,
    Bus,
    Output,
    /// Auxiliary channel strip: receives from a bus, has no regions.
    Aux,
    /// Folder (track stack): groups the tracks nested under it, has no sound of its own.
    Folder,
    /// VCA: a fader (with Mute and Solo) that moves the level of the tracks assigned to it.
    Vca,
    /// External MIDI: plays its notes on a MIDI port and channel instead of in the app.
    ExternalMidi,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct NoteDoc {
    /// Beats from the region start.
    pub start: f32,
    pub len: f32,
    pub pitch: u8,
    pub vel: u8,
}

/// One take of a take folder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TakeDoc {
    pub notes: Vec<NoteDoc>,
    pub controls: Vec<ControlDoc>,
}

/// A stretch of a take folder that plays one take.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CompDoc {
    /// Beats from the region start.
    pub start: f32,
    pub end: f32,
    pub take: u32,
}

/// A MIDI controller event (the sustain pedal is controller 64).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ControlDoc {
    /// Beats from the region start.
    pub start: f32,
    pub num: u8,
    pub val: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct RegionDoc {
    pub name: String,
    pub start_bar: f32,
    pub len_bars: f32,
    pub notes: Vec<NoteDoc>,
    /// Controller events recorded with the notes (older files have none).
    pub controls: Vec<ControlDoc>,
    /// A take folder's takes and comp (empty for ordinary regions). `notes` and `controls`
    /// then hold what the comp plays.
    pub takes: Vec<TakeDoc>,
    pub comp: Vec<CompDoc>,
    /// Path inside the archive (`media/…`) for audio regions.
    pub audio: Option<String>,
    pub mute: bool,
    /// Playback quantize grid in beats.
    pub quantize: Option<f32>,
    pub transpose: i8,
    pub velocity: i8,
    /// Audio regions: gain in dB, fades in quarter-note beats, fade curve ("linear",
    /// "s_curve", anything else = equal power), reverse.
    pub gain_db: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub fade_curve: String,
    pub reverse: bool,
    /// Crossfade with the audio region it overlaps (Drag: X-Fade): this region fades in over
    /// the overlap while the earlier one fades out.
    pub crossfade: bool,
    /// A Pattern region: the steps that make its `notes`.
    pub pattern: Option<PatternDoc>,
    /// A Drummer region: the settings that make its `notes`.
    pub drummer: Option<DrummerDoc>,
    /// Flex Time and Flex Pitch edits of an audio region (`audio` is then the recording).
    pub flex: Option<FlexDoc>,
}

/// The Flex edit of an audio region.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct FlexDoc {
    /// "Slicing", "Rhythmic", "Monophonic", "Polyphonic", "Speed" or "Tempophone".
    pub mode: String,
    /// Flex markers: [seconds into the recording, seconds from the region's start].
    pub markers: Vec<[f64; 2]>,
    /// Flex Pitch: [start, end] (seconds from the region's start) and semitones.
    pub pitch: Vec<FlexPitchDoc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct FlexPitchDoc {
    pub start: f64,
    pub end: f64,
    pub semitones: f32,
}

/// The steps of a Pattern region.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct PatternDoc {
    /// Steps in one run of the pattern (1-64).
    pub steps: usize,
    /// Length of a step: "1/4", "1/8", "1/16", "1/32", "1/8T" or "1/16T".
    pub rate: String,
    /// Fraction of a step every second step is pushed back (0-0.5).
    pub swing: f32,
    /// Fraction of a step a note lasts (0.05-1).
    pub gate: f32,
    pub rows: Vec<PatternRowDoc>,
    /// "forward", "backward", "ping_pong" or "random".
    pub direction: String,
    pub seed: u32,
}

impl Default for PatternDoc {
    fn default() -> Self {
        Self {
            steps: 16,
            rate: "1/16".into(),
            swing: 0.0,
            gate: 0.9,
            rows: Vec::new(),
            direction: "forward".into(),
            seed: 1,
        }
    }
}

/// A pitch of a pattern and the velocity of each of its steps (0 = off).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct PatternRowDoc {
    pub pitch: u8,
    pub steps: Vec<u8>,
    /// Chance (percent) each step plays; empty when every step always plays.
    pub chance: Vec<u8>,
    /// Which steps are tied to the one before; empty when none is.
    pub tie: Vec<bool>,
    /// How many times each step repeats; empty when none does.
    pub repeat: Vec<u8>,
}

/// The settings of a Drummer region.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct DrummerDoc {
    /// "rock", "pop", "funk", "jazz", "hip_hop" or "electronic".
    pub style: String,
    pub complexity: u8,
    pub loudness: u8,
    pub swing: u8,
    pub fills: u8,
    /// The kit pieces that play, one bit each: kick, snare, hi-hat, toms, cymbals, percussion.
    pub parts: u8,
    pub seed: u32,
}

impl Default for DrummerDoc {
    fn default() -> Self {
        Self {
            style: "rock".into(),
            complexity: 50,
            loudness: 60,
            swing: 0,
            fills: 30,
            parts: 63,
            seed: 1,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TrackDoc {
    pub name: String,
    pub kind: TrackKindDoc,
    pub color: [u8; 3],
    pub mute: bool,
    pub solo: bool,
    pub volume_db: f32,
    pub pan: f32,
    pub inserts: Vec<String>,
    pub instrument: Option<String>,
    /// Library category of the loaded patch.
    pub category: Option<String>,
    pub transpose: i8,
    pub velocity: i8,
    pub key_limit: (u8, u8),
    pub vel_limit: (u8, u8),
    pub send_bus: Option<u8>,
    pub send_level: f32,
    pub send_pre: bool,
    /// The effects of the channel strip, first to last (`inserts` has their names for older
    /// versions of the program).
    pub fx: Vec<InsertDoc>,
    /// All sends of the strip (the first is also in `send_bus`, `send_level` and `send_pre`).
    pub sends: Vec<SendDoc>,
    /// Automation: "Off", "Read", "Touch", "Latch" or "Write", and the points (beat, value)
    /// of the fader (dB) and pan lanes.
    pub auto_mode: String,
    /// Points as [beat, value, curve].
    pub auto_volume: Vec<[f64; 3]>,
    pub auto_pan: Vec<[f64; 3]>,
    /// The lanes of the effect parameters; a point's value is a position 0..=1 along the
    /// parameter's range.
    pub auto_fx: Vec<AutoFxDoc>,
    /// The mix group of the track (index into `ProjectDoc::groups`).
    pub group: Option<usize>,
    /// Individual track height (factor of the global track zoom).
    pub height: f32,
    /// Folder nesting level, 0 at the top. A track belongs to the closest `Folder` track above
    /// it that has a lower level (files from before folders load with 0, i.e. no nesting).
    pub indent: u8,
    /// A `Folder` track whose tracks are hidden in the Tracks area.
    pub folded: bool,
    /// The `Vca` track (its index in `tracks`) that controls this track's level.
    pub vca: Option<usize>,
    /// External MIDI tracks: the MIDI output port (by name) and the channel, 1-16.
    pub midi_port: Option<String>,
    pub midi_channel: u8,
    /// Audio tracks: the first input channel they record (0 is Input 1), and whether they
    /// record it and the next one as a stereo pair.
    pub input: u16,
    pub input_stereo: bool,
    /// The bus the track plays into; 0 is Stereo Out.
    pub output: u8,
    /// MIDI FX, first to last.
    pub midi_fx: Vec<MidiFxDoc>,
    /// The Groove Track, and the tracks that follow it.
    pub groove: bool,
    pub follow_groove: bool,
    pub regions: Vec<RegionDoc>,
}

/// One effect of a channel strip: its kind ("Channel EQ", "Compressor", "Space D", "Tape Delay",
/// "Chorus", "Phaser", "Overdrive" or "Filter"), whether it is bypassed and its settings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct InsertDoc {
    pub kind: String,
    pub bypass: bool,
    pub params: Vec<f32>,
}

/// The automation lane of one effect parameter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AutoFxDoc {
    pub slot: u8,
    pub param: u8,
    pub points: Vec<[f64; 3]>,
}

/// A mix group: tracks whose faders, pans, Mute and Solo move together as the flags say.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct GroupDoc {
    pub name: String,
    pub volume: bool,
    pub pan: bool,
    pub mute: bool,
    pub solo: bool,
}

impl Default for GroupDoc {
    fn default() -> Self {
        Self {
            name: String::new(),
            volume: true,
            pan: true,
            mute: true,
            solo: true,
        }
    }
}

/// One send of a channel strip.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SendDoc {
    pub bus: u8,
    pub level: f32,
    pub pre: bool,
}

/// One MIDI FX of a track: its kind ("transposer", "velocity", "scale", "chord", "repeater",
/// "arpeggiator" or "randomizer"), whether it is on and its settings as numbers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct MidiFxDoc {
    pub kind: String,
    pub on: bool,
    pub settings: Vec<f32>,
    /// The program of a "scripter".
    pub script: String,
}

impl Default for MidiFxDoc {
    fn default() -> Self {
        Self {
            kind: String::new(),
            on: true,
            settings: Vec::new(),
            script: String::new(),
        }
    }
}

impl Default for TrackDoc {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: TrackKindDoc::Instrument,
            color: [0x4e, 0xb0, 0x4a],
            mute: false,
            solo: false,
            volume_db: 0.0,
            pan: 0.0,
            inserts: Vec::new(),
            instrument: None,
            category: None,
            transpose: 0,
            velocity: 0,
            key_limit: (0, 127),
            vel_limit: (1, 127),
            send_bus: None,
            send_level: 0.0,
            send_pre: false,
            fx: Vec::new(),
            sends: Vec::new(),
            auto_mode: "Read".into(),
            auto_volume: Vec::new(),
            auto_pan: Vec::new(),
            auto_fx: Vec::new(),
            group: None,
            height: 1.0,
            indent: 0,
            folded: false,
            vca: None,
            midi_port: None,
            midi_channel: 1,
            input: 0,
            input_stereo: false,
            output: 0,
            midi_fx: Vec::new(),
            groove: false,
            follow_groove: false,
            regions: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ProjectDoc {
    pub format_version: u32,
    pub name: String,
    pub tempo: f32,
    pub sig: (u8, u8),
    pub key: String,
    pub position_beats: f64,
    pub cycle: bool,
    pub cycle_start: f64,
    pub cycle_end: f64,
    pub metronome: bool,
    pub count_in: bool,
    /// The Note Pad's text.
    pub notes: String,
    pub master_vol: f32,
    pub selected_track: usize,
    pub tracks: Vec<TrackDoc>,
    /// Mix groups.
    pub groups: Vec<GroupDoc>,
    /// Tempo changes after the start (Tempo track); `tempo` is the tempo at the start.
    pub tempo_changes: Vec<TempoChangeDoc>,
    /// Whether the start tempo ramps to the first tempo change (a tempo curve).
    pub tempo_ramp: bool,
    /// Marker track.
    pub markers: Vec<MarkerDoc>,
    /// Signature track: time-signature changes after bar 1 (`sig` is the first bar's).
    pub sig_changes: Vec<SigChangeDoc>,
    /// SMPTE frame rate: "24", "25", "29.97" (non-drop), "29.97df" (drop frame) or "30".
    pub smpte_rate: String,
    /// SMPTE time of the project start: hours, minutes, seconds, frames.
    pub smpte_start: [u8; 4],
    /// MIDI output port (by name) that sends MIDI Time Code, if any.
    pub mtc_out: Option<String>,
    /// MIDI input port (by name) that Pro Wave chases MIDI Time Code from, if any.
    pub mtc_in: Option<String>,
}

/// A time-signature change: `num/den` from the start of the 1-based `bar` on.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SigChangeDoc {
    pub bar: u32,
    pub num: u8,
    pub den: u8,
}

/// A marker from `beat` (quarter notes from the start) on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct MarkerDoc {
    pub beat: f64,
    pub name: String,
    pub color: [u8; 3],
}

/// A tempo change: `bpm` from `beat` (quarter notes from the start) on.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TempoChangeDoc {
    pub beat: f64,
    pub bpm: f32,
    /// Moves in a straight line to the next change's tempo instead of holding (a tempo curve).
    pub ramp: bool,
}

impl Default for ProjectDoc {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: "Untitled".into(),
            tempo: 120.0,
            sig: (4, 4),
            key: "Cmaj".into(),
            position_beats: 0.0,
            cycle: false,
            cycle_start: 0.0,
            cycle_end: 16.0,
            metronome: false,
            count_in: true,
            notes: String::new(),
            master_vol: 0.8,
            selected_track: 0,
            tracks: Vec::new(),
            groups: Vec::new(),
            tempo_changes: Vec::new(),
            tempo_ramp: false,
            markers: Vec::new(),
            sig_changes: Vec::new(),
            smpte_rate: "25".into(),
            smpte_start: [1, 0, 0, 0],
            mtc_out: None,
            mtc_in: None,
        }
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Io(std::io::Error),
    Zip(String),
    Json(String),
    /// The file was written by a newer ProWave.
    NewerVersion(u32),
    /// Not a ProWave project (no `project.json`).
    NotAProject,
    /// The archive unpacks to more than the limit (bytes).
    TooLarge(u64),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Zip(e) => write!(f, "invalid project archive: {e}"),
            Self::Json(e) => write!(f, "invalid project.json: {e}"),
            Self::NewerVersion(v) => write!(
                f,
                "project format {v} is newer than this ProWave (supports {FORMAT_VERSION})"
            ),
            Self::NotAProject => write!(f, "not a ProWave project (project.json missing)"),
            Self::TooLarge(limit) => write!(f, "project unpacks to more than {limit} bytes"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<std::io::Error> for ProjectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<zip::result::ZipError> for ProjectError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Zip(e.to_string())
    }
}

/// An audio file to embed: `source` on disk is stored at `zip_path` (`media/…`) in the archive.
#[derive(Clone, Debug)]
pub struct MediaFile {
    pub zip_path: String,
    pub source: PathBuf,
}

/// True if `p` is a safe archive-relative media path: `media/<name>` with no `..`, root,
/// drive letter or backslash, so `media_dir.join(p)` stays inside `media_dir`. Paths read from
/// `project.json` must pass this before they are opened.
pub fn is_media_path(p: &str) -> bool {
    let Some(rest) = p.strip_prefix(MEDIA_DIR) else {
        return false;
    };
    !rest.is_empty()
        && !p.contains(['\\', ':', '\0'])
        && rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Archive path for the `index`-th media file: `media/<index>_<file name>`, with any character
/// that is not safe in a file name replaced by `_`.
pub fn media_path(index: usize, source: &Path) -> String {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio");
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{MEDIA_DIR}{index}_{clean}")
}

/// Writes the project to `path`. The archive is written next to it first and then renamed, so
/// a failed save never destroys the previous file.
pub fn save(path: &Path, doc: &ProjectDoc, media: &[MediaFile]) -> Result<(), ProjectError> {
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    let written = write_archive(&tmp, doc, media);
    if written.is_err() {
        // Do not leave a half-written archive behind.
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_archive(tmp: &Path, doc: &ProjectDoc, media: &[MediaFile]) -> Result<(), ProjectError> {
    {
        let mut zip = ZipWriter::new(File::create(tmp)?);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        // Audio barely compresses: store it as-is (also faster to save and load).
        let stored = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        let mut doc = doc.clone();
        doc.format_version = FORMAT_VERSION;
        let json =
            serde_json::to_vec_pretty(&doc).map_err(|e| ProjectError::Json(e.to_string()))?;
        zip.start_file(PROJECT_JSON, deflated)?;
        zip.write_all(&json)?;
        for m in media {
            if !is_media_path(&m.zip_path) {
                return Err(ProjectError::Zip(format!("bad media path {}", m.zip_path)));
            }
            zip.start_file(m.zip_path.as_str(), stored)?;
            let mut f = File::open(&m.source)?;
            std::io::copy(&mut f, &mut zip)?;
        }
        zip.finish()?;
    }
    Ok(())
}

/// Reads `path`. Media files are extracted into `media_dir` (keeping their `media/…` names);
/// the returned document's audio paths stay archive-relative, so resolve them with
/// `media_dir.join(path)`. Entries outside `media/` or escaping the folder are ignored.
pub fn load(path: &Path, media_dir: &Path) -> Result<ProjectDoc, ProjectError> {
    load_with_limits(path, media_dir, MAX_JSON_BYTES, MAX_MEDIA_BYTES)
}

fn load_with_limits(
    path: &Path,
    media_dir: &Path,
    max_json: u64,
    max_media: u64,
) -> Result<ProjectDoc, ProjectError> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let doc: ProjectDoc = {
        let mut entry = zip
            .by_name(PROJECT_JSON)
            .map_err(|_| ProjectError::NotAProject)?;
        let mut json = String::new();
        // Read at most one byte past the limit, so a bomb is detected without unpacking it.
        (&mut entry).take(max_json + 1).read_to_string(&mut json)?;
        if json.len() as u64 > max_json {
            return Err(ProjectError::TooLarge(max_json));
        }
        serde_json::from_str(&json).map_err(|e| ProjectError::Json(e.to_string()))?
    };
    if doc.format_version > FORMAT_VERSION {
        return Err(ProjectError::NewerVersion(doc.format_version));
    }
    let mut budget = max_media;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // `enclosed_name` refuses absolute paths and `..` (zip-slip).
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        if !rel.starts_with("media") {
            continue;
        }
        let out = media_dir.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = File::create(&out)?;
        let copied = std::io::copy(&mut (&mut entry).take(budget + 1), &mut f)?;
        if copied > budget {
            drop(f);
            let _ = std::fs::remove_dir_all(media_dir);
            return Err(ProjectError::TooLarge(max_media));
        }
        budget -= copied;
    }
    Ok(doc)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample_doc() -> ProjectDoc {
        ProjectDoc {
            name: "Song".into(),
            tempo: 98.5,
            cycle: true,
            tracks: vec![
                TrackDoc {
                    name: "Keys".into(),
                    inserts: vec!["Channel EQ".into()],
                    instrument: Some("E-Piano".into()),
                    category: Some("Keyboards".into()),
                    key_limit: (36, 96),
                    send_bus: Some(3),
                    regions: vec![RegionDoc {
                        name: "Keys".into(),
                        start_bar: 1.0,
                        len_bars: 4.0,
                        notes: vec![NoteDoc {
                            start: 0.5,
                            len: 1.0,
                            pitch: 60,
                            vel: 90,
                        }],
                        quantize: Some(0.25),
                        ..RegionDoc::default()
                    }],
                    ..TrackDoc::default()
                },
                TrackDoc {
                    name: "Vox".into(),
                    kind: TrackKindDoc::Audio,
                    regions: vec![RegionDoc {
                        name: "take".into(),
                        audio: Some("media/0_take 1.wav".into()),
                        ..RegionDoc::default()
                    }],
                    ..TrackDoc::default()
                },
            ],
            ..ProjectDoc::default()
        }
    }

    #[test]
    fn round_trip_keeps_document_and_media() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("take 1.wav");
        std::fs::write(&audio, b"RIFF fake audio bytes").unwrap();
        let file = dir.path().join("song.prowave");
        let media = vec![MediaFile {
            zip_path: media_path(0, &audio),
            source: audio.clone(),
        }];
        assert_eq!(media[0].zip_path, "media/0_take 1.wav");
        save(&file, &sample_doc(), &media).unwrap();

        let out = dir.path().join("extracted");
        let doc = load(&file, &out).unwrap();
        assert_eq!(doc, sample_doc());
        let bytes = std::fs::read(out.join("media/0_take 1.wav")).unwrap();
        assert_eq!(bytes, b"RIFF fake audio bytes");
        assert!(!file.with_extension("prowave.tmp").exists());
    }

    #[test]
    fn aux_and_folder_tracks_round_trip_with_their_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("stack.prowave");
        let track = |name: &str, kind, indent, folded| TrackDoc {
            name: name.into(),
            kind,
            indent,
            folded,
            ..TrackDoc::default()
        };
        let doc = ProjectDoc {
            tracks: vec![
                track("Drums", TrackKindDoc::Folder, 0, true),
                track("Kick", TrackKindDoc::Instrument, 1, false),
                track("Sub", TrackKindDoc::Folder, 1, false),
                track("Rev", TrackKindDoc::Aux, 0, false),
            ],
            ..ProjectDoc::default()
        };
        save(&file, &doc, &[]).unwrap();
        let back = load(&file, &dir.path().join("m")).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.tracks[0].kind, TrackKindDoc::Folder);
        assert!(back.tracks[0].folded && back.tracks[2].indent == 1);
        assert_eq!(back.tracks[3].kind, TrackKindDoc::Aux);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("old.prowave");
        let mut zip = ZipWriter::new(File::create(&file).unwrap());
        zip.start_file(PROJECT_JSON, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"format_version":1,"name":"Old","tracks":[{"name":"A"}]}"#)
            .unwrap();
        zip.finish().unwrap();
        let doc = load(&file, &dir.path().join("m")).unwrap();
        assert_eq!(doc.name, "Old");
        assert_eq!(doc.tempo, 120.0);
        assert_eq!(doc.tracks[0].vel_limit, (1, 127));
        assert_eq!((doc.tracks[0].indent, doc.tracks[0].folded), (0, false));
    }

    #[test]
    fn rejects_newer_versions_and_non_projects() {
        let dir = tempfile::tempdir().unwrap();
        let newer = dir.path().join("new.prowave");
        let mut zip = ZipWriter::new(File::create(&newer).unwrap());
        zip.start_file(PROJECT_JSON, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"format_version":99}"#).unwrap();
        zip.finish().unwrap();
        assert!(matches!(
            load(&newer, dir.path()),
            Err(ProjectError::NewerVersion(99))
        ));

        let other = dir.path().join("other.zip");
        let mut zip = ZipWriter::new(File::create(&other).unwrap());
        zip.start_file("readme.txt", SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
        assert!(matches!(
            load(&other, dir.path()),
            Err(ProjectError::NotAProject)
        ));

        let junk = dir.path().join("junk.prowave");
        std::fs::write(&junk, b"not a zip").unwrap();
        assert!(matches!(load(&junk, dir.path()), Err(ProjectError::Zip(_))));
    }

    #[test]
    fn entries_outside_media_or_escaping_are_not_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("evil.prowave");
        let mut zip = ZipWriter::new(File::create(&file).unwrap());
        zip.start_file(PROJECT_JSON, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.start_file("../escape.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.start_file("other/file.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        let out = dir.path().join("out");
        load(&file, &out).unwrap();
        assert!(!dir.path().join("escape.txt").exists());
        assert!(!out.join("other").exists());
    }

    #[test]
    fn save_refuses_media_paths_outside_media() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.wav");
        std::fs::write(&src, b"x").unwrap();
        let bad = [MediaFile {
            zip_path: "../a.wav".into(),
            source: src,
        }];
        assert!(save(&dir.path().join("p.prowave"), &ProjectDoc::default(), &bad).is_err());
        assert!(
            !dir.path().join("p.prowave.tmp").exists(),
            "temp file cleaned up"
        );
    }

    #[test]
    fn only_plain_media_paths_are_accepted() {
        assert!(is_media_path("media/0_kick.wav"));
        assert!(is_media_path("media/sub/1_vox.aif"));
        for bad in [
            "media/",
            "media/../secret.wav",
            "media/a/../../x.wav",
            "media//x.wav",
            "media/./x.wav",
            "media\\..\\x.wav",
            "media/C:/Windows/x.wav",
            "/etc/passwd",
            "C:\\Users\\me\\private.wav",
            "other/x.wav",
        ] {
            assert!(!is_media_path(bad), "{bad}");
        }
    }

    fn archive_with(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let file = dir.join("bomb.prowave");
        let mut zip = ZipWriter::new(File::create(&file).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn oversized_project_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let json = format!("{{\"name\": \"{}\"}}", "x".repeat(200));
        let file = archive_with(dir.path(), &[(PROJECT_JSON, json.as_bytes())]);
        let out = dir.path().join("out");
        assert!(matches!(
            load_with_limits(&file, &out, 100, 1000),
            Err(ProjectError::TooLarge(100))
        ));
        assert!(load_with_limits(&file, &out, 1000, 1000).is_ok());
    }

    #[test]
    fn media_beyond_the_budget_is_rejected_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![0u8; 600];
        let file = archive_with(
            dir.path(),
            &[
                (PROJECT_JSON, b"{}"),
                ("media/0_a.wav", &big),
                ("media/1_b.wav", &big),
            ],
        );
        let out = dir.path().join("out");
        assert!(matches!(
            load_with_limits(&file, &out, 1000, 1000),
            Err(ProjectError::TooLarge(1000))
        ));
        assert!(!out.exists(), "partial extraction removed");
        assert!(load_with_limits(&file, &out, 1000, 1200).is_ok());
    }
}
