use super::{ClapError, Plugin, lock};
use crate::Error;
use plughost_core::render::{Process, Tail};
use plughost_core::{AudioDirection, AutomationEvent, BlockContext, InputError, Sample};
impl<S: Sample> Process<S> for Plugin {
    type Error = Error;
    fn sample_rate(&self) -> f64 {
        self.config().map_or(0.0, |c| c.sample_rate)
    }
    fn max_block_size(&self) -> usize {
        self.config().map_or(0, |c| c.max_block_size)
    }
    fn input_channels(&self) -> usize {
        channel_count(self, AudioDirection::Input)
    }
    fn output_channels(&self) -> usize {
        channel_count(self, AudioDirection::Output)
    }
    fn latency(&self) -> Result<u32, Error> {
        Ok(Plugin::timing(self)?.latency)
    }
    fn tail(&self) -> Result<Tail, Error> {
        Ok(Plugin::timing(self)?.tail)
    }
    fn begin_render(&mut self) -> Result<(), Error> {
        self.config().ok_or(ClapError::NotPrepared)?;
        Ok(())
    }
    fn validate_events(&mut self, events: &plughost_core::Events) -> Result<(), Error> {
        let engine = lock(&self.engine);
        let prepared = engine.prepared.as_ref().ok_or(ClapError::NotPrepared)?;
        if let Some(event) = events.iter().find(|event| {
            prepared
                .event_inputs
                .get(event.port)
                .copied()
                .flatten()
                .is_none()
        }) {
            return Err(InputError::EventPort { port: event.port }.into());
        }
        Ok(())
    }
    fn validate_automation(&mut self, automation: &[AutomationEvent]) -> Result<(), Error> {
        let parameters = self.parameter_cache();
        for event in automation {
            if event.slot != 0 {
                return Err(InputError::Slot.into());
            }
            let parameter =
                parameters
                    .get(event.change.id)
                    .ok_or(InputError::UnknownParameter {
                        id: event.change.id,
                    })?;
            parameter.info.automation_value(event.change.value)?;
        }
        Ok(())
    }
    fn process(
        &mut self,
        context: &BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        automation: &[AutomationEvent],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        if automation.iter().any(|event| event.slot != 0) {
            return Err(InputError::Slot.into());
        }
        let changes: Vec<_> = automation.iter().map(|event| event.change).collect();
        Ok(self
            .processor()
            .process(context, input, output, &changes, events, produced)?)
    }
}
fn channel_count(plugin: &Plugin, direction: AudioDirection) -> usize {
    let engine = lock(&plugin.engine);
    let Some(prepared) = &engine.prepared else {
        return 0;
    };
    if prepared.audio_config.is_some() {
        prepared
            .buses
            .iter()
            .filter(|b| b.direction == direction && b.active == Some(true))
            .map(|b| b.channels as usize)
            .sum()
    } else {
        match direction {
            AudioDirection::Input => prepared.main_input.map_or(0, |i| prepared.inputs[i]),
            AudioDirection::Output => prepared.main_output.map_or(0, |i| prepared.outputs[i]),
            _ => 0,
        }
    }
}
