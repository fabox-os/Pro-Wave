//! A loaded `.clap` module, which classes, plugins and preset providers are read from.
use std::path::{Path, PathBuf};

use clack_host::prelude::PluginEntry;
use plughost_core::PluginInfo;

use super::errors::ClapError;

/// A loaded `.clap` module. Its entry stays initialized while it or a plugin created from it
/// lives, and clones share it.
#[derive(Clone)]
pub struct Entry {
    entry: PluginEntry,
    path: PathBuf,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Entry {
    /// Loads and initializes the `.clap` bundle at `bundle`.
    ///
    /// # Safety
    ///
    /// Loading runs the module's own code in this process, with everything the process may do,
    /// and nothing checks that code: one that is not a sound CLAP plugin can corrupt or end the
    /// process. Load only modules trusted to behave, or load them in a process of their own, as
    /// `plughost`'s helper does.
    pub unsafe fn load(bundle: &Path) -> Result<Entry, crate::Error> {
        // SAFETY: the caller accepts running the module's code.
        let entry = unsafe { PluginEntry::load(bundle) }.map_err(ClapError::Load)?;
        Ok(Entry {
            entry,
            path: bundle.to_path_buf(),
        })
    }

    /// The bundle the module was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The plugin classes the module's factory lists.
    pub fn classes(&self) -> Result<Vec<PluginInfo>, crate::Error> {
        let factory = self
            .entry
            .get_plugin_factory()
            .ok_or(ClapError::NoFactory)?;
        Ok(factory
            .plugin_descriptors()
            .filter_map(super::plugin::info)
            .collect())
    }

    pub(super) fn native(&self) -> &PluginEntry {
        &self.entry
    }
}
