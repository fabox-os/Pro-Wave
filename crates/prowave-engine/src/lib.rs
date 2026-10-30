//! ProWave Studio real-time engine.
//!
//! Logic Pro equivalent: transport + software-instrument playback + metronome (Klopfgeist).
//! The UI talks to the engine only through [`Command`]s on a lock-free SPSC queue and reads
//! state back through a lock-free triple buffer ([`StateReader`]) and atomics ([`Shared`]); the audio callback never locks, allocates or does I/O.
#![deny(clippy::unwrap_used, clippy::expect_used)]

// Debug test builds abort if anything inside `assert_no_alloc(..)` allocates or frees.
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod analysis;
#[cfg(not(target_arch = "wasm32"))]
mod capture;
mod core;
mod disk;
mod flex;
mod fx;
mod gain_smoothing;
#[cfg(not(target_arch = "wasm32"))]
mod mapped;
mod monitor;
mod mtc;
mod output;
mod routing;
mod synth;
mod tempo_map;
mod triple;
#[cfg(windows)]
mod wasapi;

pub use crate::analysis::{tempo_curve, Analysis, Analyzer, TempoEstimate, STRENGTH_RATE};
#[cfg(not(target_arch = "wasm32"))]
pub use crate::capture::{
    input_devices, Capture, CaptureError, InputInfo, InputSource, Recorded, MAX_INPUT_CHANNELS,
};
pub use crate::core::{
    dsp_load, AudioClip, ClipEdit, Command, ControlEvent, EngineCore, EngineState, FadeCurve,
    Garbage, MidiOutEvent, NoteEvent, Shared, Song, StateReader, TrackMix, MAX_TRACKS,
};
pub use crate::disk::{
    ring_blocks_for, AudioError, AudioFile, DiskThread, RING_BLOCKS, RING_BLOCKS_MAX,
};
pub use crate::flex::{
    detect_notes, hz_to_midi, pitch_track, shift_pitch, warp, write_wav, DetectedNote, FlexMode,
    PitchShift, WarpPoint, MAX_POINTS as MAX_FLEX_POINTS,
};
pub use crate::fx::{
    eq_response_db, response_db, Chain, Fx, FxKind, ParamInfo, Pdc, MAX_FX_PARAMS, MAX_INSERTS,
};
pub use crate::monitor::{InputFeed, MonitorTrack, MAX_MONITORS};
pub use crate::mtc::{
    full_frame, quarter_frame_data, rate_code, rate_of_code, MtcDecoder, MtcEvent, MtcGenerator,
    MtcMessage,
};
pub use crate::output::{peak_of, Engine, EngineError};
pub use crate::routing::{
    curve_shape, lane_value, send_gain, AutoLane, AutoPoint, AutoTarget, Route, SendSpec,
    MAX_AUTO_POINTS, MAX_BUSES, MAX_SENDS,
};
pub use crate::synth::{Kit, SynthPatch, Wave};
pub use crate::tempo_map::{
    MusicalPosition, SignatureEvent, Smpte, SmpteRate, TempoCurve, TempoEvent, TempoMap, MAX_BPM,
    MIN_BPM,
};
