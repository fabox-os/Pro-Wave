//! Native preset containers. File I/O belongs to the caller; CLAP has no common state file codec.
use crate::errors::{Error, Repr};
use plughost_core::{InputError, MAX_PRESET_BYTES, PluginFormat, PluginState, PresetInfo};

/// Validated native preset identity and opaque processor/controller payloads.
#[non_exhaustive]
pub struct DecodedPreset {
    /// Plugin class identity and format-specific container metadata.
    pub info: PresetInfo,
    /// Processor state, or the format's single state blob.
    pub component: Vec<u8>,
    /// VST3 controller state, when the preset has one; `None` for formats with one state blob.
    pub controller: Option<Vec<u8>>,
}

/// Fails with an unsupported error unless this build can encode and decode the format's native
/// preset files.
pub fn supported(format: PluginFormat) -> Result<(), Error> {
    match format {
        #[cfg(feature = "vst3")]
        PluginFormat::Vst3 => Ok(()),
        #[cfg(all(feature = "au", target_os = "macos"))]
        PluginFormat::AudioUnit => Ok(()),
        _ => Err(Error::new(Repr::PresetUnsupported)),
    }
}

/// Validates a bounded native preset container without loading a plugin.
pub fn decode(format: PluginFormat, bytes: &[u8]) -> Result<DecodedPreset, Error> {
    if bytes.len() > MAX_PRESET_BYTES {
        return Err(InputError::PresetSize.into());
    }
    match format {
        #[cfg(feature = "vst3")]
        PluginFormat::Vst3 => {
            let preset = crate::vst3::Preset::from_bytes(bytes)?;
            Ok(DecodedPreset {
                info: PresetInfo::new(
                    preset.class_id,
                    plughost_core::PresetMetadata::Vst3 { info: preset.info },
                ),
                component: preset.component,
                controller: preset.controller,
            })
        }
        #[cfg(all(feature = "au", target_os = "macos"))]
        PluginFormat::AudioUnit => Ok(DecodedPreset {
            info: crate::au::inspect_preset(bytes)?,
            component: crate::au::from_preset(bytes)?,
            controller: None,
        }),
        _ => Err(Error::new(Repr::PresetUnsupported)),
    }
}

/// Encodes current native state. VST3 XML annotations belong to the caller, not plugin state;
/// use `vst3::Preset` when writing caller-owned annotations.
pub fn encode(state: &PluginState) -> Result<Vec<u8>, Error> {
    match state.id().format() {
        #[cfg(feature = "vst3")]
        PluginFormat::Vst3 => bounded(crate::vst3::Preset::from_state(state).to_bytes()?),
        #[cfg(all(feature = "au", target_os = "macos"))]
        PluginFormat::AudioUnit => bounded(crate::au::to_preset(state.component())?),
        _ => Err(Error::new(Repr::PresetUnsupported)),
    }
}

#[cfg(any(feature = "vst3", all(feature = "au", target_os = "macos")))]
fn bounded(bytes: Vec<u8>) -> Result<Vec<u8>, Error> {
    if bytes.len() > MAX_PRESET_BYTES {
        return Err(InputError::PresetSize.into());
    }
    Ok(bytes)
}
