//! SFZ sampler instrument reading: the import half of the Sampler (Logic Pro's Sampler, the
//! EXS24 successor, imports SFZ as well as EXS).
//!
//! The reader turns an SFZ text instrument into [regions](Region): the sample each region
//! plays, its key and velocity range, its root key, tuning, gain, pan, loop and amplitude
//! envelope, with `<global>`, `<master>` and `<group>` settings inherited as the format
//! specifies. Values are clamped to the ranges a sampler voice can use, so a malformed
//! instrument cannot push a parameter outside the engine's envelope.
//!
//! SFZ files are untrusted input (they arrive with downloaded instruments and in project
//! archives): the reader caps the file size, the region and opcode counts and the sample path
//! length, and it refuses any sample reference that is absolute or walks out of the instrument
//! directory (`..`, a Windows drive or a UNC path). `#include` is deliberately not followed,
//! for the same reason: an instrument must not be able to name another file to read.
//!
//! ```
//! use prowave_sfz::{parse, LoopMode};
//!
//! let instrument = parse("<region> sample=kick.wav key=36 loop_mode=one_shot").unwrap();
//! assert_eq!(instrument.regions[0].sample, "kick.wav");
//! assert_eq!(instrument.regions[0].loop_mode, LoopMode::OneShot);
//! ```
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::fmt;
use std::io::Read;
use std::path::Path;

/// The largest SFZ file the reader accepts (16 MiB). Real instruments are far smaller; the cap
/// keeps a hostile file from forcing a large allocation before it is rejected.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// The most regions one instrument may define.
pub const MAX_REGIONS: usize = 65_536;

/// The most opcodes one instrument may contain, across every section.
pub const MAX_OPCODES: usize = 1_000_000;

/// The longest sample path (relative to the instrument) a region may name.
pub const MAX_SAMPLE_PATH: usize = 1024;

/// How a region's sample loops, from SFZ's `loop_mode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopMode {
    /// Play once and stop at the end of the sample (`no_loop`, the default).
    #[default]
    NoLoop,
    /// Ignore loop points and play to the end (`one_shot`, for drum hits).
    OneShot,
    /// Loop for as long as the key is held and through its release (`loop_continuous`).
    Continuous,
    /// Loop while the key is held; the release plays the tail after the loop
    /// (`loop_sustain`).
    Sustain,
}

/// A region's amplitude envelope, in seconds and a 0..=1 sustain level
/// (SFZ `ampeg_attack`/`ampeg_hold`/`ampeg_decay`/`ampeg_sustain`/`ampeg_release`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AmpEnvelope {
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    /// Sustain level as a fraction: SFZ's percentage divided by 100.
    pub sustain: f32,
    pub release: f32,
}

impl Default for AmpEnvelope {
    /// SFZ's defaults: no attack, hold or decay, full sustain, and an instant release.
    fn default() -> Self {
        Self {
            attack: 0.0,
            hold: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        }
    }
}

/// One playable zone of an instrument: a sample and the keys, velocities and playback settings
/// that select it.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// The sample file, relative to the instrument, with `default_path` applied. SFZ's built-in
    /// generators (`*sine`, `*noise`, ...) are kept as written and name no file.
    pub sample: String,
    /// The lowest and highest MIDI key the region answers to (inclusive).
    pub key_low: u8,
    pub key_high: u8,
    /// The lowest and highest MIDI velocity the region answers to (inclusive).
    pub vel_low: u8,
    pub vel_high: u8,
    /// The MIDI key whose pitch the sample was recorded at.
    pub root_key: u8,
    /// Pitch offset in semitones (SFZ `transpose`).
    pub transpose: i8,
    /// Fine tuning in cents (SFZ `tune`).
    pub tune_cents: f32,
    /// Gain in dB (SFZ `volume`).
    pub volume_db: f32,
    /// Stereo position in percent, -100 (left) to 100 (right) (SFZ `pan`).
    pub pan_percent: f32,
    pub loop_mode: LoopMode,
    /// Loop bounds in sample frames, when the instrument names them.
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    pub amp: AmpEnvelope,
}

impl Region {
    /// Whether this region answers a note at `key` and `vel`.
    pub fn plays(&self, key: u8, vel: u8) -> bool {
        (self.key_low..=self.key_high).contains(&key)
            && (self.vel_low..=self.vel_high).contains(&vel)
    }
}

/// An instrument read from SFZ.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instrument {
    pub regions: Vec<Region>,
}

/// Why an instrument could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge { limit: usize },
    /// The instrument defines more than [`MAX_REGIONS`] regions.
    TooManyRegions { limit: usize },
    /// The instrument contains more than [`MAX_OPCODES`] opcodes.
    TooManyOpcodes { limit: usize },
    /// A region names a sample path that would leave the instrument's directory.
    BadSamplePath { sample: String },
    /// A quoted value is missing its closing quote.
    UnterminatedQuote,
    /// The file is not UTF-8 text.
    NotUtf8,
    /// The file could not be read.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "the instrument is larger than {limit} bytes"),
            Self::TooManyRegions { limit } => {
                write!(f, "the instrument has more than {limit} regions")
            }
            Self::TooManyOpcodes { limit } => {
                write!(f, "the instrument has more than {limit} opcodes")
            }
            Self::BadSamplePath { sample } => {
                write!(f, "the sample path leaves the instrument: {sample}")
            }
            Self::UnterminatedQuote => write!(f, "a quoted value has no closing quote"),
            Self::NotUtf8 => write!(f, "the instrument is not UTF-8 text"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads an SFZ instrument from a file, refusing one larger than [`MAX_FILE_BYTES`] before it
/// is read.
pub fn load(path: &Path) -> Result<Instrument, Error> {
    let metadata =
        std::fs::metadata(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    // The metadata length is zero for devices and some special files, and a file can grow
    // after it is checked, so read at most one byte past the limit and judge the real size.
    let file =
        std::fs::File::open(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    parse_bytes(&bytes)
}

/// Reads an SFZ instrument from text already in memory.
pub fn parse(text: &str) -> Result<Instrument, Error> {
    parse_bytes(text.as_bytes())
}

/// Reads an SFZ instrument from bytes, enforcing [`MAX_FILE_BYTES`] and UTF-8.
pub fn parse_bytes(bytes: &[u8]) -> Result<Instrument, Error> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::NotUtf8)?;
    let mut parser = Parser::default();
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for token in tokenize(line)? {
            if token.starts_with('<') && token.ends_with('>') {
                parser.begin(&token[1..token.len() - 1])?;
            } else if let Some((key, value)) = token.split_once('=') {
                parser.opcode(key, value)?;
            }
        }
    }
    parser.finish()?;
    Ok(Instrument {
        regions: parser.regions,
    })
}

/// The settings a section starts from. Every value is concrete: a section begins with its
/// parent's values and each opcode overwrites one, which is how SFZ inheritance works.
#[derive(Clone, Debug)]
struct Settings {
    key_low: u8,
    key_high: u8,
    vel_low: u8,
    vel_high: u8,
    root_key: u8,
    /// Whether `pitch_keycenter` was given, so a later `key` must not overwrite the root.
    root_explicit: bool,
    transpose: i8,
    tune_cents: f32,
    volume_db: f32,
    pan_percent: f32,
    loop_mode: LoopMode,
    loop_start: Option<u32>,
    loop_end: Option<u32>,
    amp: AmpEnvelope,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            key_low: 0,
            key_high: 127,
            vel_low: 0,
            vel_high: 127,
            // SFZ's default pitch key centre.
            root_key: 60,
            root_explicit: false,
            transpose: 0,
            tune_cents: 0.0,
            volume_db: 0.0,
            pan_percent: 0.0,
            loop_mode: LoopMode::NoLoop,
            loop_start: None,
            loop_end: None,
            amp: AmpEnvelope::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Control,
    Global,
    Master,
    Group,
    Region,
    Unknown,
}

#[derive(Default)]
struct Parser {
    /// `<control> default_path`, the directory sample names are relative to.
    default_path: Option<String>,
    global: Settings,
    master: Settings,
    group: Settings,
    current: Settings,
    section: Option<Section>,
    /// The current `<region>`'s sample, before `default_path` is applied.
    sample: Option<String>,
    regions: Vec<Region>,
    opcodes: usize,
}

impl Parser {
    /// Starts a new section: the previous one is committed (a region emitted, a parent
    /// captured) and the new one inherits from the right parent.
    fn begin(&mut self, name: &str) -> Result<(), Error> {
        self.finish()?;
        let section = match name.to_ascii_lowercase().as_str() {
            "control" => Section::Control,
            "global" => Section::Global,
            "master" => Section::Master,
            "group" => Section::Group,
            "region" => Section::Region,
            _ => Section::Unknown,
        };
        self.current = match section {
            Section::Global => {
                // A new <global> starts a new instrument-wide scope: the master and group
                // settings of the previous one must not leak into the regions after it.
                self.master = Settings::default();
                self.group = Settings::default();
                Settings::default()
            }
            Section::Master => self.global.clone(),
            Section::Group => self.master.clone(),
            Section::Region => {
                self.sample = None;
                self.group.clone()
            }
            Section::Control | Section::Unknown => Settings::default(),
        };
        self.section = Some(section);
        Ok(())
    }

    /// Commits the current section: `<global>`/`<master>`/`<group>` become the parent of the
    /// next section, and a `<region>` is resolved into a [`Region`].
    fn finish(&mut self) -> Result<(), Error> {
        match self.section {
            Some(Section::Global) => self.global = self.current.clone(),
            Some(Section::Master) => self.master = self.current.clone(),
            Some(Section::Group) => self.group = self.current.clone(),
            Some(Section::Region) => {
                let Some(raw) = self.sample.take() else {
                    // A region without a sample plays nothing; SFZ files use empty regions as
                    // holders for settings that the next sections inherit.
                    return Ok(());
                };
                let sample = self.resolve_sample(&raw)?;
                let mut settings = self.current.clone();
                if settings.key_low > settings.key_high {
                    std::mem::swap(&mut settings.key_low, &mut settings.key_high);
                }
                if settings.vel_low > settings.vel_high {
                    std::mem::swap(&mut settings.vel_low, &mut settings.vel_high);
                }
                self.regions.push(Region {
                    sample,
                    key_low: settings.key_low,
                    key_high: settings.key_high,
                    vel_low: settings.vel_low,
                    vel_high: settings.vel_high,
                    root_key: settings.root_key,
                    transpose: settings.transpose,
                    tune_cents: settings.tune_cents,
                    volume_db: settings.volume_db,
                    pan_percent: settings.pan_percent,
                    loop_mode: settings.loop_mode,
                    loop_start: settings.loop_start,
                    loop_end: settings.loop_end,
                    amp: settings.amp,
                });
                if self.regions.len() > MAX_REGIONS {
                    return Err(Error::TooManyRegions { limit: MAX_REGIONS });
                }
            }
            Some(Section::Control | Section::Unknown) | None => {}
        }
        Ok(())
    }

    fn opcode(&mut self, key: &str, value: &str) -> Result<(), Error> {
        self.opcodes += 1;
        if self.opcodes > MAX_OPCODES {
            return Err(Error::TooManyOpcodes { limit: MAX_OPCODES });
        }
        let key = key.trim().to_ascii_lowercase();
        match self.section {
            Some(Section::Region) if key == "sample" => {
                self.sample = Some(value.trim().to_string());
            }
            Some(Section::Control) => {
                if key == "default_path" {
                    self.default_path = Some(value.trim().to_string());
                }
            }
            Some(Section::Global | Section::Master | Section::Group | Section::Region) => {
                self.apply(&key, value.trim());
            }
            Some(Section::Unknown) | None => {}
        }
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &str) {
        let s = &mut self.current;
        match key {
            "key" => {
                if let Some(k) = parse_key(value) {
                    s.key_low = k;
                    s.key_high = k;
                    if !s.root_explicit {
                        s.root_key = k;
                    }
                }
            }
            "lokey" => {
                if let Some(k) = parse_key(value) {
                    s.key_low = k;
                }
            }
            "hikey" => {
                if let Some(k) = parse_key(value) {
                    s.key_high = k;
                }
            }
            "lovel" => {
                if let Some(v) = parse_level(value) {
                    s.vel_low = v;
                }
            }
            "hivel" => {
                if let Some(v) = parse_level(value) {
                    s.vel_high = v;
                }
            }
            "pitch_keycenter" => {
                if let Some(k) = parse_key(value) {
                    s.root_key = k;
                    s.root_explicit = true;
                }
            }
            "transpose" => {
                if let Some(v) = parse_number(value) {
                    s.transpose = v.round().clamp(-127.0, 127.0) as i8;
                }
            }
            "tune" => {
                if let Some(v) = parse_number(value) {
                    s.tune_cents = v.clamp(-100.0, 100.0);
                }
            }
            "volume" => {
                if let Some(v) = parse_number(value) {
                    s.volume_db = v.clamp(-144.6, 6.0);
                }
            }
            "pan" => {
                if let Some(v) = parse_number(value) {
                    s.pan_percent = v.clamp(-100.0, 100.0);
                }
            }
            "loop_mode" => match value.to_ascii_lowercase().as_str() {
                "no_loop" => s.loop_mode = LoopMode::NoLoop,
                "one_shot" => s.loop_mode = LoopMode::OneShot,
                "loop_continuous" => s.loop_mode = LoopMode::Continuous,
                "loop_sustain" => s.loop_mode = LoopMode::Sustain,
                _ => {}
            },
            "loop_start" => {
                if let Some(v) = parse_frame(value) {
                    s.loop_start = Some(v);
                }
            }
            "loop_end" => {
                if let Some(v) = parse_frame(value) {
                    s.loop_end = Some(v);
                }
            }
            "ampeg_attack" => {
                if let Some(v) = parse_seconds(value) {
                    s.amp.attack = v;
                }
            }
            "ampeg_hold" => {
                if let Some(v) = parse_seconds(value) {
                    s.amp.hold = v;
                }
            }
            "ampeg_decay" => {
                if let Some(v) = parse_seconds(value) {
                    s.amp.decay = v;
                }
            }
            "ampeg_sustain" => {
                if let Some(v) = parse_number(value) {
                    s.amp.sustain = (v / 100.0).clamp(0.0, 1.0);
                }
            }
            "ampeg_release" => {
                if let Some(v) = parse_seconds(value) {
                    s.amp.release = v;
                }
            }
            _ => {}
        }
    }

    /// Applies `<control> default_path` and checks that the sample stays inside the instrument.
    fn resolve_sample(&self, raw: &str) -> Result<String, Error> {
        if raw.starts_with('*') {
            // A built-in generator, not a file: nothing to resolve or check.
            return Ok(raw.to_string());
        }
        check_sample_path(raw)?;
        let resolved = match self.default_path.as_deref() {
            Some(prefix) if !prefix.trim().is_empty() => {
                let prefix = prefix.trim().trim_end_matches(['/', '\\']);
                if prefix.is_empty() {
                    raw.to_string()
                } else {
                    format!("{prefix}/{raw}")
                }
            }
            _ => raw.to_string(),
        };
        check_sample_path(&resolved)?;
        Ok(resolved)
    }
}

/// The sample path is relative to the instrument and may not leave it.
fn check_sample_path(path: &str) -> Result<(), Error> {
    let bad = || Error::BadSamplePath {
        sample: path.chars().take(120).collect(),
    };
    if path.is_empty() || path.len() > MAX_SAMPLE_PATH || path.contains('\0') {
        return Err(bad());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(bad());
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // A Windows drive-relative or absolute path.
        return Err(bad());
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err(bad());
    }
    Ok(())
}

/// Removes a `//` comment, leaving `//` inside a quoted value alone.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => quoted = !quoted,
            b'/' if !quoted && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Splits a line into whitespace-separated tokens, keeping quoted values (which may contain
/// spaces) in one token and dropping the quotes.
fn tokenize(line: &str) -> Result<Vec<String>, Error> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            c => token.push(c),
        }
    }
    if quoted {
        return Err(Error::UnterminatedQuote);
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

/// A key number, an SFZ note name (`C4` is middle C, `C#4`, `Db4`, `C-1`) or a negative number.
fn parse_key(value: &str) -> Option<u8> {
    if let Some(midi) = note_name(value) {
        return Some(midi);
    }
    let v = parse_number(value)?;
    Some(v.round().clamp(0.0, 127.0) as u8)
}

fn note_name(value: &str) -> Option<u8> {
    let mut chars = value.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let mut semitone: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut rest = chars.as_str();
    if let Some(accidental) = rest.chars().next() {
        if accidental == '#' || accidental == 'b' || accidental == 'B' {
            semitone += if accidental == '#' { 1 } else { -1 };
            rest = &rest[1..];
        }
    }
    // Saturating, so a huge octave in an untrusted file cannot overflow (a panic in debug builds).
    let octave = rest.parse::<i32>().ok()?;
    let midi = octave
        .saturating_add(1)
        .saturating_mul(12)
        .saturating_add(semitone);
    Some(midi.clamp(0, 127) as u8)
}

fn parse_level(value: &str) -> Option<u8> {
    let v = parse_number(value)?;
    Some(v.round().clamp(0.0, 127.0) as u8)
}

fn parse_number(value: &str) -> Option<f32> {
    let v = value.parse::<f32>().ok()?;
    v.is_finite().then_some(v)
}

fn parse_frame(value: &str) -> Option<u32> {
    let v = parse_number(value)?;
    if v < 0.0 {
        None
    } else {
        Some(v.min(u32::MAX as f32) as u32)
    }
}

fn parse_seconds(value: &str) -> Option<f32> {
    Some(parse_number(value)?.clamp(0.0, 60.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_cover_the_midi_range() {
        assert_eq!(note_name("C4"), Some(60));
        assert_eq!(note_name("A4"), Some(69));
        assert_eq!(note_name("C-1"), Some(0));
        assert_eq!(note_name("G9"), Some(127));
        assert_eq!(note_name("Cb4"), Some(59));
        assert_eq!(note_name("B#3"), Some(60));
        assert_eq!(note_name("nope"), None);
    }

    #[test]
    fn load_refuses_a_file_over_the_limit() {
        let dir = std::env::temp_dir().join(format!("sfz-load-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let big = dir.join("big.sfz");
        let small = dir.join("small.sfz");
        let Ok(()) = std::fs::write(&big, vec![b' '; MAX_FILE_BYTES + 1]) else {
            panic!("the test file should be written");
        };
        let Ok(()) = std::fs::write(&small, "<region> sample=a.wav key=60") else {
            panic!("the test file should be written");
        };
        assert_eq!(
            load(&big),
            Err(Error::TooLarge {
                limit: MAX_FILE_BYTES
            })
        );
        assert!(load(&small).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_huge_octave_does_not_overflow() {
        assert_eq!(note_name("C2147483647"), Some(127));
        assert_eq!(note_name("C-2147483648"), Some(0));
    }

    #[test]
    fn a_second_global_does_not_inherit_the_old_group() {
        let text = "<global> volume=-6\n<group> pan=50\n<region> sample=a.wav\n\
                    <global>\n<region> sample=b.wav\n";
        let Ok(inst) = parse(text) else {
            panic!("the instrument should parse");
        };
        assert_eq!(inst.regions[0].pan_percent, 50.0);
        assert_eq!(inst.regions[1].pan_percent, 0.0);
        assert_eq!(inst.regions[1].volume_db, 0.0);
    }

    #[test]
    fn sample_paths_are_relative() {
        assert!(check_sample_path("Kit/kick.wav").is_ok());
        assert!(check_sample_path("../kick.wav").is_err());
        assert!(check_sample_path("/kick.wav").is_err());
        assert!(check_sample_path("C:\\kick.wav").is_err());
        assert!(check_sample_path("Kit\\..\\kick.wav").is_err());
    }
}
