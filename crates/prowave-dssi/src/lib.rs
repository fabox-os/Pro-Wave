//! In-process DSSI synth plug-in hosting (Dizzy Soft Synth Interface).
//!
//! A DSSI library is a shared object exporting `dssi_descriptor(index)`. Each descriptor wraps
//! a LADSPA descriptor (ports, ranges, and the callbacks that instantiate, connect and render)
//! and adds MIDI input, program selection and configuration on top. [`Library::load`] reads the
//! metadata without running plug-in code; [`Library::instantiate`] creates a synth instance,
//! connects its ports, and lets the caller render blocks with MIDI events through
//! [`Instance::process_with_events`].
//!
//! Logic Pro parity: this is the hosting layer for third-party software instruments, which
//! Logic exposes as plug-ins on a software instrument track. DSSI is the Linux synth format
//! requested in issue #64 and follows the in-process LADSPA path in `prowave-ladspa`; it is not
//! sandboxed, so a crashing plug-in can still take Pro Wave down. The out-of-process path in
//! `prowave-plugin-host` remains the default for the formats it supports.
//!
//! DSSI is unsafe by nature: the library is native code running inside Pro Wave. The unsafe
//! blocks here are limited to loading the library, reading its immutable descriptor tables,
//! and calling its callbacks; each has a SAFETY comment.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_ulong, c_void};
use std::path::Path;
use std::sync::Arc;

/// The most plug-in classes one library may report.
const MAX_PLUGINS: usize = 1024;
/// The most ports one plug-in may have.
const MAX_PORTS: usize = 512;
/// The most programs one synth may list.
const MAX_PROGRAMS: usize = 4096;
/// The most MIDI events one render block may carry. The instance reserves room for them at
/// construction, so [`Instance::process_with_events`] never allocates.
pub const MAX_EVENTS: usize = 256;

/// DSSI 1.0. Every descriptor must declare this API level.
const DSSI_API_VERSION: c_int = 1;

const PORT_INPUT: c_ulong = 0x1;
const PORT_OUTPUT: c_ulong = 0x2;
const PORT_CONTROL: c_ulong = 0x4;
const PORT_AUDIO: c_ulong = 0x8;

const HINT_BOUNDED_BELOW: c_int = 0x1;
const HINT_BOUNDED_ABOVE: c_int = 0x2;
const HINT_TOGGLED: c_int = 0x4;
const HINT_SAMPLE_RATE: c_int = 0x8;
const HINT_LOGARITHMIC: c_int = 0x10;
const HINT_INTEGER: c_int = 0x20;
const HINT_DEFAULT_MASK: c_int = 0x3c0;
const HINT_DEFAULT_MINIMUM: c_int = 0x40;
const HINT_DEFAULT_LOW: c_int = 0x80;
const HINT_DEFAULT_MIDDLE: c_int = 0xc0;
const HINT_DEFAULT_HIGH: c_int = 0x100;
const HINT_DEFAULT_MAXIMUM: c_int = 0x140;
const HINT_DEFAULT_1: c_int = 0x240;
const HINT_DEFAULT_100: c_int = 0x280;
const HINT_DEFAULT_440: c_int = 0x2c0;

/// The `get_midi_controller_for_port` bit saying the value names a MIDI controller.
pub const DSSI_CC_BITS: i32 = 0x2000_0000;
/// The `get_midi_controller_for_port` bit saying the value names an NRPN.
pub const DSSI_NRPN_BITS: i32 = 0x4000_0000;
/// `get_midi_controller_for_port`'s "no controller" answer.
pub const DSSI_NONE: i32 = -1;

/// The ALSA event types DSSI uses for MIDI.
const EVENT_NOTE_ON: u8 = 6;
const EVENT_NOTE_OFF: u8 = 7;
const EVENT_CONTROLLER: u8 = 10;
const EVENT_CHANNEL_PRESSURE: u8 = 12;
const EVENT_PITCH_BEND: u8 = 13;

/// The sample rate used to scale `LADSPA_HINT_SAMPLE_RATE` defaults in [`PluginInfo`]. A plug-in
/// instance still gets the session's real rate.
const METADATA_RATE: f32 = 44_100.0;

/// The C layout of `LADSPA_Descriptor`.
#[repr(C)]
struct RawLadspaDescriptor {
    unique_id: c_ulong,
    label: *const c_char,
    properties: c_int,
    name: *const c_char,
    maker: *const c_char,
    copyright: *const c_char,
    port_count: c_ulong,
    port_descriptors: *const c_ulong,
    port_names: *const *const c_char,
    port_range_hints: *const RawRangeHint,
    implementation_data: *mut c_void,
    instantiate: Option<unsafe extern "C" fn(*const RawLadspaDescriptor, c_ulong) -> *mut c_void>,
    connect_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut f32)>,
    activate: Option<unsafe extern "C" fn(*mut c_void)>,
    run: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    run_adding: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    set_run_adding_gain: Option<unsafe extern "C" fn(*mut c_void, f32)>,
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// The C layout of `LADSPA_PortRangeHint`.
#[repr(C)]
struct RawRangeHint {
    hint_descriptor: c_int,
    lower_bound: f32,
    upper_bound: f32,
}

/// The C layout of `DSSI_Descriptor` (DSSI 1.0).
#[repr(C)]
struct RawDssiDescriptor {
    api_version: c_int,
    ladspa_plugin: *const RawLadspaDescriptor,
    configure:
        Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *mut c_char>,
    get_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> *const RawProgramDescriptor>,
    select_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong)>,
    get_midi_controller_for_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int>,
    run_synth: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut SeqEvent, c_ulong)>,
    run_synth_adding: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut SeqEvent, c_ulong)>,
    run_multiple_synths: Option<
        unsafe extern "C" fn(c_ulong, *mut *mut c_void, c_ulong, *mut *mut SeqEvent, *mut c_ulong),
    >,
    run_multiple_synths_adding: Option<
        unsafe extern "C" fn(c_ulong, *mut *mut c_void, c_ulong, *mut *mut SeqEvent, *mut c_ulong),
    >,
}

/// The C layout of `DSSI_Program_Descriptor`.
#[repr(C)]
struct RawProgramDescriptor {
    bank: c_ulong,
    program: c_ulong,
    name: *const c_char,
}

/// `snd_seq_ev_note_t` from ALSA's `seq_event.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqNote {
    channel: u8,
    note: u8,
    velocity: u8,
    off_velocity: u8,
    duration: u32,
}

/// `snd_seq_ev_ctrl_t` from ALSA's `seq_event.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqCtrl {
    channel: u8,
    unused: [u8; 3],
    param: u32,
    value: i32,
}

/// `snd_seq_event_data_t`: twelve bytes in every ALSA build.
#[repr(C)]
#[derive(Clone, Copy)]
union SeqData {
    note: SeqNote,
    control: SeqCtrl,
    raw: [u8; 12],
}

/// `snd_seq_timestamp_t`.
#[repr(C)]
#[derive(Clone, Copy)]
union SeqTimestamp {
    tick: u32,
    time: SeqRealTime,
}

/// `snd_seq_real_time_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqRealTime {
    tv_sec: u32,
    tv_nsec: u32,
}

/// `snd_seq_addr_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqAddr {
    client: u8,
    port: u8,
}

/// The C layout of `snd_seq_event_t`, which DSSI passes to `run_synth`. It is 28 bytes; the
/// plugin reads the frame count from the tick field of `time` and MIDI data from `data`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqEvent {
    event_type: u8,
    flags: u8,
    tag: u8,
    queue: u8,
    time: SeqTimestamp,
    source: SeqAddr,
    dest: SeqAddr,
    data: SeqData,
}

/// One MIDI message for [`Instance::process_with_events`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// `value` is the MIDI pitch wheel position, -8192 to 8191.
    PitchBend {
        channel: u8,
        value: i16,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
}

impl Event {
    fn raw(self, frame: u32) -> SeqEvent {
        let (event_type, data) = match self {
            Event::NoteOn {
                channel,
                note,
                velocity,
            } => (
                EVENT_NOTE_ON,
                SeqData {
                    note: SeqNote {
                        channel,
                        note,
                        velocity,
                        off_velocity: 0,
                        duration: 0,
                    },
                },
            ),
            Event::NoteOff {
                channel,
                note,
                velocity,
            } => (
                EVENT_NOTE_OFF,
                SeqData {
                    note: SeqNote {
                        channel,
                        note,
                        velocity,
                        off_velocity: 0,
                        duration: 0,
                    },
                },
            ),
            Event::ControlChange {
                channel,
                controller,
                value,
            } => (
                EVENT_CONTROLLER,
                SeqData {
                    control: SeqCtrl {
                        channel,
                        unused: [0; 3],
                        param: u32::from(controller),
                        value: i32::from(value),
                    },
                },
            ),
            Event::PitchBend { channel, value } => (
                EVENT_PITCH_BEND,
                SeqData {
                    control: SeqCtrl {
                        channel,
                        unused: [0; 3],
                        param: 0,
                        value: i32::from(value),
                    },
                },
            ),
            Event::ChannelPressure { channel, pressure } => (
                EVENT_CHANNEL_PRESSURE,
                SeqData {
                    control: SeqCtrl {
                        channel,
                        unused: [0; 3],
                        param: 0,
                        value: i32::from(pressure),
                    },
                },
            ),
        };
        SeqEvent {
            event_type,
            flags: 0,
            tag: 0,
            queue: 0,
            time: SeqTimestamp { tick: frame },
            source: SeqAddr { client: 0, port: 0 },
            dest: SeqAddr { client: 0, port: 0 },
            data,
        }
    }
}

/// One MIDI event and the frame within the block at which it starts. DSSI wants events already
/// ordered by time; [`Instance::process_with_events`] refuses a decreasing frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedEvent {
    pub frame: u32,
    pub event: Event,
}

/// What a port carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    AudioInput,
    AudioOutput,
    ControlInput,
    ControlOutput,
}

/// One descriptor port: audio or control, in or out, with its range and default.
#[derive(Clone, Debug, PartialEq)]
pub struct PortInfo {
    /// The LADSPA port index used by [`Instance::set_control`] and [`Instance::control`].
    pub index: usize,
    pub name: String,
    pub kind: PortKind,
    /// Normalized-to-raw default as the plug-in declares it (after sample-rate scaling).
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub toggled: bool,
    pub integer: bool,
    pub logarithmic: bool,
}

/// One DSSI plug-in class: its LADSPA metadata plus the DSSI extensions it implements.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    pub api_version: u32,
    pub unique_id: u64,
    pub label: String,
    pub name: String,
    pub maker: String,
    pub copyright: String,
    pub ports: Vec<PortInfo>,
    /// Whether the synth lists programs with `get_program`.
    pub has_programs: bool,
    /// Whether the synth accepts `configure` keys.
    pub has_configure: bool,
    /// Whether the synth renders MIDI with `run_synth` (otherwise only LADSPA `run` exists).
    pub has_run_synth: bool,
}

/// One preset named by a synth, as `get_program` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramDescriptor {
    pub bank: u64,
    pub program: u64,
    pub name: String,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Load(String),
    NoPlugins,
    NoSuchPlugin,
    TooManyPorts,
    Malformed(&'static str),
    Unsupported(&'static str),
    Instantiate,
    BadPort(usize),
    BadValue,
    PortCount {
        expected_inputs: usize,
        expected_outputs: usize,
        inputs: usize,
        outputs: usize,
    },
    TooManyEvents {
        max: usize,
        got: usize,
    },
    UnorderedEvents {
        previous: u32,
        frame: u32,
    },
    /// `configure` returned the plug-in's own message.
    Configured(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Load(message) => write!(f, "cannot load DSSI library: {message}"),
            Error::NoPlugins => f.write_str("the library has no DSSI plug-ins"),
            Error::NoSuchPlugin => f.write_str("there is no plug-in with that index"),
            Error::TooManyPorts => f.write_str("the plug-in has too many ports"),
            Error::Malformed(what) => write!(f, "the plug-in descriptor has no {what}"),
            Error::Unsupported(what) => write!(f, "the plug-in does not support {what}"),
            Error::Instantiate => f.write_str("the plug-in could not be instantiated"),
            Error::BadPort(port) => write!(f, "port {port} is not a writable control"),
            Error::BadValue => f.write_str("a control value must be finite"),
            Error::PortCount {
                expected_inputs,
                expected_outputs,
                inputs,
                outputs,
            } => write!(
                f,
                "expected {expected_inputs} input and {expected_outputs} output ports, got {inputs} and {outputs}"
            ),
            Error::TooManyEvents { max, got } => {
                write!(f, "a block may carry at most {max} MIDI events, got {got}")
            }
            Error::UnorderedEvents { previous, frame } => write!(
                f,
                "MIDI events must be ordered by frame: {frame} comes after {previous}"
            ),
            Error::Configured(message) => write!(f, "the plug-in refused the setting: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// A loaded DSSI library and the synth classes it exports.
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

struct Inner {
    /// Kept alive so the descriptors and code stay mapped.
    _library: libloading::Library,
    descriptors: Vec<*const RawDssiDescriptor>,
    plugins: Vec<PluginInfo>,
}

// SAFETY: after loading, the descriptor tables are immutable and owned by the library, which
// stays mapped through the `Arc` in every `Library` and `Instance`. DSSI descriptors are
// read-only metadata; sharing them across threads is what hosts do. Instances are only `Send`,
// not `Sync` (see `Instance`).
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Library {
    /// Loads a DSSI shared library and reads every class descriptor.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        // SAFETY: the path is a shared object; `libloading` validates that it can be opened.
        // The library is kept alive in `Inner` for as long as any descriptor is used.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
        let (descriptors, plugins) = {
            // SAFETY: `dssi_descriptor` is the DSSI entry point; the symbol type matches its
            // documented signature.
            let descriptor: libloading::Symbol<
                unsafe extern "C" fn(c_ulong) -> *const RawDssiDescriptor,
            > = unsafe { library.get(b"dssi_descriptor\0") }
                .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
            let mut descriptors = Vec::new();
            let mut plugins = Vec::new();
            for index in 0..MAX_PLUGINS {
                // SAFETY: DSSI says the function returns NULL after the last class and a
                // valid descriptor otherwise. The pointer stays valid while `library` lives.
                let raw = unsafe { descriptor(index as c_ulong) };
                if raw.is_null() {
                    break;
                }
                // SAFETY: the descriptor pointer is non-null and points into the loaded library.
                let api_version = unsafe { (*raw).api_version };
                if api_version != DSSI_API_VERSION {
                    // A class for another API level cannot be hosted; skip it.
                    continue;
                }
                // SAFETY: the descriptor pointer is non-null and points into the loaded library.
                let info = unsafe { read_info(raw)? };
                descriptors.push(raw);
                plugins.push(info);
            }
            (descriptors, plugins)
        };
        if plugins.is_empty() {
            return Err(Error::NoPlugins);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                _library: library,
                descriptors,
                plugins,
            }),
        })
    }

    /// Every class the library exports, in descriptor order.
    pub fn plugins(&self) -> &[PluginInfo] {
        &self.inner.plugins
    }

    /// Instantiates class `index` at `sample_rate`.
    pub fn instantiate(&self, index: usize, sample_rate: u32) -> Result<Instance, Error> {
        Instance::new(self.inner.clone(), index, sample_rate)
    }
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("plugins", &self.inner.plugins)
            .finish()
    }
}

/// One running DSSI synth, with its ports connected.
pub struct Instance {
    /// Keeps the code and descriptors alive until the instance is cleaned up.
    _inner: Arc<Inner>,
    handle: *mut c_void,
    connect_port: unsafe extern "C" fn(*mut c_void, c_ulong, *mut f32),
    run: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    run_synth: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut SeqEvent, c_ulong)>,
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    configure:
        Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *mut c_char>,
    get_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> *const RawProgramDescriptor>,
    select_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong)>,
    midi_controller_for_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int>,
    audio_in: Vec<usize>,
    audio_out: Vec<usize>,
    controls: Vec<Control>,
    /// One value per control port, in `controls` order. The buffer is allocated once and its
    /// addresses are what the plug-in reads, so it must never grow after connecting.
    values: Vec<f32>,
    /// Room for one block's MIDI events, reserved at construction so rendering never allocates.
    events: Vec<SeqEvent>,
}

struct Control {
    port: usize,
    slot: usize,
    output: bool,
    min: f32,
    max: f32,
}

// SAFETY: a DSSI instance is meant to be used by one thread at a time. Moving it to the audio
// thread after creation is the normal host pattern; it is not `Sync`.
unsafe impl Send for Instance {}

impl Instance {
    fn new(inner: Arc<Inner>, index: usize, sample_rate: u32) -> Result<Self, Error> {
        let raw = *inner.descriptors.get(index).ok_or(Error::NoSuchPlugin)?;
        // SAFETY: `raw` is one of the descriptors read in `Library::load`; the library is still
        // mapped through `inner`.
        let dssi = unsafe { &*raw };
        if dssi.ladspa_plugin.is_null() {
            return Err(Error::Malformed("LADSPA descriptor"));
        }
        // SAFETY: DSSI requires every descriptor to point at a live LADSPA descriptor.
        let ladspa = unsafe { &*dssi.ladspa_plugin };
        let instantiate = ladspa
            .instantiate
            .ok_or(Error::Unsupported("instantiate"))?;
        let connect_port = ladspa
            .connect_port
            .ok_or(Error::Unsupported("connect_port"))?;
        // SAFETY: the descriptor's `instantiate` takes its own descriptor and a sample rate, as
        // the LADSPA ABI specifies.
        let handle = unsafe { instantiate(dssi.ladspa_plugin, sample_rate as c_ulong) };
        if handle.is_null() {
            return Err(Error::Instantiate);
        }

        let info = &inner.plugins[index];
        let port_count = usize::try_from(ladspa.port_count).unwrap_or(0);
        let raw_hints = if ladspa.port_range_hints.is_null() {
            None
        } else {
            // SAFETY: the descriptor's port count bounds its hint table, which stays mapped
            // through `inner`; the slice is only read here.
            Some(unsafe { std::slice::from_raw_parts(ladspa.port_range_hints, port_count) })
        };
        let mut audio_in = Vec::new();
        let mut audio_out = Vec::new();
        let mut controls = Vec::new();
        let mut values = Vec::new();
        for port in &info.ports {
            match port.kind {
                PortKind::AudioInput => audio_in.push(port.index),
                PortKind::AudioOutput => audio_out.push(port.index),
                PortKind::ControlInput | PortKind::ControlOutput => {
                    // Sample-rate-scaled bounds and defaults must use this instance's rate,
                    // not the rate the UI metadata was computed at.
                    let range = raw_hints
                        .and_then(|hints| hints.get(port.index))
                        .map(|hint| hint_values(hint, sample_rate as f32))
                        .unwrap_or(HintValues {
                            min: 0.0,
                            max: 1.0,
                            default: 0.0,
                            toggled: false,
                            integer: false,
                            logarithmic: false,
                        });
                    let slot = values.len();
                    values.push(if port.kind == PortKind::ControlInput {
                        range.default
                    } else {
                        0.0
                    });
                    controls.push(Control {
                        port: port.index,
                        slot,
                        output: port.kind == PortKind::ControlOutput,
                        min: range.min,
                        max: range.max,
                    });
                }
            }
        }

        // The values buffer is complete now; connect its stable addresses.
        for control in &controls {
            // SAFETY: `handle` is the instance just created, each port index belongs to the
            // descriptor, and the pointer refers to `values[slot]`, which lives in the instance
            // until it is dropped.
            unsafe {
                connect_port(
                    handle,
                    control.port as c_ulong,
                    values.as_mut_ptr().add(control.slot),
                );
            }
        }
        if let Some(activate) = ladspa.activate {
            // SAFETY: `activate` is the plug-in's own callback for this handle.
            unsafe { activate(handle) };
        }
        Ok(Self {
            _inner: inner,
            handle,
            connect_port,
            run: ladspa.run,
            run_synth: dssi.run_synth,
            deactivate: ladspa.deactivate,
            cleanup: ladspa.cleanup,
            configure: dssi.configure,
            get_program: dssi.get_program,
            select_program: dssi.select_program,
            midi_controller_for_port: dssi.get_midi_controller_for_port,
            audio_in,
            audio_out,
            controls,
            values,
            events: Vec::with_capacity(MAX_EVENTS),
        })
    }

    /// The number of audio input ports and audio output ports.
    pub fn audio_port_counts(&self) -> (usize, usize) {
        (self.audio_in.len(), self.audio_out.len())
    }

    /// Renders one block with no MIDI events.
    pub fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) -> Result<(), Error> {
        self.process_with_events(inputs, outputs, &[])
    }

    /// Renders one block, delivering `events` to the synth. Each slice is one audio port, in
    /// port order. The block length is the shortest of all input and output slices, so a synth
    /// with no audio inputs uses its output length; empty buffers do nothing. Events must be in
    /// nondecreasing frame order and their frames are relative to the start of the block.
    ///
    /// The instance reserves room for [`MAX_EVENTS`] events when it is created; this method
    /// allocates nothing.
    pub fn process_with_events(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        events: &[TimedEvent],
    ) -> Result<(), Error> {
        if inputs.len() != self.audio_in.len() || outputs.len() != self.audio_out.len() {
            return Err(Error::PortCount {
                expected_inputs: self.audio_in.len(),
                expected_outputs: self.audio_out.len(),
                inputs: inputs.len(),
                outputs: outputs.len(),
            });
        }
        if events.len() > MAX_EVENTS {
            return Err(Error::TooManyEvents {
                max: MAX_EVENTS,
                got: events.len(),
            });
        }
        let frames = inputs
            .iter()
            .map(|input| input.len())
            .chain(outputs.iter().map(|output| output.len()))
            .min()
            .unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }
        self.events.clear();
        let mut previous = 0;
        for (index, event) in events.iter().enumerate() {
            if index > 0 && event.frame < previous {
                return Err(Error::UnorderedEvents {
                    previous,
                    frame: event.frame,
                });
            }
            previous = event.frame;
            self.events.push(event.event.raw(event.frame));
        }
        for (port, input) in self.audio_in.iter().zip(inputs) {
            // SAFETY: LADSPA `connect_port` takes a mutable pointer, but an audio input port is
            // read-only by the API contract; the plug-in must not write it. The buffer lives for
            // the duration of the render call.
            unsafe {
                (self.connect_port)(self.handle, *port as c_ulong, input.as_ptr() as *mut f32);
            }
        }
        for (port, output) in self.audio_out.iter().zip(outputs.iter_mut()) {
            // SAFETY: an audio output port points at this block's writable buffer, which lives
            // for the duration of the render call.
            unsafe {
                (self.connect_port)(self.handle, *port as c_ulong, output.as_mut_ptr());
            }
        }
        if let Some(run_synth) = self.run_synth {
            // SAFETY: `run_synth` is the plug-in's render callback; all ports were connected
            // and the event table matches its documented ALSA layout.
            unsafe {
                let mut_ptr = if self.events.is_empty() {
                    std::ptr::null_mut()
                } else {
                    self.events.as_mut_ptr()
                };
                run_synth(
                    self.handle,
                    frames as c_ulong,
                    mut_ptr,
                    self.events.len() as c_ulong,
                );
            }
        } else if self.events.is_empty() {
            if let Some(run) = self.run {
                // SAFETY: LADSPA `run` renders the block; it is documented to behave like
                // `run_synth` with no events.
                unsafe { run(self.handle, frames as c_ulong) };
            } else {
                return Err(Error::Unsupported("run or run_synth"));
            }
        } else {
            return Err(Error::Unsupported("run_synth"));
        }
        Ok(())
    }

    /// Sets control input `port` to `value` (clamped to its declared range).
    pub fn set_control(&mut self, port: usize, value: f32) -> Result<(), Error> {
        if !value.is_finite() {
            return Err(Error::BadValue);
        }
        let (slot, min, max) = self
            .controls
            .iter()
            .find(|control| control.port == port && !control.output)
            .map(|control| (control.slot, control.min, control.max))
            .ok_or(Error::BadPort(port))?;
        self.values[slot] = value.clamp(min, max);
        Ok(())
    }

    /// The current value of control `port` (input or output). A synth may rewrite input
    /// controls when a program is selected, so this is the live value, not the setting.
    pub fn control(&self, port: usize) -> Option<f32> {
        self.controls
            .iter()
            .find(|control| control.port == port)
            .map(|control| self.values[control.slot])
    }

    /// Every program the synth lists, in `get_program` order. Empty when it has none.
    pub fn programs(&self) -> Vec<ProgramDescriptor> {
        let Some(get_program) = self.get_program else {
            return Vec::new();
        };
        let mut programs = Vec::new();
        for index in 0..MAX_PROGRAMS {
            // SAFETY: `get_program` belongs to this instance's descriptor and returns NULL past
            // the last program. The name is copied before the next call invalidates it.
            let raw = unsafe { get_program(self.handle, index as c_ulong) };
            if raw.is_null() {
                break;
            }
            // SAFETY: the non-null descriptor stays valid until the next call; its name is a
            // NUL-terminated string owned by the plug-in.
            let raw = unsafe { &*raw };
            programs.push(ProgramDescriptor {
                // `c_ulong` is 32-bit on Windows and 64-bit elsewhere; the cast is needed there
                // even though it is a no-op on this target.
                #[allow(clippy::unnecessary_cast)]
                bank: raw.bank as u64,
                #[allow(clippy::unnecessary_cast)]
                program: raw.program as u64,
                name: unsafe { string(raw.name) },
            });
        }
        programs
    }

    /// Selects a program by its bank and program numbers. The change takes effect at the next
    /// render; the synth may rewrite its input control values as it does so.
    pub fn select_program(&mut self, bank: u64, program: u64) -> Result<(), Error> {
        let select = self
            .select_program
            .ok_or(Error::Unsupported("select_program"))?;
        // SAFETY: `select` belongs to this instance's descriptor and takes its handle.
        unsafe {
            select(self.handle, bank as c_ulong, program as c_ulong);
        }
        Ok(())
    }

    /// Sends a piece of configuration to the synth. Keys starting with `DSSI:` are reserved by
    /// the specification (for example `DSSI:PROJECT_DIRECTORY`); a synth may refuse any key,
    /// and its message is returned in [`Error::Configured`].
    pub fn configure(&mut self, key: &str, value: &str) -> Result<(), Error> {
        let configure = self.configure.ok_or(Error::Unsupported("configure"))?;
        let key = CString::new(key).map_err(|_| Error::BadValue)?;
        let value = CString::new(value).map_err(|_| Error::BadValue)?;
        // SAFETY: `configure` belongs to this instance's descriptor, and both pointers are
        // valid NUL-terminated strings for the duration of the call.
        let error = unsafe { configure(self.handle, key.as_ptr(), value.as_ptr()) };
        if error.is_null() {
            return Ok(());
        }
        // SAFETY: DSSI says a non-NULL return is the plug-in's error string, which the host
        // frees. It is read before being freed.
        let message = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: the string was allocated by the plug-in with the C allocator, as DSSI
        // requires, and the host owns it now.
        unsafe { libc::free(error as *mut c_void) };
        Err(Error::Configured(message))
    }

    /// The MIDI controller the synth wants mapped to control `port`, or `None` when it wants
    /// none. The value uses the `DSSI_CC_BITS`/`DSSI_NRPN_BITS` encoding; see [`DSSI_NONE`].
    pub fn midi_controller_for_port(&self, port: usize) -> Option<i32> {
        let map = self.midi_controller_for_port?;
        // SAFETY: `map` belongs to this instance's descriptor; the port is an input control
        // that the caller found in `PluginInfo::ports`.
        let value = unsafe { map(self.handle, port as c_ulong) };
        (value != DSSI_NONE).then_some(value)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(deactivate) = self.deactivate {
            // SAFETY: `deactivate` is the plug-in's callback for this handle, called once.
            unsafe { deactivate(self.handle) };
        }
        if let Some(cleanup) = self.cleanup {
            // SAFETY: `cleanup` frees the instance created by `instantiate`; called once.
            unsafe { cleanup(self.handle) };
        }
    }
}

/// Reads one descriptor into owned metadata.
///
/// SAFETY: `raw` must point to a valid `DSSI_Descriptor` that stays mapped while the call reads
/// it; its LADSPA descriptor must be non-null, and its port table and strings valid for
/// `port_count` entries.
unsafe fn read_info(raw: *const RawDssiDescriptor) -> Result<PluginInfo, Error> {
    let dssi = &*raw;
    if dssi.ladspa_plugin.is_null() {
        return Err(Error::Malformed("LADSPA descriptor"));
    }
    let descriptor = &*dssi.ladspa_plugin;
    let count = usize::try_from(descriptor.port_count).map_err(|_| Error::TooManyPorts)?;
    if count > MAX_PORTS {
        return Err(Error::TooManyPorts);
    }
    let descriptors = if descriptor.port_descriptors.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(descriptor.port_descriptors, count)
    };
    let names = if descriptor.port_names.is_null() {
        None
    } else {
        Some(std::slice::from_raw_parts(descriptor.port_names, count))
    };
    let hints = if descriptor.port_range_hints.is_null() {
        None
    } else {
        Some(std::slice::from_raw_parts(
            descriptor.port_range_hints,
            count,
        ))
    };

    let mut ports = Vec::with_capacity(count);
    for index in 0..count {
        let flags = descriptors.get(index).copied().unwrap_or(0);
        let input = flags & PORT_INPUT != 0;
        let output = flags & PORT_OUTPUT != 0;
        let control = flags & PORT_CONTROL != 0;
        let audio = flags & PORT_AUDIO != 0;
        let kind = match (input, output, control, audio) {
            (true, false, false, true) => PortKind::AudioInput,
            (false, true, false, true) => PortKind::AudioOutput,
            (true, false, true, false) => PortKind::ControlInput,
            (false, true, true, false) => PortKind::ControlOutput,
            _ => continue,
        };
        let mut port = PortInfo {
            index,
            name: names
                .and_then(|names| names.get(index).copied())
                .filter(|name| !name.is_null())
                .map(|name| CStr::from_ptr(name).to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind,
            default: 0.0,
            min: 0.0,
            max: 1.0,
            toggled: false,
            integer: false,
            logarithmic: false,
        };
        if let Some(hints) = hints {
            if let Some(hint) = hints.get(index) {
                read_hint(&mut port, hint);
            }
        }
        ports.push(port);
    }

    Ok(PluginInfo {
        // `c_int` is 32-bit everywhere; a negative API level is not a version we host.
        api_version: dssi.api_version as u32,
        #[allow(clippy::unnecessary_cast)]
        unique_id: descriptor.unique_id as u64,
        label: string(descriptor.label),
        name: string(descriptor.name),
        maker: string(descriptor.maker),
        copyright: string(descriptor.copyright),
        ports,
        has_programs: dssi.get_program.is_some(),
        has_configure: dssi.configure.is_some(),
        has_run_synth: dssi.run_synth.is_some(),
    })
}

struct HintValues {
    min: f32,
    max: f32,
    default: f32,
    toggled: bool,
    integer: bool,
    logarithmic: bool,
}

/// The range and default a LADSPA hint describes at `rate`.
fn hint_values(hint: &RawRangeHint, rate: f32) -> HintValues {
    let flags = hint.hint_descriptor;
    let logarithmic = flags & HINT_LOGARITHMIC != 0;
    let scale = |value: f32| {
        if flags & HINT_SAMPLE_RATE != 0 {
            value * rate
        } else {
            value
        }
    };
    let bounded_below = flags & HINT_BOUNDED_BELOW != 0 && hint.lower_bound.is_finite();
    let bounded_above = flags & HINT_BOUNDED_ABOVE != 0 && hint.upper_bound.is_finite();
    let mut min = if bounded_below {
        scale(hint.lower_bound)
    } else if logarithmic && hint.upper_bound.is_finite() {
        // A logarithmic port needs a positive lower bound; pick three decades below the top.
        scale(hint.upper_bound) / 1000.0
    } else {
        0.0
    };
    let mut max = if bounded_above {
        scale(hint.upper_bound)
    } else if min > 0.0 {
        // The host chooses an upper bound for a one-sided range; keep it practical and finite.
        min * 1000.0
    } else {
        1.0
    };
    if !min.is_finite() {
        min = 0.0;
    }
    if !max.is_finite() {
        max = 1.0;
    }
    if max < min {
        std::mem::swap(&mut min, &mut max);
    }
    let fraction = |amount: f32| {
        if logarithmic && min > 0.0 && max > 0.0 {
            (min.ln() + (max.ln() - min.ln()) * amount).exp()
        } else {
            min + (max - min) * amount
        }
    };
    let mut default = match flags & HINT_DEFAULT_MASK {
        HINT_DEFAULT_MINIMUM => min,
        HINT_DEFAULT_LOW => fraction(0.25),
        HINT_DEFAULT_MIDDLE => fraction(0.5),
        HINT_DEFAULT_HIGH => fraction(0.75),
        HINT_DEFAULT_MAXIMUM => max,
        HINT_DEFAULT_1 => 1.0,
        HINT_DEFAULT_100 => 100.0,
        HINT_DEFAULT_440 => 440.0,
        _ => 0.0,
    };
    if flags & HINT_INTEGER != 0 {
        default = default.round();
    }
    HintValues {
        min,
        max,
        default: default.clamp(min, max),
        toggled: flags & HINT_TOGGLED != 0,
        integer: flags & HINT_INTEGER != 0,
        logarithmic,
    }
}

fn read_hint(port: &mut PortInfo, hint: &RawRangeHint) {
    let values = hint_values(hint, METADATA_RATE);
    port.toggled = values.toggled;
    port.integer = values.integer;
    port.logarithmic = values.logarithmic;
    port.min = values.min;
    port.max = values.max;
    port.default = values.default;
}

/// A NUL-terminated descriptor string, or empty when absent.
///
/// SAFETY: `pointer` must be null or a valid NUL-terminated string in the library.
unsafe fn string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        CStr::from_ptr(pointer).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn the_alsa_event_matches_the_c_layout() {
        // `snd_seq_event_t` is 28 bytes: four one-byte fields, an eight-byte timestamp, two
        // two-byte addresses and the twelve-byte data union.
        assert_eq!(std::mem::size_of::<SeqEvent>(), 28);
        assert_eq!(std::mem::size_of::<SeqNote>(), 8);
        assert_eq!(std::mem::size_of::<SeqCtrl>(), 12);
        assert_eq!(std::mem::size_of::<SeqData>(), 12);
        assert_eq!(std::mem::size_of::<SeqTimestamp>(), 8);
        assert_eq!(std::mem::size_of::<SeqAddr>(), 2);
        assert_eq!(std::mem::offset_of!(SeqEvent, time), 4);
        assert_eq!(std::mem::offset_of!(SeqEvent, source), 12);
        assert_eq!(std::mem::offset_of!(SeqEvent, dest), 14);
        assert_eq!(std::mem::offset_of!(SeqEvent, data), 16);
    }

    #[test]
    fn events_carry_their_frame_and_message() {
        let raw = Event::NoteOn {
            channel: 3,
            note: 60,
            velocity: 100,
        }
        .raw(17);
        assert_eq!(raw.event_type, EVENT_NOTE_ON);
        // SAFETY: the event was just built with its note field.
        let note = unsafe { raw.data.note };
        assert_eq!((note.channel, note.note, note.velocity), (3, 60, 100));
        // SAFETY: the event was just built with its tick field.
        assert_eq!(unsafe { raw.time.tick }, 17);
    }
}
