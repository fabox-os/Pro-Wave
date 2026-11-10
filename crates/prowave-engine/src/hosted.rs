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
