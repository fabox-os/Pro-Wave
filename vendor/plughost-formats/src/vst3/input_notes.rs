//! The notes the host has started in a VST3 plugin and not ended yet. VST3 addresses a note by
//! its ID, and a plugin takes note ID -1 for the note its key started without one, so a note off
//! or an expression without an ID goes to each note on its key by that note's own ID.

use plughost_core::MAX_BLOCK_EVENTS;

use super::errors::Vst3Error;

/// A started note: event bus, channel, key and native note ID, -1 without one.
#[derive(Clone, Copy)]
struct Started {
    bus: i32,
    channel: i16,
    pitch: i16,
    id: i32,
}

impl Started {
    fn on_key(&self, bus: i32, channel: i16, pitch: i16) -> bool {
        self.bus == bus && self.channel == channel && self.pitch == pitch
    }
}

/// Storage for a full block is reserved up front; notes beyond it are not followed.
pub(crate) struct InputNotes {
    started: Vec<Started>,
}

impl InputNotes {
    pub fn new() -> Result<InputNotes, Vst3Error> {
        let mut started = Vec::new();
        started
            .try_reserve_exact(MAX_BLOCK_EVENTS)
            .map_err(|_| Vst3Error::EventStorage)?;
        Ok(InputNotes { started })
    }

    /// The plugin ended every note.
    pub fn clear(&mut self) {
        self.started.clear();
    }

    /// Takes the notes `other` follows, within the storage reserved for them.
    pub fn copy_from(&mut self, other: &InputNotes) {
        self.started.clear();
        self.started.extend_from_slice(&other.started);
    }

    pub fn note_on(&mut self, bus: i32, channel: i16, pitch: i16, id: i32) {
        if self.started.len() < MAX_BLOCK_EVENTS {
            self.started.push(Started {
                bus,
                channel,
                pitch,
                id,
            });
        }
    }

    /// Ends the notes a note off addresses and passes each ID the plugin needs a note off with:
    /// the note off's own ID, or without one the ID of each note on its key, and -1 for the notes
    /// there without an ID or when none is known to sound.
    pub fn note_off(
        &mut self,
        bus: i32,
        channel: i16,
        pitch: i16,
        id: i32,
        mut end: impl FnMut(i32),
    ) {
        if id != -1 {
            self.started.retain(|note| note.bus != bus || note.id != id);
            end(id);
            return;
        }
        let (mut on_key, mut without_id) = (false, false);
        self.started.retain(|note| {
            if !note.on_key(bus, channel, pitch) {
                return true;
            }
            on_key = true;
            if note.id == -1 {
                without_id = true;
            } else {
                end(note.id);
            }
            false
        });
        if without_id || !on_key {
            end(-1);
        }
    }

    /// The IDs of the notes on a key that have one, which an expression without an ID reaches.
    pub fn ids(&self, bus: i32, channel: i16, pitch: i16) -> impl Iterator<Item = i32> {
        self.started
            .iter()
            .filter(move |note| note.on_key(bus, channel, pitch) && note.id != -1)
            .map(|note| note.id)
    }
}
