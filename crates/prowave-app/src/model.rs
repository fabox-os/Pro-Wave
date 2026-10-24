//! Project model. UI never mutates the engine directly (see AGENTS.md: Command pattern);
//! this is the UI-side view model until `prowave-engine` lands.
// Some fields (pan, instrument, Bus/Output kinds) are reserved for upcoming mixer/instrument work.
#![allow(dead_code)]
use crate::pattern::{drummer_notes, BarSpec, DrumStyle, DrummerParams, PatternParams};
use egui::Color32;
use prowave_engine::AudioFile;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    Instrument,
    Audio,
    Drummer,
    Bus,
    Output,
    /// Auxiliary channel strip: gets its sound from a bus, so it has no regions.
    Aux,
    /// Folder (track stack): holds the tracks nested under it and makes no sound itself.
    Folder,
    /// VCA: a fader with Mute and Solo that moves the level of the tracks assigned to it. It
    /// has no sound or regions of its own.
    Vca,
    /// External MIDI: its regions play on a MIDI port and channel (a synthesizer outside the
    /// app) instead of on a software instrument.
    ExternalMidi,
}

impl TrackKind {
    /// Whether tracks of this kind hold regions (Aux and Folder tracks do not).
    pub fn has_regions(self) -> bool {
        matches!(
            self,
            Self::Instrument | Self::Audio | Self::Drummer | Self::ExternalMidi
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub start: f32, // beats from region start
    pub len: f32,
    pub pitch: u8,
    pub vel: u8,
}

/// One pass of a cycle recording.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Take {
    pub notes: Vec<Note>,
    pub controls: Vec<Control>,
}

/// A stretch of a take folder (beats from the region start) that plays this take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompSegment {
    pub start: f32,
    pub end: f32,
    pub take: usize,
}

/// Logic's take folder: several takes of the same passage and the comp that plays one of
/// them in each stretch. See `takes.rs`.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeFolder {
    pub takes: Vec<Take>,
    /// Sorted, without gaps or overlaps, covering `0..len`.
    pub comp: Vec<CompSegment>,
    /// Length of the folder in beats.
    pub len: f32,
    /// Shown opened, with a row for each take (not saved).
    pub open: bool,
}

/// A MIDI controller event in a region (the sustain pedal is controller 64).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub start: f32, // beats from region start
    pub num: u8,
    pub val: u8,
}

#[derive(Clone)]
pub struct Region {
    pub name: String,
    pub start_bar: f32,
    pub len_bars: f32,
    pub notes: Vec<Note>,
    /// Controller events recorded with the notes.
    pub controls: Vec<Control>,
    /// A take folder: the takes of a cycle recording and the comp that picks among them. The
    /// region's own `notes` and `controls` are then the comp, kept up to date by
    /// [`TakeFolder::apply`].
    pub folder: Option<TakeFolder>,
    /// Set for audio regions backed by a file on disk.
    pub audio: Option<AudioRegion>,
    /// Region inspector parameters (non-destructive, applied when the song is built).
    pub params: RegionParams,
    /// A Pattern or Drummer region: the settings its `notes` are made from.
    pub generated: Option<Generated>,
}

/// What makes the notes of a Pattern or Drummer region (see `pattern.rs`).
#[derive(Clone, Debug, PartialEq)]
pub enum Generator {
    Pattern(PatternParams),
    Drummer(DrummerParams),
}

/// A region whose notes are made from settings. The notes are made again whenever the
/// settings, the length of the region or the bars under it change.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    pub source: Generator,
    /// What the notes were last made for: the bars the region covers (see
    /// [`Project::generated_key`]).
    pub made_for: u64,
    /// The region's start, length and the song revision when `made_for` was last compared with
    /// the bars, so a frame in which none changed does not look at the bars again.
    pub seen: (u32, u32, u64),
}

impl Generated {
    pub fn new(source: Generator) -> Self {
        Self {
            source,
            made_for: 0,
            seen: (0, 0, 0),
        }
    }
}

/// Most bars the notes of a generated region are made for.
const MAX_GENERATED_BARS: usize = 1024;

/// Logic Pro Region Inspector values.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct RegionParams {
    pub mute: bool,
    /// Playback quantize grid in beats (`None` = Off).
    pub quantize: Option<f32>,
    /// Semitones.
    pub transpose: i8,
    /// Added to every note velocity.
    pub velocity: i8,
    /// Audio regions (Logic: Region Inspector Gain, Fade In / Out, Curve, Reverse), applied
    /// non-destructively by the engine. Gain in dB; fades in quarter-note beats.
    pub gain_db: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub fade_curve: prowave_engine::FadeCurve,
    pub reverse: bool,
    /// Crossfade with the audio region this one overlaps (Logic: Drag > X-Fade): it fades in
    /// over the overlap while the earlier region fades out, with the region's curve.
    pub crossfade: bool,
}

/// Logic Pro's region gain range, in dB.
pub const MIN_REGION_GAIN: f32 = -96.0;
pub const MAX_REGION_GAIN: f32 = 24.0;

impl RegionParams {
    /// The audio edits as the engine applies them.
    pub fn clip_edit(&self) -> prowave_engine::ClipEdit {
        prowave_engine::ClipEdit {
            gain_db: self.gain_db,
            fade_in_beats: self.fade_in as f64,
            fade_out_beats: self.fade_out as f64,
            fade_curve: self.fade_curve,
            reverse: self.reverse,
        }
    }

    /// Region gain that brings a file whose loudest sample is `peak` (0..1) to 0 dBFS
    /// (Logic: Normalize Region Gain). `None` for a silent file.
    pub fn normalize_gain(peak: f32) -> Option<f32> {
        (peak.is_finite() && peak > 1e-6)
            .then(|| (-20.0 * peak.log10()).clamp(MIN_REGION_GAIN, MAX_REGION_GAIN))
    }
}

/// One send slot of a channel strip (Logic: "Bus 1" with a level knob). The engine copies the
/// strip's signal to the bus; the Aux strip that listens to the bus plays it.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct SendSlot {
    /// Bus number 1..=8, or `None` for an empty slot.
    pub bus: Option<u8>,
    /// 0..=1 send level (the knob; see `prowave_engine::send_gain`).
    pub level: f32,
    /// Pre-fader send (shown with the "eye" icon).
    pub pre: bool,
}

/// Most sends in one channel strip.
pub const MAX_SENDS: usize = prowave_engine::MAX_SENDS;

/// One effect in a channel strip's Audio FX slot (Logic: a plug-in): what it is, whether it is
/// bypassed, and its settings in the units of `FxKind::params`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Insert {
    pub kind: prowave_engine::FxKind,
    pub bypass: bool,
    pub params: [f32; prowave_engine::MAX_FX_PARAMS],
}

impl Insert {
    /// A new effect of `kind` with its default settings.
    pub fn new(kind: prowave_engine::FxKind) -> Self {
        Self {
            kind,
            bypass: false,
            params: kind.defaults(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }
}

/// The effects a Library patch names (names that are no effect here are left out).
pub fn inserts_from_names(names: &[&str]) -> Vec<Insert> {
    names
        .iter()
        .filter_map(|n| prowave_engine::FxKind::from_name(n))
        .map(Insert::new)
        .take(prowave_engine::MAX_INSERTS)
        .collect()
}

/// Logic's automation modes of a channel strip.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum AutoMode {
    Off,
    /// Plays the automation back.
    #[default]
    Read,
    /// Plays it back until the control is touched, writes while it is held, and returns to
    /// the automation when it is let go.
    Touch,
    /// Like Touch, but keeps writing after the control is let go, until playback stops.
    Latch,
    /// Writes the control's value all the time while playing.
    Write,
}

impl AutoMode {
    pub const ALL: [AutoMode; 5] = [
        AutoMode::Off,
        AutoMode::Read,
        AutoMode::Touch,
        AutoMode::Latch,
        AutoMode::Write,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AutoMode::Off => "Off",
            AutoMode::Read => "Read",
            AutoMode::Touch => "Touch",
            AutoMode::Latch => "Latch",
            AutoMode::Write => "Write",
        }
    }
}

/// What an automation lane moves and its points (the engine's own types).
pub use prowave_engine::{AutoPoint, AutoTarget};

/// The automation of one effect parameter: slot and parameter number, and the points, whose
/// values are positions 0..=1 along the parameter's range.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FxLane {
    pub slot: u8,
    pub param: u8,
    pub points: Vec<AutoPoint>,
}

/// Automation of one channel strip: a lane for the fader (dB), one for the pan knob and one
/// for each effect parameter, as points in order of beat, and what the strip is doing with them.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Automation {
    pub mode: AutoMode,
    pub volume: Vec<AutoPoint>,
    pub pan: Vec<AutoPoint>,
    pub fx: Vec<FxLane>,
    /// The control held (Touch), or held and let go but still writing (Latch): the strip
    /// plays that control's own value, not its lane. Not saved.
    pub held: Option<AutoTarget>,
    /// The lane the Automation editor shows, and Write mode writes. Not saved.
    pub shown: AutoTarget,
    /// The beat and value last written in this pass, so the next write replaces what lies
    /// between. Not saved.
    pub last: Option<(f64, f32)>,
}

/// Points closer than this (in beats) are written as one unless the value moves.
const WRITE_STEP: f64 = 0.125;

impl Automation {
    /// The points of the lane of `target` (none if it has no lane).
    pub fn lane(&self, target: AutoTarget) -> &[AutoPoint] {
        match target {
            AutoTarget::Volume => &self.volume,
            AutoTarget::Pan => &self.pan,
            AutoTarget::Fx { slot, param } => self
                .fx
                .iter()
                .find(|l| l.slot == slot && l.param == param)
                .map_or(&[], |l| &l.points),
        }
    }

    /// The points of the lane of `target`, made if it had none (see [`Automation::prune`]).
    pub fn lane_mut(&mut self, target: AutoTarget) -> &mut Vec<AutoPoint> {
        match target {
            AutoTarget::Volume => &mut self.volume,
            AutoTarget::Pan => &mut self.pan,
            AutoTarget::Fx { slot, param } => {
                let i = match self
                    .fx
                    .iter()
                    .position(|l| l.slot == slot && l.param == param)
                {
                    Some(i) => i,
                    None => {
                        self.fx.push(FxLane {
                            slot,
                            param,
                            points: Vec::new(),
                        });
                        self.fx.len() - 1
                    }
                };
                &mut self.fx[i].points
            }
        }
    }

    /// Every lane (the fader's and the pan's first), with its points; for walking over them
    /// without making a list.
    pub fn lanes(&self) -> impl Iterator<Item = (AutoTarget, &[AutoPoint])> {
        [
            (AutoTarget::Volume, &self.volume[..]),
            (AutoTarget::Pan, &self.pan[..]),
        ]
        .into_iter()
        .chain(self.fx.iter().map(|l| {
            (
                AutoTarget::Fx {
                    slot: l.slot,
                    param: l.param,
                },
                &l.points[..],
            )
        }))
    }

    /// Whether any lane has points.
    pub fn has_lane(&self) -> bool {
        self.lanes().any(|(_, pts)| !pts.is_empty())
    }

    /// Takes away the effect lanes that have no points.
    pub fn prune(&mut self) {
        self.fx.retain(|l| !l.points.is_empty());
    }

    /// Whether the engine plays the lane of `target` (rather than the fader or knob).
    pub fn follows(&self, target: AutoTarget) -> bool {
        match self.mode {
            AutoMode::Off | AutoMode::Write => false,
            AutoMode::Read => true,
            AutoMode::Touch | AutoMode::Latch => self.held != Some(target),
        }
    }

    /// The lane's value at `beat`, if the strip plays the lane and it has points.
    pub fn played(&self, target: AutoTarget, beat: f64) -> Option<f32> {
        if !self.follows(target) {
            return None;
        }
        prowave_engine::lane_value(self.lane(target), beat)
    }

    /// The most points one lane keeps.
    pub const MAX_POINTS: usize = prowave_engine::MAX_AUTO_POINTS;

    /// Sets the point at `beat` (adds it, or moves the value of the one already there),
    /// keeping the lane in order.
    pub fn set_point(&mut self, target: AutoTarget, beat: f64, value: f32) {
        if !beat.is_finite() || !value.is_finite() || beat < 0.0 {
            return;
        }
        let (lo, hi) = Self::range(target);
        let value = value.clamp(lo, hi);
        let pts = self.lane_mut(target);
        let i = pts.partition_point(|p| p.beat < beat);
        if i < pts.len() && (pts[i].beat - beat).abs() < 1e-9 {
            pts[i].value = value;
        } else if pts.len() < Self::MAX_POINTS {
            pts.insert(i, AutoPoint::new(beat, value));
        }
        self.prune();
    }

    /// The range of a lane's values (an effect's lane holds positions 0..=1).
    pub fn range(target: AutoTarget) -> (f32, f32) {
        match target {
            AutoTarget::Volume => (-60.0, 6.0),
            AutoTarget::Pan => (-1.0, 1.0),
            AutoTarget::Fx { .. } => (0.0, 1.0),
        }
    }

    /// Writes the control's value while playing: the points between the last write of this
    /// pass and `beat` are replaced by the new one. Writes close together are skipped unless
    /// the value moved, or `force` (the end of a pass).
    pub fn write(&mut self, target: AutoTarget, beat: f64, value: f32, force: bool) {
        if !beat.is_finite() || !value.is_finite() {
            return;
        }
        let moved = match target {
            AutoTarget::Volume => 0.1,
            AutoTarget::Pan => 0.01,
            AutoTarget::Fx { .. } => 0.004,
        };
        let last = self.last.filter(|(b, _)| beat >= *b);
        if let Some((lb, lv)) = last {
            if !force && beat - lb < WRITE_STEP && (value - lv).abs() < moved {
                return;
            }
            self.lane_mut(target)
                .retain(|p| p.beat <= lb || p.beat > beat);
        }
        self.set_point(target, beat, value);
        self.last = Some((beat, value));
    }

    /// Ends the pass: the next write starts a new one.
    pub fn end_pass(&mut self) {
        self.last = None;
    }

    /// Removes every point of every lane.
    pub fn clear(&mut self) {
        self.volume.clear();
        self.pan.clear();
        self.fx.clear();
        self.last = None;
    }

    /// The effect in `slot` was taken away: its lanes go, those of later effects move up.
    pub fn insert_removed(&mut self, slot: usize) {
        self.fx.retain(|l| l.slot as usize != slot);
        for l in self.fx.iter_mut().filter(|l| l.slot as usize > slot) {
            l.slot -= 1;
        }
        self.drop_stale_state();
    }

    /// The effects in slots `a` and `b` changed places.
    pub fn insert_swapped(&mut self, a: usize, b: usize) {
        for l in self.fx.iter_mut() {
            if l.slot as usize == a {
                l.slot = b as u8;
            } else if l.slot as usize == b {
                l.slot = a as u8;
            }
        }
        self.drop_stale_state();
    }

    /// The effect in `slot` was replaced by another kind: its lanes mean nothing now.
    pub fn insert_replaced(&mut self, slot: usize) {
        self.fx.retain(|l| l.slot as usize != slot);
        self.drop_stale_state();
    }

    /// Forget what was written or held about a lane that moved or went.
    fn drop_stale_state(&mut self) {
        self.last = None;
        if matches!(self.held, Some(AutoTarget::Fx { .. })) {
            self.held = None;
        }
        if matches!(self.shown, AutoTarget::Fx { .. }) {
            self.shown = AutoTarget::Volume;
        }
    }
}

/// A mix group (Logic: Group): tracks whose controls move together. The attributes say which.
#[derive(Clone, PartialEq, Debug)]
pub struct MixGroup {
    pub name: String,
    /// Moving one fader moves the others by the same number of dB.
    pub volume: bool,
    pub pan: bool,
    pub mute: bool,
    pub solo: bool,
}

/// Most mix groups in a project.
pub const MAX_GROUPS: usize = 64;

impl MixGroup {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            volume: true,
            pan: true,
            mute: true,
            solo: true,
        }
    }
}

/// Logic Pro Track Inspector values for instrument tracks.
#[derive(Clone, Copy, PartialEq)]
pub struct TrackParams {
    pub transpose: i8,
    pub velocity: i8,
    /// Lowest/highest MIDI note that plays (Key Limit).
    pub key_limit: (u8, u8),
    /// Lowest/highest velocity that plays (Vel Limit).
    pub vel_limit: (u8, u8),
    /// The Groove Track: the one whose timing the following tracks take (see `groove.rs`).
    pub groove: bool,
    /// Plays with the Groove Track's timing.
    pub follow_groove: bool,
}

impl Default for TrackParams {
    fn default() -> Self {
        Self {
            transpose: 0,
            velocity: 0,
            key_limit: (0, 127),
            vel_limit: (1, 127),
            groove: false,
            follow_groove: false,
        }
    }
}

#[derive(Clone)]
pub struct AudioRegion {
    pub file: AudioFile,
    /// Min/max overview, filled by a background thread after import.
    pub peaks: Arc<OnceLock<Vec<(f32, f32)>>>,
    /// Flex Time and Flex Pitch: when set, `file` is the sound made from this edit.
    pub flex: Option<crate::flex_edit::FlexEdit>,
}

#[derive(Clone)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    pub color: Color32,
    pub mute: bool,
    pub solo: bool,
    pub rec: bool,
    /// Input monitoring ("I" button, audio tracks only).
    pub monitor: bool,
    pub volume_db: f32,
    pub pan: f32,
    pub inserts: Vec<Insert>,
    pub instrument: Option<&'static str>,
    pub regions: Vec<Region>,
    pub params: TrackParams,
    /// Sends to buses, in slot order (up to [`MAX_SENDS`]; an emptied slot is removed).
    pub sends: Vec<SendSlot>,
    pub auto: Automation,
    /// The mix group (index into `Project::groups`) the track belongs to.
    pub group: Option<usize>,
    /// Library category of the loaded patch (drives the track icon), if any.
    pub category: Option<&'static str>,
    /// Individual track height as a factor of the global track zoom (Logic: drag the lower
    /// edge of a track). 1.0 = the zoom slider's height.
    pub height: f32,
    /// Folder nesting level, 0 at the top. A track belongs to the closest `Folder` track above
    /// it with a lower level.
    pub indent: u8,
    /// A `Folder` track whose tracks are hidden in the Tracks area.
    pub folded: bool,
    /// The `Vca` track (its index in the project) that moves this track's level, if any.
    pub vca: Option<usize>,
    /// External MIDI tracks: the MIDI output port (by name) and the channel, 1-16.
    pub midi_port: Option<String>,
    pub midi_channel: u8,
    /// MIDI FX: effects that change the notes the track plays (see `midi_fx.rs`).
    pub midi_fx: Vec<crate::midi_fx::FxSlot>,
}

impl Track {
    /// Smallest and largest individual height factor.
    pub const MIN_HEIGHT: f32 = 0.5;
    pub const MAX_HEIGHT: f32 = 6.0;

    /// A new track of `kind` with default settings and no regions: software instruments get
    /// the Sampler, and every kind its own colour.
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        let (color, instrument) = match kind {
            TrackKind::Instrument => (Color32::from_rgb(0x4e, 0xb0, 0x4a), Some("Sampler")),
            TrackKind::Drummer => (Color32::from_rgb(0xd9, 0xaa, 0x2a), Some("Drum Kit")),
            TrackKind::Audio => (Color32::from_rgb(0x3f, 0x5f, 0xc0), None),
            TrackKind::Aux => (Color32::from_rgb(0x2a, 0xa8, 0xa8), None),
            TrackKind::Folder => (Color32::from_rgb(0x88, 0x8c, 0x98), None),
            TrackKind::Vca => (Color32::from_rgb(0x9a, 0x6c, 0xd0), None),
            TrackKind::ExternalMidi => (Color32::from_rgb(0xd0, 0x84, 0x3c), None),
            TrackKind::Bus | TrackKind::Output => (Color32::from_rgb(0x7a, 0x4f, 0xc0), None),
        };
        Self {
            name: name.into(),
            kind,
            color,
            mute: false,
            solo: false,
            rec: false,
            monitor: false,
            volume_db: 0.0,
            pan: 0.0,
            inserts: vec![],
            instrument,
            regions: vec![],
            params: TrackParams::default(),
            sends: Vec::new(),
            auto: Automation::default(),
            group: None,
            category: None,
            height: 1.0,
            indent: 0,
            folded: false,
            vca: None,
            midi_port: None,
            midi_channel: 1,
            midi_fx: Vec::new(),
        }
    }
}

/// A tempo change on the Tempo track (Logic: a tempo point): `bpm` from `beat` (quarter notes
/// from the start) until the next change, or, with `ramp`, moving in a straight line to the
/// next change's tempo (a tempo curve).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TempoPoint {
    pub beat: f64,
    pub bpm: f32,
    pub ramp: bool,
}

/// A marker on the Marker track (Logic: Intro, Verse, Chorus...): from `beat` (quarter notes
/// from the start) until the next marker.
#[derive(Clone, PartialEq, Debug)]
pub struct Marker {
    pub beat: f64,
    pub name: String,
    pub color: Color32,
}

/// Colours offered for markers, in the order new markers take them.
pub const MARKER_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x3f, 0x8f, 0xd8),
    Color32::from_rgb(0x4e, 0xb0, 0x4a),
    Color32::from_rgb(0xd9, 0xaa, 0x2a),
    Color32::from_rgb(0xd8, 0x6a, 0x2c),
    Color32::from_rgb(0xc8, 0x3c, 0x3c),
    Color32::from_rgb(0xb0, 0x4f, 0xb8),
    Color32::from_rgb(0x7a, 0x4f, 0xc0),
    Color32::from_rgb(0x3c, 0xa8, 0xa0),
];

/// Logic Pro's tempo range.
pub const MIN_BPM: f32 = 5.0;
pub const MAX_BPM: f32 = 990.0;

#[derive(Clone)]
pub struct Project {
    pub name: String,
    /// Tempo at the start of the song (the first point of the Tempo track).
    pub tempo: f32,
    /// Whether the start tempo ramps to the first of `tempo_changes` (a tempo curve).
    pub tempo_ramp: bool,
    /// The hits of an audio Groove Track, in beats (found in the background, not saved).
    pub groove_onsets: std::sync::Arc<Vec<f64>>,
    /// What `groove_onsets` were found for (see `groove::poll`).
    pub groove_key: u64,
    /// Later tempo changes, sorted by beat, all after the start (see [`Project::tidy_tempo`]).
    pub tempo_changes: Vec<TempoPoint>,
    /// Marker track, sorted by beat (see [`Project::tidy_markers`]).
    pub markers: Vec<Marker>,
    /// Time signature of the first bar. Later changes are in `sig_changes`.
    pub sig: (u8, u8),
    /// Signature track: signature changes after bar 1, sorted by bar (see
    /// [`Project::tidy_signatures`]).
    pub sig_changes: Vec<SigChange>,
    /// Frame rate of the SMPTE display and of MIDI Time Code (Logic: Project Settings >
    /// Synchronization > Frame Rate).
    pub smpte_rate: prowave_engine::SmpteRate,
    /// SMPTE time of the project start: hours, minutes, seconds, frames.
    pub smpte_start: [u8; 4],
    /// MIDI output port (by name) that sends MIDI Time Code while the project plays.
    pub mtc_out: Option<String>,
    /// MIDI input port (by name) the transport chases MIDI Time Code from.
    pub mtc_in: Option<String>,
    pub key: &'static str,
    pub tracks: Vec<Track>,
    pub playing: bool,
    pub recording: bool,
    pub cycle: bool,
    pub metronome: bool,
    /// Count-in before recording (the purple "1234" button).
    pub count_in: bool,
    /// Cycle (left/right) locators, in beats.
    pub cycle_start: f64,
    pub cycle_end: f64,
    pub position_beats: f64, // 0-based
    pub selected_track: usize,
    pub master_vol: f32,
    /// Mix groups; a track's `group` is an index into this list.
    pub groups: Vec<MixGroup>,
    /// Bumped whenever tracks/regions change so the engine song is rebuilt.
    pub song_rev: u64,
}

/// Deterministic pseudo-random so demo content is stable across runs.
fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed >> 8
}

fn demo_notes(seed: u32, bars: f32, lo: u8, hi: u8) -> Vec<Note> {
    let mut s = seed;
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < bars * 4.0 {
        let len = [0.5, 0.75, 1.0, 1.5, 0.25][(lcg(&mut s) % 5) as usize];
        let pitch = lo + (lcg(&mut s) % (hi - lo) as u32) as u8;
        out.push(Note {
            start: t,
            len: len * 0.9,
            pitch,
            vel: 70 + (lcg(&mut s) % 50) as u8,
        });
        t += [0.5, 1.0, 0.25, 0.75][(lcg(&mut s) % 4) as usize];
    }
    out
}

impl Project {
    /// A new, empty project with one software instrument track (File > New).
    pub fn empty() -> Self {
        let mut p = Self::demo();
        p.name = "Untitled".into();
        p.tracks = vec![Track {
            name: "Inst 1".into(),
            kind: TrackKind::Instrument,
            color: Color32::from_rgb(0x4e, 0xb0, 0x4a),
            mute: false,
            solo: false,
            rec: false,
            monitor: false,
            volume_db: 0.0,
            pan: 0.0,
            inserts: vec![],
            instrument: Some("Sampler"),
            regions: vec![],
            params: TrackParams::default(),
            category: None,
            sends: Vec::new(),
            auto: Automation::default(),
            group: None,
            height: 1.0,
            indent: 0,
            folded: false,
            vca: None,
            midi_port: None,
            midi_channel: 1,
            midi_fx: Vec::new(),
        }];
        p.position_beats = 0.0;
        p.cycle = false;
        p.selected_track = 0;
        p
    }

    pub fn demo() -> Self {
        let green = Color32::from_rgb(0x4e, 0xb0, 0x4a);
        let blue = Color32::from_rgb(0x3f, 0x5f, 0xc0);
        let yellow = Color32::from_rgb(0xd9, 0xaa, 0x2a);
        let purple = Color32::from_rgb(0x7a, 0x4f, 0xc0);
        let mk = |name: &str,
                  kind,
                  color,
                  inst: Option<&'static str>,
                  ins: Vec<&'static str>,
                  seed,
                  vol| Track {
            name: name.into(),
            kind,
            color,
            mute: false,
            solo: false,
            rec: false,
            monitor: false,
            volume_db: vol,
            pan: 0.0,
            inserts: inserts_from_names(&ins),
            instrument: inst,
            regions: vec![Region {
                name: name.into(),
                start_bar: 1.0,
                len_bars: 12.0,
                notes: if matches!(kind, TrackKind::Audio) {
                    vec![]
                } else {
                    demo_notes(seed, 12.0, 50, 76)
                },
                controls: Vec::new(),
                folder: None,
                generated: None,
                audio: None,
                params: RegionParams::default(),
            }],
            params: TrackParams::default(),
            category: None,
            sends: Vec::new(),
            auto: Automation::default(),
            group: None,
            height: 1.0,
            indent: 0,
            folded: false,
            vca: None,
            midi_port: None,
            midi_channel: 1,
            midi_fx: Vec::new(),
        };
        let tracks = vec![
            mk(
                "Classic Electric Piano",
                TrackKind::Instrument,
                green,
                Some("E-Piano"),
                vec!["Tape Delay", "Channel EQ"],
                1,
                -3.2,
            ),
            mk(
                "Steinway Grand Piano",
                TrackKind::Instrument,
                green,
                Some("Sampler"),
                vec!["Channel EQ", "Compressor"],
                2,
                -2.7,
            ),
            mk(
                "Analog Pad",
                TrackKind::Instrument,
                green,
                Some("Alchemy"),
                vec!["Chorus", "Space D"],
                3,
                -5.6,
            ),
            mk(
                "Drummer",
                TrackKind::Drummer,
                yellow,
                Some("Drum Kit"),
                vec!["Compressor"],
                4,
                -4.0,
            ),
            mk(
                "Electric Bass",
                TrackKind::Audio,
                blue,
                None,
                vec!["Channel EQ"],
                5,
                -8.2,
            ),
            mk(
                "Lead Vocal",
                TrackKind::Audio,
                purple,
                None,
                vec!["Channel EQ", "Compressor", "Space D"],
                6,
                -7.8,
            ),
        ];
        let mut tracks = tracks;
        for (t, bus) in tracks
            .iter_mut()
            .zip([Some(1u8), Some(3), Some(3), Some(3)])
        {
            t.sends = vec![SendSlot {
                bus,
                level: 0.0,
                pre: false,
            }];
        }
        Self {
            name: "Untitled".into(),
            tempo: 120.0,
            sig: (4, 4),
            key: "Cmaj",
            tracks,
            playing: false,
            recording: false,
            cycle: false,
            metronome: false,
            count_in: true,
            cycle_start: 8.0,
            cycle_end: 88.0,
            position_beats: 16.0,
            selected_track: 0,
            master_vol: 0.8,
            groups: Vec::new(),
            song_rev: 0,
            tempo_ramp: false,
            groove_onsets: Default::default(),
            groove_key: 0,
            tempo_changes: Vec::new(),
            markers: Vec::new(),
            sig_changes: Vec::new(),
            smpte_rate: prowave_engine::SmpteRate::Fps25,
            smpte_start: [1, 0, 0, 0],
            mtc_out: None,
            mtc_in: None,
        }
    }

    /// Adds a marker at `beat` named "Marker <n>" (or replaces the name of one already there).
    /// Returns its index.
    pub fn add_marker(&mut self, beat: f64) -> usize {
        let beat = beat.max(0.0);
        if let Some(i) = self
            .markers
            .iter()
            .position(|m| (m.beat - beat).abs() <= 1e-6)
        {
            return i;
        }
        let n = (1..)
            .find(|n| !self.markers.iter().any(|m| m.name == format!("Marker {n}")))
            .unwrap_or(1);
        let i = self.markers.partition_point(|m| m.beat < beat);
        let color = MARKER_COLORS[self.markers.len() % MARKER_COLORS.len()];
        self.markers.insert(
            i,
            Marker {
                beat,
                name: format!("Marker {n}"),
                color,
            },
        );
        i
    }

    /// The marker the playhead at `beat` is in (the last one at or before it).
    pub fn marker_at(&self, beat: f64) -> Option<usize> {
        let n = self.markers.partition_point(|m| m.beat <= beat + 1e-6);
        n.checked_sub(1)
    }

    /// Start of the first marker after `beat`.
    pub fn next_marker(&self, beat: f64) -> Option<f64> {
        self.markers
            .iter()
            .map(|m| m.beat)
            .find(|b| *b > beat + 1e-6)
    }

    /// Start of the last marker before `beat`.
    pub fn previous_marker(&self, beat: f64) -> Option<f64> {
        self.markers
            .iter()
            .rev()
            .map(|m| m.beat)
            .find(|b| *b < beat - 1e-6)
    }

    /// Sorts the markers and drops invalid ones (negative or non-finite positions, a second
    /// marker on the same beat).
    pub fn tidy_markers(&mut self) {
        self.markers.retain(|m| m.beat.is_finite() && m.beat >= 0.0);
        self.markers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.markers
            .dedup_by(|b, a| (a.beat - b.beat).abs() <= 1e-6);
    }

    /// The Tempo track as (beat, bpm, ramps to the next) points; the first is the start tempo.
    pub fn tempo_points(&self) -> Vec<TempoPoint> {
        std::iter::once(TempoPoint {
            beat: 0.0,
            bpm: self.tempo,
            ramp: self.tempo_ramp,
        })
        .chain(self.tempo_changes.iter().copied())
        .collect()
    }

    /// The tempo in effect at `beat`: a point's own tempo until the next one, or, on a tempo
    /// curve, the straight line from its tempo to the next point's.
    pub fn tempo_at(&self, beat: f64) -> f32 {
        let i = self.tempo_changes.partition_point(|t| t.beat <= beat);
        let (from, ramp) = match i.checked_sub(1) {
            Some(j) => (self.tempo_changes[j], self.tempo_changes[j].ramp),
            None => (
                TempoPoint {
                    beat: 0.0,
                    bpm: self.tempo,
                    ramp: self.tempo_ramp,
                },
                self.tempo_ramp,
            ),
        };
        match self.tempo_changes.get(i) {
            Some(next) if ramp && next.beat > from.beat => {
                let t = ((beat - from.beat) / (next.beat - from.beat)).clamp(0.0, 1.0) as f32;
                from.bpm + (next.bpm - from.bpm) * t
            }
            _ => from.bpm,
        }
    }

    /// Sets the tempo in effect at `beat`, as dragging the tempo in the control bar does in
    /// Logic: the change before it (or the start tempo) takes the new tempo; on a tempo curve
    /// that change moves by as much as the displayed tempo did.
    pub fn set_tempo_at(&mut self, beat: f64, bpm: f32) {
        let shown = self.tempo_at(beat);
        let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        let moved = |own: f32| (own + (bpm - shown)).clamp(MIN_BPM, MAX_BPM);
        match self.tempo_changes.iter_mut().rev().find(|t| t.beat <= beat) {
            Some(t) if t.ramp => t.bpm = moved(t.bpm),
            Some(t) => t.bpm = bpm,
            None if self.tempo_ramp => self.tempo = moved(self.tempo),
            None => self.tempo = bpm,
        }
    }

    /// Adds a tempo change at `beat` (replacing one already there). Returns its index. A change
    /// inside a tempo curve keeps the curve going.
    pub fn add_tempo_change(&mut self, beat: f64, bpm: f32) -> usize {
        let beat = beat.max(0.0);
        let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        if beat <= 0.0 {
            self.tempo = bpm;
            return usize::MAX;
        }
        self.tempo_changes.retain(|t| (t.beat - beat).abs() > 1e-6);
        let i = self.tempo_changes.partition_point(|t| t.beat < beat);
        // Inside a curve (a point before and another after it) the new point continues it.
        let ramp = i < self.tempo_changes.len()
            && match i.checked_sub(1) {
                Some(j) => self.tempo_changes[j].ramp,
                None => self.tempo_ramp,
            };
        self.tempo_changes.insert(i, TempoPoint { beat, bpm, ramp });
        i
    }

    /// Makes the tempo point `i` (0 is the start tempo) ramp to the next point, or hold. The
    /// last point has nothing to ramp to.
    pub fn set_tempo_ramp(&mut self, i: usize, ramp: bool) {
        if i >= self.tempo_changes.len() {
            // Nothing follows the last point: a flag there would only surprise the next point
            // added.
            return;
        }
        if i == 0 {
            self.tempo_ramp = ramp;
        } else {
            self.tempo_changes[i - 1].ramp = ramp;
        }
    }

    /// Sorts the tempo changes and drops invalid ones: at or before the start, duplicates of a
    /// beat (the last one wins) and non-finite values.
    pub fn tidy_tempo(&mut self) {
        self.tempo_changes
            .retain(|t| t.beat.is_finite() && t.beat > 1e-6 && t.bpm.is_finite());
        for t in &mut self.tempo_changes {
            t.bpm = t.bpm.clamp(MIN_BPM, MAX_BPM);
        }
        self.tempo_changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        let mut out: Vec<TempoPoint> = Vec::with_capacity(self.tempo_changes.len());
        for t in self.tempo_changes.drain(..) {
            match out.last_mut() {
                Some(last) if (last.beat - t.beat).abs() <= 1e-6 => *last = t,
                _ => out.push(t),
            }
        }
        self.tempo_changes = out;
        // The last point has nothing to ramp to.
        match self.tempo_changes.last_mut() {
            Some(last) => last.ramp = false,
            None => self.tempo_ramp = false,
        }
    }

    /// The tempo and signature as the engine's tempo map.
    pub fn tempo_map(&self) -> prowave_engine::TempoMap {
        use prowave_engine::{SignatureEvent, TempoCurve, TempoEvent};
        let curve = |ramp: bool| {
            if ramp {
                TempoCurve::Linear
            } else {
                TempoCurve::Step
            }
        };
        let tempos = self
            .tempo_points()
            .into_iter()
            .map(|t| TempoEvent {
                beat: t.beat,
                bpm: t.bpm,
                curve: curve(t.ramp),
            })
            .collect();
        let sigs = std::iter::once(SignatureEvent {
            bar: 0,
            num: self.sig.0,
            den: self.sig.1,
        })
        .chain(self.sig_changes.iter().map(|c| SignatureEvent {
            bar: c.bar - 1,
            num: c.num,
            den: c.den,
        }))
        .collect();
        prowave_engine::TempoMap::new(tempos, sigs)
    }

    /// The signature track as a borrowed view with the bar maths on it.
    pub fn sigs(&self) -> SigRef<'_> {
        SigRef {
            first: self.sig,
            changes: &self.sig_changes,
        }
    }

    /// Time signature of the 0-based bar `bar0`.
    pub fn signature_of_bar(&self, bar0: u32) -> (u8, u8) {
        self.sigs().signature_of_bar(bar0)
    }

    /// Quarter notes in the 0-based bar `bar0`: 4 for 4/4, 3 for 6/8 and 3/4, 3.5 for 7/8.
    pub fn bar_len(&self, bar0: u32) -> f64 {
        self.sigs().bar_len(bar0)
    }

    /// Quarter notes from the start of the song to the start of the 0-based bar `bar0`, which
    /// may be fractional (half a bar in is `bar0 = n + 0.5`). Positions ("beats" elsewhere in
    /// the app) are quarter notes, as in the engine's tempo map and Logic, whatever the
    /// signature.
    pub fn bar_to_beats(&self, bar0: f64) -> f64 {
        self.sigs().bar_to_beats(bar0)
    }

    /// The 0-based bar (with a fraction) that the position `beats` is in; the inverse of
    /// [`Project::bar_to_beats`].
    pub fn beats_to_bar(&self, beats: f64) -> f64 {
        self.sigs().beats_to_bar(beats)
    }

    /// Every bar that overlaps `from..=to` (quarter notes), in order. At most 4096 are given,
    /// so a huge range cannot hang a screen redraw.
    pub fn bars_in(&self, from: f64, to: f64) -> Vec<BarInfo> {
        self.sigs().bars_in(from, to)
    }

    /// The fades region `ri` of track `ti` really has, in quarter notes: its own, or longer
    /// where it crosses another audio region. A region with `crossfade` that starts inside an
    /// earlier region fades in over the overlap, and that earlier region fades out over it
    /// (when the later one reaches at least as far).
    pub fn effective_fades(&self, ti: usize, ri: usize) -> (f32, f32) {
        let Some(track) = self.tracks.get(ti) else {
            return (0.0, 0.0);
        };
        let Some(r) = track.regions.get(ri) else {
            return (0.0, 0.0);
        };
        let (mut fade_in, mut fade_out) = (r.params.fade_in as f64, r.params.fade_out as f64);
        if track.kind == TrackKind::Audio {
            let (s, e) = (self.region_start_beats(r), self.region_end_beats(r));
            for (j, o) in track.regions.iter().enumerate() {
                if j == ri || o.params.mute || o.audio.is_none() {
                    continue;
                }
                let (os, oe) = (self.region_start_beats(o), self.region_end_beats(o));
                if r.params.crossfade && os < s && s < oe {
                    fade_in = fade_in.max(oe.min(e) - s);
                }
                if o.params.crossfade && s < os && os < e && oe >= e {
                    fade_out = fade_out.max(e - os);
                }
            }
        }
        (fade_in as f32, fade_out as f32)
    }

    /// The audio edits of region `ri` of track `ti` as the engine applies them: the region's
    /// own plus its crossfades.
    pub fn clip_edit_of(&self, ti: usize, ri: usize) -> prowave_engine::ClipEdit {
        let mut edit = self.tracks[ti].regions[ri].params.clip_edit();
        let (fade_in, fade_out) = self.effective_fades(ti, ri);
        edit.fade_in_beats = fade_in as f64;
        edit.fade_out_beats = fade_out as f64;
        edit
    }

    /// Where a region starts, in quarter notes.
    pub fn region_start_beats(&self, r: &Region) -> f64 {
        self.bar_to_beats(r.start_bar as f64 - 1.0)
    }

    /// Where a region ends, in quarter notes.
    pub fn region_end_beats(&self, r: &Region) -> f64 {
        self.bar_to_beats(r.start_bar as f64 - 1.0 + r.len_bars as f64)
    }

    /// How long a region is, in quarter notes.
    pub fn region_len_beats(&self, r: &Region) -> f64 {
        self.region_end_beats(r) - self.region_start_beats(r)
    }

    /// Length of one signature beat (in quarter notes) at the position `beats`: 1 in x/4, 0.5
    /// in x/8, 2 in x/2.
    pub fn beat_unit_at(&self, beats: f64) -> f64 {
        self.sigs().beat_unit_at(beats)
    }

    /// Quarter notes in the bar the position `beats` is in.
    pub fn bar_len_at(&self, beats: f64) -> f64 {
        self.sigs().bar_len_at(beats)
    }

    /// Start of the bar that holds `beats`.
    pub fn bar_start(&self, beats: f64) -> f64 {
        self.sigs().bar_start(beats)
    }

    /// Start of the bar after the one that holds `beats`.
    pub fn next_bar_start(&self, beats: f64) -> f64 {
        self.sigs().next_bar_start(beats)
    }

    /// Sets the signature from the 1-based `bar` on and returns whether anything changed. Bar 1
    /// is the song's own signature. Regions sit on bars, so this moves them in time: the song
    /// revision is bumped to have the engine reposition them.
    pub fn set_signature_from(&mut self, bar: u32, sig: (u8, u8)) -> bool {
        let bar = bar.clamp(1, MAX_SIG_BAR);
        let sig = (sig.0.clamp(1, 99), valid_denominator(sig.1));
        let before = (self.sig, self.sig_changes.clone());
        let change = SigChange {
            bar,
            num: sig.0,
            den: sig.1,
        };
        if bar == 1 {
            self.sig = sig;
        } else {
            match self.sig_changes.binary_search_by_key(&bar, |c| c.bar) {
                Ok(i) => self.sig_changes[i] = change,
                Err(i) => self.sig_changes.insert(i, change),
            }
        }
        self.tidy_signatures();
        let changed = (self.sig, self.sig_changes.clone()) != before;
        if changed {
            self.song_rev += 1;
        }
        changed
    }

    /// Removes the signature change that starts at the 1-based `bar` (not bar 1 itself).
    pub fn remove_signature_at(&mut self, bar: u32) -> bool {
        let before = self.sig_changes.len();
        self.sig_changes.retain(|c| c.bar != bar);
        let changed = self.sig_changes.len() != before;
        if changed {
            self.song_rev += 1;
        }
        changed
    }

    /// Sorts the signature changes, fixes invalid values, and drops any at bar 1 or beyond
    /// [`MAX_SIG_BAR`] or a second one on the same bar.
    pub fn tidy_signatures(&mut self) {
        self.sig = (self.sig.0.clamp(1, 99), valid_denominator(self.sig.1));
        self.sig_changes
            .retain(|c| (2..=MAX_SIG_BAR).contains(&c.bar));
        self.sig_changes.sort_by_key(|c| c.bar);
        self.sig_changes.dedup_by_key(|c| c.bar);
        for c in &mut self.sig_changes {
            c.num = c.num.clamp(1, 99);
            c.den = valid_denominator(c.den);
        }
    }

    /// Appends a new track of `kind` (named "Aux 1", "Inst 3"...), selects it and tells the
    /// engine the song changed. Returns its index.
    pub fn add_track(&mut self, kind: TrackKind) -> usize {
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = match kind {
            TrackKind::Instrument => format!("Inst {n}"),
            TrackKind::Audio => format!("Audio {n}"),
            TrackKind::Drummer => format!("Drummer {n}"),
            TrackKind::Aux => format!("Aux {n}"),
            TrackKind::Folder => format!("Folder {n}"),
            TrackKind::Vca => format!("VCA {n}"),
            TrackKind::ExternalMidi => format!("External MIDI {n}"),
            TrackKind::Bus => format!("Bus {n}"),
            TrackKind::Output => format!("Output {n}"),
        };
        self.tracks.push(Track::new(name, kind));
        self.selected_track = self.tracks.len() - 1;
        self.song_rev += 1;
        self.selected_track
    }

    /// The bars `r` covers, for making the notes of a generated region: each one's length in
    /// quarter notes and signature, the last cut off where the region ends.
    pub fn region_bars(&self, r: &Region) -> Vec<BarSpec> {
        let len = self.region_len_beats(r);
        let (mut at, mut covered) = (self.region_start_beats(r), 0.0f64);
        let mut bars = Vec::new();
        while covered < len - 1e-9 && bars.len() < MAX_GENERATED_BARS {
            let bar0 = self.beats_to_bar(at + 1e-9).floor().max(0.0) as u32;
            let (num, den) = self.signature_of_bar(bar0);
            let full = self.bar_len(bar0);
            bars.push(BarSpec {
                beats: full.min(len - covered),
                num,
                den,
            });
            covered += full;
            at += full;
        }
        bars
    }

    /// What a generated region's notes are made for: a number that changes with the bars the
    /// region covers (their lengths and signatures), and so with its length.
    pub fn generated_key(&self, r: &Region) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |word: u64| {
            hash = (hash ^ word).wrapping_mul(0x0000_0100_0000_01b3);
        };
        for bar in self.region_bars(r) {
            mix(bar.beats.to_bits());
            mix(u64::from(bar.num) << 8 | u64::from(bar.den));
        }
        hash
    }

    /// Makes the notes of the generated region `ri` of track `ti` again from its settings and
    /// tells the engine.
    pub fn regenerate(&mut self, ti: usize, ri: usize) {
        let Some(r) = self.tracks.get(ti).and_then(|t| t.regions.get(ri)) else {
            return;
        };
        let Some(generated) = &r.generated else {
            return;
        };
        let notes = match &generated.source {
            Generator::Pattern(p) => p.notes(self.region_len_beats(r)),
            Generator::Drummer(d) => drummer_notes(d, &self.region_bars(r)),
        };
        let key = self.generated_key(r);
        let region = &mut self.tracks[ti].regions[ri];
        region.notes = notes;
        if let Some(g) = region.generated.as_mut() {
            g.made_for = key;
        }
        self.song_rev += 1;
    }

    /// Makes the notes of every generated region whose bars changed (it was lengthened, or a
    /// signature changed under it). Returns whether any was.
    pub fn refresh_generated(&mut self) -> bool {
        let rev = self.song_rev;
        let (mut stale, mut fresh) = (Vec::new(), Vec::new());
        for (ti, t) in self.tracks.iter().enumerate() {
            for (ri, r) in t.regions.iter().enumerate() {
                let Some(g) = &r.generated else {
                    continue;
                };
                // The bars under a region only change when it is moved or resized (its own
                // numbers) or a signature changes (the song revision).
                let seen = (r.start_bar.to_bits(), r.len_bars.to_bits(), rev);
                if g.seen == seen {
                    continue;
                }
                if g.made_for != self.generated_key(r) {
                    stale.push((ti, ri));
                } else {
                    fresh.push((ti, ri, seen));
                }
            }
        }
        for (ti, ri, seen) in fresh {
            if let Some(g) = self.tracks[ti].regions[ri].generated.as_mut() {
                g.seen = seen;
            }
        }
        for &(ti, ri) in &stale {
            self.regenerate(ti, ri);
        }
        !stale.is_empty()
    }

    /// Notes the keys of the generated regions as they are, without making the notes again (a
    /// project was opened with the notes it was saved with).
    pub fn stamp_generated(&mut self) {
        let keys: Vec<(usize, usize, u64)> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| {
                t.regions
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.generated.is_some())
                    .map(move |(ri, r)| (ti, ri, r))
            })
            .map(|(ti, ri, r)| (ti, ri, self.generated_key(r)))
            .collect();
        for (ti, ri, key) in keys {
            if let Some(g) = self.tracks[ti].regions[ri].generated.as_mut() {
                g.made_for = key;
            }
        }
    }

    /// Adds a Pattern or Drummer region of 4 bars to track `ti`, starting at the bar `at_bar`
    /// (1-based), with the notes its settings make. Returns its index; `None` for a track that
    /// holds no MIDI regions.
    pub fn add_generated_region(&mut self, ti: usize, drummer: bool, at_bar: f32) -> Option<usize> {
        let track = self.tracks.get_mut(ti)?;
        if !track.kind.has_regions() || track.kind == TrackKind::Audio {
            return None;
        }
        let drums = track.kind == TrackKind::Drummer
            || track.instrument.is_some_and(|i| i.contains("Drum"));
        let (name, source) = if drummer {
            (
                "Drummer",
                Generator::Drummer(DrummerParams::new(DrumStyle::Rock)),
            )
        } else if drums {
            ("Pattern", Generator::Pattern(PatternParams::drums()))
        } else {
            ("Pattern", Generator::Pattern(PatternParams::melodic()))
        };
        track.regions.push(Region {
            name: name.into(),
            start_bar: at_bar.max(1.0),
            len_bars: 4.0,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            audio: None,
            params: RegionParams::default(),
            generated: Some(Generated::new(source)),
        });
        let ri = track.regions.len() - 1;
        self.regenerate(ti, ri);
        Some(ri)
    }

    /// Turns a generated region into an ordinary MIDI region with the notes it has now, which
    /// can then be edited note by note.
    pub fn convert_to_midi_region(&mut self, ti: usize, ri: usize) {
        if let Some(r) = self.tracks.get_mut(ti).and_then(|t| t.regions.get_mut(ri)) {
            if r.generated.take().is_some() {
                self.song_rev += 1;
            }
        }
    }

    /// The channel name the Inspector shows for track `i`: "Audio 2", "Aux 1" or "Inst 3",
    /// counting the tracks of the same class up to and including it.
    pub fn channel_name(&self, i: usize) -> String {
        let class = |k: TrackKind| match k {
            TrackKind::Audio => 0,
            TrackKind::Aux => 1,
            _ => 2,
        };
        let c = class(self.tracks[i].kind);
        let nth = self.tracks[..=i]
            .iter()
            .filter(|t| class(t.kind) == c)
            .count();
        let label = ["Audio", "Aux", "Inst"][c];
        format!("{label} {nth}")
    }

    /// Index just past the last track inside folder `i` (a track with a higher nesting level
    /// than the folder, directly after it). For a track that is not a folder this is `i + 1`.
    pub fn folder_end(&self, i: usize) -> usize {
        let level = self.tracks[i].indent;
        let mut end = i + 1;
        while end < self.tracks.len() && self.tracks[end].indent > level {
            end += 1;
        }
        end
    }

    /// Whether track `i` is hidden because a folder around it is folded.
    pub fn hidden_by_fold(&self, i: usize) -> bool {
        let mut level = self.tracks[i].indent;
        for j in (0..i).rev() {
            let t = &self.tracks[j];
            if t.indent < level {
                if t.kind == TrackKind::Folder && t.folded {
                    return true;
                }
                level = t.indent;
            }
        }
        false
    }

    /// Track > Create Track Stack: puts track `i` (with everything inside it, if it is a
    /// folder) into a new folder at its place, selects the folder and returns its index.
    /// Tracks after it move down one slot, so the engine is told the song changed.
    pub fn create_track_stack(&mut self, i: usize) -> usize {
        let (end, level) = (self.folder_end(i), self.tracks[i].indent);
        for t in &mut self.tracks[i..end] {
            t.indent = t.indent.saturating_add(1);
        }
        let n = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Folder)
            .count()
            + 1;
        let mut folder = Track::new(format!("Folder {n}"), TrackKind::Folder);
        folder.indent = level;
        self.tracks.insert(i, folder);
        self.vca_inserted(i);
        self.selected_track = i;
        self.song_rev += 1;
        i
    }

    /// Track > Release Track Stack: removes folder `i` and lifts the tracks inside it one
    /// level. Returns false (and changes nothing) when `i` is not a folder.
    pub fn release_track_stack(&mut self, i: usize) -> bool {
        if self
            .tracks
            .get(i)
            .is_none_or(|t| t.kind != TrackKind::Folder)
        {
            return false;
        }
        let end = self.folder_end(i);
        for t in &mut self.tracks[i + 1..end] {
            t.indent = t.indent.saturating_sub(1);
        }
        self.tracks.remove(i);
        self.vca_removed(i);
        if self.selected_track > i {
            self.selected_track -= 1;
        }
        self.selected_track = self.selected_track.min(self.tracks.len().saturating_sub(1));
        self.song_rev += 1;
        true
    }

    /// Keeps VCA assignments pointing at the right tracks after a track was inserted at `at`.
    pub fn vca_inserted(&mut self, at: usize) {
        for t in &mut self.tracks {
            if let Some(v) = t.vca.as_mut() {
                if *v >= at {
                    *v += 1;
                }
            }
        }
    }

    /// Keeps VCA assignments right after track `removed` was taken out: tracks that were
    /// assigned to it have no VCA any more.
    pub fn vca_removed(&mut self, removed: usize) {
        for t in &mut self.tracks {
            t.vca = match t.vca {
                Some(v) if v == removed => None,
                Some(v) if v > removed => Some(v - 1),
                other => other,
            };
        }
    }

    /// The VCA track that controls track `i`, if it has one.
    pub fn vca_of(&self, i: usize) -> Option<&Track> {
        let t = self.tracks.get(i)?;
        let vca = self.tracks.get(t.vca?)?;
        (vca.kind == TrackKind::Vca && t.kind != TrackKind::Vca).then_some(vca)
    }

    /// Assigns track `i` to the VCA track `vca` (or to none). Only a track that is not a VCA
    /// itself can be assigned, and only to a VCA track; returns whether it changed.
    pub fn assign_vca(&mut self, i: usize, vca: Option<usize>) -> bool {
        let valid =
            vca.is_none_or(|v| self.tracks.get(v).is_some_and(|t| t.kind == TrackKind::Vca));
        match self.tracks.get(i) {
            Some(t) if valid && t.kind != TrackKind::Vca && t.vca != vca => {
                self.tracks[i].vca = vca;
                true
            }
            _ => false,
        }
    }

    /// Makes a mix group named `name` (with every attribute on) and returns its index, or
    /// `None` at the limit.
    pub fn add_group(&mut self, name: &str) -> Option<usize> {
        if self.groups.len() >= MAX_GROUPS {
            return None;
        }
        self.groups.push(MixGroup::new(name));
        Some(self.groups.len() - 1)
    }

    /// Puts track `i` in group `g` (or in none). Folders and VCA tracks have no strip to group.
    pub fn assign_group(&mut self, i: usize, g: Option<usize>) -> bool {
        let valid = g.is_none_or(|g| g < self.groups.len());
        match self.tracks.get_mut(i) {
            Some(t) if valid && !matches!(t.kind, TrackKind::Folder | TrackKind::Vca) => {
                let changed = t.group != g;
                t.group = g;
                changed
            }
            _ => false,
        }
    }

    /// Takes group `g` away; its tracks are left without one, and the groups after it move up.
    pub fn delete_group(&mut self, g: usize) {
        if g >= self.groups.len() {
            return;
        }
        self.groups.remove(g);
        for t in self.tracks.iter_mut() {
            t.group = match t.group {
                Some(x) if x == g => None,
                Some(x) if x > g => Some(x - 1),
                other => other,
            };
        }
    }

    /// The other tracks of track `i`'s group, when the group has the attribute `attr` (a
    /// function of the group).
    pub fn group_mates(&self, i: usize, attr: impl Fn(&MixGroup) -> bool) -> Vec<usize> {
        let Some(g) = self.tracks.get(i).and_then(|t| t.group) else {
            return Vec::new();
        };
        if !self.groups.get(g).is_some_and(attr) {
            return Vec::new();
        }
        (0..self.tracks.len())
            .filter(|j| *j != i && self.tracks[*j].group == Some(g))
            .collect()
    }

    /// Sets Mute on track `i` and on the tracks of its group, if the group has Mute.
    pub fn set_mute(&mut self, i: usize, on: bool) {
        for j in std::iter::once(i).chain(self.group_mates(i, |g| g.mute)) {
            if let Some(t) = self.tracks.get_mut(j) {
                t.mute = on;
            }
        }
    }

    /// Sets Solo on track `i` and on the tracks of its group, if the group has Solo.
    pub fn set_solo(&mut self, i: usize, on: bool) {
        for j in std::iter::once(i).chain(self.group_mates(i, |g| g.solo)) {
            if let Some(t) = self.tracks.get_mut(j) {
                t.solo = on;
            }
        }
    }

    /// The bus an Aux strip listens to: Aux 1 takes Bus 1, the next Aux Bus 2, and so on up to
    /// Bus 8. 0 for any other strip, and for an Aux strip past the eighth.
    pub fn aux_bus_of(&self, i: usize) -> u8 {
        match self.tracks.get(i) {
            Some(t) if t.kind == TrackKind::Aux => {
                let n = self.tracks[..=i]
                    .iter()
                    .filter(|t| t.kind == TrackKind::Aux)
                    .count();
                if n <= prowave_engine::MAX_BUSES {
                    n as u8
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Whether track `i` is muted, by its own Mute or by its VCA's.
    pub fn is_muted(&self, i: usize) -> bool {
        self.tracks.get(i).is_some_and(|t| t.mute) || self.vca_of(i).is_some_and(|v| v.mute)
    }

    /// Whether track `i` is soloed, by its own Solo or by its VCA's. A VCA track is never
    /// soloed itself: it makes no sound, its Solo reaches the tracks assigned to it.
    pub fn is_soloed(&self, i: usize) -> bool {
        self.tracks
            .get(i)
            .is_some_and(|t| t.kind != TrackKind::Vca && t.solo)
            || self.vca_of(i).is_some_and(|v| v.solo)
    }

    /// Whether any track is soloed (the others then fall silent).
    pub fn any_solo(&self) -> bool {
        (0..self.tracks.len()).any(|i| self.is_soloed(i))
    }

    /// What the engine mixes track `i` with: its level plus its VCA's, and Mute and Solo from
    /// the track or its VCA. A VCA track itself is left at unity: it has no sound.
    pub fn track_mix_of(&self, i: usize) -> (f32, bool, bool) {
        let Some(t) = self.tracks.get(i) else {
            return (0.0, false, false);
        };
        if t.kind == TrackKind::Vca {
            return (0.0, false, false);
        }
        let vca_db = self.vca_of(i).map_or(0.0, |v| v.volume_db);
        (t.volume_db + vca_db, self.is_muted(i), self.is_soloed(i))
    }

    /// Track > Delete Track: removes track `i` with its regions. Deleting a folder keeps the
    /// tracks inside it (as Release Track Stack does). The last track cannot be deleted, so
    /// there is always a selected track; returns false then and when `i` does not exist.
    pub fn delete_track(&mut self, i: usize) -> bool {
        if i >= self.tracks.len() || self.tracks.len() == 1 {
            return false;
        }
        if self.tracks[i].kind == TrackKind::Folder {
            return self.release_track_stack(i);
        }
        self.tracks.remove(i);
        self.vca_removed(i);
        if self.selected_track > i {
            self.selected_track -= 1;
        }
        self.selected_track = self.selected_track.min(self.tracks.len() - 1);
        self.normalize_nesting();
        self.song_rev += 1;
        true
    }

    /// Sets Mute and/or Solo on folder `i` and on every track inside it, as a folder's M and S
    /// buttons do.
    pub fn set_folder_flags(&mut self, i: usize, mute: Option<bool>, solo: Option<bool>) {
        let end = self.folder_end(i);
        for t in &mut self.tracks[i..end] {
            t.mute = mute.unwrap_or(t.mute);
            t.solo = solo.unwrap_or(t.solo);
        }
    }

    /// Repairs folder nesting: a track can sit at most one level below the track before it,
    /// and only when that one is a `Folder`. Run after loading a file that may be hand-edited.
    pub fn normalize_nesting(&mut self) {
        let mut allowed = 0u8;
        for t in &mut self.tracks {
            t.indent = t.indent.min(allowed);
            allowed = t.indent + u8::from(t.kind == TrackKind::Folder);
        }
    }

    /// Moves one half of the time signature by `steps` (up is positive) and returns whether it
    /// changed. The numerator runs 1-99, the denominator through the powers of two 1-64; both
    /// stop at their ends. Regions sit on bars, so a change moves them in time: the song
    /// revision is bumped to have the engine reposition them.
    pub fn step_signature(&mut self, part: SigPart, steps: i32) -> bool {
        self.set_signature_from(1, stepped_signature(self.sig, part, steps))
    }

    /// [`Project::step_signature`] for the signature in force at the 0-based bar `bar0`: the
    /// change that holds there (or the song's own) is the one that moves.
    pub fn step_signature_at(&mut self, bar0: u32, part: SigPart, steps: i32) -> bool {
        let from = self
            .sig_changes
            .iter()
            .rev()
            .find(|c| c.bar <= bar0 + 1)
            .map_or(1, |c| c.bar);
        let now = self.signature_of_bar(bar0);
        self.set_signature_from(from, stepped_signature(now, part, steps))
    }

    /// Logic-style position for a beat: (bar, beat, division, tick), all 1-based, where
    /// "beat" is the signature's beat (an eighth in 6/8), divisions are 16th notes and there
    /// are 240 ticks per division.
    pub fn bbdt(&self, beats: f64) -> (u32, u32, u32, u32) {
        let beats = beats.max(0.0);
        let bar0 = self.beats_to_bar(beats) as u32;
        let unit = unit_of(self.signature_of_bar(bar0).1);
        let bar = bar0 + 1;
        let in_beats = (beats - self.bar_to_beats(bar0 as f64)).max(0.0) / unit;
        let beat = in_beats as u32 + 1;
        let divs = (unit * 4.0).max(1.0);
        let sixteenths = in_beats.fract() * divs;
        let div = sixteenths as u32 + 1;
        let tick = (sixteenths.fract() * 240.0) as u32 + 1;
        (bar, beat, div, tick)
    }

    pub fn bar_beat_tick(&self) -> (u32, u32, u32) {
        let beats = self.position_beats.max(0.0);
        let bar0 = self.beats_to_bar(beats) as u32;
        let unit = unit_of(self.signature_of_bar(bar0).1);
        let bar = bar0 + 1;
        let in_beats = (beats - self.bar_to_beats(bar0 as f64)).max(0.0) / unit;
        let beat = in_beats as u32 + 1;
        let tick = (in_beats.fract() * 960.0) as u32;
        (bar, beat, tick)
    }

    /// The project start time, in frames of the SMPTE frame rate.
    pub fn smpte_offset_frames(&self) -> i64 {
        let [h, m, s, f] = self.smpte_start;
        self.smpte_rate.frames_of(prowave_engine::Smpte {
            h,
            m,
            s,
            f,
            subframe: 0,
        })
    }

    /// The SMPTE time of `beat` as Logic's display shows it: `hh:mm:ss:ff.ss`, 80 subframes
    /// to a frame, with a `;` before the frames in drop frame. It follows every tempo change
    /// and curve.
    pub fn timecode_at(&self, beat: f64) -> String {
        let rate = self.smpte_rate;
        let frames =
            self.tempo_map()
                .smpte_frames_at(beat.max(0.0), rate, self.smpte_offset_frames());
        let t = rate.smpte_of_frames(frames);
        // The same rounding the label got, so a position on a frame shows `.00`.
        let whole = (frames * 1e6).round() / 1e6;
        let sub = ((whole - whole.floor()) * 80.0) as u32;
        let sep = if rate.is_drop_frame() { ';' } else { ':' };
        format!("{:02}:{:02}:{:02}{sep}{:02}.{sub:02}", t.h, t.m, t.s, t.f)
    }

    /// [`Project::timecode_at`] the playhead.
    pub fn timecode(&self) -> String {
        self.timecode_at(self.position_beats)
    }
}

/// `sig` with one half moved by `steps`: the numerator runs 1-99, the denominator through the
/// powers of two 1-64; both stop at their ends.
fn stepped_signature(sig: (u8, u8), part: SigPart, steps: i32) -> (u8, u8) {
    match part {
        SigPart::Numerator => ((i32::from(sig.0) + steps).clamp(1, 99) as u8, sig.1),
        SigPart::Denominator => {
            let at = SIGNATURE_DENOMINATORS
                .iter()
                .position(|d| *d == sig.1)
                .unwrap_or(2);
            let last = SIGNATURE_DENOMINATORS.len() as i32 - 1;
            let to = (at as i32 + steps).clamp(0, last) as usize;
            (sig.0, SIGNATURE_DENOMINATORS[to])
        }
    }
}

/// A time-signature change on the Signature track: `num/den` from the start of the 1-based
/// `bar` (always 2 or later; bar 1 is [`Project::sig`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SigChange {
    pub bar: u32,
    pub num: u8,
    pub den: u8,
}

/// The signature track seen from outside the [`Project`]: the first bar's signature and the
/// changes after it, with the bar maths. Copy it from [`Project::sigs`]; [`SigRef::to_owned`]
/// gives a copy that does not borrow the project.
#[derive(Clone, Copy)]
pub struct SigRef<'a> {
    pub first: (u8, u8),
    pub changes: &'a [SigChange],
}

/// [`SigRef`] that owns its changes, to move into closures while the project is edited.
#[derive(Clone, Debug, PartialEq)]
pub struct SigOwned {
    pub first: (u8, u8),
    pub changes: Vec<SigChange>,
}

impl SigOwned {
    pub fn as_ref(&self) -> SigRef<'_> {
        SigRef {
            first: self.first,
            changes: &self.changes,
        }
    }
}

impl SigRef<'_> {
    pub fn to_owned(self) -> SigOwned {
        SigOwned {
            first: self.first,
            changes: self.changes.to_vec(),
        }
    }

    pub fn signature_of_bar(self, bar0: u32) -> (u8, u8) {
        let n = self.changes.partition_point(|c| c.bar <= bar0 + 1);
        n.checked_sub(1)
            .map_or(self.first, |i| (self.changes[i].num, self.changes[i].den))
    }

    pub fn bar_len(self, bar0: u32) -> f64 {
        let (num, den) = self.signature_of_bar(bar0);
        num.max(1) as f64 * unit_of(den)
    }

    pub fn bar_to_beats(self, bar0: f64) -> f64 {
        let bar0 = bar0.max(0.0);
        let (mut beats, mut at, mut len) = (0.0, 0.0, self.bar_len(0));
        for c in self.changes {
            let start = (c.bar - 1) as f64;
            if start >= bar0 {
                break;
            }
            beats += (start - at) * len;
            at = start;
            len = c.num.max(1) as f64 * unit_of(c.den);
        }
        beats + (bar0 - at) * len
    }

    pub fn beats_to_bar(self, beats: f64) -> f64 {
        let beats = beats.max(0.0);
        let (mut at_beats, mut at, mut len) = (0.0, 0.0, self.bar_len(0));
        for c in self.changes {
            let start = (c.bar - 1) as f64;
            let start_beats = at_beats + (start - at) * len;
            if start_beats > beats {
                break;
            }
            at_beats = start_beats;
            at = start;
            len = c.num.max(1) as f64 * unit_of(c.den);
        }
        at + (beats - at_beats) / len
    }

    pub fn beat_unit_at(self, beats: f64) -> f64 {
        unit_of(self.signature_of_bar(self.beats_to_bar(beats) as u32).1)
    }

    pub fn bar_len_at(self, beats: f64) -> f64 {
        self.bar_len(self.beats_to_bar(beats) as u32)
    }

    pub fn bar_start(self, beats: f64) -> f64 {
        self.bar_to_beats(self.beats_to_bar(beats).floor())
    }

    pub fn next_bar_start(self, beats: f64) -> f64 {
        self.bar_to_beats(self.beats_to_bar(beats).floor() + 1.0)
    }

    /// The bar line nearest to `beats`.
    pub fn nearest_bar_line(self, beats: f64) -> f64 {
        self.bar_to_beats(self.beats_to_bar(beats).round())
    }

    pub fn bars_in(self, from: f64, to: f64) -> Vec<BarInfo> {
        let first = self.beats_to_bar(from.max(0.0)).floor() as u32;
        let mut out = Vec::new();
        for bar0 in first..first.saturating_add(4096) {
            let start = self.bar_to_beats(bar0 as f64);
            if start > to {
                break;
            }
            let (num, den) = self.signature_of_bar(bar0);
            out.push(BarInfo {
                bar0,
                start,
                num,
                unit: unit_of(den),
            });
        }
        out
    }
}

/// One bar of the song, for drawing grids and rulers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarInfo {
    /// 0-based bar number.
    pub bar0: u32,
    /// Where it starts, in quarter notes.
    pub start: f64,
    /// Beats in the bar (the signature's numerator).
    pub num: u8,
    /// Quarter notes in one beat of the bar.
    pub unit: f64,
}

/// The last bar a signature change may start at.
pub const MAX_SIG_BAR: u32 = 9999;

/// Length of one signature beat in quarter notes: 1 for x/4, 0.5 for x/8, 2 for x/2. An
/// invalid denominator counts as 4.
fn unit_of(den: u8) -> f64 {
    match den {
        d @ (1 | 2 | 4 | 8 | 16 | 32 | 64) => 4.0 / d as f64,
        _ => 1.0,
    }
}

/// `den` if a time signature can have it, else 4.
fn valid_denominator(den: u8) -> u8 {
    if SIGNATURE_DENOMINATORS.contains(&den) {
        den
    } else {
        4
    }
}

/// The beat lengths a time signature can have (its denominator), as Logic offers them.
pub const SIGNATURE_DENOMINATORS: [u8; 7] = [1, 2, 4, 8, 16, 32, 64];

/// One half of a time signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigPart {
    Numerator,
    Denominator,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_parts_step_and_stop_at_their_ends() {
        let mut p = Project::demo();
        assert_eq!(p.sig, (4, 4));
        assert!(p.step_signature(SigPart::Numerator, 2));
        assert!(p.step_signature(SigPart::Denominator, 1));
        assert_eq!(p.sig, (6, 8), "the denominator steps through powers of two");
        assert!(p.step_signature(SigPart::Denominator, 3));
        assert_eq!(p.sig, (6, 64));
        assert!(!p.step_signature(SigPart::Denominator, 5), "stops at 64");
        assert!(p.step_signature(SigPart::Denominator, -9));
        assert_eq!(p.sig.1, 1, "and at 1");
        p.step_signature(SigPart::Numerator, 500);
        assert_eq!(p.sig.0, 99);
        p.step_signature(SigPart::Numerator, -500);
        assert_eq!(p.sig.0, 1);
    }

    #[test]
    fn a_changed_signature_moves_bars_and_asks_the_engine_to_follow() {
        let mut p = Project::demo();
        let rev = p.song_rev;
        assert_eq!(p.bar_len(0), 4.0);
        assert!(p.step_signature(SigPart::Numerator, -1));
        assert!(p.step_signature(SigPart::Denominator, 1));
        assert_eq!(p.sig, (3, 8));
        assert_eq!(p.bar_len(0), 1.5);
        assert_eq!(p.song_rev, rev + 2);

        // Nothing changes at an end stop, so there is nothing to resend.
        let mut q = Project::demo();
        let rev = q.song_rev;
        assert!(!q.step_signature(SigPart::Numerator, 0));
        assert_eq!(q.song_rev, rev);
    }

    /// 4/4 for two bars, 3/4 for two, 6/8 for two, then 7/8: bars 3, 5 and 7 (1-based).
    fn mixed_signatures() -> Project {
        let mut p = Project::demo();
        assert!(p.set_signature_from(3, (3, 4)));
        assert!(p.set_signature_from(5, (6, 8)));
        assert!(p.set_signature_from(7, (7, 8)));
        p
    }

    #[test]
    fn bars_have_the_length_of_their_signature() {
        let p = mixed_signatures();
        let lens: Vec<f64> = (0..8).map(|b| p.bar_len(b)).collect();
        assert_eq!(lens, [4.0, 4.0, 3.0, 3.0, 3.0, 3.0, 3.5, 3.5]);
        assert_eq!(p.signature_of_bar(1), (4, 4));
        assert_eq!(p.signature_of_bar(2), (3, 4));
        assert_eq!(p.signature_of_bar(4), (6, 8));
        assert_eq!(p.signature_of_bar(99), (7, 8));
        // Quarter notes at the start of each bar.
        let starts: Vec<f64> = (0..9).map(|b| p.bar_to_beats(b as f64)).collect();
        assert_eq!(starts, [0.0, 4.0, 8.0, 11.0, 14.0, 17.0, 20.0, 23.5, 27.0]);
        // Half way through the 3/4 bar.
        assert_eq!(p.bar_to_beats(2.5), 9.5);
        assert_eq!(p.beats_to_bar(9.5), 2.5);
    }

    #[test]
    fn beats_and_bars_convert_back_and_forth() {
        let p = mixed_signatures();
        for i in 0..400 {
            let beats = i as f64 * 0.37;
            let back = p.bar_to_beats(p.beats_to_bar(beats));
            assert!((back - beats).abs() < 1e-9, "{beats} -> {back}");
        }
        assert_eq!(p.bar_start(12.0), 11.0);
        assert_eq!(p.next_bar_start(12.0), 14.0);
        assert_eq!(p.next_bar_start(14.0), 17.0);
        assert_eq!(p.beat_unit_at(1.0), 1.0, "x/4 beat");
        assert_eq!(p.beat_unit_at(15.0), 0.5, "x/8 beat");
    }

    #[test]
    fn positions_read_in_the_signature_of_their_bar() {
        let mut p = mixed_signatures();
        // 6/8: bar 5 starts at 14; one eighth in is 14.5 -> bar 5, beat 2.
        assert_eq!(p.bbdt(14.5), (5, 2, 1, 1));
        // 3/4: bar 3 starts at 8; beat 3 of that bar is at 10.
        assert_eq!(p.bbdt(10.0), (3, 3, 1, 1));
        p.position_beats = 10.0;
        assert_eq!(p.bar_beat_tick(), (3, 3, 0));
        p.position_beats = 23.5;
        assert_eq!(p.bar_beat_tick(), (8, 1, 0));
    }

    #[test]
    fn regions_sit_on_bars_whatever_the_signatures() {
        let p = mixed_signatures();
        let mut r = p.tracks[0].regions[0].clone();
        r.start_bar = 4.0;
        r.len_bars = 2.0;
        assert_eq!(p.region_start_beats(&r), 11.0);
        assert_eq!(p.region_end_beats(&r), 17.0);
        assert_eq!(p.region_len_beats(&r), 6.0);
    }

    #[test]
    fn signature_changes_are_kept_in_order_and_edited_in_place() {
        let mut p = Project::demo();
        let rev = p.song_rev;
        assert!(p.set_signature_from(9, (5, 4)));
        assert!(p.set_signature_from(3, (3, 4)));
        assert!(
            !p.set_signature_from(3, (3, 4)),
            "the same again changes nothing"
        );
        assert!(
            p.set_signature_from(3, (3, 8)),
            "edits the change on that bar"
        );
        let bars: Vec<u32> = p.sig_changes.iter().map(|c| c.bar).collect();
        assert_eq!(bars, [3, 9]);
        assert_eq!(
            p.sig_changes[0],
            SigChange {
                bar: 3,
                num: 3,
                den: 8
            }
        );
        assert!(p.song_rev > rev, "the engine is asked to follow");
        // Bar 1 is the song's own signature.
        assert!(p.set_signature_from(1, (6, 8)));
        assert_eq!(p.sig, (6, 8));
        assert_eq!(p.sig_changes.len(), 2);
        // Bad values are fixed; a change can be removed, bar 1's cannot.
        assert!(p.set_signature_from(20, (0, 5)));
        assert_eq!(p.signature_of_bar(25), (1, 4));
        assert!(p.remove_signature_at(20));
        assert!(!p.remove_signature_at(20));
        assert!(!p.remove_signature_at(1));
    }

    #[test]
    fn stepping_edits_the_signature_in_force_at_a_bar() {
        let mut p = Project::demo();
        p.set_signature_from(5, (3, 4));
        // Bar 7 (0-based 6) is in the 3/4 block: that block changes, not the first.
        assert!(p.step_signature_at(6, SigPart::Numerator, 1));
        assert_eq!(p.sig, (4, 4));
        assert_eq!(
            p.sig_changes[0],
            SigChange {
                bar: 5,
                num: 4,
                den: 4
            }
        );
        // Bar 2 is in the first block.
        assert!(p.step_signature_at(1, SigPart::Denominator, 1));
        assert_eq!(p.sig, (4, 8));
        assert_eq!(p.sig_changes.len(), 1);
    }

    #[test]
    fn bars_in_a_range_are_listed_with_their_signature() {
        let p = mixed_signatures();
        let bars = p.bars_in(9.0, 15.0);
        let got: Vec<(u32, f64, u8)> = bars.iter().map(|b| (b.bar0, b.start, b.num)).collect();
        // Bar 3 holds beat 9 (it starts at 8); bar 5 starts at 14 and is the last one at 15.
        assert_eq!(got, [(2, 8.0, 3), (3, 11.0, 3), (4, 14.0, 6)]);
        assert_eq!(bars[2].unit, 0.5);
    }

    /// An audio track with regions at the given (start bar, length in bars), 1-based, each
    /// with a file-less audio marker so they count as audio regions.
    fn audio_regions(spans: &[(f32, f32)]) -> Project {
        let mut p = Project::empty();
        p.tracks.clear();
        let mut t = Track::new("Audio 1", TrackKind::Audio);
        for &(start_bar, len_bars) in spans {
            t.regions.push(Region {
                name: "r".into(),
                start_bar,
                len_bars,
                notes: Vec::new(),
                controls: Vec::new(),
                folder: None,
                generated: None,
                audio: Some(test_audio()),
                params: RegionParams::default(),
            });
        }
        p.tracks.push(t);
        p
    }

    fn test_audio() -> AudioRegion {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prowave-engine/tests/fixtures");
        AudioRegion {
            file: prowave_engine::AudioFile::open(dir.join("tone.caf")).unwrap(),
            peaks: Default::default(),
            flex: None,
        }
    }

    #[test]
    fn overlapping_regions_crossfade_over_the_overlap_when_asked() {
        // Region 0 plays bars 1-3, region 1 bars 3-5: one bar (4 quarter notes) overlaps.
        let mut p = audio_regions(&[(1.0, 3.0), (3.0, 3.0)]);
        assert_eq!(
            p.effective_fades(0, 0),
            (0.0, 0.0),
            "no crossfade unless asked"
        );
        assert_eq!(p.effective_fades(0, 1), (0.0, 0.0));
        p.tracks[0].regions[1].params.crossfade = true;
        assert_eq!(
            p.effective_fades(0, 0),
            (0.0, 4.0),
            "the earlier one fades out"
        );
        assert_eq!(
            p.effective_fades(0, 1),
            (4.0, 0.0),
            "the later one fades in"
        );
        // The engine gets the same.
        let edit = p.clip_edit_of(0, 1);
        assert_eq!((edit.fade_in_beats, edit.fade_out_beats), (4.0, 0.0));
        // A fade the region already had stays when it is longer.
        p.tracks[0].regions[1].params.fade_in = 10.0;
        p.tracks[0].regions[0].params.fade_out = 12.0;
        assert_eq!(p.effective_fades(0, 0).1, 12.0);
        assert_eq!(p.effective_fades(0, 1).0, 10.0);
    }

    #[test]
    fn crossfades_follow_the_signatures() {
        let mut p = audio_regions(&[(1.0, 3.0), (3.0, 3.0)]);
        p.set_signature_from(3, (3, 4));
        p.tracks[0].regions[1].params.crossfade = true;
        // The overlap is bar 3, which is 3 quarter notes now.
        assert_eq!(p.effective_fades(0, 1).0, 3.0);
    }

    #[test]
    fn regions_that_do_not_overlap_or_are_muted_do_not_crossfade() {
        let mut p = audio_regions(&[(1.0, 2.0), (3.0, 2.0)]);
        p.tracks[0].regions[1].params.crossfade = true;
        assert_eq!(p.effective_fades(0, 1), (0.0, 0.0), "they only touch");
        let mut p = audio_regions(&[(1.0, 3.0), (3.0, 3.0)]);
        p.tracks[0].regions[1].params.crossfade = true;
        p.tracks[0].regions[0].params.mute = true;
        assert_eq!(
            p.effective_fades(0, 1),
            (0.0, 0.0),
            "a muted region is not heard"
        );
        // A region wholly inside another one does not cut it short with a fade out.
        let mut p = audio_regions(&[(1.0, 6.0), (3.0, 1.0)]);
        p.tracks[0].regions[1].params.crossfade = true;
        assert_eq!(p.effective_fades(0, 0).1, 0.0);
    }

    /// Instrument tracks 0 and 1, a VCA at 2 and an instrument at 3; 0 and 1 follow the VCA.
    fn with_vca() -> Project {
        let mut p = Project::empty();
        p.tracks.clear();
        for n in 0..2 {
            p.tracks
                .push(Track::new(format!("Inst {n}"), TrackKind::Instrument));
        }
        p.tracks.push(Track::new("VCA 1", TrackKind::Vca));
        p.tracks.push(Track::new("Inst 3", TrackKind::Instrument));
        assert!(p.assign_vca(0, Some(2)));
        assert!(p.assign_vca(1, Some(2)));
        p
    }

    #[test]
    fn a_vca_moves_the_level_of_its_tracks() {
        let mut p = with_vca();
        p.tracks[0].volume_db = -3.0;
        p.tracks[2].volume_db = -6.0;
        assert_eq!(p.track_mix_of(0), (-9.0, false, false), "track + VCA");
        assert_eq!(p.track_mix_of(1), (-6.0, false, false));
        assert_eq!(p.track_mix_of(3), (0.0, false, false), "not assigned");
        assert_eq!(
            p.track_mix_of(2),
            (0.0, false, false),
            "the VCA itself makes no sound"
        );
        assert!(p.vca_of(0).is_some() && p.vca_of(3).is_none());
    }

    #[test]
    fn a_vcas_mute_and_solo_reach_its_tracks() {
        let mut p = with_vca();
        p.tracks[2].mute = true;
        assert!(p.is_muted(0) && p.is_muted(1) && !p.is_muted(3));
        assert!(p.track_mix_of(0).1);
        assert!(
            !p.track_mix_of(2).1,
            "the VCA track is not muted in the engine"
        );
        p.tracks[2].mute = false;
        p.tracks[2].solo = true;
        assert!(p.is_soloed(0) && p.is_soloed(1) && !p.is_soloed(3));
        assert!(p.any_solo(), "the others fall silent");
        assert!(
            !p.track_mix_of(2).2,
            "the VCA itself is not soloed in the engine"
        );
        // A VCA with nobody assigned solos nothing.
        let mut q = Project::empty();
        q.tracks.clear();
        q.tracks.push(Track::new("VCA 1", TrackKind::Vca));
        q.tracks.push(Track::new("Inst", TrackKind::Instrument));
        q.tracks[0].solo = true;
        assert!(!q.any_solo());
    }

    #[test]
    fn only_a_non_vca_track_can_be_assigned_to_a_vca_track() {
        let mut p = with_vca();
        assert!(!p.assign_vca(2, Some(2)), "a VCA cannot follow a VCA");
        assert!(!p.assign_vca(3, Some(1)), "the target must be a VCA");
        assert!(!p.assign_vca(3, Some(9)), "and it must exist");
        assert!(!p.assign_vca(0, Some(2)), "already assigned");
        assert!(p.assign_vca(0, None));
        assert_eq!(p.tracks[0].vca, None);
    }

    #[test]
    fn assignments_follow_the_tracks_when_tracks_come_and_go() {
        let mut p = with_vca();
        // A track is deleted before the VCA: the VCA moves up one place, and so do the links.
        assert!(p.delete_track(0));
        assert_eq!(p.tracks[1].kind, TrackKind::Vca);
        assert_eq!(p.tracks[0].vca, Some(1));
        // A folder is created in front: everything shifts down.
        p.create_track_stack(0);
        assert_eq!(
            p.tracks[1].vca,
            Some(2),
            "{:?}",
            p.tracks.iter().map(|t| t.vca).collect::<Vec<_>>()
        );
        // Deleting the VCA frees its tracks.
        let vca = p
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Vca)
            .unwrap();
        assert!(p.delete_track(vca));
        assert!(p.tracks.iter().all(|t| t.vca.is_none()));
    }

    #[test]
    fn new_tracks_have_the_defaults_of_their_kind() {
        let inst = Track::new("Inst 1", TrackKind::Instrument);
        assert_eq!((inst.instrument, inst.regions.len()), (Some("Sampler"), 0));
        for kind in [TrackKind::Aux, TrackKind::Folder] {
            let t = Track::new("x", kind);
            assert!(t.instrument.is_none() && !kind.has_regions());
            assert_eq!(
                (t.indent, t.folded, t.volume_db, t.height),
                (0, false, 0.0, 1.0)
            );
        }
        assert!(TrackKind::Audio.has_regions() && TrackKind::Drummer.has_regions());
    }

    #[test]
    fn deleting_tracks_keeps_the_selection_valid_and_never_empties_the_project() {
        let mut p = Project::demo();
        let n = p.tracks.len();
        p.selected_track = n - 1;
        let rev = p.song_rev;
        assert!(p.delete_track(1));
        assert_eq!(p.tracks.len(), n - 1);
        assert_eq!(p.selected_track, n - 2, "the selection follows its track");
        assert_ne!(p.song_rev, rev);
        assert!(!p.delete_track(99));

        // A folder goes, the tracks inside it stay.
        let stacked = p.tracks[0].name.clone();
        p.create_track_stack(0);
        assert!(p.delete_track(0));
        assert_eq!(
            (p.tracks[0].name.as_str(), p.tracks[0].indent),
            (stacked.as_str(), 0)
        );

        while p.tracks.len() > 1 {
            assert!(p.delete_track(0));
        }
        assert!(!p.delete_track(0), "the last track stays");
        assert_eq!(p.selected_track, 0);
    }

    #[test]
    fn nesting_is_repaired_to_what_the_folders_allow() {
        let mut p = Project::demo();
        let (inst, folder) = (TrackKind::Instrument, TrackKind::Folder);
        let at = |k, indent| {
            let mut t = Track::new("t", k);
            t.indent = indent;
            t
        };
        p.tracks = vec![
            at(inst, 5), // first track: nothing above it
            at(folder, 0),
            at(inst, 9), // right under a folder: at most one level
            at(folder, 1),
            at(inst, 2), // nested folder: two levels is fine
            at(inst, 7), // after a plain track: no deeper than it
        ];
        p.normalize_nesting();
        let got: Vec<u8> = p.tracks.iter().map(|t| t.indent).collect();
        assert_eq!(got, [0, 0, 1, 1, 2, 2]);
    }
    #[test]
    fn added_tracks_are_named_per_kind_selected_and_announced() {
        let mut p = Project::demo();
        let (n, rev) = (p.tracks.len(), p.song_rev);
        let aux = p.add_track(TrackKind::Aux);
        assert_eq!((aux, p.selected_track, p.tracks.len()), (n, n, n + 1));
        assert_eq!(p.tracks[aux].name, "Aux 1");
        assert_eq!(p.tracks[aux].kind, TrackKind::Aux);
        assert!(p.tracks[aux].regions.is_empty());
        assert_ne!(p.song_rev, rev, "the engine is told the track list changed");

        p.add_track(TrackKind::Aux);
        assert_eq!(p.tracks.last().unwrap().name, "Aux 2");
        p.add_track(TrackKind::Instrument);
        assert_eq!(
            p.tracks.last().unwrap().name,
            "Inst 4",
            "the demo has 3 instruments"
        );
    }

    #[test]
    fn channel_names_count_within_their_class() {
        let mut p = Project::demo();
        let a1 = p.add_track(TrackKind::Aux);
        let a2 = p.add_track(TrackKind::Aux);
        let audio = p
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Audio)
            .unwrap();
        assert_eq!(p.channel_name(a1), "Aux 1");
        assert_eq!(p.channel_name(a2), "Aux 2");
        assert_eq!(p.channel_name(audio), "Audio 1");
        assert_eq!(p.channel_name(0), "Inst 1");
    }

    /// Tracks "a b c d e": b and c in a folder (a), d outside, e in a nested folder inside d.
    fn stack_fixture() -> Project {
        let mut p = Project::demo();
        let k = |name: &str, kind, indent| {
            let mut t = Track::new(name, kind);
            t.indent = indent;
            t
        };
        let (inst, folder) = (TrackKind::Instrument, TrackKind::Folder);
        p.tracks = vec![
            k("F", folder, 0),
            k("b", inst, 1),
            k("c", inst, 1),
            k("d", inst, 0),
            k("G", folder, 0),
            k("e", inst, 1),
        ];
        p
    }

    #[test]
    fn folders_own_the_deeper_tracks_after_them() {
        let p = stack_fixture();
        assert_eq!(p.folder_end(0), 3);
        assert_eq!(p.folder_end(4), 6);
        assert_eq!(p.folder_end(3), 4, "a plain track is its own range");
    }

    #[test]
    fn folding_hides_exactly_the_tracks_inside() {
        let mut p = stack_fixture();
        let hidden = |p: &Project| {
            (0..p.tracks.len())
                .filter(|i| p.hidden_by_fold(*i))
                .collect::<Vec<_>>()
        };
        assert!(hidden(&p).is_empty());
        p.tracks[0].folded = true;
        assert_eq!(hidden(&p), [1, 2], "the folder itself and the others stay");
        p.tracks[4].folded = true;
        assert_eq!(hidden(&p), [1, 2, 5]);
        // A folded outer folder hides an open inner one's tracks as well.
        let mut p = stack_fixture();
        p.tracks[0].folded = false;
        p.create_track_stack(0);
        p.tracks[0].folded = true;
        assert_eq!((1..4).filter(|i| p.hidden_by_fold(*i)).count(), 3);
    }

    #[test]
    fn a_track_stack_wraps_the_track_and_can_be_released_again() {
        let mut p = stack_fixture();
        let (rev, names) = (p.song_rev, |p: &Project| {
            p.tracks
                .iter()
                .map(|t| (t.name.clone(), t.indent))
                .collect::<Vec<_>>()
        });
        let before = names(&p);

        // Wrap the plain track "d" (index 3).
        let at = p.create_track_stack(3);
        assert_eq!(
            (at, p.selected_track, p.tracks[3].kind),
            (3, 3, TrackKind::Folder)
        );
        assert_eq!(p.tracks[3].name, "Folder 3");
        assert_eq!((p.tracks[4].name.as_str(), p.tracks[4].indent), ("d", 1));
        assert_ne!(p.song_rev, rev);

        assert!(p.release_track_stack(3));
        assert_eq!(names(&p), before, "release undoes create");
        assert!(!p.release_track_stack(3), "d is not a folder");

        // Wrapping a folder takes everything inside it one level deeper.
        p.create_track_stack(0);
        let nested: Vec<u8> = p.tracks[..4].iter().map(|t| t.indent).collect();
        assert_eq!(nested, [0, 1, 2, 2]);
        assert_eq!(p.folder_end(0), 4);
    }

    #[test]
    fn a_folders_mute_and_solo_reach_every_track_inside_it() {
        let mut p = stack_fixture();
        p.set_folder_flags(0, Some(true), None);
        let muted: Vec<bool> = p.tracks.iter().map(|t| t.mute).collect();
        assert_eq!(muted, [true, true, true, false, false, false]);
        p.set_folder_flags(0, None, Some(true));
        assert!(p.tracks[..3].iter().all(|t| t.solo && t.mute));
        assert!(!p.tracks[3].solo, "tracks outside the folder are untouched");
        p.set_folder_flags(0, Some(false), Some(false));
        assert!(p.tracks.iter().all(|t| !t.mute && !t.solo));
    }

    #[test]
    fn an_invalid_denominator_steps_from_quarter_notes() {
        let mut p = Project::demo();
        p.sig = (4, 3);
        assert!(p.step_signature(SigPart::Denominator, 1));
        assert_eq!(p.sig.1, 8);
    }

    #[test]
    fn tempo_changes_hold_until_the_next_one() {
        let mut p = Project::empty();
        p.tempo = 120.0;
        p.add_tempo_change(16.0, 90.0);
        p.add_tempo_change(8.0, 100.0);
        assert_eq!(p.tempo_at(0.0), 120.0);
        assert_eq!(p.tempo_at(8.0), 100.0);
        assert_eq!(p.tempo_at(15.9), 100.0);
        assert_eq!(p.tempo_at(400.0), 90.0);
        // Adding at the same beat replaces; at the start sets the start tempo.
        p.add_tempo_change(8.0, 110.0);
        assert_eq!(p.tempo_changes.len(), 2);
        p.add_tempo_change(0.0, 60.0);
        assert_eq!((p.tempo, p.tempo_changes.len()), (60.0, 2));
        // Dragging the LCD tempo edits the change in effect at the playhead.
        p.set_tempo_at(20.0, 95.0);
        assert_eq!(p.tempo_changes[1].bpm, 95.0);
        p.set_tempo_at(3.0, 70.0);
        assert_eq!(p.tempo, 70.0);
    }

    #[test]
    fn the_engine_tempo_map_follows_the_tempo_track() {
        let mut p = Project::empty();
        p.tempo = 120.0;
        p.add_tempo_change(8.0, 60.0);
        let map = p.tempo_map();
        // 8 beats at 120 = 4 s, then 4 beats at 60 = 4 s.
        assert!((map.seconds_at(8.0) - 4.0).abs() < 1e-9);
        assert!((map.seconds_at(12.0) - 8.0).abs() < 1e-9);
        assert_eq!(map.tempo_at(10.0), 60.0);
    }

    #[test]
    fn a_drummer_region_makes_notes_and_follows_its_length_and_the_bars_under_it() {
        let mut p = Project::empty();
        let ti = p.add_track(TrackKind::Drummer);
        let ri = p.add_generated_region(ti, true, 3.0).unwrap();
        let r = &p.tracks[ti].regions[ri];
        assert!(r.generated.is_some() && !r.notes.is_empty());
        assert!(r.notes.iter().all(|n| n.start < 16.0), "four bars of 4/4");
        assert_eq!((r.start_bar, r.len_bars), (3.0, 4.0));
        let rev = p.song_rev;
        assert!(!p.refresh_generated(), "nothing has changed");
        assert_eq!(p.song_rev, rev);

        // Lengthened to eight bars: more notes, and the engine is told.
        let four = p.tracks[ti].regions[ri].notes.len();
        p.tracks[ti].regions[ri].len_bars = 8.0;
        assert!(p.refresh_generated());
        assert!(p.song_rev > rev);
        let notes = &p.tracks[ti].regions[ri].notes;
        assert!(notes.len() > four && notes.iter().all(|n| n.start < 32.0));
        // Moved: the bars under it are the same, so the notes are left alone.
        p.tracks[ti].regions[ri].start_bar = 10.0;
        assert!(!p.refresh_generated());
        // A signature change under it makes the bars shorter: 3/4 from bar 12.
        p.set_signature_from(12, (3, 4));
        let lens: Vec<f64> = p
            .region_bars(&p.tracks[ti].regions[ri])
            .iter()
            .map(|b| b.beats)
            .collect();
        assert_eq!(lens, [4.0, 4.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0]);
        assert!(p.refresh_generated());
        let notes = &p.tracks[ti].regions[ri].notes;
        assert!(notes.iter().all(|n| n.start < 26.0), "26 beats long now");
        // A region cut short in the middle of a bar ends with a short bar.
        p.tracks[ti].regions[ri].len_bars = 2.5;
        let region = &p.tracks[ti].regions[ri];
        let total: f64 = p.region_bars(region).iter().map(|b| b.beats).sum();
        assert!((total - p.region_len_beats(region)).abs() < 1e-9);
        // Made an ordinary region, it keeps its notes and is left alone from then on.
        p.refresh_generated();
        let kept = p.tracks[ti].regions[ri].notes.clone();
        p.convert_to_midi_region(ti, ri);
        assert!(p.tracks[ti].regions[ri].generated.is_none());
        p.tracks[ti].regions[ri].len_bars = 6.0;
        assert!(!p.refresh_generated());
        assert_eq!(p.tracks[ti].regions[ri].notes, kept);
    }

    #[test]
    fn a_pattern_region_starts_with_the_rows_of_its_instrument() {
        let mut p = Project::empty();
        let drums = p.add_track(TrackKind::Drummer);
        let r = p.add_generated_region(drums, false, 1.0).unwrap();
        let notes = &p.tracks[drums].regions[r].notes;
        assert!(notes.iter().any(|n| n.pitch == 36) && notes.iter().any(|n| n.pitch == 38));
        let synth = p.add_track(TrackKind::Instrument);
        let r = p.add_generated_region(synth, false, 1.0).unwrap();
        assert!(
            p.tracks[synth].regions[r].notes.is_empty(),
            "an empty grid to draw in"
        );
        // Audio, Aux and VCA tracks hold no such regions.
        for kind in [
            TrackKind::Audio,
            TrackKind::Aux,
            TrackKind::Vca,
            TrackKind::Folder,
        ] {
            let t = p.add_track(kind);
            assert!(p.add_generated_region(t, true, 1.0).is_none(), "{kind:?}");
        }
    }

    #[test]
    fn a_tempo_curve_moves_in_a_straight_line_to_the_next_change() {
        let mut p = Project::empty();
        p.tempo = 100.0;
        p.add_tempo_change(16.0, 140.0);
        p.add_tempo_change(32.0, 80.0);
        p.set_tempo_ramp(0, true);
        assert_eq!(p.tempo_at(0.0), 100.0);
        assert_eq!(p.tempo_at(8.0), 120.0);
        assert_eq!(p.tempo_at(15.99).round(), 140.0);
        assert_eq!(p.tempo_at(16.0), 140.0, "the second segment holds");
        assert_eq!(p.tempo_at(40.0), 80.0);
        // The engine integrates the same curve: 16 beats from 100 to 140 BPM.
        let map = p.tempo_map();
        let secs = 60.0 * 16.0 / 40.0 * (140.0f64 / 100.0).ln();
        assert!((map.seconds_at(16.0) - secs).abs() < 1e-9);
        assert!((map.tempo_at(8.0) - 120.0).abs() < 1e-4);
        // The last change has nothing to ramp to.
        let last = p.tempo_changes.len();
        p.set_tempo_ramp(last, true);
        assert!(!p.tempo_changes[last - 1].ramp);
        // Dragging the tempo on a curve moves the curve's start by as much as the display.
        p.set_tempo_at(8.0, 130.0);
        assert_eq!(p.tempo, 110.0);
        assert_eq!(p.tempo_at(8.0), 125.0);
    }

    #[test]
    fn the_smpte_display_follows_the_tempo_map_rate_and_start() {
        use prowave_engine::SmpteRate;
        let mut p = Project::empty();
        p.tempo = 120.0;
        p.position_beats = 0.0;
        assert_eq!(p.timecode(), "01:00:00:00.00", "the default start");
        p.position_beats = 2.0;
        assert_eq!(p.timecode(), "01:00:01:00.00");
        // A slower tempo after beat 4 stretches the seconds: 4 beats at 120 = 2 s, then 2
        // beats at 60 = 2 s.
        p.add_tempo_change(4.0, 60.0);
        p.position_beats = 6.0;
        assert_eq!(p.timecode(), "01:00:04:00.00");
        // A half frame (40 ms at 25 fps is a frame; 20 ms is half of one = subframe 40).
        p.position_beats = 0.04;
        assert_eq!(p.timecode(), "01:00:00:00.40");
        // Another start and rate.
        p.smpte_start = [0, 0, 0, 0];
        p.smpte_rate = SmpteRate::Fps30;
        p.position_beats = 2.0;
        assert_eq!(p.timecode(), "00:00:01:00.00");
        p.smpte_rate = SmpteRate::Fps29_97Drop;
        p.smpte_start = [0, 1, 0, 2];
        assert_eq!(p.smpte_offset_frames(), 1800);
        p.position_beats = 0.0;
        assert_eq!(p.timecode(), "00:01:00;02.00", "drop frame has a semicolon");
        // A tempo curve: 100 -> 200 BPM over 16 beats takes 60 * 16 / 100 * ln 2 seconds.
        let mut c = Project::empty();
        c.smpte_start = [0, 0, 0, 0];
        c.smpte_rate = SmpteRate::Fps24;
        c.tempo = 100.0;
        c.add_tempo_change(16.0, 200.0);
        c.set_tempo_ramp(0, true);
        c.position_beats = 16.0;
        let secs = 60.0 * 16.0 / 100.0 * 2f64.ln();
        let frames = (secs * 24.0).floor() as u32;
        let expect = format!("00:00:{:02}:{:02}", frames / 24, frames % 24);
        assert!(
            c.timecode().starts_with(&expect),
            "{} vs {expect}",
            c.timecode()
        );
    }

    #[test]
    fn a_tempo_change_inside_a_curve_keeps_the_curve_going() {
        let mut p = Project::empty();
        p.tempo = 60.0;
        p.add_tempo_change(32.0, 120.0);
        p.set_tempo_ramp(0, true);
        let i = p.add_tempo_change(16.0, 90.0);
        assert!(p.tempo_ramp && p.tempo_changes[i].ramp);
        assert_eq!(p.tempo_at(8.0), 75.0);
        assert_eq!(p.tempo_at(24.0), 105.0);
        // Deleting the end of the curve leaves a point with nothing to ramp to.
        p.tempo_changes.pop();
        p.tidy_tempo();
        assert!(!p.tempo_changes[0].ramp);
        assert_eq!(p.tempo_at(100.0), 90.0);
        p.tempo_changes.clear();
        p.tidy_tempo();
        assert!(!p.tempo_ramp);
    }

    #[test]
    fn markers_are_named_kept_in_order_and_navigable() {
        let mut p = Project::empty();
        assert_eq!(p.add_marker(16.0), 0);
        assert_eq!(p.add_marker(4.0), 0, "sorted by position");
        assert_eq!(p.add_marker(16.0), 1, "one marker per position");
        let names: Vec<&str> = p.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Marker 2", "Marker 1"]);
        assert_ne!(p.markers[0].color, p.markers[1].color);
        assert_eq!(p.marker_at(2.0), None);
        assert_eq!(p.marker_at(4.0), Some(0));
        assert_eq!(p.marker_at(30.0), Some(1));
        assert_eq!(p.next_marker(4.0), Some(16.0));
        assert_eq!(p.next_marker(16.0), None);
        assert_eq!(p.previous_marker(16.0), Some(4.0));
        assert_eq!(p.previous_marker(4.0), None);
        // A deleted name is reused.
        p.markers.remove(1);
        p.add_marker(32.0);
        assert_eq!(p.markers[1].name, "Marker 1");
    }

    #[test]
    fn tidy_markers_sorts_and_drops_bad_or_doubled_ones() {
        let mut p = Project::empty();
        for b in [8.0, -1.0, f64::NAN, 2.0, 8.0] {
            p.markers.push(Marker {
                beat: b,
                name: format!("{b}"),
                color: MARKER_COLORS[0],
            });
        }
        p.tidy_markers();
        let beats: Vec<f64> = p.markers.iter().map(|m| m.beat).collect();
        assert_eq!(beats, [2.0, 8.0]);
    }

    #[test]
    fn normalize_brings_the_peak_to_full_scale() {
        let g = RegionParams::normalize_gain(0.5).unwrap();
        assert!((g - 6.0206).abs() < 1e-3, "half scale needs +6 dB, got {g}");
        assert_eq!(RegionParams::normalize_gain(1.0), Some(0.0));
        assert_eq!(RegionParams::normalize_gain(0.0), None, "silence");
        assert_eq!(RegionParams::normalize_gain(f32::NAN), None);
        assert_eq!(
            RegionParams::normalize_gain(1e-5),
            Some(MAX_REGION_GAIN),
            "never past +24 dB"
        );
        let edit = RegionParams {
            gain_db: -2.0,
            fade_in: 1.5,
            reverse: true,
            ..RegionParams::default()
        }
        .clip_edit();
        assert_eq!(
            (edit.gain_db, edit.fade_in_beats, edit.reverse),
            (-2.0, 1.5, true)
        );
    }

    #[test]
    fn a_frame_in_which_nothing_changed_does_not_look_at_the_bars_again() {
        let mut p = Project::empty();
        let ti = p.add_track(TrackKind::Drummer);
        let ri = p.add_generated_region(ti, true, 1.0).unwrap();
        assert!(!p.refresh_generated());
        let g = p.tracks[ti].regions[ri].generated.as_ref().unwrap();
        let r = &p.tracks[ti].regions[ri];
        assert_eq!(
            g.seen,
            (r.start_bar.to_bits(), r.len_bars.to_bits(), p.song_rev)
        );
        // Break the stored key on purpose: while the fingerprint matches, it is not looked at...
        p.tracks[ti].regions[ri]
            .generated
            .as_mut()
            .unwrap()
            .made_for ^= 1;
        assert!(!p.refresh_generated(), "same bars, same song: no look");
        // ...and a change of the region or the song brings the comparison (and the new notes).
        p.tracks[ti].regions[ri].len_bars = 6.0;
        assert!(p.refresh_generated());
        p.tracks[ti].regions[ri]
            .generated
            .as_mut()
            .unwrap()
            .made_for ^= 1;
        p.song_rev += 1;
        assert!(p.refresh_generated());
    }
}
