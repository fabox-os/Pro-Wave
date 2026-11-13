//! In-process VST3, Audio Unit, and CLAP hosting for plughost.
//!
//! Plugins are loaded into the calling process. Load plugins, change their lifecycle, and use their
//! controllers on one thread, normally the main thread; plugins that show dialogs or editors need
//! that thread to run the OS event loop. Applications that need crash isolation use the plughost
//! crate, which runs these hosts in helper processes.
//!
//! [`load`] creates a plugin of any enabled format from a `PluginRef`; each format's `Plugin` can
//! also be created directly, from a loaded `vst3::Module` or `clap::Entry`, or from an Audio Unit
//! class ID. Loading is the step that runs the plugin's own code, which nothing can check:
//! [`load`], `vst3::Module::load`, `clap::Entry::load` and `au::Plugin::new` are `unsafe fn`, and
//! reading a loaded plugin afterwards is safe. Every plugin is used through [`HostedPlugin`] and
//! processed from another thread through the [`BlockProcessor`] it hands out. Every format fails with one
//! [`Error`], which tells the [`FailureKind`](plughost_core::FailureKind), the format, the native
//! result code and the cause. An owner that processes plugins on a thread of its own gives them an
//! [`AudioThread`], so the audio-thread calls they need between blocks run there too. Plugins
//! process, reset and flush with subnormal numbers flushed to zero (MXCSR FTZ and DAZ on x86-64,
//! FPCR.FZ on AArch64); the calling thread's mode is restored after each call.
//!
//! The [detailed API guide](https://github.com/emulette/plughost/blob/v0.2.3/llms.txt)
//! describes format capabilities and the application-level processing contracts.

#![warn(missing_docs)]

#[cfg(all(feature = "au", target_os = "macos"))]
pub mod au;
mod audio_thread;
#[cfg(feature = "clap")]
pub mod clap;
mod errors;
#[cfg(any(
    feature = "clap",
    feature = "vst3",
    all(feature = "au", target_os = "macos")
))]
mod float_mode;
mod hosted;
mod parameter_choices;
#[cfg(any(
    feature = "clap",
    feature = "vst3",
    all(feature = "au", target_os = "macos")
))]
mod parameter_values;
pub mod preset;
#[cfg(feature = "clap")]
mod state_stream;
#[cfg(all(
    test,
    any(
        feature = "clap",
        feature = "vst3",
        all(feature = "au", target_os = "macos")
    )
))]
mod timing_tests;
#[cfg(feature = "vst3")]
pub mod vst3;

pub use audio_thread::AudioThread;
pub use errors::Error;
pub use hosted::{BlockProcessor, EditorRequest, EditorView, HostedPlugin, ResizeRequest, load};

#[cfg(test)]
mod preset_corpus;
