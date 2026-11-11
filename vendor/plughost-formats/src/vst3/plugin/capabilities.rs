use plughost_core::{Capabilities, Support};
use vst3::Steinberg::Vst::{IUnitInfo, IUnitInfoTrait};

use super::Plugin;
use super::Vst3Error;

fn count_support(count: i32) -> Support {
    if count < 0 {
        Support::Unknown
    } else {
        (count > 0).into()
    }
}

impl Plugin {
    /// Queries native interfaces without creating an editor or serializing state.
    pub(crate) fn capabilities(&self) -> Result<Capabilities, Vst3Error> {
        let automation = self
            .parameter_cache()
            .infos()
            .iter()
            .any(|info| info.flags.automatable)
            .into();
        let queried = self.queried()?;
        let factory_presets = self
            .controller
            .cast::<IUnitInfo>()
            .map_or(Support::Unsupported, |units| {
                count_support(unsafe { units.getProgramListCount() })
            });
        Ok({
            let mut capabilities = Capabilities::default();
            capabilities.embedded_editor = if self.editor.is_some() {
                Support::Supported
            } else {
                Support::Unknown
            };
            capabilities.bus_discovery = Support::Supported;
            capabilities.note_input = queried.note_input;
            capabilities.note_output = queried.note_output;
            capabilities.sample_accurate_automation = automation;
            // IComponent has state methods but no query establishing that the plugin implements them.
            capabilities.state = Support::Unknown;
            capabilities.factory_presets = factory_presets;
            capabilities.f32 = queried.f32;
            capabilities.f64 = queried.f64;
            capabilities
        })
    }
}
