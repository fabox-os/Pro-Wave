use super::super::discovery::{input_string, location_path, native_location};
use super::{ClapError, Plugin};
use plughost_core::PresetLocation;
use std::sync::atomic::Ordering;
impl Plugin {
    /// Loads a CLAP-native preset. Chain uses a fresh candidate to preserve the live slot on failure.
    pub(crate) fn load_discovered_preset(
        &mut self,
        location: &PresetLocation,
        load_key: Option<&str>,
    ) -> Result<(), ClapError> {
        let path = location_path(location)?;
        let key = load_key.map(input_string).transpose()?;
        let extension = self
            .shared()
            .extensions()
            .preset_load
            .ok_or(ClapError::PresetUnsupported)?;
        self.shared().take_pending();
        self.shared()
            .preset_load_failed
            .store(false, Ordering::Relaxed);
        extension
            .load_from_location(
                &self.instance.plugin_handle(),
                native_location(&path),
                key.as_deref(),
            )
            .map_err(|_| ClapError::PresetLoad)?;
        if self.shared().preset_load_failed.load(Ordering::Relaxed) {
            return Err(ClapError::PresetLoad);
        }
        Ok(())
    }
}
