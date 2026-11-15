//! The musical content of a project: tracks, their plug-ins and their regions.
//!
//! Every field that the reader could not decode is an `Option` or an empty `Vec`, never a
//! guess. The notes on each type say what the reader fills in and what it leaves empty.

use crate::Project;

/// What a channel strip is, as its descriptor says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    /// A software instrument track: MIDI in, an instrument plug-in in its first slot.
    Instrument,
    /// An audio track.
    Audio,
    /// A hardware input strip.
    Input,
    /// An aux (send return) channel.
    Aux,
    /// A bus: a summing or routing channel.
    Bus,
    /// An output strip, such as `Output 1-2`.
    Output,
    /// The master strip.
    Master,
    /// A descriptor head the reader does not know. The head byte is kept.
    Unknown(u8),
}

/// Where a plug-in comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginFormat {
    /// A third-party or Apple Audio Unit, identified by its four-character codes.
    AudioUnit,
    /// A stock Logic plug-in (Sample Delay, the Sampler, the metronome), identified by name.
    Stock,
    /// A plug-in the reader saw but could not classify.
    Unknown,
}

/// One insert slot of a track, or a plug-in the reader could not attach to a track.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginSlot {
    /// The position of the plug-in among the plug-ins attached to the same track, from zero.
    pub slot: usize,
    /// The name the project stores for the plug-in (the Audio Unit name, or the stock name).
    pub name: String,
    /// The vendor name. Logic stores only the four-character manufacturer code, so this is
    /// always `None` for now.
    pub vendor: Option<String>,
    /// Where the plug-in comes from.
    pub format: PluginFormat,
    /// The Audio Unit type code (`aumu`, `aufx`, `aumf` or `aumi`), in reading order.
    pub au_type: Option<[u8; 4]>,
    /// The Audio Unit subtype code, in reading order.
    pub au_subtype: Option<[u8; 4]>,
    /// The Audio Unit manufacturer code, in reading order (`FabF` for FabFilter).
    pub au_manufacturer: Option<[u8; 4]>,
    /// Whether the slot is bypassed. The bypass flag is not decoded yet, so this is `None`.
    pub bypass: Option<bool>,
    /// The state as an Audio Unit class-info property list (an XML plist with the keys `type`,
    /// `subtype`, `manufacturer`, `version`, `name` and `data`). Hand it to
    /// `AudioUnitSetProperty(kAudioUnitProperty_ClassInfo)` after parsing. `None` for stock
    /// plug-ins and for Audio Units whose state the project does not store.
    pub state: Option<Vec<u8>>,
}

/// One note of a MIDI region, read from its 32-byte note event (see `LOGICX-FORMAT-SPEC.md`,
/// "Second survey"). The pitch is the event's byte +0x0c, the velocity its byte +0x0b, the start
/// its position minus 38400, and the length its `u32` at +0x1c.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiNote {
    /// The MIDI note number, 0 to 127.
    pub pitch: u8,
    /// The MIDI velocity, 1 to 127.
    pub velocity: u8,
    /// The start in ticks at 960 pulses per quarter note.
    pub start_tick: u64,
    /// The length in ticks at 960 pulses per quarter note.
    pub length_ticks: u64,
}

/// A MIDI region on a track. Its start comes from the placement event on its track, and its
/// notes from the note records that follow its sequence record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiRegion {
    /// The region name: the name of its sequence record, which is the track's name. `None`
    /// when the region has no name.
    pub name: Option<String>,
    /// The start in ticks from the project start (0 is bar 1). Read from the placement event of
    /// the track (position minus 34560). `None` when no placement event is found.
    pub start_tick: Option<u64>,
    /// The length in ticks. Read from the sequence record at offset 0x78, but only for records
    /// whose name is six bytes long (the offset moves with the name length, and only that case
    /// was checked). `None` otherwise.
    pub length_ticks: Option<u64>,
    /// The notes, sorted by start. Each start is relative to the region start.
    pub notes: Vec<MidiNote>,
}

/// An audio region: a piece of an audio file placed on a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRegion {
    /// The region name as the record stores it (`Inst 1_bip`, or `Inst 1_bip.1` for a second
    /// region on the same file).
    pub name: String,
    /// The audio file, relative to the bundle's `Media` folder (`Audio Files/Inst 1_bip.wav`).
    /// `None` when no listed file matches the name.
    pub file: Option<String>,
    /// The start in ticks from the project start. Read from the placement event (position minus
    /// 34560). `None` for a region that is in the file but has no placement.
    pub start_tick: Option<u64>,
    /// The length in frames of the audio file, as the region plays it. Read from the region
    /// record (`+0x16` of the payload).
    pub length_frames: Option<u32>,
    /// The length in ticks. Not derived yet: it needs the tempo map, which is not decoded.
    pub length_ticks: Option<u64>,
}

/// The audio file a region plays, from its `lFuA` reference and its `EVAW` descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFileInfo {
    /// The file name (`Inst 1_bip.wav`).
    pub name: String,
    /// The number of sample frames in the file.
    pub frames: u32,
    /// The sample rate in hertz.
    pub sample_rate: u32,
    /// The channel count.
    pub channels: u16,
    /// The bits per sample.
    pub bits_per_sample: u16,
}

/// One channel strip of the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The strip name, such as `Inst 1` or `Bus 12`.
    pub name: String,
    /// What the strip is.
    pub kind: TrackKind,
    /// Whether the strip is in use. Set when descriptor byte 4 is non-zero. This is a guess
    /// from one project: it marks the three tracks the metadata counts (`NumberOfTracks=3`),
    /// but its meaning is not confirmed.
    pub active: bool,
    /// The fader level in decibels. `None`: the byte below is read, but its curve to dB is not
    /// known yet.
    pub volume_db: Option<f32>,
    /// The fader byte of the channel record (offset 121 of the strip's `OCuA` record). It is 90
    /// on the default strips (0 dB on the project the reader was built against) and 100 and 117
    /// on the two strips the user moved.
    pub fader_raw: Option<u8>,
    /// The pan in Logic's scale, -64 (hard left) to +63 (hard right), read from byte 125 of the
    /// channel record. The default strips hold 64, which the reader maps to 0: centre.
    pub pan: Option<f32>,
    /// Whether the strip is muted. Not decoded yet, so `None`.
    pub mute: Option<bool>,
    /// Whether the strip is soloed. Not decoded yet, so `None`.
    pub solo: Option<bool>,
    /// The insert plug-ins of the strip, in slot order.
    pub plugins: Vec<PluginSlot>,
    /// The MIDI regions of the strip, from the placement events on it.
    pub midi_regions: Vec<MidiRegion>,
    /// The audio regions placed on the strip, from the placement events on it.
    pub audio_regions: Vec<AudioRegion>,
    /// The instrument files the strip's sampler refers to, such as `Classic Bass Finger.exs`.
    pub instrument_files: Vec<String>,
}

/// A whole project: the bundle's metadata, every channel strip with its plug-ins and regions,
/// the plug-ins that could not be attached to a strip, and the audio regions and files.
#[derive(Clone, Debug, PartialEq)]
pub struct FullProject {
    /// The metadata the bundle envelope gives (see [`crate::read`]).
    pub info: Project,
    /// Every channel strip, in the order the project stores them.
    pub tracks: Vec<Track>,
    /// Plug-ins that belong to no active strip, such as the metronome.
    pub unassigned_plugins: Vec<PluginSlot>,
    /// Every audio region record, in file order. A region that has a placement event carries its
    /// start; the owning track is in [`Track::audio_regions`].
    pub audio_regions: Vec<AudioRegion>,
    /// The audio files the regions play, with their descriptors.
    pub audio_files: Vec<AudioFileInfo>,
    /// The ticks per quarter note of every tick value in this crate: 960, as the reference
    /// writer documents, and as the note and region positions in the real file fit.
    pub ticks_per_quarter: u32,
}
