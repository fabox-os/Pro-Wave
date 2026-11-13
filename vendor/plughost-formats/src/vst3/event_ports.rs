//! Event buses: their description, and activation for a requested routing. A bus's ID is its
//! index, as for audio buses.
use super::audio::direction;
use super::{errors::Vst3Error, host::wide_string, instance::Instance};
use plughost_core::{AudioDirection, EventConfig, EventPortInfo, Support};
use vst3::Steinberg::Vst::{
    BusInfo, IComponentTrait, IMidiMapping, INoteExpressionController,
    INoteExpressionControllerTrait, MediaTypes_,
};
use vst3::Steinberg::kResultOk;

const EVENT: i32 = MediaTypes_::kEvent as i32;

impl Instance {
    fn event_count(&self, dir: AudioDirection) -> u64 {
        u64::try_from(unsafe { self.component.getBusCount(EVENT, direction(dir)) }).unwrap_or(0)
    }

    /// Without an event input bus, a controller's MIDI mapping still takes controller messages;
    /// they arrive on input port 0 as parameter changes, while notes and data have no bus.
    fn controllers_only(&self) -> bool {
        self.event_count(AudioDirection::Input) == 0
            && self.controller.cast::<IMidiMapping>().is_some()
    }

    /// Event input buses the plugin declares, which receive notes and data events.
    pub fn event_buses(&self) -> usize {
        self.event_count(AudioDirection::Input) as usize
    }

    /// Whether the controller lists note expressions for an event input bus. Controllers list
    /// them per channel; the SDK's samples list them for channel 0 alone.
    pub(crate) fn note_expressions(&self, bus: u64) -> Support {
        let Some(controller) = self.controller.cast::<INoteExpressionController>() else {
            return Support::Unsupported;
        };
        Support::from(unsafe { controller.getNoteExpressionCount(bus as i32, 0) } > 0)
    }

    /// The component's event buses, inputs first. VST3 events carry notes, poly pressure, note
    /// expression values and system exclusive data; other channel messages reach inputs through
    /// the controller's MIDI mapping, and outputs send them as legacy controller events. VST3 has
    /// no MPE declaration.
    pub fn event_ports(&self) -> Result<Vec<EventPortInfo>, Vst3Error> {
        self.event_ports_with(|bus| self.note_expressions(bus))
    }

    /// The event ports, with each input bus's note expressions as `expressions` tells.
    pub(crate) fn event_ports_with(
        &self,
        expressions: impl Fn(u64) -> Support,
    ) -> Result<Vec<EventPortInfo>, Vst3Error> {
        let mut ports = Vec::new();
        if self.controllers_only() {
            ports.push({
                let mut event_port_info =
                    EventPortInfo::new(0, 0, String::new(), AudioDirection::Input);
                event_port_info.midi = Support::Supported;
                event_port_info.sysex = Support::Unsupported;
                event_port_info.note_expression = Support::Unsupported;
                event_port_info
            });
        }
        for dir in [AudioDirection::Input, AudioDirection::Output] {
            for index in 0..self.event_count(dir) {
                // SAFETY: BusInfo is plain C data the component fills in.
                let mut info: BusInfo = unsafe { std::mem::zeroed() };
                let result = unsafe {
                    self.component
                        .getBusInfo(EVENT, direction(dir), index as i32, &mut info)
                };
                if result != kResultOk {
                    return Err(Vst3Error::AudioBusMetadata);
                }
                ports.push({
                    let mut event_port_info =
                        EventPortInfo::new(index, index as u32, wide_string(&info.name), dir);
                    event_port_info.midi = Support::Supported;
                    // Data events are part of the format; a plugin's use of them is not queryable.
                    event_port_info.sysex = Support::Unknown;
                    event_port_info.note_expression = match dir {
                        AudioDirection::Input => expressions(index),
                        _ => Support::Unknown,
                    };
                    event_port_info
                });
            }
        }
        Ok(ports)
    }

    /// The first event input and output, which main-bus preparation activates.
    pub fn main_event_config(&self) -> EventConfig {
        let first = |available: bool| available.then_some(0).into_iter().collect();
        EventConfig::new(
            first(self.event_count(AudioDirection::Input) > 0 || self.controllers_only()),
            first(self.event_count(AudioDirection::Output) > 0),
        )
    }

    /// Activates exactly the requested event buses and returns, per input bus, whether it is
    /// active. Activation results are not checked, as plugins commonly refuse event buses they
    /// use anyway.
    pub fn activate_events(&self, config: &EventConfig) -> Result<Vec<bool>, Vst3Error> {
        let mut inputs = Vec::new();
        if self.controllers_only() {
            if let Some(&id) = config.inputs.iter().find(|&&id| id != 0) {
                return Err(Vst3Error::UnknownEventPort { id });
            }
            inputs.push(!config.inputs.is_empty());
        }
        for (dir, requested) in [
            (AudioDirection::Input, &config.inputs),
            (AudioDirection::Output, &config.outputs),
        ] {
            let count = self.event_count(dir);
            if dir == AudioDirection::Input && self.controllers_only() {
                continue;
            }
            if let Some(&id) = requested.iter().find(|&&id| id >= count) {
                return Err(Vst3Error::UnknownEventPort { id });
            }
            for index in 0..count {
                let active = requested.contains(&index);
                unsafe {
                    self.component.activateBus(
                        EVENT,
                        direction(dir),
                        index as i32,
                        u8::from(active),
                    )
                };
                if dir == AudioDirection::Input {
                    inputs.push(active);
                }
            }
        }
        Ok(inputs)
    }
}
