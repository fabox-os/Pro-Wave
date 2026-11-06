//! The MIDI input port that Pro Wave follows the clock of (Synchronization > Follow MIDI Clock).
//! The port's thread only time-stamps the messages and queues them; the app reads the queue once
//! a frame (see `clock_follow.rs` for what they do).

use midir::{MidiInput, MidiInputConnection};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

/// How often to look again for a followed port that is not there (listing the ports is not free).
const RETRY: Duration = Duration::from_secs(3);

/// A message as the port delivered it: its arrival time (µs) and its bytes (at most three; a
/// longer one, such as System Exclusive, is not a clock message and is dropped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub at_us: u64,
    pub bytes: [u8; 3],
    pub len: usize,
}

impl Arrival {
    pub fn data(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The names of the MIDI input ports on this computer, without the system's "Through" port.
pub fn ports() -> Vec<String> {
    let Ok(input) = MidiInput::new("Pro Wave clock") else {
        return Vec::new();
    };
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .filter(|n| !n.to_lowercase().contains("through"))
        .collect()
}

/// The connection to the clock's input port, and the messages it has delivered.
pub struct ClockIn {
    connection: Option<MidiInputConnection<()>>,
    port: Option<String>,
    /// The port asked for, and when the last look for it was made.
    wanted: Option<String>,
    last_try: Option<Instant>,
    tx: SyncSender<Arrival>,
    rx: Receiver<Arrival>,
}

impl Default for ClockIn {
    fn default() -> Self {
        // Bounded: a flood of messages cannot fill the memory (the surplus is dropped).
        let (tx, rx) = sync_channel(4096);
        Self {
            connection: None,
            port: None,
            wanted: None,
            last_try: None,
            tx,
            rx,
        }
    }
}

impl ClockIn {
    /// Follows the port named `want`, or nothing (`None`). A change closes the old port.
    pub fn sync(&mut self, want: Option<&str>) {
        if self.port.as_deref() == want {
            self.wanted = want.map(str::to_string);
            return;
        }
        // A new port is looked for at once; a port that is not there is looked for again only
        // every few seconds.
        let changed = self.wanted.as_deref() != want;
        if !changed && self.last_try.is_some_and(|t| t.elapsed() < RETRY) {
            return;
        }
        self.wanted = want.map(str::to_string);
        self.last_try = Some(Instant::now());
        self.connection = None;
        self.port = None;
        let Some(name) = want else {
            return;
        };
        let Ok(input) = MidiInput::new("Pro Wave clock") else {
            return;
        };
        let Some(port) = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).is_ok_and(|n| n == name))
        else {
            return;
        };
        let tx = self.tx.clone();
        match input.connect(
            &port,
            "prowave-clock-in",
            move |at_us, msg, _| {
                if msg.is_empty() || msg.len() > 3 {
                    return;
                }
                let mut bytes = [0u8; 3];
                bytes[..msg.len()].copy_from_slice(msg);
                let _ = tx.try_send(Arrival {
                    at_us,
                    bytes,
                    len: msg.len(),
                });
            },
            (),
        ) {
            Ok(connection) => {
                tracing::info!("following MIDI clock from {name}");
                self.connection = Some(connection);
                self.port = Some(name.to_string());
            }
            Err(e) => tracing::warn!("cannot follow MIDI clock from {name}: {e}"),
        }
    }

    /// Appends the messages that arrived since the last call to `out`, in arrival order.
    pub fn poll(&mut self, out: &mut Vec<Arrival>) {
        while let Ok(a) = self.rx.try_recv() {
            out.push(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads from a real ALSA port: a virtual output that midir creates here plays the clock
    /// device, and the follower's input connects to it by name. Skipped (with a note) where the
    /// system has no MIDI sequencer.
    #[cfg(target_os = "linux")]
    #[test]
    fn clock_bytes_from_a_real_port_are_read_in_order() {
        use midir::os::unix::VirtualOutput;
        use std::time::{Duration, Instant};
        let Ok(out) = midir::MidiOutput::new("prowave-clock-device") else {
            println!("no MIDI system: skipped");
            return;
        };
        let Ok(mut device) = out.create_virtual("ProWave Clock Device") else {
            println!("no virtual MIDI port: skipped");
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        let Some(name) = ports()
            .into_iter()
            .find(|n| n.contains("ProWave Clock Device"))
        else {
            println!("the virtual port is not listed: skipped");
            return;
        };
        let mut clock = ClockIn::default();
        clock.sync(Some(&name));
        std::thread::sleep(Duration::from_millis(100));
        for bytes in [&[0xfau8][..], &[0xf8], &[0xf2, 16, 0]] {
            assert!(device.send(bytes).is_ok());
        }
        let mut got = Vec::new();
        let until = Instant::now() + Duration::from_secs(3);
        while got.len() < 3 && Instant::now() < until {
            clock.poll(&mut got);
            std::thread::sleep(Duration::from_millis(10));
        }
        let data: Vec<Vec<u8>> = got.iter().map(|a| a.data().to_vec()).collect();
        assert_eq!(data, vec![vec![0xfa], vec![0xf8], vec![0xf2, 16, 0]]);
        assert!(
            got.windows(2).all(|w| w[0].at_us <= w[1].at_us),
            "time order"
        );
    }

    /// A followed port that is not there is looked for once, and again only after the retry
    /// time: the port list is not asked every frame.
    #[test]
    fn a_missing_clock_port_is_looked_for_at_most_every_few_seconds() {
        let mut c = ClockIn::default();
        c.sync(Some("No Such Clock Port 5b1e"));
        let first = c.last_try;
        assert!(first.is_some(), "the first look is made at once");
        c.sync(Some("No Such Clock Port 5b1e"));
        assert_eq!(c.last_try, first, "no second look within the retry time");
        c.sync(None);
        assert!(c.wanted.is_none() && c.port.is_none());
    }
}
