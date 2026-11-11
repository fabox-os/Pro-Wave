//! Output events of one block: parameter notifications go to their queue; notes, note expressions
//! and MIDI become events within the block's budgets, copied while the plugin's data is valid.
use super::*;
use clack_host::events::UnknownEvent;
use clack_host::events::io::{OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clap_sys::events::{
    CLAP_NOTE_EXPRESSION_BRIGHTNESS, CLAP_NOTE_EXPRESSION_EXPRESSION, CLAP_NOTE_EXPRESSION_PAN,
    CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING, CLAP_NOTE_EXPRESSION_VIBRATO,
    CLAP_NOTE_EXPRESSION_VOLUME, clap_note_expression,
};
use plughost_core::{
    EventData, ExpressionKind, MAX_BLOCK_EVENTS, MAX_BLOCK_SYSEX_BYTES, MAX_NOTE_ID, Note,
    NoteExpression,
};
use std::collections::HashMap;

/// Where the plugin's output notes with IDs play, by ID, from the note on it delivered to the
/// note off, choke or end that ends them: their port, channel and key. A note off or expression
/// may name its note by ID alone.
pub(super) type Sounding = HashMap<u32, (usize, u8, u8)>;

/// Room for twice the notes it follows, so that it does not grow on the processing thread after
/// notes come and go.
pub(super) fn sounding() -> Result<Sounding, ClapError> {
    let mut sounding = HashMap::new();
    sounding
        .try_reserve(2 * MAX_BLOCK_EVENTS)
        .map_err(|_| ClapError::EventStorage)?;
    Ok(sounding)
}

pub(super) struct ProcessOutput<'a> {
    pub parameters: events::ParameterOutput<'a>,
    pub events: &'a mut plughost_core::Events,
    pub sounding: &'a mut Sounding,
    /// The block's length; events at or after its end land on its last frame.
    pub frames: usize,
    pub sysex: usize,
    /// The plugin pushed more events or system exclusive bytes than the block allows.
    pub overflow: bool,
    /// Events with no portable form (choke, MIDI 2.0, notes for any key that no ID names, events
    /// whose header gives another size than their type's); a note end only reports a voice that
    /// ended and is not counted.
    pub unconvertible: u64,
}

/// The portable expression of a CLAP one.
fn expression_kind(id: clap_note_expression) -> Option<ExpressionKind> {
    Some(match id {
        CLAP_NOTE_EXPRESSION_VOLUME => ExpressionKind::Volume,
        CLAP_NOTE_EXPRESSION_PAN => ExpressionKind::Pan,
        CLAP_NOTE_EXPRESSION_TUNING => ExpressionKind::Tuning,
        CLAP_NOTE_EXPRESSION_VIBRATO => ExpressionKind::Vibrato,
        CLAP_NOTE_EXPRESSION_EXPRESSION => ExpressionKind::Expression,
        CLAP_NOTE_EXPRESSION_BRIGHTNESS => ExpressionKind::Brightness,
        CLAP_NOTE_EXPRESSION_PRESSURE => ExpressionKind::Pressure,
        _ => return None,
    })
}

impl ProcessOutput<'_> {
    /// The port, channel, key and ID a note event addresses: its own when they are specific, or
    /// those of the note its ID names.
    fn address(&self, pckn: Pckn) -> Option<(usize, u8, u8, Option<u32>)> {
        let id = pckn.note_id.into_specific().filter(|id| *id <= MAX_NOTE_ID);
        let (Match::Specific(port), Match::Specific(channel), Match::Specific(key)) =
            (pckn.port_index, pckn.channel, pckn.key)
        else {
            let &(port, channel, key) = self.sounding.get(&id?)?;
            return Some((port, channel, key, id));
        };
        let channel = u8::try_from(channel).ok()?;
        let key = u8::try_from(key).ok()?;
        Some((usize::from(port), channel, key, id))
    }

    fn note(&self, pckn: Pckn, velocity: f64) -> Option<(usize, Note)> {
        let (port, channel, key, id) = self.address(pckn)?;
        // Plugins compute velocities; one slightly out of range is not a different note.
        let mut note = Note::new(channel, key, velocity.clamp(0.0, 1.0));
        note.id = id;
        Some((port, note))
    }

    /// Forgets the notes a note off, choke or end names.
    fn end(&mut self, pckn: Pckn) {
        if let Some(id) = pckn.note_id.into_specific() {
            if let std::collections::hash_map::Entry::Occupied(entry) = self.sounding.entry(id) {
                let &(port, channel, key) = entry.get();
                if pckn.matches(&Pckn::new(
                    port as u16,
                    u16::from(channel),
                    u16::from(key),
                    id,
                )) {
                    entry.remove();
                }
            }
            return;
        }
        if self.sounding.is_empty() {
            return;
        }
        self.sounding.retain(|&id, &mut (port, channel, key)| {
            !pckn.matches(&Pckn::new(
                port as u16,
                u16::from(channel),
                u16::from(key),
                id,
            ))
        });
    }

    /// Refuses an event that does not fit the block's budgets, which fails the block.
    fn overflowed(&mut self) -> Result<(), TryPushError> {
        self.overflow = true;
        Err(TryPushError::new())
    }

    fn push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let converted = match events::core_event(event) {
            Some(CoreEventSpace::NoteOn(event)) => self
                .note(event.pckn(), event.velocity())
                .map(|(port, note)| (port, EventData::NoteOn(note))),
            Some(CoreEventSpace::NoteOff(event)) => self
                .note(event.pckn(), event.velocity())
                .map(|(port, note)| (port, EventData::NoteOff(note))),
            Some(CoreEventSpace::NoteExpression(event)) => self
                .address(event.pckn())
                .zip(expression_kind(event.as_raw().expression_id))
                .map(|((port, channel, key, id), kind)| {
                    let value = event
                        .value()
                        .clamp(*kind.range().start(), *kind.range().end());
                    let mut expression = NoteExpression::new(channel, key, kind, value);
                    expression.id = id;
                    (port, EventData::Expression(expression))
                }),
            Some(CoreEventSpace::Midi(event)) => Some((
                usize::from(event.port_index()),
                EventData::Midi(event.data()),
            )),
            Some(CoreEventSpace::MidiSysEx(event)) => {
                // The bytes are read only once the size the plugin declares fits the block.
                if self.sysex.saturating_add(event.buffer_size() as usize) > MAX_BLOCK_SYSEX_BYTES {
                    return self.overflowed();
                }
                // A plugin declaring bytes it gives no buffer for sent nothing to read.
                if event.buffer_ptr().is_null() && event.buffer_size() > 0 {
                    None
                } else {
                    // SAFETY: the plugin's buffer is valid during this call; it is copied now.
                    Some((
                        usize::from(event.port_index()),
                        EventData::SysEx(unsafe { event.data() }),
                    ))
                }
            }
            Some(
                CoreEventSpace::ParamValue(_)
                | CoreEventSpace::ParamMod(_)
                | CoreEventSpace::ParamGestureBegin(_)
                | CoreEventSpace::ParamGestureEnd(_),
            ) => return self.parameters.try_push(event),
            // The plugin reports a voice it ended; there is nothing to deliver.
            Some(CoreEventSpace::NoteEnd(_)) => return Ok(()),
            _ => None,
        };
        let Some((port, data)) = converted else {
            self.unconvertible += 1;
            return Ok(());
        };
        let offset = (event.header().time() as usize).min(self.frames.saturating_sub(1));
        let event = plughost_core::Event::new(offset, data).on_port(port);
        if !event.is_valid() {
            self.unconvertible += 1;
            return Ok(());
        }
        let sysex = match event.data {
            EventData::SysEx(bytes) => bytes.len(),
            _ => 0,
        };
        if self.events.len() == MAX_BLOCK_EVENTS {
            return self.overflowed();
        }
        // Within the block's budgets, which the checks above keep, pushing allocates nothing for
        // events with one block's room.
        if self.events.push(event).is_err() {
            return self.overflowed();
        }
        self.sysex += sysex;
        if let EventData::NoteOn(note) = event.data
            && let Some(id) = note.id
            && (self.sounding.len() < MAX_BLOCK_EVENTS || self.sounding.contains_key(&id))
        {
            self.sounding
                .insert(id, (event.port, note.channel, note.key));
        }
        Ok(())
    }
}

impl OutputEventBuffer for ProcessOutput<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let _audit = plughost_core::audit::callback();
        let pushed = self.push(event);
        // A note off, choke or end ends the notes it names whether or not it is delivered.
        match events::core_event(event) {
            Some(CoreEventSpace::NoteOff(off)) => self.end(off.pckn()),
            Some(CoreEventSpace::NoteChoke(choke)) => self.end(choke.pckn()),
            Some(CoreEventSpace::NoteEnd(end)) => self.end(end.pckn()),
            _ => {}
        }
        pushed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_host::events::Event as _;
    use clack_host::events::event_types::{
        NoteChokeEvent, NoteEndEvent, NoteExpressionEvent, NoteExpressionType,
    };
    use plughost_core::Events;
    use plughost_core::internal::ParameterEventBuffer;

    /// What the plugin's `events` become in one block, and how many had no portable form.
    fn block(sounding: &mut Sounding, events: &[&UnknownEvent]) -> (Events, u64) {
        let (parameters, queue) = (ParameterCache::default(), ParameterEventBuffer::default());
        let mut produced = Events::for_block();
        let mut output = ProcessOutput {
            parameters: events::ParameterOutput {
                parameters: &parameters,
                queue: &queue,
            },
            events: &mut produced,
            sounding,
            frames: 8,
            sysex: 0,
            overflow: false,
            unconvertible: 0,
        };
        for event in events {
            assert!(output.try_push(event).is_ok());
        }
        let unconvertible = output.unconvertible;
        (produced, unconvertible)
    }

    fn data(events: &Events) -> Vec<EventData<'_>> {
        events.iter().map(|event| event.data).collect()
    }

    #[test]
    fn a_note_off_or_expression_may_name_its_note_by_id_alone() {
        let mut sounding = sounding().unwrap();
        let on = NoteOnEvent::new(0, Pckn::new(1u16, 2u16, 64u16, 5u32), 0.5);
        let (produced, _) = block(&mut sounding, &[on.as_unknown()]);
        assert_eq!(
            data(&produced),
            [EventData::NoteOn(Note::new(2, 64, 0.5).with_id(5))]
        );

        let by_id = Pckn::new(Match::All, Match::All, Match::All, 5u32);
        let tuning = NoteExpressionEvent::new(1, by_id, NoteExpressionType::Tuning, 12.0);
        let off = NoteOffEvent::new(2, by_id, 0.0);
        let (produced, unconvertible) =
            block(&mut sounding, &[tuning.as_unknown(), off.as_unknown()]);
        let expression = NoteExpression::new(2, 64, ExpressionKind::Tuning, 12.0).with_id(5);
        assert_eq!(
            (data(&produced), unconvertible),
            (
                vec![
                    EventData::Expression(expression),
                    EventData::NoteOff(Note::new(2, 64, 0.0).with_id(5)),
                ],
                0
            )
        );

        // The note ended; its ID names no note any more.
        let (produced, unconvertible) = block(&mut sounding, &[tuning.as_unknown()]);
        assert_eq!((produced.is_empty(), unconvertible), (true, 1));
    }

    #[test]
    fn notes_ended_by_a_choke_or_an_end_leave_room_to_follow_later_notes() {
        let mut sounding = sounding().unwrap();
        for id in 0..MAX_BLOCK_EVENTS as u32 {
            let pckn = Pckn::new(0u16, 0u16, 60u16, id);
            let on = NoteOnEvent::new(0, pckn, 0.5);
            let (choke, end) = (NoteChokeEvent::new(1, pckn), NoteEndEvent::new(1, pckn));
            let ending = if id % 2 == 0 {
                choke.as_unknown()
            } else {
                end.as_unknown()
            };
            block(&mut sounding, &[on.as_unknown(), ending]);
        }
        let on = NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 62u16, 9999u32), 0.5);
        let by_id = Pckn::new(Match::All, Match::All, Match::All, 9999u32);
        let tuning = NoteExpressionEvent::new(1, by_id, NoteExpressionType::Tuning, 12.0);
        let (produced, unconvertible) =
            block(&mut sounding, &[on.as_unknown(), tuning.as_unknown()]);
        let expression = NoteExpression::new(0, 62, ExpressionKind::Tuning, 12.0).with_id(9999);
        assert_eq!(
            (data(&produced), unconvertible),
            (
                vec![
                    EventData::NoteOn(Note::new(0, 62, 0.5).with_id(9999)),
                    EventData::Expression(expression),
                ],
                0
            )
        );
    }

    #[test]
    #[ignore = "CLAP output-note benchmark; run in release mode with --nocapture"]
    fn output_note_off_benchmark() {
        use std::io::Write;
        use std::time::Instant;
        for identified in [true, false] {
            let mut sounding = sounding().unwrap();
            if identified {
                sounding.extend((0..MAX_BLOCK_EVENTS as u32).map(|id| (id, (0, 0, 60))));
            }
            let parameters = ParameterCache::default();
            let queue = ParameterEventBuffer::default();
            let mut produced = Events::for_block();
            let mut output = ProcessOutput {
                parameters: events::ParameterOutput {
                    parameters: &parameters,
                    queue: &queue,
                },
                events: &mut produced,
                sounding: &mut sounding,
                frames: 64,
                sysex: 0,
                overflow: false,
                unconvertible: 0,
            };
            let note_id = if identified {
                Match::Specific(0u32)
            } else {
                Match::All
            };
            let off = NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 60u16, note_id), 0.0);
            let started = Instant::now();
            for _ in 0..100_000 {
                if identified {
                    output.sounding.insert(0, (0, 0, 60));
                }
                output.events.clear();
                output
                    .try_push(std::hint::black_box(off.as_unknown()))
                    .unwrap();
            }
            let elapsed = started.elapsed();
            assert_eq!(output.events.len(), 1);
            assert_eq!(
                output.sounding.len(),
                if identified { MAX_BLOCK_EVENTS - 1 } else { 0 }
            );
            // An exact ID must still obey explicitly supplied port/channel/key constraints.
            output.sounding.insert(0, (0, 0, 60));
            let wrong_port = NoteOffEvent::new(0, Pckn::new(1u16, 0u16, 60u16, 0u32), 0.0);
            output.try_push(wrong_port.as_unknown()).unwrap();
            assert!(output.sounding.contains_key(&0));
            let by_id =
                NoteOffEvent::new(0, Pckn::new(Match::All, Match::All, Match::All, 0u32), 0.0);
            output.try_push(by_id.as_unknown()).unwrap();
            assert!(!output.sounding.contains_key(&0));
            writeln!(
                std::io::stdout(),
                "identified={identified} note_offs=100000 ms={:.3}",
                elapsed.as_secs_f64() * 1000.0
            )
            .unwrap();
        }
    }
}
