//! The event list a processor fills during `process`. Each event becomes a portable event as it
//! arrives, while the plugin's data (system exclusive bytes) is still valid, within the block's
//! prepared budgets: notes, poly pressure and note expression values as notes and expressions,
//! and system exclusive data and legacy controller output as MIDI.
use super::*;
use crate::vst3::errors::Vst3Error;
use crate::vst3::note_expression;
use plughost_core::{
    Event as PortEvent, EventData, Events, ExpressionKind, MAX_BLOCK_EVENTS, MAX_BLOCK_SYSEX_BYTES,
    MAX_NOTE_ID, Note, NoteExpression,
};
use std::collections::HashMap;
use vst3::Steinberg::Vst::ControllerNumbers_::{
    kAfterTouch, kCtrlPolyPressure, kCtrlProgramChange, kPitchBend,
};
use vst3::Steinberg::Vst::DataEvent_::DataTypes_::kMidiSysEx;
use vst3::Steinberg::Vst::Event_::EventTypes_;

/// Where an output note with an ID plays, by its native ID: its bus, channel and key. A note
/// expression value names its note by ID alone. IDs in the range VST3 reserves for plugins have no
/// portable form, so their notes and expressions leave the plugin without one.
type Sounding = HashMap<i32, (usize, u8, u8)>;

struct Output {
    /// One block's room, reserved: adding an event allocates nothing.
    events: Events,
    sysex: usize,
    /// The plugin added more events or system exclusive bytes than the block allows.
    overflow: bool,
    /// Events with no portable form (text expression, chords, scales, out-of-range values,
    /// expressions of unknown notes).
    unconvertible: u64,
    /// The plugin's notes with IDs, from the note on that left it to their note off, up to the
    /// event budget.
    sounding: Sounding,
}

impl Output {
    /// Refuses `event`, which does not fit the block's budgets, and fails the block.
    fn overflowed(&mut self, event: &Event) -> tresult {
        follow(&mut self.sounding, event, None);
        self.overflow = true;
        kResultFalse
    }
}

pub(crate) struct OutputEventList {
    output: Mutex<Output>,
}
impl Class for OutputEventList {
    type Interfaces = (IEventList,);
}
impl OutputEventList {
    pub fn new() -> Result<Self, Vst3Error> {
        // Room for twice the notes it follows, so that it does not grow on the processing thread
        // after notes come and go.
        let mut sounding = HashMap::new();
        sounding
            .try_reserve(2 * MAX_BLOCK_EVENTS)
            .map_err(|_| Vst3Error::EventStorage)?;
        Ok(Self {
            output: Mutex::new(Output {
                events: Events::try_for_block().map_err(|_| Vst3Error::EventStorage)?,
                sysex: 0,
                overflow: false,
                unconvertible: 0,
                sounding,
            }),
        })
    }
    /// Moves the events added since the last call into `into` in offset order, keeping the
    /// plugin's order at equal offsets; events at or after the end of the block of `frames` land
    /// on its last frame. Fails when the plugin exceeded the block's budgets, whose events are
    /// discarded.
    pub fn take(&self, into: &mut Events, frames: usize) -> Result<(), Vst3Error> {
        let mut output = lock(&self.output);
        output.sysex = 0;
        if std::mem::take(&mut output.overflow) {
            output.events.clear();
            return Err(Vst3Error::OutputEventCapacity);
        }
        let last = frames.saturating_sub(1);
        for event in &output.events {
            // The events fit one block's budget, which `into` has room for.
            into.push(event.at(event.offset.min(last)))
                .map_err(|_| Vst3Error::OutputEventCapacity)?;
        }
        output.events.clear();
        into.sort_by_offset();
        Ok(())
    }
    /// Drops the events of a block that failed.
    pub fn discard(&self) {
        let mut output = lock(&self.output);
        output.events.clear();
        output.sysex = 0;
        output.overflow = false;
    }
    /// Forgets the plugin's notes, which a reset ended.
    pub fn forget_notes(&self) {
        lock(&self.output).sounding.clear();
    }
    /// Events that had no portable form, since the last call.
    pub fn take_unconvertible(&self) -> u64 {
        std::mem::take(&mut lock(&self.output).unconvertible)
    }
    pub fn ptr(list: &ComWrapper<Self>) -> *mut IEventList {
        list.as_com_ref::<IEventList>()
            .map_or(std::ptr::null_mut(), |l| l.as_ptr())
    }
}

// Event and data type constants have the platform's C enum type; events store them as integers.
const NOTE_ON: u32 = EventTypes_::kNoteOnEvent as u32;
const NOTE_OFF: u32 = EventTypes_::kNoteOffEvent as u32;
const POLY_PRESSURE: u32 = EventTypes_::kPolyPressureEvent as u32;
const NOTE_EXPRESSION_VALUE: u32 = EventTypes_::kNoteExpressionValueEvent as u32;
const DATA: u32 = EventTypes_::kDataEvent as u32;
const LEGACY_MIDI_CC_OUT: u32 = EventTypes_::kLegacyMIDICCOutEvent as u32;
const SYSEX: u32 = kMidiSysEx as u32;

/// The portable ID of a VST3 note ID; -1 means none.
fn note_id(id: i32) -> Option<u32> {
    u32::try_from(id).ok().filter(|id| *id <= MAX_NOTE_ID)
}

fn note(channel: i16, pitch: i16, velocity: f32, id: i32) -> Option<Note> {
    let channel = u8::try_from(channel).ok()?;
    let key = u8::try_from(pitch).ok()?;
    // Plugins compute velocities; one slightly out of range is not a different note.
    let mut note = Note::new(channel, key, f64::from(velocity).clamp(0.0, 1.0));
    note.id = note_id(id);
    Some(note)
}

fn expression(
    channel: u8,
    key: u8,
    kind: ExpressionKind,
    value: f64,
    id: i32,
) -> EventData<'static> {
    let value = value.clamp(*kind.range().start(), *kind.range().end());
    let mut expression = NoteExpression::new(channel, key, kind, value);
    expression.id = note_id(id);
    EventData::Expression(expression)
}

/// The portable form of one output event: one event, or a note on and its tuning; `None` when it
/// has none. Note expression values find their notes in `sounding`. System exclusive bytes stay
/// the plugin's, valid for the call that adds the event.
fn convert<'a>(
    event: &'a Event,
    sounding: &Sounding,
) -> Option<(usize, EventData<'a>, Option<EventData<'a>>)> {
    let port = usize::try_from(event.busIndex).ok()?;
    let channel_message = |status: u8, channel: i32, first: i32, second: u8| {
        let channel = u8::try_from(channel).ok().filter(|c| *c < 16)?;
        let first = u8::try_from(first).ok().filter(|b| *b < 0x80)?;
        Some(EventData::Midi([status | channel, first, second]))
    };
    // SAFETY: the union field read matches the event type the plugin declared.
    let (data, tuning) = unsafe {
        match u32::from(event.r#type) {
            NOTE_ON => {
                let on = event.__field0.noteOn;
                let note = note(on.channel, on.pitch, on.velocity, on.noteId)?;
                // The tuning of a note on is in cents; one that is not a number is none.
                let tuning = (on.tuning.is_finite() && on.tuning != 0.0).then(|| {
                    let semitones = f64::from(on.tuning) / 100.0;
                    let kind = ExpressionKind::Tuning;
                    expression(note.channel, note.key, kind, semitones, on.noteId)
                });
                (EventData::NoteOn(note), tuning)
            }
            NOTE_OFF => {
                let off = event.__field0.noteOff;
                let note = note(off.channel, off.pitch, off.velocity, off.noteId)?;
                (EventData::NoteOff(note), None)
            }
            POLY_PRESSURE => {
                let pressure = event.__field0.polyPressure;
                let channel = u8::try_from(pressure.channel).ok()?;
                let key = u8::try_from(pressure.pitch).ok()?;
                let value = f64::from(pressure.pressure);
                let kind = ExpressionKind::Pressure;
                (expression(channel, key, kind, value, pressure.noteId), None)
            }
            NOTE_EXPRESSION_VALUE => {
                let value = event.__field0.noteExpressionValue;
                let kind = note_expression::kind(value.typeId)?;
                let &(bus, channel, key) = sounding.get(&value.noteId)?;
                if bus != port {
                    return None;
                }
                let plain = note_expression::plain(kind, value.value);
                (expression(channel, key, kind, plain, value.noteId), None)
            }
            DATA => {
                let data = event.__field0.data;
                if data.r#type != SYSEX || data.bytes.is_null() {
                    return None;
                }
                // The plugin owns the bytes for this call; they are copied as the event is added.
                let bytes = std::slice::from_raw_parts(data.bytes, data.size as usize);
                (EventData::SysEx(bytes), None)
            }
            LEGACY_MIDI_CC_OUT => {
                let cc = event.__field0.midiCCOut;
                let (value, value2) = (u8::try_from(cc.value).ok()?, u8::try_from(cc.value2).ok()?);
                let channel = i32::from(cc.channel);
                let data = match i16::from(cc.controlNumber) {
                    number if number < 128 => channel_message(0xB0, channel, number.into(), value)?,
                    number if number == kAfterTouch as i16 => {
                        channel_message(0xD0, channel, value.into(), 0)?
                    }
                    number if number == kPitchBend as i16 => {
                        channel_message(0xE0, channel, value.into(), value2)?
                    }
                    number if number == kCtrlProgramChange as i16 => {
                        channel_message(0xC0, channel, value.into(), 0)?
                    }
                    number if number == kCtrlPolyPressure as i16 => {
                        channel_message(0xA0, channel, value.into(), value2)?
                    }
                    _ => return None,
                };
                (data, None)
            }
            _ => return None,
        }
    };
    Some((port, data, tuning))
}

/// The system exclusive bytes an output event declares, which are read only once they are known to
/// fit the block.
fn declared_sysex(event: &Event) -> usize {
    if u32::from(event.r#type) != DATA {
        return 0;
    }
    // SAFETY: the union field read matches the event type the plugin declared.
    let data = unsafe { event.__field0.data };
    if data.r#type == SYSEX && !data.bytes.is_null() {
        data.size as usize
    } else {
        0
    }
}

/// Follows the plugin's notes with IDs: a note on it delivered starts one, and a note off ends
/// the note it names, or without an ID the notes on its key, whether or not it converts.
fn follow(sounding: &mut Sounding, event: &Event, delivered: Option<&EventData>) {
    let Ok(port) = usize::try_from(event.busIndex) else {
        return;
    };
    // SAFETY: the union field read matches the event type the plugin declared.
    match u32::from(event.r#type) {
        NOTE_ON => {
            let id = unsafe { event.__field0.noteOn.noteId };
            if let Some(EventData::NoteOn(note)) = delivered
                && id != -1
                && (sounding.len() < MAX_BLOCK_EVENTS || sounding.contains_key(&id))
            {
                sounding.insert(id, (port, note.channel, note.key));
            }
        }
        NOTE_OFF => {
            let off = unsafe { event.__field0.noteOff };
            if off.noteId != -1 {
                sounding.remove(&off.noteId);
            } else if !sounding.is_empty() {
                sounding.retain(|_, &mut (bus, channel, key)| {
                    (bus, i16::from(channel), i16::from(key)) != (port, off.channel, off.pitch)
                });
            }
        }
        _ => {}
    }
}

impl IEventListTrait for OutputEventList {
    unsafe fn getEventCount(&self) -> int32 {
        let _audit = plughost_core::audit::callback();
        lock(&self.output).events.len() as i32
    }
    /// Converted events are not handed back to the plugin.
    unsafe fn getEvent(&self, _index: int32, _event: *mut Event) -> tresult {
        let _audit = plughost_core::audit::callback();
        kResultFalse
    }
    unsafe fn addEvent(&self, event: *mut Event) -> tresult {
        let _audit = plughost_core::audit::callback();
        if event.is_null() {
            return kInvalidArgument;
        }
        let mut output = lock(&self.output);
        let event = unsafe { &*event };
        let Ok(offset) = usize::try_from(event.sampleOffset) else {
            output.unconvertible += 1;
            return kResultOk;
        };
        if output.sysex.saturating_add(declared_sysex(event)) > MAX_BLOCK_SYSEX_BYTES {
            return output.overflowed(event);
        }
        let converted = convert(event, &output.sounding).and_then(|(port, data, tuning)| {
            let event = PortEvent::new(offset, data).on_port(port);
            let tuning = tuning.map(|data| PortEvent::new(offset, data).on_port(port));
            (event.is_valid() && tuning.as_ref().is_none_or(PortEvent::is_valid))
                .then_some((event, tuning))
        });
        let Some((converted, tuning)) = converted else {
            follow(&mut output.sounding, event, None);
            output.unconvertible += 1;
            return kResultOk;
        };
        let sysex = match converted.data {
            EventData::SysEx(bytes) => bytes.len(),
            _ => 0,
        };
        let count = 1 + usize::from(tuning.is_some());
        if output.events.len() + count > MAX_BLOCK_EVENTS {
            return output.overflowed(event);
        }
        follow(&mut output.sounding, event, Some(&converted.data));
        output.sysex += sysex;
        // The checks above keep the events within the block's room.
        let added = output.events.push(converted);
        let added = added.and_then(|()| tuning.map_or(Ok(()), |tuning| output.events.push(tuning)));
        if added.is_err() {
            return output.overflowed(event);
        }
        kResultOk
    }
}

#[cfg(test)]
#[path = "output_events_tests.rs"]
mod tests;
