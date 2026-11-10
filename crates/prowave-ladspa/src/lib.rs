//! In-process LADSPA plug-in hosting (Linux Audio Developer's Simple Plugin API).
//!
//! A LADSPA library is a shared object exporting `ladspa_descriptor(index)`. The descriptor
//! lists ports (audio in/out, control in/out), their ranges and defaults, and the callbacks to
//! instantiate, connect, run and clean up an instance. [`Library::load`] reads the metadata
//! without running plug-in code; [`Library::instantiate`] creates one instance, connects its
//! control ports, and lets the caller render audio blocks with [`Instance::process`].
//!
//! LADSPA is unsafe by nature: the library is native code running inside Pro Wave. The unsafe
//! blocks here are limited to loading the library, reading its immutable descriptor tables,
//! and calling its callbacks; each has a SAFETY comment. A crashing or misbehaving plug-in can
//! still take the process down, which is why the out-of-process path in `prowave-plugin-host`
//! remains the default for plug-ins it supports. This crate is the in-process LADSPA path and
//! the foundation for DSSI/VAMP work.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_ulong, c_void};
use std::path::Path;
use std::sync::Arc;

/// The most plug-in classes one library may report.
const MAX_PLUGINS: usize = 1024;
/// The most ports one plug-in may have.
const MAX_PORTS: usize = 512;

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
const HINT_DEFAULT_1: c_int = 0x80;
const HINT_DEFAULT_100: c_int = 0x100;
const HINT_DEFAULT_440: c_int = 0x140;
const HINT_DEFAULT_MINIMUM: c_int = 0x180;
const HINT_DEFAULT_MAXIMUM: c_int = 0x1c0;
const HINT_DEFAULT_LOW: c_int = 0x200;
const HINT_DEFAULT_HIGH: c_int = 0x240;

/// The sample rate used to scale `LADSPA_HINT_SAMPLE_RATE` defaults in [`PluginInfo`]. A plug-in
/// instance still gets the session's real rate.
const METADATA_RATE: f32 = 44_100.0;

/// The C layout of `LADSPA_Descriptor`.
#[repr(C)]
struct RawDescriptor {
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
    instantiate: Option<unsafe extern "C" fn(*const RawDescriptor, c_ulong) -> *mut c_void>,
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

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Load(String),
    NoPlugins,
    NoSuchPlugin,
    TooManyPorts,
    Unsupported(&'static str),
    Instantiate,
    BadPort(usize),
    BadValue,
    Buffer(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Load(message) => write!(f, "cannot load LADSPA library: {message}"),
            Error::NoPlugins => f.write_str("the library has no LADSPA plug-ins"),
            Error::NoSuchPlugin => f.write_str("there is no plug-in with that index"),
            Error::TooManyPorts => f.write_str("the plug-in has too many ports"),
            Error::Unsupported(what) => write!(f, "the plug-in does not support {what}"),
            Error::Instantiate => f.write_str("the plug-in could not be instantiated"),
            Error::BadPort(port) => write!(f, "port {port} is not a writable control"),
            Error::BadValue => f.write_str("a control value must be finite"),
            Error::Buffer(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// A loaded LADSPA library and the plug-in classes it exports.
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

struct Inner {
    /// Kept alive so the descriptors and code stay mapped.
    _library: libloading::Library,
    descriptors: Vec<*const RawDescriptor>,
    plugins: Vec<PluginInfo>,
}

// SAFETY: after loading, the descriptor tables are immutable and owned by the library, which
// stays mapped through the `Arc` in every `Library` and `Instance`. LADSPA descriptors are
// read-only metadata; sharing them across threads is what hosts do. Instances are only `Send`,
// not `Sync` (see `Instance`).
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

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

/// One plug-in class.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    pub unique_id: u64,
    pub label: String,
    pub name: String,
    pub maker: String,
    pub copyright: String,
    pub ports: Vec<PortInfo>,
}

impl Library {
    /// Loads a LADSPA shared library and reads every class descriptor.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        // SAFETY: the path is a shared object; `libloading` validates that it can be opened.
        // The library is kept alive in `Inner` for as long as any descriptor is used.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
        let (descriptors, plugins) = {
            // SAFETY: `ladspa_descriptor` is the LADSPA entry point; the symbol type matches its
            // documented signature.
            let descriptor: libloading::Symbol<
                unsafe extern "C" fn(c_ulong) -> *const RawDescriptor,
            > = unsafe { library.get(b"ladspa_descriptor\0") }
                .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
            let mut descriptors = Vec::new();
            let mut plugins = Vec::new();
            for index in 0..MAX_PLUGINS {
                // SAFETY: LADSPA says the function returns NULL after the last class and a
                // valid descriptor otherwise. The pointer stays valid while `library` lives.
                let raw = unsafe { descriptor(index as c_ulong) };
                if raw.is_null() {
                    break;
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

/// One running LADSPA plug-in, with its control ports connected.
pub struct Instance {
    /// Keeps the code and descriptors alive until the instance is cleaned up.
    _inner: Arc<Inner>,
    handle: *mut c_void,
    connect_port: unsafe extern "C" fn(*mut c_void, c_ulong, *mut f32),
    run: unsafe extern "C" fn(*mut c_void, c_ulong),
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    audio_in: Vec<usize>,
    audio_out: Vec<usize>,
    controls: Vec<Control>,
    /// One value per control port, in `controls` order. The buffer is allocated once and its
    /// addresses are what the plug-in reads, so it must never grow after connecting.
    values: Vec<f32>,
}

struct Control {
    port: usize,
    slot: usize,
    output: bool,
    min: f32,
    max: f32,
}

// SAFETY: a LADSPA instance is meant to be used by one thread at a time. Moving it to the audio
// thread after creation is the normal host pattern; it is not `Sync`.
unsafe impl Send for Instance {}

impl Instance {
    fn new(inner: Arc<Inner>, index: usize, sample_rate: u32) -> Result<Self, Error> {
        let raw = *inner.descriptors.get(index).ok_or(Error::NoSuchPlugin)?;
        // SAFETY: `raw` is one of the descriptors read in `Library::load`; the library is still
        // mapped through `inner`.
        let descriptor = unsafe { &*raw };
        let instantiate = descriptor
            .instantiate
            .ok_or(Error::Unsupported("instantiate"))?;
        let connect_port = descriptor
            .connect_port
            .ok_or(Error::Unsupported("connect_port"))?;
        let run = descriptor.run.ok_or(Error::Unsupported("run"))?;
        // SAFETY: the descriptor's `instantiate` takes its own descriptor and a sample rate, as
        // the LADSPA ABI specifies.
        let handle = unsafe { instantiate(raw, sample_rate as c_ulong) };
        if handle.is_null() {
            return Err(Error::Instantiate);
        }

        let info = &inner.plugins[index];
        let mut audio_in = Vec::new();
        let mut audio_out = Vec::new();
        let mut controls = Vec::new();
        let mut values = Vec::new();
        for port in &info.ports {
            match port.kind {
                PortKind::AudioInput => audio_in.push(port.index),
                PortKind::AudioOutput => audio_out.push(port.index),
                PortKind::ControlInput | PortKind::ControlOutput => {
                    let slot = values.len();
                    values.push(if port.kind == PortKind::ControlInput {
                        port.default
                    } else {
                        0.0
                    });
                    controls.push(Control {
                        port: port.index,
                        slot,
                        output: port.kind == PortKind::ControlOutput,
                        min: port.min,
                        max: port.max,
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
        if let Some(activate) = descriptor.activate {
            // SAFETY: `activate` is the plug-in's own callback for this handle.
            unsafe { activate(handle) };
        }
        Ok(Self {
            _inner: inner,
            handle,
            connect_port,
            run,
            deactivate: descriptor.deactivate,
            cleanup: descriptor.cleanup,
            audio_in,
            audio_out,
            controls,
            values,
        })
    }

    /// The number of audio input ports and audio output ports.
    pub fn audio_port_counts(&self) -> (usize, usize) {
        (self.audio_in.len(), self.audio_out.len())
    }

    /// Renders `frames` from `inputs` into `outputs`. Each slice is one audio port, in port
    /// order; slices must cover at least the shortest input.
    pub fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) -> Result<(), Error> {
        let frames = inputs.first().map_or(0, |input| input.len());
        if frames == 0 {
            return Ok(());
        }
        if inputs.len() != self.audio_in.len() || outputs.len() != self.audio_out.len() {
            return Err(Error::Buffer(format!(
                "expected {} input and {} output ports, got {} and {}",
                self.audio_in.len(),
                self.audio_out.len(),
                inputs.len(),
                outputs.len()
            )));
        }
        if inputs.iter().any(|input| input.len() < frames)
            || outputs.iter().any(|output| output.len() < frames)
        {
            return Err(Error::Buffer(
                "every port buffer must cover the block".to_string(),
            ));
        }
        for (port, input) in self.audio_in.iter().zip(inputs) {
            // SAFETY: LADSPA `connect_port` takes a mutable pointer, but an audio input port is
            // read-only by the API contract; the plug-in must not write it. The buffer lives for
            // the duration of `run`.
            unsafe {
                (self.connect_port)(self.handle, *port as c_ulong, input.as_ptr() as *mut f32);
            }
        }
        for (port, output) in self.audio_out.iter().zip(outputs.iter_mut()) {
            // SAFETY: an audio output port points at this block's writable buffer, which lives
            // for the duration of `run`.
            unsafe {
                (self.connect_port)(self.handle, *port as c_ulong, output.as_mut_ptr());
            }
        }
        // SAFETY: `run` is the plug-in's render callback; all audio and control ports were
        // connected and the block length is passed in frames.
        unsafe { (self.run)(self.handle, frames as c_ulong) };
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

    /// The current value of control `port` (input or output).
    pub fn control(&self, port: usize) -> Option<f32> {
        self.controls
            .iter()
            .find(|control| control.port == port)
            .map(|control| self.values[control.slot])
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
/// SAFETY: `raw` must point to a valid `LADSPA_Descriptor` that stays mapped while the call
/// reads it; its port table and strings must be valid for `port_count` entries.
unsafe fn read_info(raw: *const RawDescriptor) -> Result<PluginInfo, Error> {
    let descriptor = &*raw;
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
        // `c_ulong` is 32-bit on Windows and 64-bit elsewhere; the cast is needed there even
        // though it is a no-op on this target.
        #[allow(clippy::unnecessary_cast)]
        unique_id: descriptor.unique_id as u64,
        label: string(descriptor.label),
        name: string(descriptor.name),
        maker: string(descriptor.maker),
        copyright: string(descriptor.copyright),
        ports,
    })
}

fn read_hint(port: &mut PortInfo, hint: &RawRangeHint) {
    let flags = hint.hint_descriptor;
    let scale = |value: f32| {
        if flags & HINT_SAMPLE_RATE != 0 {
            value * METADATA_RATE
        } else {
            value
        }
    };
    port.toggled = flags & HINT_TOGGLED != 0;
    port.integer = flags & HINT_INTEGER != 0;
    port.logarithmic = flags & HINT_LOGARITHMIC != 0;
    port.min = if flags & HINT_BOUNDED_BELOW != 0 {
        scale(hint.lower_bound)
    } else {
        0.0
    };
    port.max = if flags & HINT_BOUNDED_ABOVE != 0 {
        scale(hint.upper_bound)
    } else {
        1.0
    };
    if port.max < port.min {
        std::mem::swap(&mut port.min, &mut port.max);
    }
    let (min, max) = (port.min, port.max);
    port.default = match flags & HINT_DEFAULT_MASK {
        HINT_DEFAULT_1 => 1.0,
        HINT_DEFAULT_100 => 100.0,
        HINT_DEFAULT_440 => 440.0,
        HINT_DEFAULT_MINIMUM => min,
        HINT_DEFAULT_MAXIMUM => max,
        HINT_DEFAULT_LOW => min + 0.25 * (max - min),
        HINT_DEFAULT_HIGH => min + 0.75 * (max - min),
        _ => 0.0,
    }
    .clamp(min, max);
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
