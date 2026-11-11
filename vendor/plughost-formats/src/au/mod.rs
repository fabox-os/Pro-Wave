//! Audio Unit hosting (macOS), through `AUAudioUnit` for both v2 and v3 units.

mod buffers;
mod components;
mod errors;
mod plist;
mod plugin;
mod wait;

pub use components::components;
pub(crate) use errors::AuError;
pub use plist::{from_preset, inspect_preset, to_preset};
pub use plugin::{Plugin, Processor};
