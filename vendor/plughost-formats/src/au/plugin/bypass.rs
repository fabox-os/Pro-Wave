use objc2_audio_toolbox::{
    AUAudioUnit, AUAudioUnitV2Bridge, AudioUnitGetPropertyInfo, kAudioUnitProperty_BypassEffect,
    kAudioUnitScope_Global,
};
use plughost_core::{BypassState, PluginKind, Support};

use super::{AuError, Plugin};

fn support(unit: &AUAudioUnit, kind: PluginKind) -> Support {
    if kind != PluginKind::Effect {
        return Support::Unsupported;
    }
    let Some(bridge) = unit.downcast_ref::<AUAudioUnitV2Bridge>() else {
        // AUv3 exposes the property but has no independent bypass capability flag.
        return Support::Unknown;
    };
    let mut bytes = 0;
    let mut writable = 0;
    let status = unsafe {
        AudioUnitGetPropertyInfo(
            bridge.audioUnit(),
            kAudioUnitProperty_BypassEffect,
            kAudioUnitScope_Global,
            0,
            &mut bytes,
            &mut writable,
        )
    };
    (status == 0 && writable != 0 && bytes == size_of::<u32>() as u32).into()
}

impl Plugin {
    pub(crate) fn bypass(&self) -> Result<BypassState, AuError> {
        let unit = self.shared_unit()?;
        let support = support(&unit, self.info.kind);
        Ok(BypassState::new(
            support,
            (support != Support::Unsupported).then(|| unsafe { unit.shouldBypassEffect() }),
        ))
    }

    /// Sets native bypass without replacing state or overriding native latency/tail reports.
    pub(crate) fn set_bypass(&mut self, enabled: bool) -> Result<(), AuError> {
        let unit = self.shared_unit()?;
        if support(&unit, self.info.kind) == Support::Unsupported {
            return Err(AuError::BypassUnsupported);
        }
        unsafe { unit.setShouldBypassEffect(enabled) };
        if unsafe { unit.shouldBypassEffect() } != enabled {
            return Err(AuError::BypassUnsupported);
        }
        Ok(())
    }
}
