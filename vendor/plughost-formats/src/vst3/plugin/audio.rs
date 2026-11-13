use super::{Plugin, Vst3Error, lock};
use crate::vst3::audio::Queried;
use plughost_core::{AudioBusInfo, AudioConfig, AudioConfiguration, BypassState, Support};
use vst3::Steinberg::Vst::{IComponentHandlerTrait, IEditControllerTrait};
use vst3::Steinberg::kResultOk;

impl Plugin {
    /// The complete request currently prepared on this native instance.
    pub fn audio_config(&self) -> Option<AudioConfig> {
        lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.audio_config.clone())
    }
    /// Full native bus order. Current activation is only known after host preparation.
    pub(crate) fn audio_buses(&self) -> Result<Vec<AudioBusInfo>, Vst3Error> {
        let instance = {
            let engine = lock(&self.engine);
            if let Some(prepared) = &engine.prepared {
                return Ok(prepared.audio_buses.clone());
            }
            engine.handle()?
        };
        instance.audio_buses()
    }
    /// Event buses in both directions, inputs first; each ID is the bus index. The note
    /// expressions are the controller's, asked now without the engine lock.
    pub(crate) fn event_ports(&self) -> Result<Vec<plughost_core::EventPortInfo>, Vst3Error> {
        let (mut ports, instance) = {
            let engine = lock(&self.engine);
            match &engine.prepared {
                Some(prepared) => (Some(prepared.queried.event_ports.clone()), engine.handle()?),
                None => (None, engine.handle()?),
            }
        };
        let Some(ports) = ports.as_mut() else {
            return instance.event_ports();
        };
        // The cache leaves the input buses' note expressions unknown; a controller-only port's
        // are unsupported.
        for port in ports.iter_mut() {
            if port.direction == plughost_core::AudioDirection::Input
                && port.note_expression == Support::Unknown
            {
                port.note_expression = instance.note_expressions(port.id);
            }
        }
        Ok(std::mem::take(ports))
    }
    /// The queries VST3 allows only while the processor does not process: read now while the
    /// plugin is not prepared, or as it was prepared. Only this thread prepares the plugin.
    pub(crate) fn queried(&self) -> Result<Queried, Vst3Error> {
        let instance = {
            let engine = lock(&self.engine);
            if let Some(prepared) = &engine.prepared {
                return Ok(prepared.queried.clone());
            }
            engine.handle()?
        };
        instance.queried()
    }
    /// VST3 negotiates speaker arrangements but has no complete configuration enumerator.
    pub(crate) fn audio_configurations(&self) -> Result<Vec<AudioConfiguration>, Vst3Error> {
        lock(&self.engine).instance()?;
        Ok(Vec::new())
    }
    /// Negotiates every requested bus, deactivating omitted buses. Preparation failure
    /// may invalidate this instance; the helper stages another instance before replacement.
    /// Reserves native audio storage for the maximum block size. Larger maxima increase
    /// memory requirements; native plugin code and control callbacks may still allocate while processing.
    pub(crate) fn prepare_audio(
        &mut self,
        config: &AudioConfig,
    ) -> Result<Vec<AudioBusInfo>, Vst3Error> {
        let parameters = self.parameter_cache();
        let buses = lock(&self.engine).prepare_audio(config, &parameters)?;
        self.seed_held_values();
        Ok(buses)
    }
    pub(crate) fn bypass(&self) -> Result<BypassState, Vst3Error> {
        let Some(id) = self.bypass_parameter() else {
            return Ok(BypassState::new(Support::Unsupported, None));
        };
        self.sync_controller();
        // A value that is not finite leaves the state unknown.
        Ok(BypassState::new(
            Support::Supported,
            self.controller_value(id).map(|value| value >= 0.5),
        ))
    }
    /// Uses the plugin's native bypass parameter; no host dry path is substituted.
    pub(crate) fn set_bypass(&mut self, enabled: bool) -> Result<(), Vst3Error> {
        let id = self
            .bypass_parameter()
            .ok_or(Vst3Error::BypassUnsupported)?;
        let value = if enabled { 1.0 } else { 0.0 };
        if !self.handler.can_edit(id) {
            return Err(Vst3Error::Input(plughost_core::InputError::EventCapacity));
        }
        let result = unsafe { self.controller.setParamNormalized(id, value) };
        if result != kResultOk {
            return Err(Vst3Error::BypassEdit(result));
        }
        if unsafe { self.handler.performEdit(id, value) } != kResultOk {
            return Err(Vst3Error::Input(plughost_core::InputError::EventCapacity));
        }
        self.flush_edits()
    }

    /// The first writable parameter flagged as bypass, as JUCE chooses it.
    fn bypass_parameter(&self) -> Option<u32> {
        let parameters = self.parameter_cache();
        (0..parameters.len())
            .find(|&index| {
                let flags = parameters.info(index).flags;
                flags.bypass && !flags.read_only
            })
            .map(|index| parameters.native_id(index))
    }
}
