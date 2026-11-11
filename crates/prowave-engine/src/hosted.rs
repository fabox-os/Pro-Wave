//! A plug-in hosted outside the engine's built-in effects (Pro Wave's VST3, Audio Unit and
//! CLAP support).
//!
//! The plug-in itself runs in a helper process; the engine only sees this per-sample adapter,
//! which batches the samples into the helper's quanta. Implementations are created on the UI
//! thread (where loading a plug-in is allowed) and moved into the engine through
//! [`crate::Command::SetMasterHosted`].
//!
//! Audio-thread contract: [`HostedEffect::process`] is called once per output sample and must
//! not allocate, lock, block or do I/O. The host adapter reserves all of its buffers before it
//! is handed over.

/// One hosted effect in the audio graph.
pub trait HostedEffect: Send {
    /// Processes one stereo sample. Called on the audio thread.
    fn process(&mut self, left: f32, right: f32) -> (f32, f32);

    /// The plug-in's latency plus the adapter's buffering, in samples at the engine rate.
    fn latency_samples(&self) -> usize;

    /// Passes audio through unchanged while leaving the plug-in loaded (Logic: Bypass).
    fn set_bypass(&mut self, bypass: bool);

    /// Automation for parameter `param` (index in the host's parameter list), normalized to
    /// 0..=1. Called on the audio thread at the automation rate.
    fn set_param_norm(&mut self, param: usize, value: f32);
}

/// One hosted instrument in the audio graph (a VST3, Audio Unit or CLAP synth).
///
/// Created on the UI thread and moved into the engine through
/// [`crate::Command::SetTrackHostedInstrument`]. The plug-in itself runs in a helper process;
/// this adapter batches note events and output samples into its quanta.
///
/// Audio-thread contract: [`HostedInstrument::process`], the note methods and
/// [`HostedInstrument::all_notes_off`] are called on the audio thread and must not allocate,
/// lock, block or do I/O. The host adapter reserves its buffers and event room before it is
/// handed over.
pub trait HostedInstrument: Send {
    /// Produces one stereo sample. Called on the audio thread.
    fn process(&mut self) -> (f32, f32);

    /// Starts a note (channel 0-based, velocity 1..=127). Called on the audio thread; the
    /// adapter places it at the current sample inside its helper quantum.
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8);

    /// Ends a note (channel 0-based).
    fn note_off(&mut self, channel: u8, key: u8);

    /// Ends every note it started (transport stop, seek, instrument replaced).
    fn all_notes_off(&mut self);

    /// The plug-in's latency plus the adapter's buffering, in samples at the engine rate.
    fn latency_samples(&self) -> usize;

    /// Automation for parameter `param` (index in the host's parameter list), normalized to
    /// 0..=1.
    fn set_param_norm(&mut self, param: usize, value: f32);

    /// Leaves the plug-in loaded but silent (Logic: Bypass).
    fn set_bypass(&mut self, bypass: bool);
}
