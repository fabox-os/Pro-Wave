//! Owned, bounded standard input events; Clack supplies the native list callbacks.
use super::*;
use clack_host::events::event_types::{MidiSysExEvent, NoteExpressionEvent, NoteExpressionType};
use clack_host::events::io::InputEventBuffer;
use clack_host::events::{Event as _, UnknownEvent};
use plughost_core::{EventData, ExpressionKind};

// Pending control edits and the current block each have their own bounded input budget.
const CAPACITY: usize = 2 * plughost_core::MAX_BLOCK_EVENTS;

enum NativeEvent {
    Parameter(ParamValueEvent),
    NoteOn(NoteOnEvent),
    NoteOff(NoteOffEvent),
    Expression(NoteExpressionEvent),
    Midi(ClapMidiEvent),
    /// Points into the block's input events, which outlive the process call.
    SysEx(MidiSysExEvent),
}
impl NativeEvent {
    fn unknown(&self) -> &UnknownEvent {
        match self {
            Self::Parameter(event) => event.as_unknown(),
            Self::NoteOn(event) => event.as_unknown(),
            Self::NoteOff(event) => event.as_unknown(),
            Self::Expression(event) => event.as_unknown(),
            Self::Midi(event) => event.as_unknown(),
            Self::SysEx(event) => event.as_unknown(),
        }
    }
}

/// The CLAP expression of a portable one.
fn expression_type(kind: ExpressionKind) -> Option<NoteExpressionType> {
    Some(match kind {
        ExpressionKind::Volume => NoteExpressionType::Volume,
        ExpressionKind::Pan => NoteExpressionType::Pan,
        ExpressionKind::Tuning => NoteExpressionType::Tuning,
        ExpressionKind::Vibrato => NoteExpressionType::Vibrato,
        ExpressionKind::Expression => NoteExpressionType::Expression,
        ExpressionKind::Brightness => NoteExpressionType::Brightness,
        ExpressionKind::Pressure => NoteExpressionType::Pressure,
        _ => return None,
    })
}

pub(super) struct InputBuffer {
    events: Vec<(usize, NativeEvent)>,
}
impl InputBuffer {
    pub fn new() -> Result<Self, ClapError> {
        let mut events = Vec::new();
        events
            .try_reserve_exact(CAPACITY)
            .map_err(|_| ClapError::EventStorage)?;
        Ok(Self { events })
    }
    pub fn clear(&mut self) {
        self.events.clear();
    }
    fn push(&mut self, event: NativeEvent) {
        self.events.push((self.events.len(), event));
    }
    pub fn parameter(&mut self, offset: u32, id: ClapId, value: f64) {
        self.push(NativeEvent::Parameter(ParamValueEvent::new(
            offset,
            id,
            Pckn::match_all(),
            value,
        )));
    }
    /// Queues `event` for its port, whose dialects decide the native form. Ports that take the
    /// CLAP dialect get notes and expressions as CLAP note and note expression events, and MIDI
    /// notes as CLAP notes too, as well as MIDI poly pressure as pressure when they take no MIDI;
    /// ports that take MIDI or MIDI with MPE get the other channel messages, and the MIDI 1.0
    /// form of notes and pressure where they have no CLAP dialect.
    /// System exclusive data goes to ports that take MIDI. The rest are not delivered.
    /// A system exclusive event refers to the bytes of the `Events` holding `event`, which the
    /// process call reading the buffer must not outlive.
    pub fn note(&mut self, event: &plughost_core::Event, dialects: NoteDialects) {
        let time = event.offset as u32;
        let port = event.port as u16;
        let pckn = |channel: u8, key: u8, id: Option<u32>| {
            let id = id.map_or(Match::All, Match::Specific);
            Pckn::new(port, u16::from(channel), u16::from(key), id)
        };
        let clap_notes = dialects.supports(NoteDialect::Clap);
        let midi = dialects.supports(NoteDialect::Midi) || dialects.supports(NoteDialect::MidiMpe);
        match &event.data {
            EventData::SysEx(bytes) => {
                if dialects.supports(NoteDialect::Midi) {
                    // SAFETY: the bytes belong to the caller's `Events`, which outlives the process
                    // call that reads the buffer; the next call clears the buffer before use.
                    let sysex = unsafe { MidiSysExEvent::new(time, port, bytes) };
                    self.push(NativeEvent::SysEx(sysex));
                }
                return;
            }
            EventData::NoteOn(note) if clap_notes => {
                let pckn = pckn(note.channel, note.key, note.id);
                self.push(NativeEvent::NoteOn(NoteOnEvent::new(
                    time,
                    pckn,
                    note.velocity,
                )));
                return;
            }
            EventData::NoteOff(note) if clap_notes => {
                let pckn = pckn(note.channel, note.key, note.id);
                self.push(NativeEvent::NoteOff(NoteOffEvent::new(
                    time,
                    pckn,
                    note.velocity,
                )));
                return;
            }
            EventData::Expression(expression) if clap_notes => {
                if let Some(kind) = expression_type(expression.kind) {
                    let pckn = pckn(expression.channel, expression.key, expression.id);
                    self.push(NativeEvent::Expression(NoteExpressionEvent::new(
                        time,
                        pckn,
                        kind,
                        expression.value,
                    )));
                }
                return;
            }
            _ => {}
        }
        match event.message() {
            Some(Message::NoteOn {
                channel,
                key,
                velocity,
            }) if clap_notes => {
                self.push(NativeEvent::NoteOn(NoteOnEvent::new(
                    time,
                    pckn(channel, key, None),
                    f64::from(velocity) / 127.0,
                )));
            }
            Some(Message::NoteOff {
                channel,
                key,
                velocity,
            }) if clap_notes => {
                self.push(NativeEvent::NoteOff(NoteOffEvent::new(
                    time,
                    pckn(channel, key, None),
                    f64::from(velocity) / 127.0,
                )));
            }
            Some(Message::PolyPressure {
                channel,
                key,
                pressure,
            }) if clap_notes && !midi => {
                self.push(NativeEvent::Expression(NoteExpressionEvent::new(
                    time,
                    pckn(channel, key, None),
                    NoteExpressionType::Pressure,
                    f64::from(pressure) / 127.0,
                )));
            }
            _ if midi => {
                if let Some(data) = event.data.to_midi() {
                    self.push(NativeEvent::Midi(ClapMidiEvent::new(time, port, data)));
                }
            }
            _ => {}
        }
    }
    pub fn sort(&mut self) {
        // Insertion order breaks ties, preserving pending edits, automation, then MIDI at a time.
        self.events
            .sort_unstable_by_key(|(order, event)| (event.unknown().header().time(), *order));
    }
}
impl InputEventBuffer for InputBuffer {
    fn len(&self) -> u32 {
        let _audit = plughost_core::audit::callback();
        self.events.len() as u32
    }
    fn get(&self, index: u32) -> Option<&UnknownEvent> {
        let _audit = plughost_core::audit::callback();
        self.events
            .get(index as usize)
            .map(|(_, event)| event.unknown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_host::events::spaces::CoreEventSpace;
    use plughost_core::{Event, Note, NoteExpression};

    #[test]
    fn ordered_native_events_keep_duplicate_edits_and_note_off_after_note_on() {
        let mut input = InputBuffer::new().unwrap();
        input.parameter(0, ClapId::new(7), 0.25);
        input.parameter(2, ClapId::new(7), 0.5);
        input.parameter(2, ClapId::new(7), 0.75);
        input.note(&Event::note_on(1, 0, 60, 127), NoteDialects::CLAP);
        input.note(&Event::note_off(1, 0, 60, 0), NoteDialects::CLAP);
        input.note(&Event::midi(2, [0xb0, 1, 64]), NoteDialects::MIDI);
        input.sort();
        let events = InputEvents::from_buffer(&input);
        let times: Vec<_> = events.iter().map(|e| e.header().time()).collect();
        assert_eq!(times, [0, 1, 1, 2, 2, 2]);
        assert!(matches!(
            input.get(1).unwrap().as_core_event(),
            Some(CoreEventSpace::NoteOn(_))
        ));
        assert!(matches!(
            input.get(2).unwrap().as_core_event(),
            Some(CoreEventSpace::NoteOff(_))
        ));
        for (index, expected) in [(0, 0.25), (3, 0.5), (4, 0.75)] {
            let Some(CoreEventSpace::ParamValue(event)) = input.get(index).unwrap().as_core_event()
            else {
                panic!("expected a parameter value");
            };
            assert_eq!(event.value(), expected);
        }
        assert!(matches!(
            input.get(5).unwrap().as_core_event(),
            Some(CoreEventSpace::Midi(_))
        ));
        input.clear();
        input.note(&Event::note_off(0, 3, 65, 0), NoteDialects::MIDI);
        assert_eq!(input.len(), 1);
        assert!(matches!(
            input.get(0).unwrap().as_core_event(),
            Some(CoreEventSpace::Midi(_))
        ));
    }

    #[test]
    fn notes_keep_their_ids_on_clap_ports_and_take_their_midi_form_elsewhere() {
        let note = Note::new(2, 64, 0.25).with_id(7);
        let tuning = NoteExpression::new(2, 64, ExpressionKind::Tuning, 1.5).with_id(7);
        let pressure = NoteExpression::new(2, 64, ExpressionKind::Pressure, 1.0);
        let events = [
            Event::new(0, EventData::NoteOn(note)).on_port(1),
            Event::new(1, EventData::Expression(tuning)).on_port(1),
            Event::new(2, EventData::Expression(pressure)).on_port(1),
        ];
        let mut input = InputBuffer::new().unwrap();
        for event in &events {
            input.note(event, NoteDialects::CLAP | NoteDialects::MIDI);
        }
        let Some(CoreEventSpace::NoteOn(on)) = input.get(0).unwrap().as_core_event() else {
            panic!("expected a note on");
        };
        assert_eq!(on.pckn(), Pckn::new(1u16, 2u16, 64u16, 7u32));
        assert_eq!(on.velocity(), 0.25);
        let Some(CoreEventSpace::NoteExpression(expression)) =
            input.get(1).unwrap().as_core_event()
        else {
            panic!("expected a note expression");
        };
        assert_eq!(
            expression.expression_type(),
            Some(NoteExpressionType::Tuning)
        );
        assert_eq!(expression.value(), 1.5);
        assert_eq!(expression.pckn().note_id, Match::Specific(7));
        let Some(CoreEventSpace::NoteExpression(expression)) =
            input.get(2).unwrap().as_core_event()
        else {
            panic!("expected a note expression");
        };
        assert_eq!(expression.pckn().note_id, Match::All);

        input.clear();
        for event in &events {
            input.note(event, NoteDialects::MIDI_MPE);
        }
        let midi: Vec<_> = (0..input.len())
            .map(|index| match input.get(index).unwrap().as_core_event() {
                Some(CoreEventSpace::Midi(midi)) => midi.data(),
                _ => panic!("expected MIDI"),
            })
            .collect();
        // Tuning has no MIDI 1.0 form.
        assert_eq!(midi, [[0x92, 64, 32], [0xA2, 64, 127]]);
    }

    #[test]
    fn midi_poly_pressure_reaches_a_port_without_midi_as_pressure() {
        let mut input = InputBuffer::new().unwrap();
        input.note(
            &Event::midi(3, [0xA2, 64, 127]).on_port(1),
            NoteDialects::CLAP,
        );
        let Some(CoreEventSpace::NoteExpression(pressure)) = input.get(0).unwrap().as_core_event()
        else {
            panic!("expected a note expression");
        };
        assert_eq!(
            pressure.expression_type(),
            Some(NoteExpressionType::Pressure)
        );
        assert_eq!(pressure.value(), 1.0);
        assert_eq!(pressure.pckn(), Pckn::new(1u16, 2u16, 64u16, Match::All));
    }
}
