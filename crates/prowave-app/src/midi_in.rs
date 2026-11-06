//! MIDI keyboard input: connects to the first MIDI input port (not the system's "Through"
//! port) and turns its note messages into [`LiveNote`]s for the app to play and record.
//! Without a MIDI device nothing happens; the app looks again every few seconds.

use crate::recorder::LiveNote;
use midir::{MidiInput, MidiInputConnection};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

/// How often to look for a MIDI keyboard while none is connected.
const RESCAN: Duration = Duration::from_secs(3);

pub struct MidiIn {
    connection: Option<MidiInputConnection<()>>,
    port: Option<String>,
    /// A port another input uses (MIDI Time Code): this one leaves it alone, since some systems
    /// let only one program or connection open a port.
    avoided: Option<String>,
    /// The port the project asks for (Project Settings > MIDI), used when it is there.
    preferred: Option<String>,
    tx: SyncSender<LiveNote>,
    rx: Receiver<LiveNote>,
    last_try: Option<Instant>,
}

/// Which of the ports to open: the one the project asks for if it is there, else the first
/// that is not a system "Through" port. A port another input uses (`avoided`) is never taken.
fn pick_port(names: &[String], preferred: Option<&str>, avoided: Option<&str>) -> Option<usize> {
    let usable = |name: &str| !name.is_empty() && avoided != Some(name);
    preferred
        .and_then(|want| names.iter().position(|n| n == want && usable(n)))
        .or_else(|| {
            names
                .iter()
                .position(|n| !n.to_lowercase().contains("through") && usable(n))
        })
}

/// The note, sustain pedal, pitch bend or channel pressure in a raw MIDI message, if it is one
/// (Note On with velocity 0 is a Note Off). The channel is ignored; other messages are too.
pub fn parse(msg: &[u8]) -> Option<LiveNote> {
    let status = *msg.first()?;
    match status & 0xf0 {
        // Pitch bend: two data bytes, the least significant 7 bits first.
        0xe0 => {
            let [_, lsb, msb, ..] = *msg else {
                return None;
            };
            (lsb <= 127 && msb <= 127).then_some(LiveNote::Bend {
                val: u16::from(lsb) | (u16::from(msb) << 7),
            })
        }
        // Channel pressure: one data byte.
        0xd0 => {
            let [_, val, ..] = *msg else {
                return None;
            };
            (val <= 127).then_some(LiveNote::Pressure { val })
        }
        _ => {
            let [_, pitch, vel, ..] = *msg else {
                return None;
            };
            if pitch > 127 {
                return None;
            }
            match status & 0xf0 {
                0xb0 if pitch == crate::recorder::SUSTAIN => {
                    Some(LiveNote::Sustain { val: vel.min(127) })
                }
                0x90 if vel > 0 => Some(LiveNote::On {
                    pitch,
                    vel: vel.min(127),
                }),
                0x90 | 0x80 => Some(LiveNote::Off { pitch }),
                _ => None,
            }
        }
    }
}

impl MidiIn {
    pub fn new() -> Self {
        // Bounded: a device that floods the port cannot fill the memory.
        let (tx, rx) = sync_channel(4096);
        let mut midi = Self {
            connection: None,
            port: None,
            avoided: None,
            preferred: None,
            tx,
            rx,
            last_try: None,
        };
        midi.connect();
        midi
    }

    /// The name of the connected keyboard.
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port.as_deref()
    }

    /// Keeps away from the port named `name` (or stops doing so with `None`): a connection to
    /// it is closed, and the keyboard is looked for on the other ports.
    pub fn avoid(&mut self, name: Option<&str>) {
        if self.avoided.as_deref() == name {
            return;
        }
        self.avoided = name.map(str::to_string);
        if self.port.is_some() && self.port == self.avoided {
            self.connection = None;
            self.port = None;
        }
        // Look again at once: the port that was avoided may be the keyboard.
        self.last_try = None;
    }

    /// Asks for the port named `name` (or any keyboard again with `None`): a connection to
    /// another port is closed, and the wanted one is opened as soon as it is there.
    pub fn prefer(&mut self, name: Option<&str>) {
        if self.preferred.as_deref() == name {
            return;
        }
        self.preferred = name.map(str::to_string);
        if self.preferred.is_some() && self.port != self.preferred {
            self.connection = None;
            self.port = None;
        }
        self.last_try = None;
    }

    fn connect(&mut self) {
        self.last_try = Some(Instant::now());
        let Ok(input) = MidiInput::new("Pro Wave") else {
            return;
        };
        let ports = input.ports();
        let names: Vec<String> = ports
            .iter()
            .map(|p| input.port_name(p).unwrap_or_default())
            .collect();
        let Some(index) = pick_port(&names, self.preferred.as_deref(), self.avoided.as_deref())
        else {
            return;
        };
        let (port, name) = (&ports[index], names[index].clone());
        let tx = self.tx.clone();
        match input.connect(
            port,
            "prowave-in",
            move |_stamp, msg, _| {
                if let Some(note) = parse(msg) {
                    let _ = tx.try_send(note);
                }
            },
            (),
        ) {
            Ok(connection) => {
                tracing::info!("MIDI input: {name}");
                self.connection = Some(connection);
                self.port = Some(name);
            }
            Err(e) => tracing::warn!("cannot open MIDI input {name}: {e}"),
        }
    }

    /// Appends the notes played since the last call to `out`.
    pub fn poll(&mut self, out: &mut Vec<LiveNote>) {
        if self.connection.is_none() && self.last_try.is_none_or(|t| t.elapsed() >= RESCAN) {
            self.connect();
        }
        while let Ok(note) = self.rx.try_recv() {
            out.push(note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_messages_become_live_notes_on_any_channel() {
        assert_eq!(
            parse(&[0x90, 60, 100]),
            Some(LiveNote::On {
                pitch: 60,
                vel: 100
            })
        );
        assert_eq!(
            parse(&[0x93, 72, 1]),
            Some(LiveNote::On { pitch: 72, vel: 1 })
        );
        assert_eq!(parse(&[0x80, 60, 64]), Some(LiveNote::Off { pitch: 60 }));
        // Note On with velocity 0 is a Note Off.
        assert_eq!(parse(&[0x90, 60, 0]), Some(LiveNote::Off { pitch: 60 }));
    }

    #[test]
    fn the_sustain_pedal_is_controller_64() {
        assert_eq!(
            parse(&[0xb0, 64, 127]),
            Some(LiveNote::Sustain { val: 127 })
        );
        assert_eq!(parse(&[0xb2, 64, 0]), Some(LiveNote::Sustain { val: 0 }));
    }

    #[test]
    fn pitch_bend_and_channel_pressure_are_live_events() {
        assert_eq!(
            parse(&[0xe0, 0, 64]),
            Some(LiveNote::Bend { val: 8192 }),
            "centre"
        );
        assert_eq!(
            parse(&[0xe3, 0x7f, 0x7f]),
            Some(LiveNote::Bend { val: 16383 })
        );
        assert_eq!(parse(&[0xe0, 0, 0]), Some(LiveNote::Bend { val: 0 }));
        assert_eq!(parse(&[0xd0, 90]), Some(LiveNote::Pressure { val: 90 }));
        assert_eq!(parse(&[0xd1, 127]), Some(LiveNote::Pressure { val: 127 }));
        assert_eq!(parse(&[0xe0, 0]), None, "cut short");
        assert_eq!(parse(&[0xd0]), None, "cut short");
    }

    #[test]
    fn other_messages_are_ignored() {
        assert_eq!(parse(&[0xb0, 1, 127]), None, "modulation");
        assert_eq!(parse(&[0xf8]), None, "clock");
        assert_eq!(parse(&[0x90, 60]), None, "cut short");
        assert_eq!(parse(&[0x90, 200, 100]), None, "pitch out of range");
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn the_wanted_port_wins_when_it_is_there() {
        let names: Vec<String> = ["Midi Through", "Keys A", "Keys B"]
            .map(String::from)
            .to_vec();
        // Nothing asked for: the first one that is not the system's Through port.
        assert_eq!(pick_port(&names, None, None), Some(1));
        assert_eq!(pick_port(&names, Some("Keys B"), None), Some(2));
        // The wanted port is gone: any keyboard does.
        assert_eq!(pick_port(&names, Some("Keys C"), None), Some(1));
        // A port another input uses is left alone, even when it is the wanted one.
        assert_eq!(pick_port(&names, Some("Keys B"), Some("Keys B")), Some(1));
        assert_eq!(pick_port(&names, None, Some("Keys A")), Some(2));
        assert_eq!(pick_port(&[], Some("Keys A"), None), None);
    }
}
