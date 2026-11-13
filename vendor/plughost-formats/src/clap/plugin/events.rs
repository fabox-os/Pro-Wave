use super::*;
use clack_host::events::event_types::{
    Midi2Event, MidiEvent, MidiSysExEvent, NoteChokeEvent, NoteEndEvent, NoteExpressionEvent,
    NoteOffEvent, NoteOnEvent, ParamGestureBeginEvent, ParamGestureEndEvent, ParamModEvent,
    ParamValueEvent, TransportEvent,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{Event as _, UnknownEvent};
use plughost_core::internal::ParameterEventBuffer;
use plughost_core::{ParameterEvent, ParameterEventBatch};

/// The core event a plugin's output `event` is, when its header gives the size of its type.
/// `UnknownEvent::as_core_event` reads the type alone, so an event the plugin declared shorter
/// than its type would be read past its end; one that does not fit has no portable form.
pub(super) fn core_event(event: &UnknownEvent) -> Option<CoreEventSpace<'_>> {
    use CoreEventSpace::*;
    // `as_event` checks the space, the type and the size.
    Some(match event.header().type_id() {
        NoteOnEvent::TYPE_ID => NoteOn(event.as_event()?),
        NoteOffEvent::TYPE_ID => NoteOff(event.as_event()?),
        NoteChokeEvent::TYPE_ID => NoteChoke(event.as_event()?),
        NoteEndEvent::TYPE_ID => NoteEnd(event.as_event()?),
        NoteExpressionEvent::TYPE_ID => NoteExpression(event.as_event()?),
        ParamValueEvent::TYPE_ID => ParamValue(event.as_event()?),
        ParamModEvent::TYPE_ID => ParamMod(event.as_event()?),
        ParamGestureBeginEvent::TYPE_ID => ParamGestureBegin(event.as_event()?),
        ParamGestureEndEvent::TYPE_ID => ParamGestureEnd(event.as_event()?),
        TransportEvent::TYPE_ID => Transport(event.as_event()?),
        MidiEvent::TYPE_ID => Midi(event.as_event()?),
        Midi2Event::TYPE_ID => Midi2(event.as_event()?),
        MidiSysExEvent::TYPE_ID => MidiSysEx(event.as_event()?),
        _ => return None,
    })
}

impl Plugin {
    /// Drains the plugin's parameter events. A rescan of parameter info among them also refreshes
    /// the processing thread's parameter list.
    pub(crate) fn take_parameter_events(&mut self) -> ParameterEventBatch {
        self.parameter_cache();
        self.shared().parameter_events.take()
    }
}

pub(super) struct ParameterOutput<'a> {
    pub parameters: &'a ParameterCache,
    pub queue: &'a ParameterEventBuffer,
}

impl clack_host::events::io::OutputEventBuffer for ParameterOutput<'_> {
    fn try_push(
        &mut self,
        event: &UnknownEvent,
    ) -> Result<(), clack_host::events::io::TryPushError> {
        let _audit = plughost_core::audit::callback();
        let parameters = self.parameters;
        let queue = self.queue;
        let notification = match core_event(event) {
            Some(CoreEventSpace::ParamValue(value)) => {
                // Per-note expression is not the global parameter value exposed by this API.
                if !value.pckn().matches_all() {
                    return Ok(());
                }
                let Some(id) = value.param_id().map(|id| u64::from(id.get())) else {
                    return Ok(());
                };
                let Some(parameter) = parameters.get(id) else {
                    queue.record(ParameterEvent::ValuesChanged);
                    return Ok(());
                };
                // A value past the range by more than rounding is not one of the parameter's:
                // the application reads the values again.
                let Some(normalized) = parameter.reported(value.value()) else {
                    queue.record(ParameterEvent::ValuesChanged);
                    return Ok(());
                };
                // A meter reports its value every block; the queue keeps the latest.
                queue.record_value(id, normalized);
                return Ok(());
            }
            Some(CoreEventSpace::ParamGestureBegin(value)) => {
                let Some(id) = value.param_id() else {
                    return Ok(());
                };
                ParameterEvent::BeginEdit {
                    id: u64::from(id.get()),
                }
            }
            Some(CoreEventSpace::ParamGestureEnd(value)) => {
                let Some(id) = value.param_id() else {
                    return Ok(());
                };
                ParameterEvent::EndEdit {
                    id: u64::from(id.get()),
                }
            }
            _ => return Ok(()),
        };
        queue.record(notification);
        Ok(())
    }
}
