//! MIDI out for External MIDI tracks: the notes the engine sends for those tracks go to the
//! MIDI port and channel each track is set to. A thread of its own does the sending, so a slow
//! or missing MIDI device never holds up the audio or the interface.
//!
//! The notes are sent as soon as the thread sees them (a millisecond after the audio block
//! that played them), not at their exact sample, which is fine for a synthesizer on a cable.
//!
//! The same thread sends MIDI Time Code, which the engine makes on the audio clock, to the port
//! the project names (`Project::mtc_out`).

use crate::model::{Project, TrackKind, CHANNEL_PRESSURE, PITCH_BEND, SYSEX_BYTE};
use midir::{MidiOutput, MidiOutputConnection};
use prowave_engine::{ClockMessage, MidiOutEvent, MtcMessage};
use rtrb::Consumer;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long the sending thread sleeps between looks at the queue.
const POLL: Duration = Duration::from_millis(1);
/// How often to try again to open a port that could not be opened.
const RETRY: Duration = Duration::from_secs(3);

/// Where one track's notes go: a port (by name), a channel 1-16, and the sound the track asks
/// for when it is routed: a bank and a program (`None` leaves the device as it is).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Route {
    pub port: Option<String>,
    pub channel: u8,
    pub program: Option<u8>,
    pub bank: Option<u16>,
}

/// The raw MIDI message for a note.
pub fn message(on: bool, channel: u8, pitch: u8, vel: u8) -> [u8; 3] {
    let status = if on { 0x90 } else { 0x80 };
    [
        status | (channel.clamp(1, 16) - 1),
        pitch.min(127),
        vel.min(127),
    ]
}

/// The raw MIDI message for a controller change (the sustain pedal is controller 64).
pub fn controller_message(channel: u8, number: u8, value: u8) -> [u8; 3] {
    [
        0xb0 | (channel.clamp(1, 16) - 1),
        number.min(127),
        value.min(127),
    ]
}

/// The pitch bend message of a channel: the value is 14 bits, the least significant 7 first.
pub fn pitch_bend_message(channel: u8, value: u16) -> [u8; 3] {
    let v = value.min(16383);
    [
        0xe0 | (channel.clamp(1, 16) - 1),
        (v & 0x7f) as u8,
        (v >> 7) as u8,
    ]
}

/// The channel pressure message of a channel.
pub fn channel_pressure_message(channel: u8, value: u16) -> [u8; 2] {
    [0xd0 | (channel.clamp(1, 16) - 1), value.min(127) as u8]
}

/// The longest System Exclusive message the sender will hold back (a longer one is dropped).
const MAX_SYSEX: usize = 4096;

/// The messages that select a sound on a channel: Bank Select (controllers 0 and 32, the most
/// and least significant 7 bits of the bank) when there is a bank, then Program Change.
pub fn program_messages(channel: u8, program: Option<u8>, bank: Option<u16>) -> Vec<Vec<u8>> {
    let ch = channel.clamp(1, 16) - 1;
    let mut out = Vec::new();
    if let Some(bank) = bank {
        let bank = bank.min(16383);
        out.push(vec![0xb0 | ch, 0, ((bank >> 7) & 0x7f) as u8]);
        out.push(vec![0xb0 | ch, 32, (bank & 0x7f) as u8]);
    }
    if let Some(program) = program {
        out.push(vec![0xc0 | ch, program.min(127)]);
    }
    out
}

/// The "all sound off" and "all notes off" messages of one channel: MIDI panic.
pub fn panic_messages(channel: u8) -> [[u8; 3]; 2] {
    [
        controller_message(channel, 120, 0),
        controller_message(channel, 123, 0),
    ]
}

/// The names of the MIDI output ports on this computer, without the system's "Through" port.
pub fn ports() -> Vec<String> {
    let Ok(out) = MidiOutput::new("Pro Wave") else {
        return Vec::new();
    };
    out.ports()
        .iter()
        .filter_map(|p| out.port_name(p).ok())
        .filter(|n| !n.to_lowercase().contains("through"))
        .collect()
}

/// The routes of a project, by track: only External MIDI tracks have a port.
pub fn routes_of(p: &Project) -> Vec<Route> {
    p.tracks
        .iter()
        .map(|t| {
            if t.kind == TrackKind::ExternalMidi {
                Route {
                    port: t.midi_port.clone(),
                    channel: t.midi_channel,
                    program: t.midi_program,
                    bank: t.midi_bank,
                }
            } else {
                Route::default()
            }
        })
        .collect()
}

/// Where bytes go: an open MIDI output port, or a recorder in tests.
pub trait Sink: Send {
    /// Sends one message; `false` when the device refused it.
    fn send(&mut self, bytes: &[u8]) -> bool;
}

struct Port(MidiOutputConnection);

impl Sink for Port {
    fn send(&mut self, bytes: &[u8]) -> bool {
        self.0.send(bytes).is_ok()
    }
}

/// Opens the sink of a port by name (`None` if it is not there).
pub type Opener = Box<dyn FnMut(&str) -> Option<Box<dyn Sink>> + Send>;

/// The sending thread and the routes it follows.
pub struct MidiOut {
    routes: Arc<Mutex<Vec<Route>>>,
    mtc_port: Arc<Mutex<Option<String>>>,
    clock_port: Arc<Mutex<Option<String>>>,
    last: Vec<Route>,
    last_mtc: Option<String>,
    last_clock: Option<String>,
    panic: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The queues the engine gives the MIDI sender.
pub struct Queues {
    pub notes: Consumer<MidiOutEvent>,
    pub mtc: Consumer<MtcMessage>,
    pub clock: Consumer<ClockMessage>,
}

impl MidiOut {
    /// Starts sending what the engine delivers, to the ports the app names.
    pub fn start(queues: Queues) -> Self {
        let open: Opener =
            Box::new(|name: &str| open_port(name).map(|c| Box::new(Port(c)) as Box<dyn Sink>));
        Self::start_with(queues, open)
    }

    /// Like [`MidiOut::start`], with the function that opens ports (tests use a recorder).
    pub fn start_with(queues: Queues, open: Opener) -> Self {
        let routes = Arc::new(Mutex::new(Vec::new()));
        let mtc_port = Arc::new(Mutex::new(None));
        let clock_port = Arc::new(Mutex::new(None));
        let panic = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let ports = Ports {
            routes: routes.clone(),
            mtc: mtc_port.clone(),
            clock: clock_port.clone(),
        };
        let (pa, s) = (panic.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("prowave-midi-out".into())
            .spawn(move || Sender::new(queues, ports, pa, s, open).run())
            .ok();
        Self {
            routes,
            mtc_port,
            clock_port,
            last: Vec::new(),
            last_mtc: None,
            last_clock: None,
            panic,
            stop,
            thread,
        }
    }

    /// Call once per frame: passes the tracks' ports, channels and sounds, and the ports of MIDI
    /// Time Code and MIDI Clock, on when they changed.
    pub fn sync(&mut self, p: &Project) {
        let now = routes_of(p);
        if now != self.last {
            if let Ok(mut routes) = self.routes.lock() {
                routes.clone_from(&now);
            }
            self.last = now;
        }
        if p.mtc_out != self.last_mtc {
            if let Ok(mut port) = self.mtc_port.lock() {
                port.clone_from(&p.mtc_out);
            }
            self.last_mtc.clone_from(&p.mtc_out);
        }
        if p.midi_clock_out != self.last_clock {
            if let Ok(mut port) = self.clock_port.lock() {
                port.clone_from(&p.midi_clock_out);
            }
            self.last_clock.clone_from(&p.midi_clock_out);
        }
    }

    /// MIDI panic (Project Settings > MIDI): every note on every output the tracks use is
    /// released, on all 16 channels.
    pub fn panic(&self) {
        self.panic.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for MidiOut {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the app tells the sender, shared with it: the routes and the three ports.
struct Ports {
    routes: Arc<Mutex<Vec<Route>>>,
    mtc: Arc<Mutex<Option<String>>>,
    clock: Arc<Mutex<Option<String>>>,
}

struct Sender {
    events: Consumer<MidiOutEvent>,
    mtc_queue: Consumer<MtcMessage>,
    clock_queue: Consumer<ClockMessage>,
    ports: Ports,
    panic: Arc<AtomicU32>,
    panic_seen: u32,
    stop: Arc<AtomicBool>,
    open: Opener,
    connections: HashMap<String, Box<dyn Sink>>,
    failed: HashMap<String, Instant>,
    /// Notes that were started and not yet released: (track, port, channel, pitch). A note is
    /// released on the port it started on, even when its track was routed elsewhere since.
    sounding: HashSet<(u16, String, u8, u8)>,
    /// The System Exclusive message each track is in the middle of, byte by byte.
    sysex: HashMap<u16, Vec<u8>>,
    /// The routes as they were the last time they were followed.
    known: Vec<Route>,
}

impl Sender {
    fn new(
        queues: Queues,
        ports: Ports,
        panic: Arc<AtomicU32>,
        stop: Arc<AtomicBool>,
        open: Opener,
    ) -> Self {
        let panic_seen = panic.load(Ordering::Acquire);
        Self {
            events: queues.notes,
            mtc_queue: queues.mtc,
            clock_queue: queues.clock,
            ports,
            panic,
            panic_seen,
            stop,
            open,
            connections: HashMap::new(),
            failed: HashMap::new(),
            sounding: HashSet::new(),
            sysex: HashMap::new(),
            known: Vec::new(),
        }
    }

    fn run(mut self) {
        while !self.stop.load(Ordering::Acquire) {
            self.follow_routes();
            self.follow_panic();
            while let Ok(ev) = self.events.pop() {
                self.send(ev);
            }
            while let Ok(m) = self.mtc_queue.pop() {
                self.send_mtc(&m);
            }
            while let Ok(m) = self.clock_queue.pop() {
                self.send_clock(m);
            }
            std::thread::sleep(POLL);
        }
        self.release_all();
    }

    /// The current routes (a copy, so the lock is not held while sending).
    fn current_routes(&self) -> Vec<Route> {
        self.ports
            .routes
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Looks for routes that changed since the last look. A track that moved to another port or
    /// channel releases the notes it had sounding there; a track that now names a sound gets it.
    fn follow_routes(&mut self) {
        let now = self.current_routes();
        if now == self.known {
            return;
        }
        let old = std::mem::take(&mut self.known);
        for track in 0..old.len().max(now.len()) {
            let before = old.get(track).cloned().unwrap_or_default();
            let after = now.get(track).cloned().unwrap_or_default();
            if before == after {
                continue;
            }
            if let Some(port) = &before.port {
                if before.port != after.port || before.channel != after.channel {
                    self.release_track(track as u16, port, before.channel);
                }
            }
            if let Some(port) = after.port.clone() {
                let same_device = before.port.as_deref() == Some(port.as_str());
                if !same_device || before.program != after.program || before.bank != after.bank {
                    for msg in program_messages(after.channel, after.program, after.bank) {
                        self.write(&port, &msg);
                    }
                }
            }
        }
        self.known = now;
    }

    /// Releases the notes `track` has sounding on `port` and `channel`.
    fn release_track(&mut self, track: u16, port: &str, channel: u8) {
        let ours: Vec<(u16, String, u8, u8)> = self
            .sounding
            .iter()
            .filter(|(t, p, c, _)| *t == track && p == port && *c == channel)
            .cloned()
            .collect();
        for (_, p, c, pitch) in ours {
            self.sounding.remove(&(track, p.clone(), c, pitch));
            self.write(&p, &message(false, c, pitch, 0));
        }
    }

    /// MIDI panic: all sound off and all notes off on every channel of every output in use.
    fn follow_panic(&mut self) {
        let seen = self.panic.load(Ordering::Acquire);
        if seen == self.panic_seen {
            return;
        }
        self.panic_seen = seen;
        let mut ports: Vec<String> = self
            .current_routes()
            .into_iter()
            .filter_map(|r| r.port)
            .collect();
        ports.sort();
        ports.dedup();
        for port in &ports {
            for channel in 1..=16 {
                for msg in panic_messages(channel) {
                    self.write(port, &msg);
                }
            }
        }
        self.sounding.clear();
    }

    fn route_of(&self, track: u16) -> Option<(String, u8)> {
        let routes = self.ports.routes.lock().ok()?;
        let r = routes.get(track as usize)?;
        Some((r.port.clone()?, r.channel.clamp(1, 16)))
    }

    fn send(&mut self, ev: MidiOutEvent) {
        let Some((port, channel)) = self.route_of(ev.track) else {
            return;
        };
        if ev.controller {
            match ev.pitch {
                PITCH_BEND => {
                    self.write(&port, &pitch_bend_message(channel, ev.value));
                }
                CHANNEL_PRESSURE => {
                    self.write(&port, &channel_pressure_message(channel, ev.value));
                }
                SYSEX_BYTE => self.sysex_byte(ev.track, &port, ev.value.min(255) as u8),
                number => {
                    self.write(&port, &controller_message(channel, number, ev.vel));
                }
            }
            return;
        }
        let key = (ev.track, port.clone(), channel, ev.pitch);
        if ev.on {
            self.sounding.insert(key);
        } else if !self.sounding.remove(&key) {
            // Its Note On never went out (no port, or another one): nothing to release.
            return;
        }
        self.write(&port, &message(ev.on, channel, ev.pitch, ev.vel));
    }

    /// Sends a MIDI Time Code message to the project's MTC port, if it names one.
    fn send_mtc(&mut self, m: &MtcMessage) {
        let port = self.ports.mtc.lock().ok().and_then(|p| p.clone());
        if let Some(port) = port {
            self.write(&port, m.bytes());
        }
    }

    /// One byte of a System Exclusive message. The message starts at 0xF0 and goes out whole at
    /// its 0xF7; bytes that come without a start are ignored, and a message that grows too long
    /// is dropped.
    fn sysex_byte(&mut self, track: u16, port: &str, byte: u8) {
        let done = {
            let buf = self.sysex.entry(track).or_default();
            if byte == 0xf0 {
                buf.clear();
                buf.push(byte);
            } else if !buf.is_empty() {
                if buf.len() >= MAX_SYSEX {
                    buf.clear();
                } else {
                    buf.push(byte);
                }
            }
            if byte == 0xf7 && buf.len() > 1 {
                Some(std::mem::take(buf))
            } else {
                None
            }
        };
        if let Some(message) = done {
            self.write(port, &message);
        }
    }

    /// Sends a MIDI Clock message to the project's clock port, if it names one.
    fn send_clock(&mut self, m: ClockMessage) {
        let port = self.ports.clock.lock().ok().and_then(|p| p.clone());
        if let Some(port) = port {
            let (bytes, len) = m.bytes();
            self.write(&port, &bytes[..len]);
        }
    }

    /// Sends `bytes` to `port`, opening it when needed. Returns whether it went out.
    fn write(&mut self, port: &str, bytes: &[u8]) -> bool {
        match self.connection(port) {
            Some(sink) => sink.send(bytes),
            None => false,
        }
    }

    /// The open connection to `port`, opening it when needed (and again every few seconds
    /// while it cannot be opened).
    fn connection(&mut self, port: &str) -> Option<&mut Box<dyn Sink>> {
        if !self.connections.contains_key(port) {
            if self.failed.get(port).is_some_and(|t| t.elapsed() < RETRY) {
                return None;
            }
            match (self.open)(port) {
                Some(sink) => {
                    self.failed.remove(port);
                    self.connections.insert(port.to_string(), sink);
                }
                None => {
                    self.failed.insert(port.to_string(), Instant::now());
                    return None;
                }
            }
        }
        self.connections.get_mut(port)
    }

    /// Releases every note still sounding (the app is closing, or the thread stopped).
    fn release_all(&mut self) {
        let sounding: Vec<_> = self.sounding.drain().collect();
        for (_, port, channel, pitch) in sounding {
            self.write(&port, &message(false, channel, pitch, 0));
        }
    }
}

fn open_port(port: &str) -> Option<MidiOutputConnection> {
    let out = MidiOutput::new("Pro Wave").ok()?;
    let found = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).is_ok_and(|n| n == port))?;
    out.connect(&found, "prowave-out").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use midir::os::unix::VirtualInput;
    use std::sync::Mutex as StdMutex;

    /// Everything sent, as (port, bytes), in order.
    type Log = Arc<StdMutex<Vec<(String, Vec<u8>)>>>;

    struct Rec {
        port: String,
        log: Log,
    }

    impl Sink for Rec {
        fn send(&mut self, bytes: &[u8]) -> bool {
            self.log
                .lock()
                .unwrap()
                .push((self.port.clone(), bytes.to_vec()));
            true
        }
    }

    /// A sender with recording ports, not started as a thread: the tests drive it directly.
    /// The sender, the queue that feeds it, what it sent, its routes and its panic counter.
    type Rig = (
        Sender,
        rtrb::Producer<MidiOutEvent>,
        Log,
        Arc<Mutex<Vec<Route>>>,
        Arc<AtomicU32>,
    );

    fn rig(routes: Vec<Route>) -> Rig {
        let (tx, rx) = rtrb::RingBuffer::new(64);
        let (_mtc_tx, mtc_rx) = rtrb::RingBuffer::new(8);
        let (_clock_tx, clock_rx) = rtrb::RingBuffer::new(8);
        let log: Log = Arc::new(StdMutex::new(Vec::new()));
        let shared = Arc::new(Mutex::new(routes));
        let panic = Arc::new(AtomicU32::new(0));
        let l = log.clone();
        let open: Opener = Box::new(move |name: &str| {
            Some(Box::new(Rec {
                port: name.to_string(),
                log: l.clone(),
            }) as Box<dyn Sink>)
        });
        let sender = Sender::new(
            Queues {
                notes: rx,
                mtc: mtc_rx,
                clock: clock_rx,
            },
            Ports {
                routes: shared.clone(),
                mtc: Arc::new(Mutex::new(None)),
                clock: Arc::new(Mutex::new(None)),
            },
            panic.clone(),
            Arc::new(AtomicBool::new(false)),
            open,
        );
        (sender, tx, log, shared, panic)
    }

    fn ev(track: u16, on: bool, pitch: u8) -> MidiOutEvent {
        MidiOutEvent {
            track,
            on,
            pitch,
            vel: 90,
            controller: false,
            value: 0,
        }
    }

    /// Runs what is queued, as the thread does between two looks at the routes.
    fn pump(sender: &mut Sender) {
        sender.follow_routes();
        sender.follow_panic();
        while let Ok(e) = sender.events.pop() {
            sender.send(e);
        }
    }

    fn sent(log: &Log) -> Vec<(String, Vec<u8>)> {
        log.lock().unwrap().clone()
    }

    fn route(port: &str, channel: u8) -> Route {
        Route {
            port: Some(port.into()),
            channel,
            ..Route::default()
        }
    }

    #[test]
    fn notes_are_raw_midi_on_their_channel() {
        assert_eq!(message(true, 1, 60, 100), [0x90, 60, 100]);
        assert_eq!(message(false, 1, 60, 0), [0x80, 60, 0]);
        assert_eq!(message(true, 16, 127, 127), [0x9f, 127, 127]);
        assert_eq!(
            message(true, 0, 200, 200),
            [0x90, 127, 127],
            "out of range is repaired"
        );
        assert_eq!(
            message(false, 10, 36, 64),
            [0x89, 36, 64],
            "channel 10 is drums"
        );
    }

    #[test]
    fn pitch_bend_and_pressure_are_raw_midi_on_their_channel() {
        assert_eq!(pitch_bend_message(1, 8192), [0xe0, 0, 0x40]);
        assert_eq!(pitch_bend_message(3, 16383), [0xe2, 0x7f, 0x7f]);
        assert_eq!(
            pitch_bend_message(1, u16::MAX),
            [0xe0, 0x7f, 0x7f],
            "clamped"
        );
        assert_eq!(channel_pressure_message(16, 90), [0xdf, 90]);
        assert_eq!(channel_pressure_message(1, 500), [0xd0, 127]);
    }

    #[test]
    fn controllers_are_raw_midi_on_their_channel() {
        assert_eq!(controller_message(1, 64, 127), [0xb0, 64, 127]);
        assert_eq!(controller_message(16, 1, 0), [0xbf, 1, 0]);
        assert_eq!(controller_message(0, 200, 200), [0xb0, 127, 127]);
    }

    #[test]
    fn a_sound_is_bank_select_then_program_change() {
        assert_eq!(
            program_messages(3, Some(5), Some(0x1234)),
            vec![vec![0xb2, 0, 0x24], vec![0xb2, 32, 0x34], vec![0xc2, 5]],
            "bank 0x1234 = MSB 0x24, LSB 0x34"
        );
        assert_eq!(program_messages(1, Some(127), None), vec![vec![0xc0, 127]]);
        assert_eq!(
            program_messages(1, None, Some(1)),
            vec![vec![0xb0, 0, 0], vec![0xb0, 32, 1]]
        );
        assert!(
            program_messages(1, None, None).is_empty(),
            "no sound asked for"
        );
        assert_eq!(
            program_messages(1, Some(200), Some(65535)).last().unwrap(),
            &vec![0xc0, 127]
        );
    }

    #[test]
    fn only_external_tracks_have_a_route() {
        let mut p = Project::empty();
        p.tracks.clear();
        p.tracks
            .push(crate::model::Track::new("Inst", TrackKind::Instrument));
        let mut ext = crate::model::Track::new("Ext", TrackKind::ExternalMidi);
        ext.midi_port = Some("Synth".into());
        ext.midi_channel = 3;
        ext.midi_program = Some(9);
        ext.midi_bank = Some(2);
        p.tracks.push(ext);
        let routes = routes_of(&p);
        assert_eq!(routes[0], Route::default());
        assert_eq!(
            routes[1],
            Route {
                port: Some("Synth".into()),
                channel: 3,
                program: Some(9),
                bank: Some(2),
            }
        );
    }

    #[test]
    fn notes_go_to_the_routed_port_and_channel_only() {
        let (mut sender, mut tx, log, _routes, _) = rig(vec![Route::default(), route("Synth", 3)]);
        for track in [0u16, 1, 9] {
            assert!(tx.push(ev(track, true, 60)).is_ok());
        }
        pump(&mut sender);
        assert_eq!(sent(&log), vec![("Synth".to_string(), vec![0x92, 60, 90])]);
    }

    #[test]
    fn a_missing_port_is_not_fatal_and_its_note_is_released_later() {
        let (mut sender, mut tx, log, _routes, _) = rig(vec![route("No Such Port 7f3a", 1)]);
        // The opener finds every port in the tests, so make this one missing.
        sender.open = Box::new(|_: &str| None);
        assert!(tx.push(ev(0, true, 60)).is_ok());
        pump(&mut sender);
        assert!(sent(&log).is_empty());
        assert!(sender.failed.contains_key("No Such Port 7f3a"));
        assert!(sender
            .sounding
            .contains(&(0, "No Such Port 7f3a".to_string(), 1, 60)));
    }

    #[test]
    fn moving_a_track_to_another_port_releases_its_sounding_notes_on_the_old_one() {
        let (mut sender, mut tx, log, routes, _) = rig(vec![route("A", 1)]);
        assert!(tx.push(ev(0, true, 60)).is_ok());
        pump(&mut sender);
        // The track is re-routed while the note sounds, to another port and channel.
        *routes.lock().unwrap() = vec![route("B", 2)];
        assert!(tx.push(ev(0, false, 60)).is_ok());
        pump(&mut sender);
        let out = sent(&log);
        assert!(out.contains(&("A".to_string(), vec![0x90, 60, 90])));
        assert!(
            out.contains(&("A".to_string(), vec![0x80, 60, 0])),
            "released on A: {out:?}"
        );
        assert!(
            !out.iter().any(|(p, _)| p == "B"),
            "nothing goes to B for a note that never started there: {out:?}"
        );
        assert!(sender.sounding.is_empty());
    }

    #[test]
    fn a_routed_sound_is_sent_when_the_route_is_set() {
        let (mut sender, _tx, log, routes, _) = rig(vec![Route::default()]);
        pump(&mut sender);
        assert!(sent(&log).is_empty(), "no port, no sound");
        *routes.lock().unwrap() = vec![Route {
            port: Some("Synth".into()),
            channel: 2,
            program: Some(40),
            bank: Some(1),
        }];
        pump(&mut sender);
        assert_eq!(
            sent(&log),
            vec![
                ("Synth".to_string(), vec![0xb1, 0, 0]),
                ("Synth".to_string(), vec![0xb1, 32, 1]),
                ("Synth".to_string(), vec![0xc1, 40]),
            ]
        );
        // Nothing changed, so nothing is sent again.
        pump(&mut sender);
        assert_eq!(sent(&log).len(), 3);
    }

    #[test]
    fn panic_releases_every_channel_of_every_port_in_use() {
        let (mut sender, mut tx, log, _routes, panic) = rig(vec![route("Synth", 1)]);
        assert!(tx.push(ev(0, true, 64)).is_ok());
        pump(&mut sender);
        log.lock().unwrap().clear();
        panic.fetch_add(1, Ordering::AcqRel);
        pump(&mut sender);
        let out = sent(&log);
        assert_eq!(
            out.len(),
            32,
            "all sound off and all notes off on 16 channels"
        );
        assert!(out.iter().all(|(p, _)| p == "Synth"));
        assert!(out.contains(&("Synth".to_string(), vec![0xb0, 123, 0])));
        assert!(out.contains(&("Synth".to_string(), vec![0xbf, 120, 0])));
        assert!(sender.sounding.is_empty());
        // A second panic is a second request; nothing else is pending.
        log.lock().unwrap().clear();
        pump(&mut sender);
        assert!(sent(&log).is_empty());
    }

    #[test]
    fn stopping_releases_every_note_still_sounding() {
        let (mut sender, mut tx, log, _routes, _) = rig(vec![route("Synth", 1), route("Other", 2)]);
        assert!(tx.push(ev(0, true, 60)).is_ok());
        assert!(tx.push(ev(1, true, 64)).is_ok());
        pump(&mut sender);
        log.lock().unwrap().clear();
        sender.release_all();
        let mut out = sent(&log);
        out.sort();
        assert_eq!(
            out,
            vec![
                ("Other".to_string(), vec![0x81, 64, 0]),
                ("Synth".to_string(), vec![0x80, 60, 0]),
            ]
        );
    }

    /// Sends through a real ALSA port: a virtual input that midir creates here, which the sender
    /// then opens by name like any keyboard. Skipped (with a note) where the system has no MIDI
    /// sequencer.
    #[cfg(target_os = "linux")]
    #[test]
    fn bytes_reach_a_real_midi_port() {
        let Ok(input) = midir::MidiInput::new("prowave-test-sink") else {
            println!("no MIDI system: skipped");
            return;
        };
        let got: Arc<StdMutex<Vec<Vec<u8>>>> = Arc::new(StdMutex::new(Vec::new()));
        let g = got.clone();
        let name = "ProWave Test Sink";
        let Ok(_sink) = input.create_virtual(
            name,
            move |_, msg, _| g.lock().unwrap().push(msg.to_vec()),
            (),
        ) else {
            println!("no virtual MIDI port: skipped");
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        // The system names the port with its client ("client:port address"); a route stores the
        // name exactly as the port list gives it, so the test uses that name too.
        let Some(name) = ports().into_iter().find(|n| n.contains(name)) else {
            println!("the virtual port is not listed: skipped");
            return;
        };

        let (mut tx, rx) = rtrb::RingBuffer::new(16);
        let (_mtc_tx, mtc_rx) = rtrb::RingBuffer::new(8);
        let (mut clock_tx, clock_rx) = rtrb::RingBuffer::new(8);
        let routes = Arc::new(Mutex::new(vec![Route {
            port: Some(name.clone()),
            channel: 4,
            ..Route::default()
        }]));
        let mut sender = Sender::new(
            Queues {
                notes: rx,
                mtc: mtc_rx,
                clock: clock_rx,
            },
            Ports {
                routes,
                mtc: Arc::new(Mutex::new(None)),
                clock: Arc::new(Mutex::new(Some(name.clone()))),
            },
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicBool::new(false)),
            Box::new(|port: &str| open_port(port).map(|c| Box::new(Port(c)) as Box<dyn Sink>)),
        );
        assert!(tx.push(ev(0, true, 67)).is_ok());
        pump(&mut sender);
        // Pitch bend and a SysEx message on the same route.
        assert!(tx.push(ctl(0, PITCH_BEND, 8192)).is_ok());
        for b in [0xf0u8, 0x7e, 0x7f, 0xf7] {
            assert!(tx.push(ctl(0, SYSEX_BYTE, u16::from(b))).is_ok());
        }
        // The engine's Note Off carries velocity 0.
        assert!(tx
            .push(MidiOutEvent {
                track: 0,
                on: false,
                pitch: 67,
                vel: 0,
                controller: false,
                value: 0,
            })
            .is_ok());
        pump(&mut sender);

        // MIDI Clock on the same port: a Start and a Timing Clock tick.
        assert!(clock_tx.push(ClockMessage::Start).is_ok());
        assert!(clock_tx.push(ClockMessage::Tick).is_ok());
        while let Ok(m) = sender.clock_queue.pop() {
            sender.send_clock(m);
        }

        // In the order they were queued: note on, bend, SysEx, note off; then the clock.
        // Channel 4 is status 0xE3 for a bend.
        let want = vec![
            message(true, 4, 67, 90).to_vec(),
            vec![0xe3, 0, 0x40],
            vec![0xf0, 0x7e, 0x7f, 0xf7],
            message(false, 4, 67, 0).to_vec(),
            vec![0xfa],
            vec![0xf8],
        ];
        for _ in 0..100 {
            if got.lock().unwrap().len() >= want.len() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            *got.lock().unwrap(),
            want,
            "the port received the note on and off"
        );
    }

    /// A controller-kind event of a track, as the engine makes it.
    fn ctl(track: u16, pitch: u8, value: u16) -> MidiOutEvent {
        MidiOutEvent {
            track,
            on: false,
            pitch,
            vel: value.min(127) as u8,
            controller: true,
            value,
        }
    }

    #[test]
    fn pitch_bend_pressure_and_sysex_reach_the_routed_port() {
        let (mut sender, mut tx, log, _routes, _) = rig(vec![route("Synth", 2)]);
        assert!(tx.push(ctl(0, PITCH_BEND, 16383)).is_ok());
        assert!(tx.push(ctl(0, CHANNEL_PRESSURE, 90)).is_ok());
        // A System Exclusive message is one byte per event; a stray byte with no start is ignored.
        for b in [0xf0u8, 0x41, 0x10, 0xf7] {
            assert!(tx.push(ctl(0, SYSEX_BYTE, u16::from(b))).is_ok());
        }
        assert!(tx.push(ctl(0, SYSEX_BYTE, 0x55)).is_ok());
        pump(&mut sender);
        assert_eq!(
            sent(&log),
            vec![
                ("Synth".to_string(), vec![0xe1, 0x7f, 0x7f]),
                ("Synth".to_string(), vec![0xd1, 90]),
                ("Synth".to_string(), vec![0xf0, 0x41, 0x10, 0xf7]),
            ]
        );
    }

    #[test]
    fn midi_clock_goes_to_the_clock_port_and_nowhere_else() {
        let (mut tx, rx) = rtrb::RingBuffer::new(8);
        let (_mtc_tx, mtc_rx) = rtrb::RingBuffer::new(8);
        let (mut clock_tx, clock_rx) = rtrb::RingBuffer::new(8);
        let log: Log = Arc::new(StdMutex::new(Vec::new()));
        let l = log.clone();
        let open: Opener = Box::new(move |name: &str| {
            Some(Box::new(Rec {
                port: name.to_string(),
                log: l.clone(),
            }) as Box<dyn Sink>)
        });
        let mut sender = Sender::new(
            Queues {
                notes: rx,
                mtc: mtc_rx,
                clock: clock_rx,
            },
            Ports {
                routes: Arc::new(Mutex::new(vec![route("Synth", 1)])),
                mtc: Arc::new(Mutex::new(None)),
                clock: Arc::new(Mutex::new(Some("Clock Box".into()))),
            },
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicBool::new(false)),
            open,
        );
        assert!(clock_tx.push(ClockMessage::Start).is_ok());
        assert!(clock_tx.push(ClockMessage::Tick).is_ok());
        assert!(clock_tx.push(ClockMessage::SongPosition(16)).is_ok());
        assert!(tx.push(ev(0, true, 60)).is_ok());
        while let Ok(m) = sender.clock_queue.pop() {
            sender.send_clock(m);
        }
        pump(&mut sender);
        let out = sent(&log);
        assert_eq!(
            out,
            vec![
                ("Clock Box".to_string(), vec![0xfa]),
                ("Clock Box".to_string(), vec![0xf8]),
                ("Clock Box".to_string(), vec![0xf2, 16, 0]),
                ("Synth".to_string(), vec![0x90, 60, 90]),
            ]
        );
    }
}
