//! EXS24 instrument reading: the Logic-format half of the Sampler's import (Logic Pro's
//! Sampler, the EXS24 successor, opens `.exs` instruments; this reader supplies the zones,
//! groups and sample references an importer needs).
//!
//! An `.exs` file is a sequence of fixed 84-byte-header blocks: the instrument header, zones,
//! groups, sample references and an optional parameters block. Each block records its
//! signature and data size, and the first block's signature also tells the byte order. The
//! layout follows the reverse-engineered EXS24 specification at
//! <https://github.com/asatamax/tonverk-elmulti-converter/blob/main/docs/EXS24_FORMAT_SPEC.md>
//! (v1.0, derived from ConvertWithMoss); the fields read here are the ones a sampler needs and
//! the rest is skipped by the size in each header.
//!
//! Instruments are untrusted input, so the reader caps the file, block, zone, group and sample
//! counts and refuses blocks whose declared size runs past the end of the file. A sample
//! reference in EXS is normally an absolute path; the reader never opens it, returns it as
//! written, and exposes [`Sample::file_name_is_safe`] so an importer can resolve the bare file
//! name inside the instrument's own directory instead.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::fmt;
use std::path::Path;

/// The largest `.exs` file the reader accepts (16 MiB). Real instruments are far smaller.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// The most blocks one instrument may contain.
pub const MAX_BLOCKS: usize = 131_072;

/// The most zones one instrument may define.
pub const MAX_ZONES: usize = 65_536;

/// The most groups one instrument may define.
pub const MAX_GROUPS: usize = 16_384;

/// The most sample references one instrument may define.
pub const MAX_SAMPLES: usize = 65_536;

/// Every block starts with this header; the block's data follows it.
const BLOCK_HEADER: usize = 84;
/// The documented zone data size.
const ZONE_BYTES: usize = 96;
/// The documented group data size.
const GROUP_BYTES: usize = 92;
/// The documented sample data size.
const SAMPLE_BYTES: usize = 592;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

/// How a zone's loop plays (EXS's `loopDirection`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopDirection {
    #[default]
    Forward,
    Backward,
    PingPong,
}

/// One zone: a sample and the keys, velocities and playback settings that select it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    /// The lowest and highest MIDI key the zone answers to (inclusive).
    pub key_low: u8,
    pub key_high: u8,
    /// The lowest and highest MIDI velocity the zone answers to (inclusive).
    pub vel_low: u8,
    pub vel_high: u8,
    /// The MIDI key whose pitch the sample was recorded at.
    pub root_key: u8,
    /// Fine tuning in cents, and coarse tuning in semitones.
    pub fine_tuning: i8,
    pub coarse_tuning: i8,
    /// Stereo position (-64 left to +63 right) and volume adjust, as EXS stores them.
    pub pan: i8,
    pub volume_adjust: i8,
    /// Play the sample once, ignoring note-off (`oneshot`).
    pub one_shot: bool,
    /// Whether the note's pitch shifts the sample (`!pitch` clear).
    pub pitch_tracking: bool,
    pub reverse: bool,
    /// Sample bounds in frames.
    pub sample_start: u32,
    pub sample_end: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub loop_enabled: bool,
    pub loop_equal_power: bool,
    /// Play past the loop when the key is released, instead of continuing to loop.
    pub loop_play_to_end_on_release: bool,
    pub loop_direction: LoopDirection,
    /// The group this zone belongs to, when the index resolves.
    pub group: Option<usize>,
    /// The sample this zone plays, when the index resolves.
    pub sample: Option<usize>,
}

/// A group: settings shared by zones, including round-robin and exclusive grouping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub volume: u8,
    pub pan: u8,
    /// Voices the group may play; 0 means unlimited.
    pub polyphony: u8,
    pub exclusive: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    /// Release-trigger time in milliseconds.
    pub release_trigger_time: u16,
    /// Round-robin position, when the group has one.
    pub round_robin_position: Option<u32>,
    pub start_note: u8,
    pub end_note: u8,
}

/// A sample reference (EXS stores the path, not the audio).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// The block's object name.
    pub name: String,
    /// Length in frames, sample rate in Hz, bit depth and channel count.
    pub length: u32,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u32,
    /// The file type recorded in the instrument (`WAVE`, `AIFF`, ...).
    pub file_type: String,
    /// The path exactly as the instrument stores it. EXS normally writes an absolute path.
    pub file_path: String,
    /// The bare file name, which an importer can resolve inside the instrument's directory.
    pub file_name: String,
}

impl Sample {
    /// Whether [`file_name`](Self::file_name) is a single name that cannot leave the directory
    /// it is resolved in: not empty, not `.` or `..`, and with no path separator.
    pub fn file_name_is_safe(&self) -> bool {
        !self.file_name.is_empty()
            && self.file_name != "."
            && self.file_name != ".."
            && !self
                .file_name
                .chars()
                .any(|c| matches!(c, '/' | '\\' | '\0'))
    }
}

/// An instrument read from EXS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instrument {
    pub name: String,
    pub byte_order: ByteOrder,
    pub zones: Vec<Zone>,
    pub groups: Vec<Group>,
    pub samples: Vec<Sample>,
}

/// Why an instrument could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge { limit: usize },
    /// The file does not begin with an EXS header block.
    NotExs,
    /// A block's declared data runs past the end of the file, or a block is shorter than its
    /// documented fields.
    Truncated,
    /// A block has a signature this reader does not know.
    BadSignature { signature: u32 },
    /// The instrument contains more than [`MAX_BLOCKS`] blocks.
    TooManyBlocks { limit: usize },
    /// The instrument defines more than [`MAX_ZONES`] zones.
    TooManyZones { limit: usize },
    /// The instrument defines more than [`MAX_GROUPS`] groups.
    TooManyGroups { limit: usize },
    /// The instrument defines more than [`MAX_SAMPLES`] sample references.
    TooManySamples { limit: usize },
    /// The file could not be read.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "the instrument is larger than {limit} bytes"),
            Self::NotExs => f.write_str("the file is not an EXS instrument"),
            Self::Truncated => f.write_str("a block runs past the end of the file"),
            Self::BadSignature { signature } => {
                write!(f, "unknown block signature {signature:#010x}")
            }
            Self::TooManyBlocks { limit } => {
                write!(f, "the instrument has more than {limit} blocks")
            }
            Self::TooManyZones { limit } => write!(f, "the instrument has more than {limit} zones"),
            Self::TooManyGroups { limit } => {
                write!(f, "the instrument has more than {limit} groups")
            }
            Self::TooManySamples { limit } => {
                write!(f, "the instrument has more than {limit} samples")
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Reads an EXS instrument from a file, refusing one larger than [`MAX_FILE_BYTES`] before it
/// is read.
pub fn load(path: &Path) -> Result<Instrument, Error> {
    let metadata =
        std::fs::metadata(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    parse(&bytes)
}

/// Reads an EXS instrument from bytes already in memory.
pub fn parse(bytes: &[u8]) -> Result<Instrument, Error> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    let order = detect(bytes)?;
    let mut instrument = Instrument {
        name: String::new(),
        byte_order: order,
        zones: Vec::new(),
        groups: Vec::new(),
        samples: Vec::new(),
    };
    let mut offset = 0;
    let mut blocks = 0;
    while offset < bytes.len() {
        if bytes.len() - offset < BLOCK_HEADER {
            return Err(Error::Truncated);
        }
        let signature = read_u32(bytes, offset, order);
        let kind = if blocks == 0 {
            Kind::Header
        } else {
            kind_of(signature, order).ok_or(Error::BadSignature { signature })?
        };
        let data_size = read_u32(bytes, offset + 4, order) as usize;
        let total = BLOCK_HEADER
            .checked_add(data_size)
            .ok_or(Error::Truncated)?;
        if total > bytes.len() - offset {
            return Err(Error::Truncated);
        }
        let data = &bytes[offset + BLOCK_HEADER..offset + total];
        match kind {
            Kind::Header => {
                if blocks == 0 {
                    instrument.name = text(&bytes[offset + 20..offset + BLOCK_HEADER]);
                }
            }
            Kind::Zone => {
                instrument.zones.push(parse_zone(data, order)?);
                if instrument.zones.len() > MAX_ZONES {
                    return Err(Error::TooManyZones { limit: MAX_ZONES });
                }
            }
            Kind::Group => {
                instrument.groups.push(parse_group(data, order)?);
                if instrument.groups.len() > MAX_GROUPS {
                    return Err(Error::TooManyGroups { limit: MAX_GROUPS });
                }
            }
            Kind::Sample => {
                let name = text(&bytes[offset + 20..offset + BLOCK_HEADER]);
                instrument.samples.push(parse_sample(data, order, name)?);
                if instrument.samples.len() > MAX_SAMPLES {
                    return Err(Error::TooManySamples { limit: MAX_SAMPLES });
                }
            }
            Kind::Parameters => {}
        }
        blocks += 1;
        if blocks > MAX_BLOCKS {
            return Err(Error::TooManyBlocks { limit: MAX_BLOCKS });
        }
        offset += total;
    }
    for zone in &mut instrument.zones {
        if zone
            .group
            .is_some_and(|index| index >= instrument.groups.len())
        {
            zone.group = None;
        }
        if zone
            .sample
            .is_some_and(|index| index >= instrument.samples.len())
        {
            zone.sample = None;
        }
    }
    Ok(instrument)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Header,
    Zone,
    Group,
    Sample,
    Parameters,
}

/// The byte order of the file. The four signature constants are byte-symmetric (a little- and
/// a big-endian signature have the same bytes), so the header alone cannot say which order the
/// multi-byte fields use. What distinguishes them is the block chain: a size read in the wrong
/// order is enormous and runs past the file. Little-endian wins a tie, as it is the standard
/// and the specification lists big-endian as legacy.
fn detect(bytes: &[u8]) -> Result<ByteOrder, Error> {
    if bytes.len() < BLOCK_HEADER {
        return Err(Error::NotExs);
    }
    let little = plausible(bytes, ByteOrder::Little);
    let big = plausible(bytes, ByteOrder::Big);
    match (little, big) {
        (false, false) => Err(Error::NotExs),
        (false, true) => Ok(ByteOrder::Big),
        (true, _) => Ok(ByteOrder::Little),
    }
}

/// Whether the first (and, when present, second) block can be walked in `order`: the first
/// block is a header, and the next block's declared size stays inside the file. This is enough
/// to tell the orders apart without rejecting a truncated file early.
fn plausible(bytes: &[u8], order: ByteOrder) -> bool {
    let header = read_u32(bytes, 0, order);
    if kind_of(header, order) != Some(Kind::Header) {
        return false;
    }
    let total = match BLOCK_HEADER.checked_add(read_u32(bytes, 4, order) as usize) {
        Some(total) if total <= bytes.len() => total,
        _ => return false,
    };
    if total == bytes.len() {
        return true;
    }
    if bytes.len() - total < BLOCK_HEADER {
        // A partial block: the header is still recognisable, so let `parse` report it.
        return true;
    }
    let second = read_u32(bytes, total, order);
    if kind_of(second, order).is_none() {
        return false;
    }
    let second_total = match BLOCK_HEADER.checked_add(read_u32(bytes, total + 4, order) as usize) {
        Some(second_total) => second_total,
        None => return false,
    };
    second_total <= bytes.len() - total
}

/// The block kind for a signature in a file of the given byte order. The `0x4?000101`
/// signatures are the newer variants, which the specification lists only in little-endian.
fn kind_of(signature: u32, order: ByteOrder) -> Option<Kind> {
    match (order, signature) {
        (ByteOrder::Little, 0x0000_0101 | 0x4000_0101) => Some(Kind::Header),
        (ByteOrder::Little, 0x0100_0101 | 0x4100_0101) => Some(Kind::Zone),
        (ByteOrder::Little, 0x0200_0101 | 0x4200_0101) => Some(Kind::Group),
        (ByteOrder::Little, 0x0300_0101 | 0x4300_0101) => Some(Kind::Sample),
        (ByteOrder::Little, 0x0400_0101 | 0x4400_0101) => Some(Kind::Parameters),
        (ByteOrder::Big, 0x0101_0000) => Some(Kind::Header),
        (ByteOrder::Big, 0x0101_0001) => Some(Kind::Zone),
        (ByteOrder::Big, 0x0101_0002) => Some(Kind::Group),
        (ByteOrder::Big, 0x0101_0003) => Some(Kind::Sample),
        (ByteOrder::Big, 0x0101_0004) => Some(Kind::Parameters),
        _ => None,
    }
}

fn parse_zone(data: &[u8], order: ByteOrder) -> Result<Zone, Error> {
    if data.len() < ZONE_BYTES {
        return Err(Error::Truncated);
    }
    let flags = data[0];
    let (key_low, key_high) = order_range((data[6], data[7]));
    let (vel_low, vel_high) = order_range((data[9], data[10]));
    let group = match read_i32(data, 88, order) {
        index if index < 0 => None,
        index => Some(index as usize),
    };
    Ok(Zone {
        key_low,
        key_high,
        vel_low,
        vel_high,
        root_key: data[1],
        fine_tuning: data[2] as i8,
        coarse_tuning: data[80] as i8,
        pan: data[3] as i8,
        volume_adjust: data[4] as i8,
        one_shot: flags & 0b001 != 0,
        // Bit 1 is `!pitch`: clear means the sample follows the note's pitch.
        pitch_tracking: flags & 0b010 == 0,
        reverse: flags & 0b100 != 0,
        sample_start: read_u32(data, 12, order),
        sample_end: read_u32(data, 16, order),
        loop_start: read_u32(data, 20, order),
        loop_end: read_u32(data, 24, order),
        loop_enabled: data[33] & 0b001 != 0,
        loop_equal_power: data[33] & 0b010 != 0,
        loop_play_to_end_on_release: data[33] & 0b100 != 0,
        loop_direction: match data[34] {
            1 => LoopDirection::Backward,
            2 => LoopDirection::PingPong,
            _ => LoopDirection::Forward,
        },
        group,
        sample: Some(read_u32(data, 92, order) as usize),
    })
}

fn parse_group(data: &[u8], order: ByteOrder) -> Result<Group, Error> {
    if data.len() < GROUP_BYTES {
        return Err(Error::Truncated);
    }
    let (vel_low, vel_high) = order_range((data[5], data[6]));
    let position = read_i32(data, 80, order);
    Ok(Group {
        volume: data[0],
        pan: data[1],
        polyphony: data[2],
        exclusive: data[4],
        vel_low,
        vel_high,
        release_trigger_time: read_u16(data, 16, order),
        round_robin_position: (position >= 0).then_some(position as u32),
        start_note: data[88],
        end_note: data[89],
    })
}

fn parse_sample(data: &[u8], order: ByteOrder, name: String) -> Result<Sample, Error> {
    if data.len() < SAMPLE_BYTES {
        return Err(Error::Truncated);
    }
    Ok(Sample {
        name,
        length: read_u32(data, 4, order),
        sample_rate: read_u32(data, 8, order),
        bit_depth: read_u32(data, 12, order),
        channels: read_u32(data, 16, order),
        file_type: text(&data[28..32]),
        file_path: text(&data[80..80 + 256]),
        file_name: text(&data[336..336 + 256]),
    })
}

/// Swaps a range whose ends are the wrong way round, as a malformed instrument may have them.
fn order_range((low, high): (u8, u8)) -> (u8, u8) {
    if low <= high {
        (low, high)
    } else {
        (high, low)
    }
}

/// A block string: NUL-terminated UTF-8, decoded lossily so a bad byte does not lose the name.
fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .trim_end()
        .to_string()
}

/// Reads a `u32`; the caller checks that the four bytes are in range.
fn read_u32(bytes: &[u8], at: usize, order: ByteOrder) -> u32 {
    let b = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
    match order {
        ByteOrder::Little => u32::from_le_bytes(b),
        ByteOrder::Big => u32::from_be_bytes(b),
    }
}

fn read_u16(bytes: &[u8], at: usize, order: ByteOrder) -> u16 {
    let b = [bytes[at], bytes[at + 1]];
    match order {
        ByteOrder::Little => u16::from_le_bytes(b),
        ByteOrder::Big => u16::from_be_bytes(b),
    }
}

fn read_i32(bytes: &[u8], at: usize, order: ByteOrder) -> i32 {
    read_u32(bytes, at, order) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_byte_orders_are_recognised() {
        assert_eq!(kind_of(0x0000_0101, ByteOrder::Little), Some(Kind::Header));
        assert_eq!(kind_of(0x4000_0101, ByteOrder::Little), Some(Kind::Header));
        assert_eq!(kind_of(0x0101_0000, ByteOrder::Big), Some(Kind::Header));
        assert_eq!(kind_of(0x0101_0001, ByteOrder::Big), Some(Kind::Zone));
        assert_eq!(kind_of(0x0101_0002, ByteOrder::Big), Some(Kind::Group));
        assert_eq!(kind_of(0x0101_0003, ByteOrder::Big), Some(Kind::Sample));
        assert_eq!(kind_of(0x0101_0000, ByteOrder::Little), None);
        assert_eq!(kind_of(0xdead_beef, ByteOrder::Little), None);
    }

    #[test]
    fn reversed_ranges_are_ordered() {
        assert_eq!(order_range((48, 36)), (36, 48));
        assert_eq!(order_range((36, 48)), (36, 48));
    }

    #[test]
    fn text_stops_at_a_nul_and_survives_bad_utf8() {
        assert_eq!(text(b"Kick\0rest"), "Kick");
        assert_eq!(text(b"A\xffB"), "A\u{fffd}B");
    }
}
