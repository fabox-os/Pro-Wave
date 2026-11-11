//! VST3 hosting.

// VST3 enum constants are i32 on Windows and u32 elsewhere, so a cast needed on one platform is
// a no-op on the other.
#![allow(clippy::unnecessary_cast)]

pub(crate) mod audio;
mod buffers;
mod editor;
mod engine;
mod errors;
mod event_ports;
mod host;
mod input_notes;
mod instance;
mod module;
mod note_expression;
mod parameter_cache;
mod plugin;
mod preset;
mod uid;

pub use engine::Processor;
pub(crate) use errors::Vst3Error;
pub use module::Module;
pub use plugin::Plugin;
pub use preset::Preset;
