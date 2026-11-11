//! Where a plugin's audio-thread functions run when the host calls them between blocks.

#[cfg(any(feature = "vst3", feature = "clap"))]
use std::sync::Arc;

/// Runs the audio-thread calls a plugin needs outside the blocks it processes on the thread that
/// processes it, so the plugin sees them on its audio thread: delivering edits to a prepared VST3
/// processor with a process call without audio, and CLAP `params.flush` while active, `reset`,
/// and `stop_processing` before deactivation. A plugin without one makes these calls on the
/// thread that asks for them, as its audio thread for that call.
pub trait AudioThread: Send + Sync {
    /// Runs `call` once, on the processing thread, or on this thread when that thread has
    /// exited, and returns when it has finished. The processing thread must not wait for this
    /// thread meanwhile.
    fn run(&self, call: &mut (dyn FnMut() + Send));
}

/// The audio thread the owner of a plugin set, if any.
#[cfg(any(feature = "vst3", feature = "clap"))]
#[derive(Clone, Default)]
pub(crate) struct AudioCalls(Option<Arc<dyn AudioThread>>);

#[cfg(any(feature = "vst3", feature = "clap"))]
impl AudioCalls {
    pub fn new(thread: Arc<dyn AudioThread>) -> AudioCalls {
        AudioCalls(Some(thread))
    }

    /// Runs `call` on the audio thread and returns its result.
    pub fn run<R: Send>(&self, call: impl FnOnce() -> R + Send) -> R {
        let Some(thread) = &self.0 else {
            return call();
        };
        let mut call = Some(call);
        let mut result = None;
        thread.run(&mut || {
            if let Some(call) = call.take() {
                result = Some(call());
            }
        });
        match (call, result) {
            // A thread that broke its contract and did not run it leaves it to this one.
            (Some(call), _) => call(),
            (None, Some(result)) => result,
            (None, None) => unreachable!(),
        }
    }
}
