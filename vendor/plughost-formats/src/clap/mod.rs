//! CLAP hosting, through clack-host.

mod entry;
mod errors;
mod host;
mod plugin;

pub use entry::Entry;
pub(crate) use errors::ClapError;
pub use plugin::{Plugin, Processor};

mod transport;

mod discovery;
pub use discovery::{discover_presets, preset_providers};
