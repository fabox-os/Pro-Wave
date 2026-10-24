//! Device-independent engine core. `EngineCore::process` is the audio-thread entry point:
//! no allocation, no locks, no syscalls. Unit-testable without an audio device.
use crate::disk::{Block, ClipShared};
use crate::fx::{Chain, Pdc, MAX_FX_PARAMS};
use crate::gain_smoothing::GainRamp;
use crate::mtc::{MtcGenerator, MtcMessage};
#[cfg(test)]
use crate::routing::AutoPoint;
#[cfg(test)]
use crate::routing::SendSpec;
use crate::routing::{send_gain, AutoLane, AutoTarget, Route, MAX_BUSES};
use crate::synth::{kit_voice, osc, Kit, Svf, SynthPatch};
use crate::tempo_map::{SmpteRate, TempoMap};
use crate::triple::{triple_buffer, Reader, Writer};
use rtrb::{Consumer, Producer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Tracks the engine mixes (Logic: 1000 tracks per project).
pub const MAX_TRACKS: usize = 1024;
const MAX_VOICES: usize = 48;
/// Samples between two readings of the automation (0.7 ms at 48 kHz).
const AUTO_BLOCK: u32 = 32;

/// A note scheduled on the timeline (beats are 0-based quarter notes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    pub start_beat: f64,
    pub end_beat: f64,
    pub pitch: u8,
    pub vel: u8,
    pub track: u16,
}

/// A MIDI note the engine sends out for a track that plays an external instrument (Logic:
/// External MIDI track). The app turns it into a MIDI message on the track's port and channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiOutEvent {
    pub track: u16,
    /// Note On (`true`) or Note Off. Not used by a controller.
    pub on: bool,
    /// The note, or for a controller its number.
    pub pitch: u8,
    /// The velocity, or for a controller its value.
    pub vel: u8,
    /// A controller change (such as the sustain pedal) instead of a note.
    pub controller: bool,
}

/// A controller change on a track's timeline (beats are 0-based quarter notes). Only tracks
/// that play an external instrument send them (as [`MidiOutEvent`]s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlEvent {
    pub beat: f64,
    pub track: u16,
    pub number: u8,
    pub value: u8,
}

/// A note sent to an external instrument that has not been released yet.
#[derive(Clone, Copy)]
struct ExtNote {
    track: u16,
    pitch: u8,
    end_beat: f64,
    /// Played from the keyboard: it ends on Note Off, not at `end_beat`.
    live: bool,
}

/// External notes that can sound at once.
const MAX_EXT_NOTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct TrackMix {
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
    /// -1.0 (left) .. 1.0 (right), equal-power, unity at centre.
    pub pan: f32,
}

impl Default for TrackMix {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mute: false,
            solo: false,
            pan: 0.0,
        }
    }
}

impl TrackMix {
    fn sanitized(self) -> Self {
        Self {
            gain_db: if self.gain_db.is_finite() {
                self.gain_db.clamp(-96.0, 6.0)
            } else {
                0.0
            },
            pan: if self.pan.is_finite() {
                self.pan.clamp(-1.0, 1.0)
            } else {
                0.0
            },
            ..self
        }
    }
}

/// Immutable-by-the-audio-thread snapshot of the arrangement.
#[derive(Default)]
pub struct Song {
    /// Must be sorted by `start_beat`; use [`Song::new`].
    pub notes: Vec<NoteEvent>,
    pub tracks: Vec<TrackMix>,
    /// Streamed audio regions (create with `Engine::stream_clip`).
    pub clips: Vec<AudioClip>,
    /// Controller changes for external instruments, sorted by beat (see [`Song::with_controls`]).
    pub controls: Vec<ControlEvent>,
}

impl Song {
    pub fn new(mut notes: Vec<NoteEvent>, tracks: Vec<TrackMix>) -> Self {
        notes.sort_by(|a, b| a.start_beat.total_cmp(&b.start_beat));
        Self {
            notes,
            tracks,
            clips: Vec::new(),
            controls: Vec::new(),
        }
    }

    /// The song with these controller changes (sorted here).
    pub fn with_controls(mut self, mut controls: Vec<ControlEvent>) -> Self {
        controls.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.controls = controls;
        self
    }

    pub fn with_clips(mut self, clips: Vec<AudioClip>) -> Self {
        self.clips = clips;
        self
    }
}

/// Audio-thread side of a streamed audio region (Logic Pro: audio region on an audio track).
/// Reads [`Block`]s produced by the disk thread; never allocates or blocks.
pub struct AudioClip {
    pub track: u16,
    pub start_beat: f64,
    end_beat: f64,
    /// Length in device-rate frames.
    pub len_frames: u64,
    edit: ClipEdit,
    gain_linear: f32,
    rx: Consumer<Block>,
    shared: Arc<ClipShared>,
    gen: u16,
    cur: Option<Block>,
    /// Next clip-relative frame to play (negative = before the region starts).
    pos: i64,
    underruns: u32,
}

/// Non-destructive region edits (Logic Pro: Region Inspector Gain, Fade In/Out, Reverse).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipEdit {
    /// Region gain in dB, clamped to -96..=+24.
    pub gain_db: f32,
    /// Fade durations in quarter-note beats, following the project tempo map.
    pub fade_in_beats: f64,
    pub fade_out_beats: f64,
    pub fade_curve: FadeCurve,
    pub reverse: bool,
}

impl Default for ClipEdit {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            fade_in_beats: 0.0,
            fade_out_beats: 0.0,
            fade_curve: FadeCurve::EqualPower,
            reverse: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FadeCurve {
    Linear,
    #[default]
    EqualPower,
    SCurve,
}

impl ClipEdit {
    fn clean(self) -> Self {
        Self {
            gain_db: if self.gain_db.is_finite() {
                self.gain_db.clamp(-96.0, 24.0)
            } else {
                0.0
            },
            fade_in_beats: if self.fade_in_beats.is_finite() {
                self.fade_in_beats.max(0.0)
            } else {
                0.0
            },
            fade_out_beats: if self.fade_out_beats.is_finite() {
                self.fade_out_beats.max(0.0)
            } else {
                0.0
            },
            ..self
        }
    }
}

impl AudioClip {
    pub(crate) fn new(
        track: u16,
        start_beat: f64,
        len_frames: u64,
        edit: ClipEdit,
        rx: Consumer<Block>,
        shared: Arc<ClipShared>,
    ) -> Self {
        let edit = edit.clean();
        let gain_linear = 10.0f32.powf(edit.gain_db / 20.0);
        Self {
            track,
            start_beat,
            end_beat: start_beat,
            len_frames,
            edit,
            gain_linear,
            rx,
            shared,
            gen: 0,
            cur: None,
            pos: 0,
            underruns: 0,
        }
    }

    /// Continue playback `offset` frames into the clip (negative: before it starts).
    fn reposition(&mut self, offset: i64) {
        self.pos = offset;
        self.gen = self.gen.wrapping_add(1);
        self.cur = None;
        let want = self.pos.clamp(0, self.len_frames as i64) as u64;
        self.shared.request(self.gen, want);
    }

    /// Once per callback: drop stale blocks so the disk thread gets ring space back.
    fn service(&mut self) {
        while self.cur.as_ref().is_none_or(|b| b.gen != self.gen) {
            match self.rx.pop() {
                Ok(b) if b.gen == self.gen => self.cur = Some(b),
                Ok(_) => self.cur = None,
                Err(_) => break,
            }
        }
    }

    fn tick(&mut self) -> Option<(f32, f32)> {
        let p = self.pos;
        self.pos += 1;
        if p < 0 || p as u64 >= self.len_frames {
            return None;
        }
        let p = p as u64;
        loop {
            if let Some(b) = &self.cur {
                if b.gen == self.gen {
                    if p < b.start {
                        return None; // gap: data for later frames already queued
                    }
                    if p < b.start + b.frames as u64 {
                        let i = (p - b.start) as usize * 2;
                        return Some((b.data[i], b.data[i + 1]));
                    }
                }
                self.cur = None;
            }
            match self.rx.pop() {
                Ok(b) if b.gen == self.gen => self.cur = Some(b),
                Ok(_) => {}
                Err(_) => {
                    self.underruns += 1;
                    return None;
                }
            }
        }
    }

    fn envelope(&self, beat: f64, end_beat: f64) -> f32 {
        let curve = |x: f64| -> f32 {
            let x = x.clamp(0.0, 1.0);
            // Outside the fade, return its exact endpoint without evaluating sin.
            if x == 0.0 || x == 1.0 {
                return x as f32;
            }
            match self.edit.fade_curve {
                FadeCurve::Linear => x as f32,
                FadeCurve::EqualPower => (x * std::f64::consts::FRAC_PI_2).sin() as f32,
                FadeCurve::SCurve => (x * x * (3.0 - 2.0 * x)) as f32,
            }
        };
        let mut gain = self.gain_linear;
        if self.edit.fade_in_beats > 0.0 {
            gain *= curve((beat - self.start_beat) / self.edit.fade_in_beats);
        }
        if self.edit.fade_out_beats > 0.0 {
            gain *= curve((end_beat - beat) / self.edit.fade_out_beats);
        }
        gain
    }
}

/// UI -> engine messages. Heap payloads are boxed so the queue slot stays small; the old
/// payload travels back through the garbage queue so it is freed off the audio thread.
pub enum Command {
    Play,
    Pause,
    /// Stop and return to the position the transport started from.
    Stop,
    SetPosition {
        beat: f64,
    },
    /// Tempo and time-signature changes (Logic: Tempo / Signature tracks). Build the map on
    /// the UI thread; the old one is sent back through the garbage queue.
    SetTempoMap(Box<TempoMap>),
    SetMetronome {
        on: bool,
    },
    /// Master fader (Logic: Stereo Out level), clamped to 0..2 linear. Changes ramp over
    /// 5 ms during playback/audition; silent stopped setup is immediate. Non-finite values
    /// are ignored so malformed commands cannot poison the output.
    SetMasterGain {
        linear: f32,
    },
    /// Logic's cycle locators, in quarter-note beats. The end is exclusive; wrapping
    /// preserves fractional-sample elapsed time. Invalid or non-finite bounds disable cycle.
    SetCycle {
        on: bool,
        start_beat: f64,
        end_beat: f64,
    },
    /// Track fader/pan (Logic: channel strip). Gain and pan ramp over 5 ms while sounding;
    /// mute and solo retain immediate switching. New song snapshots initialize immediately.
    SetTrackMix {
        track: u16,
        mix: TrackMix,
    },
    SetSong(Box<Song>),
    /// Replaces only the song's notes (sorted by start; build with [`Command::set_notes`]).
    /// Streamed audio clips keep playing and sounding voices are not cut, so MIDI edits while
    /// playing cause no audio dropout. Use `SetSong` when clips or the track count change.
    SetNotes(Vec<NoteEvent>),
    /// Replaces the song's controller changes for external instruments (sorted by beat; build
    /// with [`Command::set_controls`]).
    SetControls(Vec<ControlEvent>),
    /// Logic Pro "Test Oscillator": a sine on the output, independent of the transport.
    /// `hz <= 0` turns it off.
    SetTestTone {
        hz: f32,
        gain: f32,
    },
    /// Live note (clicking the Piano Roll keys, auditioning while editing). Works while the
    /// transport is stopped and is released by `NoteOff` (or automatically after 4 s).
    NoteOn {
        pitch: u8,
        vel: u8,
        track: u16,
    },
    /// Releases this live pitch on every track (legacy/global audition release).
    NoteOff {
        pitch: u8,
    },
    /// Releases a live pitch only on its instrument track, like Logic's per-track MIDI
    /// routing. Other tracks and timeline notes are unaffected; unknown tracks are ignored.
    NoteOffTrack {
        pitch: u8,
        track: u16,
    },
    /// Makes `track` play an external instrument: its notes go out as [`MidiOutEvent`]s
    /// instead of sounding in the engine.
    SetTrackExternal {
        track: u16,
        external: bool,
    },
    /// Sets the synth patch used by instrument notes on `track` (Library patch).
    SetTrackPatch {
        track: u16,
        patch: SynthPatch,
    },
    /// The effects of a channel strip (Logic: Audio FX slots), made off the audio thread. The
    /// strip's old effects come back through the garbage queue; their tails stop.
    SetInserts {
        track: u16,
        chain: Box<Chain>,
    },
    /// New parameters and bypass for effect `slot` of a strip's chain (no allocation, so
    /// knobs can move while it plays). Ignored when the slot does not exist.
    SetFx {
        track: u16,
        slot: u8,
        params: [f32; MAX_FX_PARAMS],
        bypass: bool,
    },
    /// Delay compensation of a strip (see [`Pdc`]): every strip is delayed so that effects with
    /// a look-ahead do not put it out of time with the others. The old delay comes back
    /// through the garbage queue.
    SetPdc {
        track: u16,
        pdc: Box<Pdc>,
    },
    /// Where a strip's signal goes: its sends, and for an Aux strip the bus it listens to.
    SetRoute {
        track: u16,
        route: Route,
    },
    /// Replaces all automation lanes (build each with [`AutoLane::sanitized`]).
    SetAutomation(Vec<AutoLane>),
    /// Turns MIDI Time Code output on or off (Logic: Synchronization > MTC). While on, the
    /// engine sends the SMPTE time of the playhead as [`MtcMessage`]s: quarter frames while
    /// playing, a full frame when the position jumps. `offset_frames` is the SMPTE time of the
    /// project start, in frames.
    SetMtc {
        on: bool,
        rate: SmpteRate,
        offset_frames: i64,
    },
}

impl Command {
    pub(crate) fn retires_state(&self) -> bool {
        matches!(
            self,
            Self::SetSong(_)
                | Self::SetNotes(_)
                | Self::SetControls(_)
                | Self::SetTempoMap(_)
                | Self::SetInserts { .. }
                | Self::SetPdc { .. }
                | Self::SetAutomation(_)
        )
    }

    /// [`Command::SetControls`] with `controls` sorted as the engine requires.
    pub fn set_controls(mut controls: Vec<ControlEvent>) -> Self {
        controls.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Command::SetControls(controls)
    }

    /// [`Command::SetNotes`] with `notes` sorted as the engine requires.
    pub fn set_notes(mut notes: Vec<NoteEvent>) -> Self {
        notes.sort_by(|a, b| a.start_beat.total_cmp(&b.start_beat));
        Command::SetNotes(notes)
    }
}

pub enum Garbage {
    Song(Box<Song>),
    Notes(Vec<NoteEvent>),
    Controls(Vec<ControlEvent>),
    TempoMap(Box<TempoMap>),
    Chain(Box<Chain>),
    Pdc(Box<Pdc>),
    Automation(Vec<AutoLane>),
}

/// What the engine shows the UI after each audio block: the playhead, the transport and the
/// meters, all from the same block. Read with [`StateReader::read`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineState {
    pub position_beats: f64,
    pub playing: bool,
    /// Master output peak (left, right), linear, falling back slowly like a meter.
    pub peaks: (f32, f32),
    /// Post-fader stereo peak per track, linear, for the mixer meters.
    pub track_peaks: [(f32, f32); MAX_TRACKS],
    /// Gain reduction in dB (0 or more) of the compressors of each strip, for the mixer's
    /// Gain Reduction meter.
    pub gain_reduction: [f32; MAX_TRACKS],
    /// Frames in the most recent device callback.
    pub block_frames: u32,
    /// Device-reported delay from callback to DAC, in microseconds (0 if unknown).
    pub output_latency_us: u32,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            position_beats: 0.0,
            playing: false,
            peaks: (0.0, 0.0),
            track_peaks: [(0.0, 0.0); MAX_TRACKS],
            gain_reduction: [0.0; MAX_TRACKS],
            block_frames: 0,
            output_latency_us: 0,
        }
    }
}

impl EngineState {
    /// Gain reduction in dB of the compressors of `track` (0 when there is none).
    pub fn gain_reduction(&self, track: usize) -> f32 {
        self.gain_reduction.get(track).copied().unwrap_or(0.0)
    }

    /// Post-fader (left, right) peak of `track`, linear; silence for a track out of range.
    pub fn track_peaks(&self, track: usize) -> (f32, f32) {
        self.track_peaks.get(track).copied().unwrap_or((0.0, 0.0))
    }
}

/// UI side of the engine's state: the newest complete [`EngineState`], through a lock-free
/// triple buffer (the audio thread never waits for the UI, and the UI never sees a value made
/// of two different blocks).
pub struct StateReader(Reader<EngineState>);

impl StateReader {
    /// The state after the newest audio block. Wait-free.
    pub fn read(&mut self) -> &EngineState {
        self.0.read()
    }
}

/// Engine -> UI counters, all lock-free. The per-block state is in [`EngineState`].
pub struct Shared {
    pub xruns: AtomicU32,
    /// Callbacks in which a streamed clip had no data ready (disk too slow).
    pub underruns: AtomicU32,
    /// Highest DSP load (f32 bits, 1.0 = the whole callback budget) since the UI last read it.
    dsp_load_max: AtomicU32,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            xruns: AtomicU32::new(0),
            underruns: AtomicU32::new(0),
            dsp_load_max: AtomicU32::new(0),
        }
    }

    /// Audio thread: report one callback's DSP load (see [`dsp_load`]). Lock- and alloc-free.
    pub fn record_dsp_load(&self, load: f32) {
        // Non-negative f32s order like their bit patterns, so an integer max works.
        self.dsp_load_max
            .fetch_max(load.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// UI thread: the highest DSP load since the previous call (Logic's CPU meter), then reset.
    pub fn take_dsp_load(&self) -> f32 {
        f32::from_bits(self.dsp_load_max.swap(0, Ordering::Relaxed))
    }
}

/// Share of the real-time budget a callback used: time spent rendering `frames` divided by
/// the time those frames take to play. Above 1.0 the device underruns (Logic: "System
/// Overload").
pub fn dsp_load(busy: std::time::Duration, frames: usize, sample_rate: u32) -> f32 {
    if frames == 0 || sample_rate == 0 {
        return 0.0;
    }
    (busy.as_secs_f64() * sample_rate as f64 / frames as f64) as f32
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    releasing: bool,
    phase_a: f32,
    phase_b: f32,
    base_hz: f32,
    amp: f32,
    env: f32,
    /// 0 attack, 1 decay, 2 sustain, 3 release.
    stage: u8,
    rel_step: f32,
    end_beat: f64,
    track: u16,
    /// Live note: ignores the transport and the timeline, ends on `NoteOff`.
    live: bool,
    pitch: u8,
    /// Safety net so a lost `NoteOff` cannot hang a note forever.
    live_left: u32,
    patch: SynthPatch,
    filter: Svf,
    vibrato: f32,
    noise: u32,
    pitch_env: f32,
    /// Oscillator B / A frequency ratio (fixed per note).
    ratio_b: f32,
    /// `pitch_env` decay over one control block.
    pitch_decay_block: f32,
    /// Samples until the control-rate values below are recomputed.
    ctrl_left: u32,
    dt_a: f32,
    dt_b: f32,
    filter_f: f32,
    filter_damping: f32,
    /// Envelope increments per sample for the attack and decay stages (fixed per note).
    attack_step: f32,
    decay_step: f32,
}

/// Modulation (vibrato, pitch drop, filter envelope) is recomputed every `CTRL_BLOCK` samples
/// (0.33 ms at 48 kHz) instead of every sample: it removes the per-sample `powf`/`sin` calls,
/// the bulk of the synth's cost, without audible stepping.
const CTRL_BLOCK: u32 = 16;

/// The app's halves of the queues a core publishes to.
pub(crate) struct Readers {
    pub state: Option<StateReader>,
    pub midi: Option<Consumer<MidiOutEvent>>,
    pub mtc: Option<Consumer<MtcMessage>>,
}

pub struct EngineCore {
    sr: f32,
    rx: Consumer<Command>,
    trash: Producer<Garbage>,
    shared: Arc<Shared>,
    /// What the UI was last shown; the meters fall back from it.
    shown: EngineState,
    state: Writer<EngineState>,
    state_reader: Option<StateReader>,
    song: Box<Song>,
    next_note: usize,
    next_control: usize,
    /// External tracks whose sustain pedal was last sent down.
    ext_sustain: Vec<bool>,
    beat: f64,
    start_beat: f64,
    playing: bool,
    tempo_map: Box<TempoMap>,
    metronome: bool,
    cycle: Option<(f64, f64)>,
    master: GainRamp,
    voices: [Voice; MAX_VOICES],
    click_remaining: u32,
    click_phase: f32,
    click_freq: f32,
    track_gain_l: Vec<f32>,
    track_gain_r: Vec<f32>,
    track_ramp_l: Vec<GainRamp>,
    track_ramp_r: Vec<GainRamp>,
    mix_ramp_remaining: u32,
    track_pk_l: Vec<f32>,
    track_pk_r: Vec<f32>,
    track_audible: Vec<bool>,
    track_patch: Vec<SynthPatch>,
    track_external: Vec<bool>,
    /// Effects per strip (an empty box for none), sends and bus inputs.
    chains: Vec<Option<Box<Chain>>>,
    /// Delay compensation per strip.
    pdc: Vec<Option<Box<Pdc>>>,
    routes: Vec<Route>,
    /// Strips with effects or sends (processed in order), and Aux strips (processed after
    /// them, once the buses are full).
    complex: Vec<u16>,
    aux: Vec<u16>,
    /// 0 plain strip, 1 complex, 2 aux.
    strip_kind: Vec<u8>,
    /// The signal of each strip for the current sample, before effects and fader.
    /// The strips' signals and the buses are summed in 64 bits, so many strips (and the
    /// buses they feed) add up without losing the quiet ones to rounding.
    in_l: Vec<f64>,
    in_r: Vec<f64>,
    touched: Vec<bool>,
    dirty: Vec<u16>,
    bus_l: [f64; MAX_BUSES],
    bus_r: [f64; MAX_BUSES],
    automation: Vec<AutoLane>,
    /// The value the automation gives each strip right now (None: its fader or knob rules).
    auto_vol: Vec<Option<f32>>,
    auto_pan: Vec<Option<f32>>,
    auto_on: Vec<bool>,
    auto_tracks: Vec<u16>,
    /// Some lane is played.
    auto_any: bool,
    auto_left: u32,
    midi_out: Producer<MidiOutEvent>,
    midi_out_reader: Option<Consumer<MidiOutEvent>>,
    mtc: MtcGenerator,
    mtc_rate: SmpteRate,
    mtc_offset_frames: i64,
    /// Seconds from the project start to the playhead, kept sample by sample while MTC is on.
    mtc_secs: f64,
    mtc_out: Producer<MtcMessage>,
    mtc_out_reader: Option<Consumer<MtcMessage>>,
    ext_notes: Vec<ExtNote>,
    test_hz: f32,
    test_gain: f32,
    test_phase: f32,
}

/// Keeps an oscillator phase in 0..1. `fract` costs a `floorf` call per sample on baseline
/// x86-64; the phase only passes 1.0 once per period, so test first.
#[inline]
fn wrap_phase(p: f32) -> f32 {
    if p < 1.0 {
        p
    } else {
        p.fract()
    }
}

/// `x`, or 0 when it is NaN or infinite.
#[inline]
fn finite_or_zero(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// A meter's new peak: this callback's peak, or the old one decayed. Never NaN or infinite, so
/// a bad value cannot stick (`inf * 0.85` is still `inf`).
#[inline]
fn decayed_peak(new: f32, old: f32) -> f32 {
    finite_or_zero(new.max(old * 0.85)).min(16.0)
}

fn midi_hz(p: u8) -> f32 {
    440.0 * 2f32.powf((p as f32 - 69.0) / 12.0)
}

impl EngineCore {
    pub fn new(
        sample_rate: u32,
        rx: Consumer<Command>,
        trash: Producer<Garbage>,
        shared: Arc<Shared>,
    ) -> Self {
        let (state, reader) = triple_buffer(EngineState::default());
        let (midi_out, midi_out_reader) = rtrb::RingBuffer::new(1024);
        let (mtc_out, mtc_out_reader) = rtrb::RingBuffer::new(256);
        Self {
            sr: sample_rate.max(1) as f32,
            rx,
            trash,
            shared,
            shown: EngineState::default(),
            state,
            state_reader: Some(StateReader(reader)),
            song: Box::new(Song::default()),
            next_note: 0,
            next_control: 0,
            ext_sustain: vec![false; MAX_TRACKS],
            beat: 0.0,
            start_beat: 0.0,
            playing: false,
            tempo_map: Box::default(),
            metronome: false,
            cycle: None,
            master: GainRamp::new(0.8),
            voices: [Voice::default(); MAX_VOICES],
            click_remaining: 0,
            click_phase: 0.0,
            click_freq: 1000.0,
            track_gain_l: vec![1.0; MAX_TRACKS],
            track_gain_r: vec![1.0; MAX_TRACKS],
            track_ramp_l: vec![GainRamp::new(1.0); MAX_TRACKS],
            track_ramp_r: vec![GainRamp::new(1.0); MAX_TRACKS],
            mix_ramp_remaining: 0,
            track_pk_l: vec![0.0; MAX_TRACKS],
            track_pk_r: vec![0.0; MAX_TRACKS],
            track_audible: vec![true; MAX_TRACKS],
            track_patch: vec![SynthPatch::default(); MAX_TRACKS],
            track_external: vec![false; MAX_TRACKS],
            chains: (0..MAX_TRACKS).map(|_| None).collect(),
            pdc: (0..MAX_TRACKS).map(|_| None).collect(),
            routes: vec![Route::default(); MAX_TRACKS],
            complex: Vec::with_capacity(MAX_TRACKS),
            aux: Vec::with_capacity(MAX_TRACKS),
            strip_kind: vec![0; MAX_TRACKS],
            in_l: vec![0.0; MAX_TRACKS],
            in_r: vec![0.0; MAX_TRACKS],
            touched: vec![false; MAX_TRACKS],
            dirty: Vec::with_capacity(MAX_TRACKS),
            bus_l: [0.0; MAX_BUSES],
            bus_r: [0.0; MAX_BUSES],
            auto_any: false,
            automation: Vec::new(),
            auto_vol: vec![None; MAX_TRACKS],
            auto_pan: vec![None; MAX_TRACKS],
            auto_on: vec![false; MAX_TRACKS],
            auto_tracks: Vec::with_capacity(MAX_TRACKS),
            auto_left: 0,
            midi_out,
            midi_out_reader: Some(midi_out_reader),
            mtc: MtcGenerator::default(),
            mtc_rate: SmpteRate::Fps25,
            mtc_offset_frames: 0,
            mtc_secs: 0.0,
            mtc_out,
            mtc_out_reader: Some(mtc_out_reader),
            ext_notes: Vec::with_capacity(MAX_EXT_NOTES),
            test_hz: 0.0,
            test_gain: 0.0,
            test_phase: 0.0,
        }
    }

    /// The UI's half of the state this core publishes after every block. There is one;
    /// it is `None` once taken.
    pub fn take_state_reader(&mut self) -> Option<StateReader> {
        self.state_reader.take()
    }

    /// The app's half of the MIDI the engine sends to external instruments. There is one; it
    /// is `None` once taken.
    pub fn take_midi_out_reader(&mut self) -> Option<Consumer<MidiOutEvent>> {
        self.midi_out_reader.take()
    }

    /// The app's half of the MIDI Time Code the engine sends. There is one; it is `None` once
    /// taken.
    pub fn take_mtc_reader(&mut self) -> Option<Consumer<MtcMessage>> {
        self.mtc_out_reader.take()
    }

    /// Every reader the app takes from the core, together.
    pub(crate) fn take_readers(&mut self) -> Readers {
        Readers {
            state: self.take_state_reader(),
            midi: self.take_midi_out_reader(),
            mtc: self.take_mtc_reader(),
        }
    }

    /// SMPTE frames of the playhead as MTC counts them.
    fn mtc_frames(&self) -> f64 {
        self.mtc_secs * self.mtc_rate.frames_per_second() + self.mtc_offset_frames as f64
    }

    /// The playhead moved or the tempo map changed: tell MTC receivers where it is.
    fn mtc_locate(&mut self) {
        if !self.mtc.is_on() {
            return;
        }
        self.mtc_secs = self.tempo_map.seconds_at(self.beat);
        let frames = self.mtc_frames();
        let tx = &mut self.mtc_out;
        self.mtc.locate(frames, |m| {
            let _ = tx.push(m);
        });
    }

    /// Sends a Note On or Off to the external instrument of `track`. Lost when the queue is
    /// full (nobody is listening), never blocks.
    fn send_midi(&mut self, track: u16, on: bool, pitch: u8, vel: u8) {
        let _ = self.midi_out.push(MidiOutEvent {
            track,
            on,
            pitch,
            vel,
            controller: false,
        });
    }

    /// A note for an external instrument starts.
    fn ext_note_on(&mut self, n: NoteEvent, live: bool) {
        // A note that is still sounding is released first, as a keyboard would.
        if let Some(i) = self
            .ext_notes
            .iter()
            .position(|e| e.track == n.track && e.pitch == n.pitch)
        {
            let old = self.ext_notes.swap_remove(i);
            self.send_midi(old.track, false, old.pitch, 0);
        }
        if self.ext_notes.len() < MAX_EXT_NOTES {
            self.ext_notes.push(ExtNote {
                track: n.track,
                pitch: n.pitch,
                end_beat: n.end_beat,
                live,
            });
            self.send_midi(n.track, true, n.pitch, n.vel.max(1));
        }
    }

    /// Releases external notes that are due: those of the timeline once their end is reached
    /// or the transport stopped, all of them when `all`.
    fn release_ext_notes(&mut self, all: bool) {
        let mut i = 0;
        while i < self.ext_notes.len() {
            let e = self.ext_notes[i];
            let due = all || (!e.live && (self.beat >= e.end_beat || !self.playing));
            if due {
                self.ext_notes.swap_remove(i);
                self.send_midi(e.track, false, e.pitch, 0);
            } else {
                i += 1;
            }
        }
    }

    /// The device callback's size and output delay, published with the next block's state.
    pub fn set_device_info(&mut self, block_frames: u32, output_latency_us: u32) {
        self.shown.block_frames = block_frames;
        self.shown.output_latency_us = output_latency_us;
    }

    /// Sends a controller change to the external instrument of its track (once; the sustain
    /// pedal is remembered so a stop can let it up).
    fn send_control(&mut self, c: ControlEvent) {
        if !self
            .track_external
            .get(c.track as usize)
            .copied()
            .unwrap_or(false)
        {
            return;
        }
        if c.number == 64 {
            if let Some(down) = self.ext_sustain.get_mut(c.track as usize) {
                *down = c.value >= 64;
            }
        }
        let _ = self.midi_out.push(MidiOutEvent {
            track: c.track,
            on: false,
            pitch: c.number,
            vel: c.value,
            controller: true,
        });
    }

    /// Lets up the sustain pedal of every external instrument it is down on.
    fn release_ext_sustain(&mut self) {
        for track in 0..self.ext_sustain.len() {
            if std::mem::replace(&mut self.ext_sustain[track], false) {
                let _ = self.midi_out.push(MidiOutEvent {
                    track: track as u16,
                    on: false,
                    pitch: 64,
                    vel: 0,
                    controller: true,
                });
            }
        }
    }

    /// Silences timeline voices (seek, pause). Live notes keep playing until `NoteOff`.
    fn kill_voices(&mut self) {
        self.release_ext_sustain();
        for v in self.voices.iter_mut().filter(|v| !v.live) {
            v.active = false;
        }
        // The external instruments' timeline notes stop as well (live ones are kept).
        let mut i = 0;
        while i < self.ext_notes.len() {
            if self.ext_notes[i].live {
                i += 1;
            } else {
                let e = self.ext_notes.swap_remove(i);
                self.send_midi(e.track, false, e.pitch, 0);
            }
        }
    }

    fn seek(&mut self, beat: f64) {
        self.beat = if beat.is_finite() { beat.max(0.0) } else { 0.0 };
        self.kill_voices();
        self.next_note = self
            .song
            .notes
            .partition_point(|n| n.start_beat < self.beat);
        self.next_control = self.song.controls.partition_point(|c| c.beat < self.beat);
        self.reposition_clips();
        self.mtc_locate();
    }

    fn reposition_clips(&mut self) {
        // Clips play in real time, so their offset is the time since the region start.
        let map = &self.tempo_map;
        let now = map.seconds_at(self.beat);
        for c in self.song.clips.iter_mut() {
            let start = map.seconds_at(c.start_beat);
            // Refresh on song/map changes and seeks. The region's end is constant
            // between these events, including while transport is stopped.
            c.end_beat = map.beat_at(start + c.len_frames as f64 / self.sr as f64);
            let since = now - start;
            c.reposition((since * self.sr as f64).round() as i64);
        }
    }

    fn smoothing_samples(&self) -> u32 {
        let sounding = self.playing
            || self.voices.iter().any(|v| v.active)
            || self.click_remaining > 0
            || (self.test_hz > 0.0 && self.test_gain > 0.0);
        if sounding {
            (self.sr * 0.005).round().max(1.0) as u32
        } else {
            0
        }
    }

    fn rebuild_mix(&mut self, smooth: bool, changed_track: Option<usize>) {
        let samples = if smooth { self.smoothing_samples() } else { 0 };
        self.mix_ramp_remaining = samples;
        let any_solo = self.song.tracks.iter().any(|t| t.solo);
        for i in 0..MAX_TRACKS {
            match self.song.tracks.get(i) {
                Some(t) => {
                    // Solo affects every channel, but another channel's gain/pan
                    // target is unchanged. Preserve full immediate resets when silent.
                    // An Aux strip stays audible under solo: it carries the sends of the
                    // soloed strips (the others have no signal to send).
                    let aux = self.routes[i].bus_in > 0;
                    self.track_audible[i] = !t.mute && (!any_solo || t.solo || aux);
                    if samples > 0 && changed_track.is_some_and(|track| track != i) {
                        continue;
                    }
                    let t = t.sanitized();
                    self.set_strip_gain(
                        i,
                        self.auto_vol[i].unwrap_or(t.gain_db),
                        self.auto_pan[i].unwrap_or(t.pan),
                        samples,
                    );
                }
                None => {
                    self.track_ramp_l[i].set(1.0, 0);
                    self.track_ramp_r[i].set(1.0, 0);
                    self.track_audible[i] = true;
                }
            }
            if samples == 0 {
                self.track_gain_l[i] = self.track_ramp_l[i].next();
                self.track_gain_r[i] = self.track_ramp_r[i].next();
            }
        }
    }

    /// Aims strip `i`'s left and right gains at a fader (dB) and pan, over `samples`.
    fn set_strip_gain(&mut self, i: usize, gain_db: f32, pan: f32, samples: u32) {
        let g = 10f32.powf(gain_db / 20.0);
        // Equal-power pan law, normalised so the centre is unity gain.
        let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        self.track_ramp_l[i].set(g * a.cos() * std::f32::consts::SQRT_2, samples);
        self.track_ramp_r[i].set(g * a.sin() * std::f32::consts::SQRT_2, samples);
    }

    /// Sorts the strips into plain, complex (effects or sends) and Aux after any of them changed.
    fn sort_strips(&mut self) {
        self.complex.clear();
        self.aux.clear();
        for i in 0..MAX_TRACKS {
            let has_chain = self.chains[i].as_ref().is_some_and(|c| !c.fx.is_empty());
            let has_pdc = self.pdc[i].as_ref().is_some_and(|p| !p.is_empty());
            self.strip_kind[i] = if self.routes[i].bus_in > 0 {
                self.aux.push(i as u16);
                2
            } else if has_chain || has_pdc || self.routes[i].has_sends() {
                self.complex.push(i as u16);
                1
            } else {
                0
            };
        }
    }

    /// Reads the automation lanes at the playhead and aims the strips they move.
    fn update_automation(&mut self) {
        let beat = self.beat;
        let samples = AUTO_BLOCK;
        for k in 0..self.automation.len() {
            let lane = &self.automation[k];
            if !lane.follow {
                continue;
            }
            let t = lane.track as usize;
            if t >= MAX_TRACKS {
                continue;
            }
            let Some(v) = lane.value_at(beat) else {
                continue;
            };
            match lane.target {
                AutoTarget::Volume => {
                    self.auto_vol[t] = Some((v + lane.offset_db).clamp(-96.0, 6.0))
                }
                AutoTarget::Pan => self.auto_pan[t] = Some(v.clamp(-1.0, 1.0)),
                AutoTarget::Fx { slot, param } => {
                    if let Some(chain) = self.chains[t].as_mut() {
                        chain.set_param_norm(slot as usize, param as usize, v);
                    }
                }
            }
        }
        for k in 0..self.auto_tracks.len() {
            let t = self.auto_tracks[k] as usize;
            let own = self.song.tracks.get(t).copied().unwrap_or_default();
            self.set_strip_gain(
                t,
                self.auto_vol[t].unwrap_or(own.gain_db),
                self.auto_pan[t].unwrap_or(own.pan),
                samples,
            );
        }
    }

    /// Runs one strip: effects, delay compensation, pre-fader sends, fader and pan,
    /// post-fader sends, and into the mix.
    #[inline]
    fn run_strip(&mut self, t: usize, out: &mut (f64, f64)) {
        let route = self.routes[t];
        let audible = self.track_audible[t];
        let (mut xl, mut xr) = (self.in_l[t], self.in_r[t]);
        self.in_l[t] = 0.0;
        self.in_r[t] = 0.0;
        if route.bus_in > 0 {
            let b = route.bus_in as usize - 1;
            xl += self.bus_l[b];
            xr += self.bus_r[b];
        }
        if !audible {
            xl = 0.0;
            xr = 0.0;
        }
        // The effects work in 32 bits.
        let (mut xl, mut xr) = (xl as f32, xr as f32);
        if let Some(chain) = self.chains[t].as_mut() {
            (xl, xr) = chain.process(xl, xr);
        }
        if let Some(pdc) = self.pdc[t].as_mut() {
            (xl, xr) = pdc.delay_before(xl, xr);
        }
        let (xl, xr) = (xl as f64, xr as f64);
        if audible && route.bus_in == 0 {
            for s in route.sends.iter().filter(|s| s.bus > 0 && s.pre) {
                let g = send_gain(s.level) as f64;
                self.bus_l[s.bus as usize - 1] += xl * g;
                self.bus_r[s.bus as usize - 1] += xr * g;
            }
        }
        let (yl, yr) = (
            xl * self.track_gain_l[t] as f64,
            xr * self.track_gain_r[t] as f64,
        );
        if audible {
            if route.bus_in == 0 {
                for s in route.sends.iter().filter(|s| s.bus > 0 && !s.pre) {
                    let g = send_gain(s.level) as f64;
                    self.bus_l[s.bus as usize - 1] += yl * g;
                    self.bus_r[s.bus as usize - 1] += yr * g;
                }
            }
            self.track_pk_l[t] = self.track_pk_l[t].max(yl.abs() as f32);
            self.track_pk_r[t] = self.track_pk_r[t].max(yr.abs() as f32);
        }
        // The compensation after the fader keeps running while the strip is silent, so its
        // timing is right the moment it is heard.
        let (yl, yr) = match self.pdc[t].as_mut() {
            Some(pdc) => {
                let (a, b) = pdc.delay_after(yl as f32, yr as f32);
                (a as f64, b as f64)
            }
            None => (yl, yr),
        };
        if audible {
            out.0 += yl;
            out.1 += yr;
        }
    }

    /// The strips' signals of this sample, through their effects, faders and sends, summed.
    fn mix_strips(&mut self) -> (f64, f64) {
        let mut out = (0f64, 0f64);
        for k in 0..self.dirty.len() {
            let t = self.dirty[k] as usize;
            self.touched[t] = false;
            if self.strip_kind[t] != 0 {
                continue;
            }
            let (yl, yr) = (
                self.in_l[t] * self.track_gain_l[t] as f64,
                self.in_r[t] * self.track_gain_r[t] as f64,
            );
            self.in_l[t] = 0.0;
            self.in_r[t] = 0.0;
            out.0 += yl;
            out.1 += yr;
            self.track_pk_l[t] = self.track_pk_l[t].max(yl.abs() as f32);
            self.track_pk_r[t] = self.track_pk_r[t].max(yr.abs() as f32);
        }
        self.dirty.clear();
        for k in 0..self.complex.len() {
            self.run_strip(self.complex[k] as usize, &mut out);
        }
        for k in 0..self.aux.len() {
            self.run_strip(self.aux[k] as usize, &mut out);
        }
        if !self.aux.is_empty() || !self.complex.is_empty() {
            self.bus_l = [0.0; MAX_BUSES];
            self.bus_r = [0.0; MAX_BUSES];
        }
        out
    }

    fn drain_commands(&mut self) {
        while let Ok(next) = self.rx.peek() {
            // Leave owning commands in the queue until their old state can be returned.
            // This is the only garbage producer; the consumer can only free more slots,
            // so the single push below is guaranteed to fit after this check.
            if next.retires_state() && self.trash.is_full() {
                break;
            }
            let Ok(cmd) = self.rx.pop() else { break };
            match cmd {
                Command::Play => {
                    if !self.playing {
                        self.start_beat = self.beat;
                    }
                    self.playing = true;
                    self.mtc_locate();
                }
                Command::Pause => {
                    self.playing = false;
                    self.kill_voices();
                    self.mtc_locate();
                }
                Command::Stop => {
                    self.playing = false;
                    let b = self.start_beat;
                    self.seek(b);
                }
                Command::SetPosition { beat } => self.seek(beat),
                Command::SetTempoMap(map) => {
                    let old = std::mem::replace(&mut self.tempo_map, map);
                    let _ = self.trash.push(Garbage::TempoMap(old));
                    self.reposition_clips();
                    // A tempo edit after the playhead leaves the time where it was.
                    if (self.tempo_map.seconds_at(self.beat) - self.mtc_secs).abs() > 0.001 {
                        self.mtc_locate();
                    }
                }
                Command::SetMtc {
                    on,
                    rate,
                    offset_frames,
                } => {
                    self.mtc.configure(on, rate);
                    self.mtc_rate = rate;
                    self.mtc_offset_frames = offset_frames;
                    self.mtc_locate();
                }
                Command::SetMetronome { on } => self.metronome = on,
                Command::SetMasterGain { linear } => {
                    if linear.is_finite() {
                        let samples = self.smoothing_samples();
                        self.master.set(linear.clamp(0.0, 2.0), samples);
                    }
                }
                Command::SetCycle {
                    on,
                    start_beat,
                    end_beat,
                } => {
                    self.cycle = (on
                        && start_beat.is_finite()
                        && end_beat.is_finite()
                        && start_beat >= 0.0
                        && end_beat > start_beat)
                        .then_some((start_beat, end_beat));
                }
                Command::SetTrackMix { track, mix } => {
                    if let Some(t) = self.song.tracks.get_mut(track as usize) {
                        *t = mix.sanitized();
                        self.rebuild_mix(true, Some(track as usize));
                    }
                }
                Command::NoteOn { pitch, vel, track } => {
                    self.live_note_on(pitch.min(127), vel.clamp(1, 127), track)
                }
                Command::NoteOff { pitch } => self.live_note_off(pitch, None),
                Command::NoteOffTrack { pitch, track } => self.live_note_off(pitch, Some(track)),
                Command::SetTrackExternal { track, external } => {
                    if let Some(slot) = self.track_external.get_mut(track as usize) {
                        let was = std::mem::replace(slot, external);
                        // Notes already out to the instrument end when the track stops using it.
                        if was && !external {
                            let mut i = 0;
                            while i < self.ext_notes.len() {
                                if self.ext_notes[i].track == track {
                                    let e = self.ext_notes.swap_remove(i);
                                    self.send_midi(e.track, false, e.pitch, 0);
                                } else {
                                    i += 1;
                                }
                            }
                        }
                    }
                }
                Command::SetTrackPatch { track, patch } => {
                    if let Some(slot) = self.track_patch.get_mut(track as usize) {
                        *slot = patch.sanitized();
                    }
                }
                Command::SetInserts { track, chain } => {
                    let t = track as usize;
                    if t < MAX_TRACKS {
                        let old = self.chains[t].replace(chain);
                        if let Some(old) = old {
                            let _ = self.trash.push(Garbage::Chain(old));
                        }
                        self.shown.gain_reduction[t] = 0.0;
                        self.sort_strips();
                    } else {
                        let _ = self.trash.push(Garbage::Chain(chain));
                    }
                }
                Command::SetPdc { track, pdc } => {
                    let t = track as usize;
                    if t < MAX_TRACKS {
                        if let Some(old) = self.pdc[t].replace(pdc) {
                            let _ = self.trash.push(Garbage::Pdc(old));
                        }
                        self.sort_strips();
                    } else {
                        let _ = self.trash.push(Garbage::Pdc(pdc));
                    }
                }
                Command::SetFx {
                    track,
                    slot,
                    params,
                    bypass,
                } => {
                    if let Some(fx) = self
                        .chains
                        .get_mut(track as usize)
                        .and_then(|c| c.as_mut())
                        .and_then(|c| c.fx.get_mut(slot as usize))
                    {
                        fx.set(params, bypass);
                    }
                }
                Command::SetRoute { track, route } => {
                    let t = track as usize;
                    if t < MAX_TRACKS {
                        self.routes[t] = route.sanitized();
                        self.sort_strips();
                        self.rebuild_mix(true, Some(t));
                    }
                }
                Command::SetAutomation(lanes) => {
                    let old = std::mem::replace(&mut self.automation, lanes);
                    let _ = self.trash.push(Garbage::Automation(old));
                    self.auto_vol.fill(None);
                    self.auto_pan.fill(None);
                    self.auto_on.fill(false);
                    self.auto_tracks.clear();
                    // Effect parameters go back to what the user set; the lanes that are
                    // played move them again from the next block.
                    for chain in self.chains.iter_mut().flatten() {
                        chain.restore();
                    }
                    self.auto_any = self.automation.iter().any(|l| l.follow);
                    for lane in self
                        .automation
                        .iter()
                        .filter(|l| l.follow && !matches!(l.target, AutoTarget::Fx { .. }))
                    {
                        let t = lane.track as usize;
                        if t < MAX_TRACKS && !self.auto_on[t] {
                            self.auto_on[t] = true;
                            self.auto_tracks.push(lane.track);
                        }
                    }
                    self.auto_left = 0;
                    self.rebuild_mix(true, None);
                }
                Command::SetTestTone { hz, gain } => {
                    self.test_hz = if hz.is_finite() {
                        hz.clamp(0.0, self.sr * 0.45)
                    } else {
                        0.0
                    };
                    self.test_gain = if gain.is_finite() {
                        gain.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                }
                Command::SetSong(new) => {
                    let old = std::mem::replace(&mut self.song, new);
                    let _ = self.trash.push(Garbage::Song(old));
                    self.rebuild_mix(false, None);
                    let b = self.beat;
                    self.seek(b);
                }
                Command::SetControls(controls) => {
                    let old = std::mem::replace(&mut self.song.controls, controls);
                    let _ = self.trash.push(Garbage::Controls(old));
                    self.next_control = self.song.controls.partition_point(|c| c.beat < self.beat);
                }
                Command::SetNotes(notes) => {
                    let old = std::mem::replace(&mut self.song.notes, notes);
                    let _ = self.trash.push(Garbage::Notes(old));
                    self.next_note = self
                        .song
                        .notes
                        .partition_point(|n| n.start_beat < self.beat);
                }
            }
        }
    }

    fn trigger(&mut self, n: NoteEvent) -> usize {
        if self
            .track_external
            .get(n.track as usize)
            .copied()
            .unwrap_or(false)
        {
            // An external instrument makes its own sound.
            self.ext_note_on(n, false);
            return 0;
        }
        // No free voice: steal the first one.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_default();
        let track = n.track.min(MAX_TRACKS as u16 - 1);
        let mut patch = self.track_patch[track as usize];
        let mut base_hz = midi_hz(n.pitch);
        if patch.kit != Kit::None {
            (patch, base_hz) = kit_voice(&patch, n.pitch);
        }
        self.voices[idx] = Voice {
            active: true,
            releasing: false,
            phase_a: 0.0,
            phase_b: 0.0,
            base_hz,
            amp: (n.vel as f32 / 127.0) * 0.18 * patch.gain,
            env: 0.0,
            stage: 0,
            rel_step: 0.0,
            end_beat: n.end_beat,
            track,
            live: false,
            pitch: n.pitch,
            live_left: 0,
            patch,
            filter: Svf::default(),
            vibrato: 0.0,
            noise: 0x9E37_79B9 ^ (n.pitch as u32).wrapping_mul(2_654_435_761) | 1,
            pitch_env: patch.pitch_drop,
            ratio_b: 2f32.powf((patch.semi_b + patch.detune_b / 100.0) / 12.0),
            pitch_decay_block: (-(CTRL_BLOCK as f32) / (patch.pitch_decay.max(0.001) * self.sr))
                .exp(),
            ctrl_left: 0,
            dt_a: 0.0,
            dt_b: 0.0,
            filter_f: 0.0,
            filter_damping: Svf::damping(patch.resonance),
            attack_step: 1.0 / (patch.attack.max(0.001) * self.sr),
            decay_step: (1.0 - patch.sustain).max(0.0) / (patch.decay.max(0.001) * self.sr),
        };
        idx
    }

    fn live_note_on(&mut self, pitch: u8, vel: u8, track: u16) {
        if self
            .track_external
            .get(track as usize)
            .copied()
            .unwrap_or(false)
        {
            self.ext_note_on(
                NoteEvent {
                    start_beat: 0.0,
                    end_beat: f64::MAX,
                    pitch,
                    vel,
                    track,
                },
                true,
            );
            return;
        }
        let idx = self.trigger(NoteEvent {
            start_beat: 0.0,
            end_beat: f64::MAX,
            pitch,
            vel,
            track,
        });
        // Use the exact slot allocated or stolen, including same-pitch timeline voices.
        let live_left = (4.0 * self.sr) as u32;
        let v = &mut self.voices[idx];
        v.live = true;
        v.live_left = live_left;
    }

    fn live_note_off(&mut self, pitch: u8, track: Option<u16>) {
        let mut i = 0;
        while i < self.ext_notes.len() {
            let e = self.ext_notes[i];
            if e.live && e.pitch == pitch && track.is_none_or(|t| e.track == t) {
                self.ext_notes.swap_remove(i);
                self.send_midi(e.track, false, e.pitch, 0);
            } else {
                i += 1;
            }
        }
        for v in self.voices.iter_mut().filter(|v| {
            v.active && v.live && v.pitch == pitch && track.is_none_or(|t| v.track == t)
        }) {
            v.releasing = true;
        }
    }

    /// Render interleaved stereo (`out.len() == frames * 2`).
    pub fn process(&mut self, out: &mut [f32]) {
        self.drain_commands();
        let frame_secs = 1.0 / self.sr as f64;
        let (mut pk_l, mut pk_r) = (0f32, 0f32);
        for c in self.song.clips.iter_mut() {
            c.service();
        }
        self.track_pk_l.fill(0.0);
        self.track_pk_r.fill(0.0);

        for frame in out.as_chunks_mut::<2>().0 {
            let sample_beat = self.beat;
            // Advance once per track/sample, not once per voice or overlapping audio clip.
            if self.mix_ramp_remaining > 0 {
                for i in 0..MAX_TRACKS {
                    if !self.auto_on[i] {
                        self.track_gain_l[i] = self.track_ramp_l[i].next();
                        self.track_gain_r[i] = self.track_ramp_r[i].next();
                    }
                }
                self.mix_ramp_remaining -= 1;
            }
            if self.auto_any {
                if self.auto_left == 0 {
                    self.update_automation();
                    self.auto_left = AUTO_BLOCK;
                }
                self.auto_left -= 1;
                for k in 0..self.auto_tracks.len() {
                    let t = self.auto_tracks[k] as usize;
                    self.track_gain_l[t] = self.track_ramp_l[t].next();
                    self.track_gain_r[t] = self.track_ramp_r[t].next();
                }
            }
            let (mut l, mut r) = (0f64, 0f64);
            if self.playing {
                while let Some(n) = self.song.notes.get(self.next_note).copied() {
                    if n.start_beat > self.beat {
                        break;
                    }
                    self.next_note += 1;
                    if n.end_beat > self.beat {
                        self.trigger(n);
                    }
                }
                while let Some(c) = self.song.controls.get(self.next_control).copied() {
                    if c.beat > self.beat {
                        break;
                    }
                    self.next_control += 1;
                    self.send_control(c);
                }
                for c in self.song.clips.iter_mut() {
                    if let Some((cl, cr)) = c.tick() {
                        let t = (c.track as usize).min(MAX_TRACKS - 1);
                        if self.track_audible[t] {
                            let edit_gain = c.envelope(sample_beat, c.end_beat);
                            self.in_l[t] += (cl * edit_gain) as f64;
                            self.in_r[t] += (cr * edit_gain) as f64;
                            if !self.touched[t] {
                                self.touched[t] = true;
                                self.dirty.push(t as u16);
                            }
                        }
                    }
                }
            }
            if !self.ext_notes.is_empty() {
                self.release_ext_notes(false);
            }
            for v in self.voices.iter_mut().filter(|v| v.active) {
                if v.live {
                    v.live_left = v.live_left.saturating_sub(1);
                    if v.live_left == 0 {
                        v.releasing = true;
                    }
                } else if self.beat >= v.end_beat || !self.playing {
                    v.releasing = true;
                }
                let sr = self.sr;
                if v.releasing && v.stage != 3 {
                    v.stage = 3;
                    v.rel_step = v.env / (v.patch.release.max(0.002) * sr);
                }
                let pt = &v.patch;
                match v.stage {
                    0 => {
                        v.env += v.attack_step;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = 1;
                        }
                    }
                    1 => {
                        v.env -= v.decay_step;
                        if v.env <= pt.sustain {
                            v.env = pt.sustain;
                            v.stage = 2;
                        }
                    }
                    2 => {}
                    _ => {
                        v.env -= v.rel_step;
                        if v.env <= 0.0 {
                            v.active = false;
                            continue;
                        }
                    }
                }
                let filtered = pt.cutoff < 19_000.0 || pt.highpass;
                if v.ctrl_left == 0 {
                    v.ctrl_left = CTRL_BLOCK;
                    let vib = if pt.vibrato_cents != 0.0 {
                        v.vibrato = (v.vibrato + pt.vibrato_hz * CTRL_BLOCK as f32 / sr).fract();
                        (v.vibrato * std::f32::consts::TAU).sin() * pt.vibrato_cents / 1200.0
                    } else {
                        0.0
                    };
                    let bend = v.pitch_env / 12.0 + vib;
                    let mul = if bend == 0.0 { 1.0 } else { 2f32.powf(bend) };
                    if pt.pitch_drop != 0.0 {
                        v.pitch_env *= v.pitch_decay_block;
                    }
                    v.dt_a = v.base_hz * mul / sr;
                    v.dt_b = v.dt_a * v.ratio_b;
                    if filtered {
                        let fc = if pt.filter_env == 0.0 {
                            pt.cutoff
                        } else {
                            pt.cutoff * 2f32.powf(pt.filter_env * v.env * 4.0)
                        };
                        v.filter_f =
                            Svf::coefficient(fc, sr).min(Svf::max_coefficient(v.filter_damping));
                    }
                }
                v.ctrl_left -= 1;
                let (dt_a, dt_b) = (v.dt_a, v.dt_b);
                v.phase_a = wrap_phase(v.phase_a + dt_a);
                v.phase_b = wrap_phase(v.phase_b + dt_b);
                let mut x = osc(pt.wave_a, v.phase_a, dt_a, pt.pulse_width) * pt.level_a
                    + osc(pt.wave_b, v.phase_b, dt_b, pt.pulse_width) * pt.level_b;
                if pt.noise > 0.0 {
                    v.noise ^= v.noise << 13;
                    v.noise ^= v.noise >> 17;
                    v.noise ^= v.noise << 5;
                    x += ((v.noise as f32 / u32::MAX as f32) * 2.0 - 1.0) * pt.noise;
                }
                if filtered {
                    let (low, high) = v.filter.tick(x, v.filter_f, v.filter_damping);
                    x = if pt.highpass { high } else { low };
                }
                // Last line of defence: a voice that ever goes NaN or infinite is silenced for
                // that sample and its filter reset, instead of poisoning the mix and meters.
                if !x.is_finite() {
                    v.filter = Svf::default();
                    x = 0.0;
                }
                let t = v.track as usize;
                if self.track_audible[t] {
                    let x = finite_or_zero(x * v.amp * v.env);
                    self.in_l[t] += x as f64;
                    self.in_r[t] += x as f64;
                    if !self.touched[t] {
                        self.touched[t] = true;
                        self.dirty.push(t as u16);
                    }
                }
            }
            let (ml, mr) = self.mix_strips();
            l += ml;
            r += mr;
            if self.test_hz > 0.0 {
                self.test_phase += self.test_hz / self.sr;
                if self.test_phase >= 1.0 {
                    self.test_phase -= 1.0;
                }
                let t = (self.test_phase * std::f32::consts::TAU).sin() * self.test_gain;
                l += t as f64;
                r += t as f64;
            }
            if self.click_remaining > 0 {
                self.click_remaining -= 1;
                self.click_phase += self.click_freq / self.sr;
                let env = self.click_remaining as f32 / (0.03 * self.sr);
                let c = (self.click_phase * std::f32::consts::TAU).sin() * env * 0.3;
                l += c as f64;
                r += c as f64;
            }
            let master = self.master.next() as f64;
            // `clamp` lets NaN through, so a non-finite sum becomes silence first.
            let l = finite_or_zero((l * master) as f32).clamp(-1.0, 1.0);
            let r = finite_or_zero((r * master) as f32).clamp(-1.0, 1.0);
            pk_l = pk_l.max(l.abs());
            pk_r = pk_r.max(r.abs());
            frame[0] = l;
            frame[1] = r;
            // Render events at the current sample position, then advance to the next one.
            if self.playing {
                let prev = self.beat;
                self.beat = self.tempo_map.advance(prev, frame_secs);
                if self.mtc.is_on() {
                    self.mtc_secs += frame_secs;
                    let frames = self.mtc_frames();
                    let tx = &mut self.mtc_out;
                    self.mtc.tick(frames, |m| {
                        let _ = tx.push(m);
                    });
                }
                if let Some((s, e)) = self.cycle {
                    if self.beat >= e {
                        let start = self.tempo_map.seconds_at(s);
                        let end = self.tempo_map.seconds_at(e);
                        let duration = end - start;
                        if duration > 0.0 {
                            let excess = self.tempo_map.seconds_at(self.beat) - end;
                            let wrapped =
                                self.tempo_map.beat_at(start + excess.rem_euclid(duration));
                            self.seek(wrapped);
                            // A fractional-sample wrap must still trigger notes at cycle start.
                            self.next_note = self.song.notes.partition_point(|n| n.start_beat < s);
                            self.next_control = self.song.controls.partition_point(|c| c.beat < s);
                        }
                    }
                }
                if self.metronome {
                    let (a, b) = (
                        self.tempo_map.position(prev),
                        self.tempo_map.position(self.beat),
                    );
                    if (a.bar, a.beat) != (b.bar, b.beat) {
                        self.click_remaining = (0.03 * self.sr) as u32;
                        self.click_phase = 0.0;
                        self.click_freq = if b.beat == 0 { 1500.0 } else { 1000.0 };
                    }
                }
            }
        }
        let mut underruns = 0;
        for c in self.song.clips.iter_mut() {
            underruns += std::mem::take(&mut c.underruns).min(1);
        }
        if underruns > 0 {
            self.shared
                .underruns
                .fetch_add(underruns, Ordering::Relaxed);
        }
        let shown = &mut self.shown;
        shown.position_beats = self.beat;
        shown.playing = self.playing;
        // Peak with simple decay so the UI meter does not flicker between callbacks.
        let (ol, or) = shown.peaks;
        shown.peaks = (decayed_peak(pk_l, ol), decayed_peak(pk_r, or));
        for (t, (l, r)) in shown.track_peaks.iter_mut().enumerate() {
            *l = decayed_peak(self.track_pk_l[t], *l);
            *r = decayed_peak(self.track_pk_r[t], *r);
        }
        for &t in self.complex.iter().chain(self.aux.iter()) {
            let t = t as usize;
            shown.gain_reduction[t] = self.chains[t].as_ref().map_or(0.0, |c| c.reduction_db());
        }
        *self.state.input() = *shown;
        self.state.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> (EngineCore, Producer<Command>, StateReader) {
        let (tx, rx) = rtrb::RingBuffer::new(64);
        let (gtx, _grx) = rtrb::RingBuffer::new(8);
        let mut core = EngineCore::new(48_000, rx, gtx, Arc::new(Shared::new()));
        let Some(state) = core.take_state_reader() else {
            panic!("one reader");
        };
        (core, tx, state)
    }

    fn note(start: f64, end: f64) -> NoteEvent {
        NoteEvent {
            start_beat: start,
            end_beat: end,
            pitch: 69,
            vel: 100,
            track: 0,
        }
    }

    #[test]
    fn clip_fade_timing_refreshes_after_tempo_and_song_changes_without_allocating() {
        let (mut core, mut tx, _) = rig();
        let (mut producer, consumer) = rtrb::RingBuffer::new(8);
        let clip = AudioClip::new(
            0,
            4.0,
            48_000,
            ClipEdit {
                fade_out_beats: 1.0,
                fade_curve: FadeCurve::Linear,
                ..ClipEdit::default()
            },
            consumer,
            Arc::new(ClipShared::default()),
        );
        let song = Song::new(vec![], vec![TrackMix::default()]).with_clips(vec![clip]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());

        for (bpm, expected_end, expected_sample) in
            [(120.0, 6.0, 0.8), (60.0, 5.0, 0.4), (180.0, 7.0, 0.8)]
        {
            assert!(tx
                .push(Command::SetTempoMap(Box::new(TempoMap::constant(
                    bpm,
                    (4, 4)
                ))))
                .is_ok());
            assert!(tx.push(Command::SetPosition { beat: 4.5 }).is_ok());
            assert!(tx.push(Command::Play).is_ok());
            assert_no_alloc::assert_no_alloc(|| core.process(&mut []));
            let clip = &core.song.clips[0];
            assert!((clip.end_beat - expected_end).abs() < 1e-12);
            assert!(producer
                .push(Block {
                    gen: clip.gen,
                    start: clip.pos as u64,
                    frames: 1024,
                    data: [1.0; 2048],
                })
                .is_ok());
            let mut out = [0.0; 2];
            assert_no_alloc::assert_no_alloc(|| core.process(&mut out));
            assert!(out.iter().all(|s| (*s - expected_sample).abs() < 1e-6));
        }

        let (_producer, consumer) = rtrb::RingBuffer::new(8);
        let replacement = AudioClip::new(
            0,
            8.0,
            96_000,
            ClipEdit::default(),
            consumer,
            Arc::new(ClipShared::default()),
        );
        assert!(tx
            .push(Command::SetSong(Box::new(
                Song::default().with_clips(vec![replacement]),
            )))
            .is_ok());
        assert_no_alloc::assert_no_alloc(|| core.process(&mut []));
        assert!((core.song.clips[0].end_beat - 14.0).abs() < 1e-12);
    }

    #[test]
    fn fade_curves_preserve_reference_gain_inside_outside_and_overlapping_fades() {
        let (_producer, consumer) = rtrb::RingBuffer::new(8);
        let mut clip = AudioClip::new(
            0,
            0.0,
            96_000,
            ClipEdit::default(),
            consumer,
            Arc::new(ClipShared::default()),
        );
        for curve in [FadeCurve::Linear, FadeCurve::EqualPower, FadeCurve::SCurve] {
            for duration in [1.0, 3.0] {
                clip.edit.fade_curve = curve;
                clip.edit.fade_in_beats = duration;
                clip.edit.fade_out_beats = duration;
                for beat in [-1.0_f64, 0.0, 0.5, 1.0, 2.0, 3.0, 3.5, 4.0, 5.0] {
                    let reference = |x: f64| {
                        let x = x.clamp(0.0, 1.0);
                        match curve {
                            FadeCurve::Linear => x as f32,
                            FadeCurve::EqualPower => (x * std::f64::consts::FRAC_PI_2).sin() as f32,
                            FadeCurve::SCurve => (x * x * (3.0 - 2.0 * x)) as f32,
                        }
                    };
                    let expected = reference(beat / duration) * reference((4.0 - beat) / duration);
                    assert_eq!(clip.envelope(beat, 4.0), expected);
                }
            }
        }
    }

    #[test]
    fn single_channel_mix_updates_match_full_rebuild_without_allocating() {
        let (mut changed, mut changed_tx, _) = rig();
        let (mut reference, mut reference_tx, _) = rig();
        for tx in [&mut changed_tx, &mut reference_tx] {
            assert!(tx
                .push(Command::SetSong(Box::new(Song::new(
                    vec![],
                    vec![TrackMix::default(); 2],
                ))))
                .is_ok());
            for (track, pitch) in [(0, 60), (1, 67)] {
                assert!(tx
                    .push(Command::NoteOn {
                        track,
                        pitch,
                        vel: 80
                    })
                    .is_ok());
            }
        }
        let mut actual = [0.0; 128];
        let mut expected = [0.0; 128];
        changed.process(&mut actual);
        reference.process(&mut expected);
        assert_eq!(actual, expected);

        for (track, gain_db, pan, mute, solo) in [
            (0, -6.0, -0.5, false, false),
            (1, -9.0, 0.75, false, true),
            (0, -3.0, 0.5, true, false),
            (1, 0.0, 0.0, false, false),
        ] {
            let mix = TrackMix {
                gain_db,
                pan,
                mute,
                solo,
            };
            assert!(changed_tx.push(Command::SetTrackMix { track, mix }).is_ok());
            reference.song.tracks[track as usize] = mix;
            reference.rebuild_mix(true, None);
            // Retarget while earlier 240-sample ramps are still running.
            for _ in 0..2 {
                assert_no_alloc::assert_no_alloc(|| changed.process(&mut actual));
                assert_no_alloc::assert_no_alloc(|| reference.process(&mut expected));
                assert_eq!(actual, expected);
                assert_eq!(changed.track_audible, reference.track_audible);
            }
        }

        // A silent edit must still complete every pending ramp immediately.
        for core in [&mut changed, &mut reference] {
            for voice in &mut core.voices {
                voice.active = false;
            }
        }
        let mix = TrackMix::default();
        assert!(changed_tx
            .push(Command::SetTrackMix { track: 0, mix })
            .is_ok());
        reference.song.tracks[0] = mix;
        reference.rebuild_mix(false, None);
        assert_no_alloc::assert_no_alloc(|| changed.process(&mut []));
        assert_eq!(changed.track_gain_l, reference.track_gain_l);
        assert_eq!(changed.track_gain_r, reference.track_gain_r);
    }

    #[test]
    fn full_garbage_queue_defers_state_in_order_and_does_not_allocate() {
        let (mut tx, rx) = rtrb::RingBuffer::new(8);
        let (gtx, mut grx) = rtrb::RingBuffer::new(1);
        let shared = Arc::new(Shared::new());
        let mut e = EngineCore::new(48_000, rx, gtx, shared);
        let commands = [
            Command::SetSong(Box::new(Song::new(vec![note(0.0, 1.0)], vec![]))),
            Command::SetNotes(vec![note(2.0, 3.0)]),
            Command::SetTempoMap(Box::new(TempoMap::constant(90.0, (6, 8)))),
            Command::SetSong(Box::new(Song::new(vec![note(4.0, 5.0)], vec![]))),
            Command::Play,
        ];
        for cmd in commands {
            assert!(tx.push(cmd).is_ok());
        }
        let mut buf = [0.0; 256];
        for remaining in [4, 3, 2, 0] {
            assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
            assert_eq!(e.rx.slots(), remaining);
            assert!(buf.iter().all(|s| s.is_finite()));
            if remaining > 0 {
                // Repeated callbacks must neither consume nor destroy the blocked command.
                assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
                assert_eq!(e.rx.slots(), remaining);
                assert!(!e.playing);
            }
            assert!(grx.pop().is_ok()); // reclamation outside the audio callback
        }
        assert!(e.playing);
        assert_eq!(e.song.notes[0].start_beat, 4.0);
    }

    #[test]
    fn one_second_of_silence_is_finite_and_zero() {
        let (mut e, _tx, _s) = rig();
        let mut buf = vec![1.0f32; 48_000 * 2];
        e.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite() && *x == 0.0));
    }

    #[test]
    fn invalid_public_parameters_cannot_poison_the_mix_or_transport() {
        let (mut e, mut tx, _) = rig();
        let tracks = vec![
            TrackMix {
                gain_db: f32::NAN,
                pan: f32::INFINITY,
                ..TrackMix::default()
            },
            TrackMix::default(),
        ];
        let song = Song::new(
            vec![
                note(0.0, 1.0),
                NoteEvent {
                    track: 1,
                    ..note(0.0, 1.0)
                },
            ],
            tracks,
        );
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx
            .push(Command::SetTrackPatch {
                track: 0,
                patch: SynthPatch {
                    gain: f32::INFINITY,
                    semi_b: f32::MAX,
                    ..SynthPatch::default()
                },
            })
            .is_ok());
        assert!(tx
            .push(Command::SetTestTone {
                hz: f32::NAN,
                gain: f32::NAN
            })
            .is_ok());
        assert!(tx
            .push(Command::SetPosition {
                beat: f64::INFINITY
            })
            .is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = [0.0; 1024];
        assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
        assert!(buf.iter().all(|x| x.is_finite()));
        assert!(buf.iter().any(|x| x.abs() > 0.0));
        assert!(e.beat.is_finite());
    }

    #[test]
    fn transport_advances_one_beat_per_half_second_at_120() {
        let (mut e, mut tx, mut s) = rig();
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 24_000 * 2];
        e.process(&mut buf);
        assert!(
            (s.read().position_beats - 1.0).abs() < 1e-3,
            "{}",
            s.read().position_beats
        );
        assert!(s.read().playing);
    }

    #[test]
    fn stop_returns_to_start_position() {
        let (mut e, mut tx, mut s) = rig();
        assert!(tx.push(Command::SetPosition { beat: 8.0 }).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 4800 * 2];
        e.process(&mut buf);
        assert!(s.read().position_beats > 8.0);
        assert!(tx.push(Command::Stop).is_ok());
        e.process(&mut buf);
        assert_eq!(s.read().position_beats, 8.0);
    }

    #[test]
    fn note_makes_sound_and_never_nan() {
        let (mut e, mut tx, _s) = rig();
        let song = Song::new(
            vec![note(0.0, 1.0), note(0.5, 2.0)],
            vec![TrackMix::default()],
        );
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 48_000 * 2];
        e.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
        assert!(buf.iter().any(|x| x.abs() > 0.01));
    }

    #[test]
    fn muted_track_is_silent() {
        let (mut e, mut tx, _s) = rig();
        let song = Song::new(
            vec![note(0.0, 1.0)],
            vec![TrackMix {
                gain_db: 0.0,
                mute: true,
                solo: false,
                pan: 0.0,
            }],
        );
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 24_000 * 2];
        e.process(&mut buf);
        assert!(buf.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn metronome_clicks_on_beat() {
        let (mut e, mut tx, _s) = rig();
        assert!(tx.push(Command::SetMetronome { on: true }).is_ok());
        assert!(tx.push(Command::SetPosition { beat: 0.99 }).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 4_800 * 2];
        e.process(&mut buf);
        assert!(buf.iter().any(|x| x.abs() > 0.05));
    }

    #[test]
    fn hard_left_pan_silences_right_and_meters_track() {
        let (mut e, mut tx, mut s) = rig();
        let mix = TrackMix {
            pan: -1.0,
            ..TrackMix::default()
        };
        let song = Song::new(vec![note(0.0, 2.0)], vec![mix]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 4800 * 2];
        e.process(&mut buf);
        let right = buf
            .iter()
            .skip(1)
            .step_by(2)
            .fold(0f32, |m, x| m.max(x.abs()));
        let left = buf.iter().step_by(2).fold(0f32, |m, x| m.max(x.abs()));
        assert!(right < 1e-6, "right {right}");
        assert!(left > 0.05, "left {left}");
        let (tl, tr) = s.read().track_peaks(0);
        assert!(tl > 0.05 && tr < 1e-6, "meters {tl} {tr}");
    }

    #[test]
    fn live_note_sounds_while_stopped_and_stops_on_note_off() {
        let (mut e, mut tx, mut s) = rig();
        assert!(tx
            .push(Command::SetSong(Box::new(Song::new(
                vec![],
                vec![TrackMix::default()]
            ))))
            .is_ok());
        assert!(tx
            .push(Command::NoteOn {
                pitch: 69,
                vel: 100,
                track: 0
            })
            .is_ok());
        let mut buf = vec![0.0f32; 4800 * 2];
        e.process(&mut buf);
        assert!(buf.iter().any(|x| x.abs() > 0.05), "no sound while stopped");
        assert!(!s.read().playing);
        // A seek must not cut a live note.
        assert!(tx.push(Command::SetPosition { beat: 1.0 }).is_ok());
        e.process(&mut buf);
        assert!(
            buf.iter().any(|x| x.abs() > 0.05),
            "seek killed the live note"
        );
        assert!(tx.push(Command::NoteOff { pitch: 69 }).is_ok());
        e.process(&mut buf); // 100 ms: release is 90 ms
        e.process(&mut buf);
        assert!(buf.iter().all(|x| x.abs() < 1e-4), "note did not release");
    }

    #[test]
    fn live_note_releases_by_itself_after_four_seconds() {
        let (mut e, mut tx, _s) = rig();
        assert!(tx
            .push(Command::NoteOn {
                pitch: 60,
                vel: 100,
                track: 0
            })
            .is_ok());
        let mut buf = vec![0.0f32; 48_000 * 2];
        for _ in 0..3 {
            e.process(&mut buf);
        }
        assert!(
            buf.iter().any(|x| x.abs() > 0.05),
            "should still sound at 3 s"
        );
        // The safety release starts at 4.0 s and fades over 90 ms: silent from the 6th second.
        for _ in 0..3 {
            e.process(&mut buf);
        }
        assert!(
            buf.iter().all(|x| x.abs() < 1e-4),
            "should be silent after 6 s"
        );
    }

    #[test]
    fn live_note_marks_its_own_voice_and_preserves_timeline_voice() {
        let (mut e, _tx, _s) = rig();
        let timeline = e.trigger(note(0.0, 8.0));
        e.live_note_on(69, 100, 1);
        assert!(!e.voices[timeline].live);
        assert!(e.voices[1].live);
        assert_eq!(e.voices[1].track, 1);
        // Fill the remaining pool, then exercise the voice-stealing path.
        for _ in 2..MAX_VOICES {
            e.trigger(note(0.0, 8.0));
        }
        e.live_note_on(72, 100, 2);
        assert!(e.voices[0].live);
        assert_eq!((e.voices[0].pitch, e.voices[0].track), (72, 2));
        assert_eq!(e.voices.iter().filter(|v| v.active).count(), MAX_VOICES);
    }

    #[test]
    fn scoped_note_off_preserves_other_tracks_and_timeline_notes() {
        let (mut e, mut tx, _s) = rig();
        let timeline = e.trigger(note(0.0, 8.0));
        e.live_note_on(69, 100, 0);
        e.live_note_on(69, 100, 1);
        e.live_note_on(72, 100, 0);
        assert!(tx
            .push(Command::NoteOffTrack {
                pitch: 69,
                track: 0
            })
            .is_ok());
        assert_no_alloc::assert_no_alloc(|| e.process(&mut []));
        assert!(!e.voices[timeline].releasing);
        assert!(e.voices[1].releasing);
        assert!(!e.voices[2].releasing);
        assert!(!e.voices[3].releasing);
        assert!(tx
            .push(Command::NoteOffTrack {
                pitch: 69,
                track: 255
            })
            .is_ok());
        assert_no_alloc::assert_no_alloc(|| e.process(&mut []));
        assert!(!e.voices[2].releasing);
        // Existing callers retain the global-release behavior.
        assert!(tx.push(Command::NoteOff { pitch: 69 }).is_ok());
        assert_no_alloc::assert_no_alloc(|| e.process(&mut []));
        assert!(e.voices[2].releasing);
        assert!(!e.voices[timeline].releasing);
        assert!(!e.voices[3].releasing);
    }

    fn play_note_with(patch: SynthPatch, pitch: u8, frames: usize) -> Vec<f32> {
        let (mut e, mut tx, _s) = rig();
        assert!(tx
            .push(Command::SetSong(Box::new(Song::new(
                vec![],
                vec![TrackMix::default()]
            ))))
            .is_ok());
        assert!(tx.push(Command::SetTrackPatch { track: 0, patch }).is_ok());
        assert!(tx
            .push(Command::NoteOn {
                pitch,
                vel: 110,
                track: 0
            })
            .is_ok());
        let mut buf = vec![0.0f32; frames * 2];
        e.process(&mut buf);
        buf.iter().step_by(2).copied().collect()
    }

    #[test]
    fn slow_attack_patch_starts_quieter_than_the_default() {
        let slow = SynthPatch {
            attack: 0.4,
            ..SynthPatch::default()
        };
        let early = |x: &[f32]| x[..480].iter().map(|v| v * v).sum::<f32>();
        let fast = play_note_with(SynthPatch::default(), 69, 4800);
        let slow = play_note_with(slow, 69, 4800);
        assert!(
            early(&slow) < early(&fast) * 0.05,
            "{} vs {}",
            early(&slow),
            early(&fast)
        );
    }

    /// A GitHub report: piano patches (no resonance, a filter envelope that opens the cutoff
    /// to its limit) made the filter diverge, and the track's peak meter stuck at `inf`.
    #[test]
    fn a_fully_opened_filter_without_resonance_stays_finite() {
        use crate::synth::Wave;
        let patch = SynthPatch {
            wave_a: Wave::Saw,
            cutoff: 4000.0,
            resonance: 0.0,
            filter_env: 1.0,
            ..SynthPatch::default()
        };
        for pitch in [48, 72, 96] {
            let out = play_note_with(patch, pitch, 48_000);
            assert!(out.iter().all(|x| x.is_finite()), "pitch {pitch}");
            assert!(out.iter().any(|x| x.abs() > 0.01), "pitch {pitch} silent");
        }
    }

    #[test]
    fn stuck_or_bad_peaks_fall_back_to_finite_values() {
        assert_eq!(decayed_peak(f32::INFINITY, 0.5), 0.0);
        assert_eq!(decayed_peak(0.2, f32::INFINITY), 0.0);
        assert_eq!(
            decayed_peak(f32::NAN, 0.5),
            0.5 * 0.85,
            "NaN is ignored by max"
        );
        assert_eq!(decayed_peak(0.0, 1.0), 0.85, "decays");
        assert_eq!(finite_or_zero(f32::NAN), 0.0);
        assert_eq!(finite_or_zero(-0.25), -0.25);
    }

    #[test]
    fn the_stability_limit_holds_for_every_resonance() {
        for r in [0.0, 0.05, 0.3, 1.0] {
            let d = Svf::damping(r);
            let f = Svf::max_coefficient(d);
            assert!(f * f + 2.0 * f * d < 4.0, "resonance {r}");
            assert!(f > 0.9, "the limit leaves the filter bright: {f}");
        }
    }

    #[test]
    fn a_dark_filtered_saw_is_smoother_than_a_bright_one() {
        use crate::synth::Wave;
        let base = SynthPatch {
            wave_a: Wave::Saw,
            level_b: 0.0,
            ..SynthPatch::default()
        };
        let dark = SynthPatch {
            cutoff: 400.0,
            ..base
        };
        let rough = |x: &[f32]| {
            x.windows(3)
                .map(|w| (w[2] - 2.0 * w[1] + w[0]).abs())
                .sum::<f32>()
        };
        let a = play_note_with(base, 57, 9600);
        let b = play_note_with(dark, 57, 9600);
        assert!(rough(&b[2400..]) < rough(&a[2400..]) * 0.6);
        assert!(b.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn drum_kit_notes_sound_different_and_decay() {
        let kit = SynthPatch {
            kit: Kit::Acoustic,
            ..SynthPatch::default()
        };
        let kick = play_note_with(kit, 36, 24_000);
        let hat = play_note_with(kit, 42, 24_000);
        assert!(kick.iter().any(|x| x.abs() > 0.05), "kick silent");
        assert!(hat.iter().any(|x| x.abs() > 0.02), "hat silent");
        // Both are short: the last quarter second is quiet.
        let tail = |x: &[f32]| x[18_000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(tail(&kick) < 0.02 && tail(&hat) < 0.02);
        assert!(kick != hat);
    }

    /// Roadmap §5 acceptance: zero allocations/frees on the audio thread.
    #[test]
    fn process_does_not_allocate() {
        let (mut e, mut tx, _s) = rig();
        let notes = (0..64)
            .map(|i| note(i as f64 * 0.25, i as f64 * 0.25 + 0.5))
            .collect();
        let song = Song::new(notes, vec![TrackMix::default()]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        let mut buf = vec![0.0f32; 512 * 2];
        let cmds = [
            Command::SetMetronome { on: true },
            Command::SetCycle {
                on: true,
                start_beat: 0.0,
                end_beat: 4.0,
            },
            Command::SetTestTone {
                hz: 440.0,
                gain: 0.1,
            },
            Command::SetTrackMix {
                track: 0,
                mix: TrackMix {
                    gain_db: -6.0,
                    mute: false,
                    solo: true,
                    pan: 0.0,
                },
            },
            Command::SetTempoMap(Box::new(TempoMap::constant(140.0, (3, 4)))),
            Command::NoteOn {
                pitch: 64,
                vel: 90,
                track: 0,
            },
            Command::SetTrackPatch {
                track: 0,
                patch: SynthPatch {
                    kit: Kit::Acoustic,
                    ..SynthPatch::default()
                },
            },
            Command::NoteOn {
                pitch: 42,
                vel: 90,
                track: 0,
            },
            Command::Play,
        ];
        for c in cmds {
            assert!(tx.push(c).is_ok());
        }
        for i in 0..400 {
            if i == 200 {
                assert!(tx.push(Command::SetPosition { beat: 2.0 }).is_ok());
            }
            assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
        }
        assert!(tx.push(Command::Stop).is_ok());
        assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
        assert!(buf.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn cycle_loops_back() {
        let (mut e, mut tx, mut s) = rig();
        assert!(tx
            .push(Command::SetCycle {
                on: true,
                start_beat: 0.0,
                end_beat: 1.0
            })
            .is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 48_000 * 2];
        e.process(&mut buf);
        assert!(s.read().position_beats < 1.0);
    }

    #[test]
    fn midi_note_does_not_start_one_sample_early() {
        let (mut e, mut tx, _s) = rig();
        let song = Song::new(vec![note(1.0 / 24_000.0, 1.0)], vec![TrackMix::default()]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut frame = [0.0; 2];
        e.process(&mut frame);
        assert_eq!(frame, [0.0; 2]);
        assert_eq!(e.next_note, 0);
        e.process(&mut frame);
        assert_eq!(e.next_note, 1);
        assert!(frame[0].abs() > 0.0);
    }

    #[test]
    fn fractional_cycle_wrap_retriggers_note_at_cycle_start() {
        let (mut e, mut tx, _s) = rig();
        let song = Song::new(vec![note(0.0, 1.0)], vec![TrackMix::default()]);
        for cmd in [
            Command::SetSong(Box::new(song)),
            Command::SetCycle {
                on: true,
                start_beat: 0.0,
                end_beat: 2.5 / 24_000.0,
            },
            Command::Play,
        ] {
            assert!(tx.push(cmd).is_ok());
        }
        e.process(&mut [0.0; 6]);
        assert!(e.beat > 0.0, "fractional remainder preserved");
        assert_eq!(e.next_note, 0, "cycle-start note must not be skipped");
        let mut frame = [0.0; 2];
        e.process(&mut frame);
        assert_eq!(e.next_note, 1);
        assert!(frame[0].abs() > 0.0);
    }

    #[test]
    fn invalid_cycle_bounds_are_ignored() {
        let (mut e, mut tx, _s) = rig();
        for (start, end) in [
            (0.0, f64::INFINITY),
            (f64::NAN, 1.0),
            (-1.0, 1.0),
            (1.0, 1.0),
        ] {
            assert!(tx
                .push(Command::SetCycle {
                    on: true,
                    start_beat: start,
                    end_beat: end
                })
                .is_ok());
            e.process(&mut []);
            assert!(e.cycle.is_none());
        }
    }

    #[test]
    fn dsp_load_is_busy_time_over_buffer_time() {
        use std::time::Duration;
        // 256 frames at 48 kHz play for 5.333 ms; 1.333 ms of work is a quarter of that.
        let load = dsp_load(Duration::from_micros(1_333), 256, 48_000);
        assert!((load - 0.25).abs() < 0.001, "{load}");
        assert_eq!(dsp_load(Duration::from_millis(1), 0, 48_000), 0.0);
    }

    #[test]
    fn dsp_load_meter_keeps_the_peak_until_read() {
        let s = Shared::new();
        for load in [0.2, 0.7, 0.3] {
            assert_no_alloc::assert_no_alloc(|| s.record_dsp_load(load));
        }
        assert_eq!(s.take_dsp_load(), 0.7);
        assert_eq!(s.take_dsp_load(), 0.0);
        s.record_dsp_load(1.5);
        assert_eq!(s.take_dsp_load(), 1.5, "overloads above 100% are reported");
    }

    #[test]
    fn transport_follows_tempo_changes() {
        use crate::tempo_map::TempoEvent;
        let (mut e, mut tx, mut s) = rig();
        // 120 BPM for the first beat (0.5 s), then 60 BPM.
        let map = TempoMap::new(
            vec![
                TempoEvent {
                    beat: 0.0,
                    bpm: 120.0,
                    curve: crate::tempo_map::TempoCurve::Step,
                },
                TempoEvent {
                    beat: 1.0,
                    bpm: 60.0,
                    curve: crate::tempo_map::TempoCurve::Step,
                },
            ],
            vec![],
        );
        assert!(tx.push(Command::SetTempoMap(Box::new(map))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 48_000 * 2];
        e.process(&mut buf);
        assert!(
            (s.read().position_beats - 1.5).abs() < 1e-6,
            "{}",
            s.read().position_beats
        );
    }

    #[test]
    fn metronome_accents_the_first_beat_of_each_bar_in_three_four() {
        let (mut e, mut tx, _s) = rig();
        let map = TempoMap::constant(120.0, (3, 4));
        assert!(tx.push(Command::SetTempoMap(Box::new(map))).is_ok());
        assert!(tx.push(Command::SetMetronome { on: true }).is_ok());
        let mut buf = vec![0.0f32; 64 * 2];
        let mut click_at = |e: &mut EngineCore, tx: &mut Producer<Command>, beat: f64| {
            assert!(tx.push(Command::SetPosition { beat: beat - 0.001 }).is_ok());
            assert!(tx.push(Command::Play).is_ok());
            e.process(&mut buf);
            assert!(e.click_remaining > 0, "no click at beat {beat}");
            e.click_freq
        };
        assert_eq!(click_at(&mut e, &mut tx, 2.0), 1000.0);
        assert_eq!(
            click_at(&mut e, &mut tx, 3.0),
            1500.0,
            "bar 2 starts at beat 3"
        );
        assert_eq!(click_at(&mut e, &mut tx, 4.0), 1000.0, "not a 4/4 downbeat");
    }

    #[test]
    fn set_notes_swaps_notes_without_cutting_voices() {
        let (mut e, mut tx, _s) = rig();
        let song = Song::new(vec![note(0.0, 8.0)], vec![TrackMix::default()]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 4_800 * 2];
        e.process(&mut buf);
        assert_eq!(e.voices.iter().filter(|v| v.active).count(), 1);
        // Unsorted on purpose: `set_notes` sorts.
        let notes = vec![note(1.0, 2.0), note(0.05, 0.5), note(0.5, 1.0)];
        assert!(tx.push(Command::set_notes(notes)).is_ok());
        assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
        assert_eq!(e.song.notes.len(), 3);
        assert!(e
            .song
            .notes
            .windows(2)
            .all(|w| w[0].start_beat <= w[1].start_beat));
        // The held note survives; the next note to play is the one at beat 0.5.
        assert!(e.voices.iter().any(|v| v.active && v.end_beat == 8.0));
        assert_eq!(e.song.notes[e.next_note].start_beat, 0.5);
    }

    #[test]
    fn solo_plays_only_soloed_tracks_until_released() {
        let (mut e, mut tx, mut s) = rig();
        let on = |track| NoteEvent {
            start_beat: 0.0,
            end_beat: 8.0,
            pitch: 60,
            vel: 100,
            track,
        };
        let soloed = TrackMix {
            solo: true,
            ..TrackMix::default()
        };
        let song = Song::new(vec![on(0), on(1)], vec![TrackMix::default(), soloed]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let mut buf = vec![0.0f32; 4_800 * 2];
        e.process(&mut buf);
        assert_eq!(s.read().track_peaks(0), (0.0, 0.0), "not soloed: silent");
        assert!(s.read().track_peaks(1).0 > 0.0, "soloed: audible");

        // Releasing solo brings the other track back.
        let mix = TrackMix::default();
        assert!(tx.push(Command::SetTrackMix { track: 1, mix }).is_ok());
        e.process(&mut buf);
        assert!(s.read().track_peaks(0).0 > 0.0 && s.read().track_peaks(1).0 > 0.0);
    }

    fn two_tracks_song(notes: Vec<NoteEvent>) -> Box<Song> {
        Box::new(Song::new(
            notes,
            vec![TrackMix::default(), TrackMix::default()],
        ))
    }

    fn strip_note(track: u16, end: f64) -> NoteEvent {
        NoteEvent {
            track,
            ..note(0.0, end)
        }
    }

    fn render(e: &mut EngineCore, secs: f32) -> Vec<f32> {
        let mut buf = vec![0.0f32; (secs * 48_000.0) as usize * 2];
        for chunk in buf.chunks_mut(512 * 2) {
            e.process(chunk);
        }
        buf
    }

    fn energy(buf: &[f32], from: f32, to: f32) -> f32 {
        let (a, b) = ((from * 48_000.0) as usize * 2, (to * 48_000.0) as usize * 2);
        buf[a..b.min(buf.len())].iter().map(|x| x * x).sum()
    }

    fn effect(kind: crate::fx::FxKind, set: &[(usize, f32)]) -> Box<Chain> {
        let mut p = kind.defaults();
        for (i, v) in set {
            p[*i] = *v;
        }
        Box::new(Chain::new(vec![crate::fx::Fx::new(
            kind, p, false, 48_000.0,
        )]))
    }

    #[test]
    fn an_effect_on_a_strip_changes_what_is_heard() {
        use crate::fx::FxKind;
        let play = |chain: Option<Box<Chain>>| {
            let (mut e, mut tx, _s) = rig();
            assert!(tx
                .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 0.25)])))
                .is_ok());
            if let Some(chain) = chain {
                assert!(tx.push(Command::SetInserts { track: 0, chain }).is_ok());
            }
            assert!(tx.push(Command::Play).is_ok());
            render(&mut e, 0.8)
        };
        let dry = play(None);
        // A 150 ms delay, all wet: the note comes back after it ended (0.125 s at 120 BPM).
        let wet = play(Some(effect(
            FxKind::Delay,
            &[(0, 150.0), (1, 0.0), (4, 1.0)],
        )));
        assert!(
            energy(&dry, 0.45, 0.8) < 1e-6,
            "{}",
            energy(&dry, 0.45, 0.8)
        );
        assert!(energy(&wet, 0.2, 0.5) > 0.05, "{}", energy(&wet, 0.2, 0.5));
        assert!(wet.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn a_send_feeds_the_aux_strip_that_listens_to_the_bus() {
        use crate::fx::FxKind;
        let play = |send_pre: bool, aux_muted: bool, level: f32| {
            let (mut e, mut tx, mut s) = rig();
            let mut mixes = vec![TrackMix::default(), TrackMix::default()];
            // The note's own fader is all the way down: only a pre-fader send is heard.
            mixes[0].gain_db = -96.0;
            mixes[1].mute = aux_muted;
            let song = Song::new(vec![strip_note(0, 2.0)], mixes);
            assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
            let mut route = Route::default();
            route.sends[0] = SendSpec {
                bus: 1,
                level,
                pre: send_pre,
            };
            assert!(tx.push(Command::SetRoute { track: 0, route }).is_ok());
            let aux = Route {
                bus_in: 1,
                ..Route::default()
            };
            assert!(tx
                .push(Command::SetRoute {
                    track: 1,
                    route: aux
                })
                .is_ok());
            assert!(tx
                .push(Command::SetInserts {
                    track: 1,
                    chain: effect(FxKind::ChannelEq, &[]),
                })
                .is_ok());
            assert!(tx.push(Command::Play).is_ok());
            let buf = render(&mut e, 0.3);
            let (a0l, a0r) = s.read().track_peaks(0);
            let (a1l, a1r) = s.read().track_peaks(1);
            (energy(&buf, 0.0, 0.3), a0l.max(a0r), a1l.max(a1r))
        };
        let (pre, strip, aux) = play(true, false, 0.7);
        assert!(
            pre > 0.01 && strip < 1e-3 && aux > 0.05,
            "{pre} {strip} {aux}"
        );
        // After the fader the send carries nothing (the fader is down).
        let (post, _, aux_post) = play(false, false, 0.7);
        assert!(post < 1e-5 && aux_post < 1e-4, "{post} {aux_post}");
        // A muted Aux strip is silent; a send at zero sends nothing.
        assert!(play(true, true, 0.7).0 < 1e-5);
        assert!(play(true, false, 0.0).0 < 1e-5);
        // The send level counts: half the knob is about 6 dB under the top.
        let top = play(true, false, 1.0).0;
        let half = play(true, false, 0.5).0;
        assert!(half < top * 0.2 && half > top * 0.01, "{half} {top}");
    }

    #[test]
    fn automation_plays_the_fader_and_stops_when_it_is_not_followed() {
        let level = |lane: Option<(bool, f32, f32)>| {
            let (mut e, mut tx, _s) = rig();
            assert!(tx
                .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 4.0)])))
                .is_ok());
            if let Some((follow, db, offset)) = lane {
                let lane = AutoLane {
                    track: 0,
                    target: AutoTarget::Volume,
                    follow,
                    offset_db: offset,
                    points: vec![AutoPoint::new(0.0, db), AutoPoint::new(100.0, db)],
                };
                assert!(tx.push(Command::SetAutomation(vec![lane])).is_ok());
            }
            assert!(tx.push(Command::Play).is_ok());
            let buf = render(&mut e, 0.4);
            energy(&buf, 0.2, 0.4)
        };
        let plain = level(None);
        let quiet = level(Some((true, -20.0, 0.0)));
        let ignored = level(Some((false, -20.0, 0.0)));
        // -20 dB is a tenth of the level, a hundredth of the energy.
        assert!((quiet / plain - 0.01).abs() < 0.004, "{}", quiet / plain);
        assert!((ignored / plain - 1.0).abs() < 1e-3);
        // The VCA's level is added to the lane's value.
        let with_vca = level(Some((true, -14.0, -6.0)));
        assert!(
            (with_vca / quiet - 1.0).abs() < 0.03,
            "{}",
            with_vca / quiet
        );
    }

    #[test]
    fn pan_automation_moves_the_sound_and_a_compressor_reports_its_reduction() {
        use crate::fx::FxKind;
        let (mut e, mut tx, mut s) = rig();
        assert!(tx
            .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 4.0)])))
            .is_ok());
        let lane = AutoLane {
            track: 0,
            target: AutoTarget::Pan,
            follow: true,
            offset_db: 0.0,
            points: vec![AutoPoint::new(0.0, 1.0)],
        };
        assert!(tx.push(Command::SetAutomation(vec![lane])).is_ok());
        assert!(tx
            .push(Command::SetInserts {
                track: 0,
                chain: effect(FxKind::Compressor, &[(0, -40.0), (1, 10.0), (2, 1.0)]),
            })
            .is_ok());
        assert!(tx.push(Command::Play).is_ok());
        let buf = render(&mut e, 0.3);
        let left: f32 = buf.iter().step_by(2).skip(2400).map(|x| x * x).sum();
        let right: f32 = buf
            .iter()
            .skip(1)
            .step_by(2)
            .skip(2400)
            .map(|x| x * x)
            .sum();
        assert!(left < right * 1e-4 && right > 0.01, "{left} {right}");
        assert!(
            s.read().gain_reduction(0) > 3.0,
            "{}",
            s.read().gain_reduction(0)
        );
        assert_eq!(s.read().gain_reduction(1), 0.0);
        // Taking the effects away clears the meter.
        assert!(tx
            .push(Command::SetInserts {
                track: 0,
                chain: Box::default(),
            })
            .is_ok());
        render(&mut e, 0.05);
        assert_eq!(s.read().gain_reduction(0), 0.0);
    }

    #[test]
    fn effects_sends_and_automation_do_not_allocate_and_survive_bad_commands() {
        use crate::fx::FxKind;
        let (mut e, mut tx, _s) = rig();
        assert!(tx
            .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 8.0)])))
            .is_ok());
        let aux = Route {
            bus_in: 2,
            ..Route::default()
        };
        let mut route = Route::default();
        route.sends[0] = SendSpec {
            bus: 2,
            level: 0.8,
            pre: false,
        };
        let lane = AutoLane {
            track: 0,
            target: AutoTarget::Volume,
            follow: true,
            offset_db: 0.0,
            points: vec![AutoPoint::new(0.0, 0.0), AutoPoint::new(4.0, -12.0)],
        };
        let cmds = vec![
            Command::SetInserts {
                track: 0,
                chain: effect(FxKind::Reverb, &[]),
            },
            Command::SetInserts {
                track: 1,
                chain: effect(FxKind::Phaser, &[]),
            },
            Command::SetRoute { track: 0, route },
            Command::SetRoute {
                track: 1,
                route: aux,
            },
            Command::SetAutomation(vec![lane.clone()]),
            Command::SetFx {
                track: 0,
                slot: 0,
                params: [f32::NAN; MAX_FX_PARAMS],
                bypass: false,
            },
            Command::SetFx {
                track: 700,
                slot: 9,
                params: [1.0; MAX_FX_PARAMS],
                bypass: true,
            },
            Command::SetRoute {
                track: 5000,
                route: Route::default(),
            },
            Command::SetAutomation(vec![AutoLane {
                track: 4000,
                ..lane
            }]),
            Command::Play,
        ];
        for c in cmds {
            assert!(tx.push(c).is_ok());
        }
        let mut buf = vec![0.0f32; 512 * 2];
        for _ in 0..300 {
            assert_no_alloc::assert_no_alloc(|| e.process(&mut buf));
            assert!(buf.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        }
    }

    #[test]
    fn an_effect_parameter_follows_its_lane_and_goes_back_when_the_lane_stops() {
        use crate::fx::FxKind;
        // A Channel EQ with its mid band at +12 dB: the lane (position 0 = -18 dB) pulls it
        // down while it is played.
        let level = |lane: Option<bool>| {
            let (mut e, mut tx, _s) = rig();
            assert!(tx
                .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 4.0)])))
                .is_ok());
            let chain = effect(FxKind::MultiEq, &[(5, 440.0), (6, 12.0), (7, 0.5)]);
            assert!(tx.push(Command::SetInserts { track: 0, chain }).is_ok());
            if let Some(follow) = lane {
                let lane = AutoLane {
                    track: 0,
                    target: AutoTarget::Fx { slot: 0, param: 6 },
                    follow,
                    offset_db: 0.0,
                    points: vec![AutoPoint::new(0.0, 0.0)],
                };
                assert!(tx.push(Command::SetAutomation(vec![lane])).is_ok());
            }
            assert!(tx.push(Command::Play).is_ok());
            energy(&render(&mut e, 0.4), 0.2, 0.4)
        };
        let plain = level(None);
        let played = level(Some(true));
        let ignored = level(Some(false));
        assert!(played < plain * 0.5, "{played} {plain}");
        assert!((ignored / plain - 1.0).abs() < 1e-3, "{ignored} {plain}");

        // Stopping the lane puts the user's value back.
        let (mut e, mut tx, _s) = rig();
        assert!(tx
            .push(Command::SetSong(two_tracks_song(vec![strip_note(0, 8.0)])))
            .is_ok());
        assert!(tx
            .push(Command::SetInserts {
                track: 0,
                chain: effect(FxKind::MultiEq, &[(6, 12.0)]),
            })
            .is_ok());
        let mut lane = AutoLane {
            track: 0,
            target: AutoTarget::Fx { slot: 0, param: 6 },
            follow: true,
            offset_db: 0.0,
            points: vec![AutoPoint::new(0.0, 0.0)],
        };
        assert!(tx.push(Command::SetAutomation(vec![lane.clone()])).is_ok());
        assert!(tx.push(Command::Play).is_ok());
        render(&mut e, 0.1);
        let moved = e.chains[0].as_ref().map(|c| c.fx[0].current()[6]);
        assert_eq!(moved, Some(-18.0));
        lane.follow = false;
        assert!(tx.push(Command::SetAutomation(vec![lane])).is_ok());
        render(&mut e, 0.1);
        let back = e.chains[0].as_ref().map(|c| c.fx[0].current()[6]);
        assert_eq!(back, Some(12.0));
    }

    #[test]
    fn compensation_delays_a_strip_and_the_sum_keeps_quiet_signals() {
        // A strip delayed by 100 samples before its fader and 50 after.
        let (mut e, mut tx, _s) = rig();
        assert!(tx.push(Command::SetSong(two_tracks_song(vec![]))).is_ok());
        assert!(tx
            .push(Command::SetPdc {
                track: 0,
                pdc: Box::new(Pdc::new(100, 50)),
            })
            .is_ok());
        e.drain_commands();
        let mut first = None;
        for i in 0..400 {
            if i == 10 {
                e.in_l[0] = 0.5;
                e.in_r[0] = -0.5;
                e.touched[0] = true;
                e.dirty.push(0);
            }
            let (l, r) = e.mix_strips();
            if first.is_none() && l != 0.0 {
                assert!((l - 0.5).abs() < 1e-6 && (r + 0.5).abs() < 1e-6, "{l} {r}");
                first = Some(i);
            }
        }
        assert_eq!(first, Some(10 + 150));

        // 64-bit sums: 1000 and -1000 cancel and what was between them is still there (in
        // 32 bits the 0.00001 would be lost: the spacing of numbers near 1000 is 0.00006).
        let (mut e, _tx, _s) = rig();
        for (t, v) in [(0usize, 1000.0f64), (1, 0.00001), (2, -1000.0)] {
            e.in_l[t] = v;
            e.touched[t] = true;
            e.dirty.push(t as u16);
        }
        let (l, _) = e.mix_strips();
        assert!((l - 0.00001).abs() < 1e-12, "{l}");
        assert!(e.dirty.is_empty() && e.in_l[0] == 0.0);
        // Garbage: the old compensation comes back through the queue, and a track past the
        // end is ignored.
        let (mut e, mut tx, _s) = rig();
        for pdc in [Pdc::new(1, 1), Pdc::new(2, 2)] {
            assert!(tx
                .push(Command::SetPdc {
                    track: 3,
                    pdc: Box::new(pdc)
                })
                .is_ok());
        }
        assert!(tx
            .push(Command::SetPdc {
                track: 9000,
                pdc: Box::new(Pdc::new(5, 5))
            })
            .is_ok());
        render(&mut e, 0.01);
        assert_eq!(e.pdc[3].as_ref().map(|p| p.lengths()), Some((2, 2)));
    }
}
