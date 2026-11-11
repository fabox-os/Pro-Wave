//! Loading DSSI plug-in libraries and reading their metadata (Dizzy Soft Synth Interface).
//!
//! A DSSI library is a shared object exporting `dssi_descriptor(index)`. Each descriptor wraps
//! a LADSPA descriptor (ports, ranges and hints) and adds MIDI input, program selection and
//! configuration on top. [`Library::load`] reads that metadata without running plug-in code.
//! The synth instance and rendering half follows in its own change.
//!
//! Logic Pro parity: this is the first half of the hosting layer for third-party software
//! instruments, which Logic exposes as plug-ins on a software instrument track. DSSI is the
//! Linux synth format requested in issue #64 and follows the in-process LADSPA path in
//! `prowave-ladspa`; it is not sandboxed. The out-of-process path in `prowave-plugin-host`
//! remains the default for the formats it supports.
//!
//! DSSI is unsafe by nature: the library is native code running inside Pro Wave. The unsafe
//! blocks here are limited to loading the library and reading its immutable descriptor tables;
//! each has a SAFETY comment.
//!
//! The whole reader is behind the non-default `in-process` feature: AGENTS.md keeps plug-in
//! hosting out of process by default, so a build that does not opt in compiles this crate to
//! nothing and links neither `libc` nor `libloading`.
#![cfg(feature = "in-process")]
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_ulong, c_void};
use std::path::Path;
use std::sync::Arc;

/// The most plug-in classes one library may report.
const MAX_PLUGINS: usize = 1024;
/// The most ports one plug-in may have.
const MAX_PORTS: usize = 512;
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
const HINT_DEFAULT_0: c_int = 0x200;
const HINT_DEFAULT_MINIMUM: c_int = 0x40;
const HINT_DEFAULT_LOW: c_int = 0x80;
const HINT_DEFAULT_MIDDLE: c_int = 0xc0;
const HINT_DEFAULT_HIGH: c_int = 0x100;
const HINT_DEFAULT_MAXIMUM: c_int = 0x140;
const HINT_DEFAULT_1: c_int = 0x240;
const HINT_DEFAULT_100: c_int = 0x280;
const HINT_DEFAULT_440: c_int = 0x2c0;

/// The sample rate used to scale `LADSPA_HINT_SAMPLE_RATE` defaults in [`PluginInfo`]. A plug-in
/// instance still gets the session's real rate.
const METADATA_RATE: f32 = 44_100.0;

/// The C layout of `LADSPA_Descriptor`. The callbacks are part of the ABI; this half reads
/// only the metadata (ids, names, ports, hints), so the rest is allowed to go unread.
#[repr(C)]
#[allow(dead_code)]
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

/// The C layout of `DSSI_Descriptor` (DSSI 1.0). Fields the loader does not read are part of
/// the ABI and are allowed to go unread.
#[repr(C)]
#[allow(dead_code)]
struct RawDssiDescriptor {
    api_version: c_int,
    ladspa_plugin: *const RawLadspaDescriptor,
    configure:
        Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *mut c_char>,
    get_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> *const RawProgramDescriptor>,
    select_program: Option<unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong)>,
    get_midi_controller_for_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int>,
    /// The event table type is defined by the render half; the pointer is never called here.
    run_synth: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut c_void, c_ulong)>,
    run_synth_adding: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut c_void, c_ulong)>,
    run_multiple_synths: Option<
        unsafe extern "C" fn(c_ulong, *mut *mut c_void, c_ulong, *mut *mut c_void, *mut c_ulong),
    >,
    run_multiple_synths_adding: Option<
        unsafe extern "C" fn(c_ulong, *mut *mut c_void, c_ulong, *mut *mut c_void, *mut c_ulong),
    >,
}

/// The C layout of `DSSI_Program_Descriptor`.
#[repr(C)]
struct RawProgramDescriptor {
    bank: c_ulong,
    program: c_ulong,
    name: *const c_char,
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
    /// The lowest value the plug-in declares, or `None` when it declares no lower bound.
    pub min: Option<f32>,
    /// The highest value the plug-in declares, or `None` when it declares no upper bound.
    pub max: Option<f32>,
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
    /// An event starts at or after the end of the block.
    EventPastBlock {
        frame: u32,
        frames: usize,
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
            Error::EventPastBlock { frame, frames } => write!(
                f,
                "an event at frame {frame} is outside the block of {frames} frames"
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
    /// The raw descriptors; the render half instantiates from them.
    #[allow(dead_code)]
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
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("plugins", &self.inner.plugins)
            .finish()
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
            // A port whose plug-in gives no hint has no declared bounds.
            min: None,
            max: None,
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
    min: Option<f32>,
    max: Option<f32>,
    default: f32,
    toggled: bool,
    integer: bool,
    logarithmic: bool,
}

/// `value` clamped only to the bounds given; `None` is an unbounded side.
fn clamp_value(value: f32, min: Option<f32>, max: Option<f32>) -> f32 {
    let mut out = value;
    if let Some(min) = min {
        out = out.max(min);
    }
    if let Some(max) = max {
        out = out.min(max);
    }
    out
}

/// The range and default a LADSPA hint describes at `rate`. Only the bounds the plug-in
/// declares are kept: an unbounded side is `None`, so a valid value there is never clamped to
/// a range the host made up.
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
    let min = (flags & HINT_BOUNDED_BELOW != 0)
        .then(|| scale(hint.lower_bound))
        .filter(|value| value.is_finite());
    let max = (flags & HINT_BOUNDED_ABOVE != 0)
        .then(|| scale(hint.upper_bound))
        .filter(|value| value.is_finite());
    // A range the plug-in gave backwards is still a range.
    let (min, max) = match (min, max) {
        (Some(low), Some(high)) if low > high => (Some(high), Some(low)),
        other => other,
    };
    let fraction = |amount: f32| -> Option<f32> {
        match (min, max) {
            (Some(low), Some(high)) => Some(if logarithmic && low > 0.0 && high > 0.0 {
                (low.ln() + (high.ln() - low.ln()) * amount).exp()
            } else {
                low + (high - low) * amount
            }),
            _ => None,
        }
    };
    let mut default = match flags & HINT_DEFAULT_MASK {
        HINT_DEFAULT_MINIMUM => min.unwrap_or(0.0),
        // A fraction of the range needs both ends; without them the lower end is the default.
        HINT_DEFAULT_LOW => fraction(0.25).or(min).unwrap_or(0.0),
        HINT_DEFAULT_MIDDLE => fraction(0.5).or(min).unwrap_or(0.0),
        HINT_DEFAULT_HIGH => fraction(0.75).or(min).unwrap_or(0.0),
        HINT_DEFAULT_MAXIMUM => max.or(min).unwrap_or(0.0),
        HINT_DEFAULT_0 => 0.0,
        HINT_DEFAULT_1 => 1.0,
        HINT_DEFAULT_100 => 100.0,
        HINT_DEFAULT_440 => 440.0,
        _ => min.unwrap_or(0.0),
    };
    if flags & HINT_INTEGER != 0 {
        default = default.round();
    }
    HintValues {
        min,
        max,
        default: clamp_value(default, min, max),
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

    fn hint(flags: c_int, lower: f32, upper: f32) -> RawRangeHint {
        RawRangeHint {
            hint_descriptor: flags,
            lower_bound: lower,
            upper_bound: upper,
        }
    }

    #[test]
    fn only_the_bounds_a_plug_in_declares_are_kept_and_clamped() {
        // Bounded below only, like a time or gain control: no upper bound is invented, so a
        // valid value above the lower bound is not changed.
        let values = hint_values(&hint(HINT_BOUNDED_BELOW, 0.0, 0.0), 44_100.0);
        assert_eq!(values.min, Some(0.0));
        assert_eq!(values.max, None);
        assert_eq!(clamp_value(500.0, values.min, values.max), 500.0);
        assert_eq!(clamp_value(-1.0, values.min, values.max), 0.0);

        // Bounded above only: a negative value is left alone.
        let values = hint_values(&hint(HINT_BOUNDED_ABOVE, 0.0, 1.0), 44_100.0);
        assert_eq!(values.min, None);
        assert_eq!(values.max, Some(1.0));
        assert_eq!(clamp_value(-500.0, values.min, values.max), -500.0);
        assert_eq!(clamp_value(2.0, values.min, values.max), 1.0);

        // No hints at all: neither side is bounded and no [0, 1] range is made up.
        let values = hint_values(&hint(0, 0.0, 0.0), 44_100.0);
        assert_eq!((values.min, values.max), (None, None));
        assert_eq!(clamp_value(123.0, values.min, values.max), 123.0);
    }

    #[test]
    fn bounds_are_scaled_ordered_and_the_default_follows_them() {
        let values = hint_values(
            &hint(
                HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE | HINT_SAMPLE_RATE,
                2.0,
                1.0,
            ),
            48_000.0,
        );
        // The plug-in gave the range backwards; the reader puts it the right way round.
        assert_eq!(values.min, Some(48_000.0));
        assert_eq!(values.max, Some(96_000.0));

        let middle = hint_values(
            &hint(
                HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE | HINT_DEFAULT_MIDDLE,
                0.0,
                1.0,
            ),
            44_100.0,
        );
        assert_eq!(middle.default, 0.5);

        // A port that names zero as its default gets zero, not the lower bound, and the
        // default is still clamped to the declared bounds.
        let zero = hint_values(
            &hint(
                HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE | HINT_DEFAULT_0,
                -1.0,
                1.0,
            ),
            44_100.0,
        );
        assert_eq!(zero.default, 0.0);
        let below_zero = hint_values(
            &hint(
                HINT_BOUNDED_BELOW | HINT_BOUNDED_ABOVE | HINT_DEFAULT_0,
                1.0,
                2.0,
            ),
            44_100.0,
        );
        assert_eq!(below_zero.default, 1.0);
    }
}
