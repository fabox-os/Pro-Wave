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
mod core;
mod disk;
mod flex;
mod gain_smoothing;
#[cfg(not(target_arch = "wasm32"))]
mod mapped;
mod mtc;
mod output;
mod synth;
mod tempo_map;
mod triple;
#[cfg(windows)]
mod wasapi;

pub use crate::analysis::{tempo_curve, Analysis, Analyzer, TempoEstimate, STRENGTH_RATE};
pub use crate::core::{
    dsp_load, AudioClip, ClipEdit, Command, ControlEvent, EngineCore, EngineState, FadeCurve,
    Garbage, MidiOutEvent, NoteEvent, Shared, Song, StateReader, TrackMix, MAX_TRACKS,
};
pub use crate::disk::{
    ring_blocks_for, AudioError, AudioFile, DiskThread, RING_BLOCKS, RING_BLOCKS_MAX,
};
pub use crate::flex::{
    detect_notes, hz_to_midi, pitch_track, shift_pitch, warp, write_wav, DetectedNote, FlexMode,
    PitchShift, WarpPoint,
};
pub use crate::mtc::{
    full_frame, quarter_frame_data, rate_code, rate_of_code, MtcDecoder, MtcEvent, MtcGenerator,
    MtcMessage,
};
pub use crate::output::{peak_of, Engine, EngineError};
pub use crate::synth::{Kit, SynthPatch, Wave};
pub use crate::tempo_map::{
    MusicalPosition, SignatureEvent, Smpte, SmpteRate, TempoCurve, TempoEvent, TempoMap, MAX_BPM,
    MIN_BPM,
};
