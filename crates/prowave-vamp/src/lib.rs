//! In-process Vamp audio analysis plug-in hosting.
//!
//! A Vamp library is a shared object exporting `vampGetPluginDescriptor(api, index)`. The
//! descriptor names the analyser, its parameters, programs and (through an instance) its
//! feature outputs, and carries the callbacks to create, initialise and run it. [`Library::load`]
//! reads the descriptor metadata; [`Library::instantiate`] creates an analyser, and
//! [`Instance::process`] feeds it blocks of audio and returns the features it found.
//!
//! Logic Pro parity: Logic analyses audio for tempo, beats, key and pitch, for Smart Tempo, Beat
//! Mapping, Flex Time and the tuner. Vamp is the open analysis plug-in format that supplies
//! those same results (issue #69), and it is offline analysis, so unlike the audio thread this
//! crate may allocate.
//!
//! Vamp is unsafe by nature: the library is native code running inside Pro Wave. The unsafe
//! blocks here are limited to loading the library, reading its immutable descriptor tables,
//! and calling its callbacks; each has a SAFETY comment.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::Path;
use std::sync::Arc;

/// The most plug-in classes one library may report.
const MAX_PLUGINS: usize = 1024;
/// The most parameters, programs, bin names or value names one plug-in may have.
const MAX_LISTS: usize = 4096;
/// The most feature outputs one analyser may report.
const MAX_OUTPUTS: usize = 256;
/// The most bins one output may have.
const MAX_BINS: usize = 65_536;
/// The most features one process call may return.
const MAX_FEATURES: usize = 100_000;
/// The most values one feature may hold.
const MAX_VALUES: usize = 65_536;
/// Bounds the shared `instantiate` and `cleanup` calls (see [`shared_calls`]).
static SHARED_CALLS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Some Vamp plug-ins keep state shared by every instance. The host serialises `instantiate`
/// and `cleanup` so no two threads are inside them at once; this is what makes the `Sync` claim
/// on [`Inner`] true for such a library. A poisoned gate still lets the call through: the
/// plug-in was not the one that panicked.
fn shared_calls() -> std::sync::MutexGuard<'static, ()> {
    match SHARED_CALLS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
/// The sample window a host picks when the plug-in states no preference.
pub const DEFAULT_BLOCK_SIZE: u32 = 1024;
/// The Vamp API level this crate speaks.
const HOST_API_VERSION: c_uint = 2;

const INPUT_TIME_DOMAIN: c_int = 0;
const SAMPLE_ONE_PER_STEP: c_int = 0;
const SAMPLE_FIXED_RATE: c_int = 1;

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Load(String),
    NoPlugins,
    NoSuchPlugin,
    Malformed(&'static str),
    Unsupported(&'static str),
    Instantiate,
    Initialize,
    NotInitialized,
    AlreadyInitialized(&'static str),
    ChannelCount { expected: usize, got: usize },
    ChannelRange { min: usize, max: usize, got: usize },
    BadProgram(usize),
    BlockSize { expected: usize, got: usize },
    BadParameter(usize),
    BadValue,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Load(message) => write!(f, "cannot load Vamp library: {message}"),
            Error::NoPlugins => f.write_str("the library has no Vamp plug-ins"),
            Error::NoSuchPlugin => f.write_str("there is no plug-in with that index"),
            Error::Malformed(what) => write!(f, "the plug-in descriptor has no {what}"),
            Error::Unsupported(what) => write!(f, "the plug-in does not support {what}"),
            Error::Instantiate => f.write_str("the plug-in could not be instantiated"),
            Error::Initialize => f.write_str("the plug-in could not be initialised"),
            Error::NotInitialized => f.write_str("the plug-in has not been initialised"),
            Error::AlreadyInitialized(what) => {
                write!(f, "{what} must be set before the plug-in is initialised")
            }
            Error::ChannelCount { expected, got } => {
                write!(f, "expected {expected} input channels, got {got}")
            }
            Error::ChannelRange { min, max, got } => write!(
                f,
                "the analyser takes {min} to {max} input channels, got {got}"
            ),
            Error::BadProgram(index) => write!(f, "there is no program {index}"),
            Error::BlockSize { expected, got } => {
                write!(
                    f,
                    "every input channel must have {expected} samples, got {got}"
                )
            }
            Error::BadParameter(index) => write!(f, "there is no parameter {index}"),
            Error::BadValue => f.write_str("a parameter value must be finite"),
        }
    }
}

impl std::error::Error for Error {}

/// The C layout of `VampParameterDescriptor`.
#[repr(C)]
struct RawParameterDescriptor {
    identifier: *const c_char,
    name: *const c_char,
    description: *const c_char,
    unit: *const c_char,
    min_value: f32,
    max_value: f32,
    default_value: f32,
    is_quantized: c_int,
    quantize_step: f32,
    value_names: *const *const c_char,
}

/// The C layout of `VampOutputDescriptor` (API version 2; version 1 stops before the last
/// field).
#[repr(C)]
struct RawOutputDescriptor {
    identifier: *const c_char,
    name: *const c_char,
    description: *const c_char,
    unit: *const c_char,
    has_fixed_bin_count: c_int,
    bin_count: c_uint,
    bin_names: *const *const c_char,
    has_known_extents: c_int,
    min_value: f32,
    max_value: f32,
    is_quantized: c_int,
    quantize_step: f32,
    sample_type: c_int,
    sample_rate: f32,
    has_duration: c_int,
}

/// The C layout of `VampFeature`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawFeature {
    has_timestamp: c_int,
    sec: c_int,
    nsec: c_int,
    value_count: c_uint,
    values: *mut f32,
    label: *mut c_char,
}

/// The C layout of `VampFeatureV2`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawFeatureV2 {
    has_duration: c_int,
    duration_sec: c_int,
    duration_nsec: c_int,
}

/// `VampFeatureUnion`: API version 2 feature lists hold two of these per feature.
#[repr(C)]
union RawFeatureUnion {
    v1: RawFeature,
    v2: RawFeatureV2,
}

/// The C layout of `VampFeatureList`.
#[repr(C)]
struct RawFeatureList {
    feature_count: c_uint,
    features: *mut RawFeatureUnion,
}

/// The C layout of `VampPluginDescriptor`.
#[repr(C)]
struct RawDescriptor {
    api_version: c_uint,
    identifier: *const c_char,
    name: *const c_char,
    description: *const c_char,
    maker: *const c_char,
    plugin_version: c_int,
    copyright: *const c_char,
    parameter_count: c_uint,
    parameters: *const *const RawParameterDescriptor,
    program_count: c_uint,
    programs: *const *const c_char,
    input_domain: c_int,
    instantiate: Option<unsafe extern "C" fn(*const RawDescriptor, f32) -> *mut c_void>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    initialise: Option<unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint) -> c_int>,
    reset: Option<unsafe extern "C" fn(*mut c_void)>,
    get_parameter: Option<unsafe extern "C" fn(*mut c_void, c_int) -> f32>,
    set_parameter: Option<unsafe extern "C" fn(*mut c_void, c_int, f32)>,
    get_current_program: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    select_program: Option<unsafe extern "C" fn(*mut c_void, c_uint)>,
    preferred_step_size: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    preferred_block_size: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    min_channel_count: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    max_channel_count: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    output_count: Option<unsafe extern "C" fn(*mut c_void) -> c_uint>,
    get_output_descriptor:
        Option<unsafe extern "C" fn(*mut c_void, c_uint) -> *mut RawOutputDescriptor>,
    release_output_descriptor: Option<unsafe extern "C" fn(*mut RawOutputDescriptor)>,
    process: Option<
        unsafe extern "C" fn(*mut c_void, *const *const f32, c_int, c_int) -> *mut RawFeatureList,
    >,
    get_remaining_features: Option<unsafe extern "C" fn(*mut c_void) -> *mut RawFeatureList>,
    release_feature_set: Option<unsafe extern "C" fn(*mut RawFeatureList)>,
}

/// Whether the analyser wants time-domain samples or a frequency-domain spectrum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDomain {
    Time,
    Frequency,
}

/// How a feature's timestamp is positioned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleType {
    /// One result per process call, aligned with the block start.
    OneSamplePerStep,
    /// Results evenly spaced at [`OutputInfo::sample_rate`].
    FixedSampleRate,
    /// Every result carries its own timestamp.
    VariableSampleRate,
}

/// One analyser parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct ParameterInfo {
    pub identifier: String,
    pub name: String,
    pub description: String,
    pub unit: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub quantized: bool,
    pub quantize_step: f32,
    /// Human-readable names of the quantized steps, or empty.
    pub value_names: Vec<String>,
}

/// One feature output of an analyser.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    pub identifier: String,
    pub name: String,
    pub description: String,
    pub unit: String,
    pub bin_count: usize,
    pub bin_names: Vec<String>,
    pub has_known_extents: bool,
    pub min: f32,
    pub max: f32,
    pub quantized: bool,
    pub quantize_step: f32,
    pub sample_type: SampleType,
    pub sample_rate: f32,
    /// API version 2 only: whether features carry a duration.
    pub has_duration: bool,
}

/// One analyser class.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    pub api_version: u32,
    pub identifier: String,
    pub name: String,
    pub description: String,
    pub maker: String,
    pub plugin_version: u32,
    pub copyright: String,
    pub input_domain: InputDomain,
    pub parameters: Vec<ParameterInfo>,
    pub programs: Vec<String>,
}

/// One result from an analyser.
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    /// Seconds from the start of the analysed audio, or `None` for the end-of-stream features
    /// [`Instance::finish`] returns that carry no time of their own.
    pub timestamp: Option<f64>,
    /// API version 2 duration in seconds, when the output declares one.
    pub duration: Option<f64>,
    pub values: Vec<f32>,
    pub label: Option<String>,
}

/// A loaded Vamp library and the analysers it exports.
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
// stays mapped through the `Arc` in every `Library` and `Instance`. Vamp descriptors are
// read-only metadata; sharing them across threads is what hosts do. Instances are only `Send`,
// not `Sync` (see `Instance`).
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Library {
    /// Loads a Vamp shared library and reads every analyser descriptor.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        // SAFETY: the path is a shared object; `libloading` validates that it can be opened.
        // The library is kept alive in `Inner` for as long as any descriptor is used.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
        let (descriptors, plugins) = {
            // SAFETY: `vampGetPluginDescriptor` is the Vamp entry point; the symbol type
            // matches its documented signature.
            let descriptor: libloading::Symbol<
                unsafe extern "C" fn(c_uint, c_uint) -> *const RawDescriptor,
            > = unsafe { library.get(b"vampGetPluginDescriptor\0") }
                .map_err(|error| Error::Load(format!("{}: {error}", path.display())))?;
            let mut descriptors = Vec::new();
            let mut plugins = Vec::new();
            for index in 0..MAX_PLUGINS {
                // SAFETY: Vamp says the function returns NULL after the last class and a valid
                // descriptor otherwise. The pointer stays valid while `library` lives.
                let raw = unsafe { descriptor(HOST_API_VERSION, index as c_uint) };
                if raw.is_null() {
                    break;
                }
                // SAFETY: the descriptor pointer is non-null and points into the loaded library.
                let api_version = unsafe { (*raw).api_version };
                if api_version != 1 && api_version != HOST_API_VERSION {
                    // A class for another API level cannot be hosted; skip it.
                    continue;
                }
                // SAFETY: the descriptor pointer is non-null and points into the loaded library.
                let info = unsafe { read_info(raw) }?;
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

    /// Every analyser the library exports, in descriptor order.
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

/// One running analyser.
pub struct Instance {
    /// Keeps the code and descriptors alive until the instance is cleaned up.
    _inner: Arc<Inner>,
    handle: *mut c_void,
    descriptor: *const RawDescriptor,
    api_version: u32,
    parameter_count: usize,
    program_count: usize,
    min_channels: usize,
    max_channels: usize,
    initialized: bool,
    channels: usize,
    block_size: usize,
    /// Channel pointers handed to `process`, reserved at construction.
    channel_ptrs: Vec<*const f32>,
}

// SAFETY: a Vamp instance is meant to be used by one thread at a time. Moving it to a worker
// thread after creation is the normal analysis pattern; it is not `Sync`.
unsafe impl Send for Instance {}

impl Instance {
    fn new(inner: Arc<Inner>, index: usize, sample_rate: u32) -> Result<Self, Error> {
        let raw = *inner.descriptors.get(index).ok_or(Error::NoSuchPlugin)?;
        // SAFETY: `raw` is one of the descriptors read in `Library::load`; the library is still
        // mapped through `inner`.
        let descriptor = unsafe { &*raw };
        let instantiate = descriptor
            .instantiate
            .ok_or(Error::Malformed("instantiate"))?;
        // SAFETY: the descriptor's `instantiate` takes its own descriptor and a sample rate, as
        // the Vamp ABI specifies. The gate keeps two threads out of a plug-in that shares state
        // between instances.
        let handle = {
            let _gate = shared_calls();
            unsafe { instantiate(raw, sample_rate as f32) }
        };
        if handle.is_null() {
            return Err(Error::Instantiate);
        }
        let info = &inner.plugins[index];
        let api_version = info.api_version;
        let parameter_count = info.parameters.len();
        let program_count = info.programs.len();
        let min_channels = descriptor
            .min_channel_count
            .map(|callback| {
                // SAFETY: the callback belongs to this descriptor and takes its handle.
                unsafe { callback(handle) as usize }
            })
            .unwrap_or(1);
        let max_channels = descriptor
            .max_channel_count
            .map(|callback| {
                // SAFETY: the callback belongs to this descriptor and takes its handle.
                unsafe { callback(handle) as usize }
            })
            .unwrap_or(min_channels);
        Ok(Self {
            _inner: inner,
            handle,
            descriptor: raw,
            api_version,
            parameter_count,
            program_count,
            min_channels,
            max_channels: max_channels.max(min_channels),
            initialized: false,
            channels: 0,
            block_size: 0,
            channel_ptrs: Vec::with_capacity(max_channels.max(min_channels)),
        })
    }

    /// The channel counts the analyser accepts.
    pub fn channel_range(&self) -> (usize, usize) {
        (self.min_channels, self.max_channels)
    }

    /// The analyser's preferred processing increment, or `None` when it leaves the choice to
    /// the host.
    pub fn preferred_step_size(&self) -> Option<u32> {
        let callback = self.descriptor_ref().preferred_step_size?;
        // SAFETY: the callback belongs to this descriptor and takes its handle.
        let value = unsafe { callback(self.handle) };
        (value > 0).then_some(value)
    }

    /// The analyser's preferred processing window, or `None` when it leaves the choice to the
    /// host.
    pub fn preferred_block_size(&self) -> Option<u32> {
        let callback = self.descriptor_ref().preferred_block_size?;
        // SAFETY: the callback belongs to this descriptor and takes its handle.
        let value = unsafe { callback(self.handle) };
        (value > 0).then_some(value)
    }

    /// Every feature output the analyser has, with its bin count, extents and timing.
    pub fn outputs(&self) -> Result<Vec<OutputInfo>, Error> {
        let descriptor = self.descriptor_ref();
        let count = descriptor
            .output_count
            .ok_or(Error::Malformed("getOutputCount"))?;
        let get = descriptor
            .get_output_descriptor
            .ok_or(Error::Malformed("getOutputDescriptor"))?;
        let release = descriptor
            .release_output_descriptor
            .ok_or(Error::Malformed("releaseOutputDescriptor"))?;
        // SAFETY: the callback belongs to this descriptor and takes its handle.
        let output_count = unsafe { count(self.handle) } as usize;
        // The count is the plug-in's, so it is capped before the loop trusts it.
        if output_count > MAX_OUTPUTS {
            return Err(Error::Malformed("output count"));
        }
        let mut outputs = Vec::new();
        for index in 0..output_count {
            // SAFETY: the index is below the count the plug-in reported. The returned
            // descriptor is released below, after its strings and bins are copied.
            let raw = unsafe { get(self.handle, index as c_uint) };
            if raw.is_null() {
                return Err(Error::Malformed("output descriptor"));
            }
            // SAFETY: the pointer was just returned by the plug-in and is non-null.
            let output = unsafe { &*raw };
            // The bin count is the plug-in's too: never read more names than the cap allows.
            let bin_count = if output.has_fixed_bin_count != 0 {
                (output.bin_count as usize).min(MAX_BINS)
            } else {
                0
            };
            let bin_names = if bin_count > 0 && !output.bin_names.is_null() {
                // SAFETY: a fixed-bin output has `bin_count` names.
                let names = unsafe { std::slice::from_raw_parts(output.bin_names, bin_count) };
                names
                    .iter()
                    .map(|pointer| unsafe { string(*pointer) })
                    .collect()
            } else {
                Vec::new()
            };
            let info = OutputInfo {
                identifier: unsafe { string(output.identifier) },
                name: unsafe { string(output.name) },
                description: unsafe { string(output.description) },
                unit: unsafe { string(output.unit) },
                bin_count,
                bin_names,
                has_known_extents: output.has_known_extents != 0,
                min: output.min_value,
                max: output.max_value,
                quantized: output.is_quantized != 0,
                quantize_step: output.quantize_step,
                sample_type: match output.sample_type {
                    SAMPLE_ONE_PER_STEP => SampleType::OneSamplePerStep,
                    SAMPLE_FIXED_RATE => SampleType::FixedSampleRate,
                    _ => SampleType::VariableSampleRate,
                },
                sample_rate: output.sample_rate,
                // `hasDuration` only exists in API version 2; do not read past an older,
                // shorter descriptor.
                has_duration: self.api_version >= 2 && output.has_duration != 0,
            };
            // SAFETY: the descriptor was allocated by the plug-in for the host and is freed
            // once, after its contents were copied.
            unsafe { release(raw) };
            outputs.push(info);
        }
        Ok(outputs)
    }

    /// The value of parameter `index`.
    pub fn parameter(&self, index: usize) -> Option<f32> {
        let callback = self.descriptor_ref().get_parameter?;
        if index >= self.parameter_count {
            return None;
        }
        // SAFETY: the index is within the parameter count and the callback belongs to this
        // descriptor.
        Some(unsafe { callback(self.handle, index as c_int) })
    }

    /// Sets parameter `index` to `value`, before the analyser is initialised.
    pub fn set_parameter(&mut self, index: usize, value: f32) -> Result<(), Error> {
        if index >= self.parameter_count {
            return Err(Error::BadParameter(index));
        }
        if !value.is_finite() {
            return Err(Error::BadValue);
        }
        if self.initialized {
            return Err(Error::AlreadyInitialized("parameters"));
        }
        let callback = self
            .descriptor_ref()
            .set_parameter
            .ok_or(Error::Unsupported("setParameter"))?;
        // SAFETY: the index is within the parameter count and the callback belongs to this
        // descriptor.
        unsafe { callback(self.handle, index as c_int, value) };
        Ok(())
    }

    /// The selected program, or `None` when the analyser has none.
    pub fn current_program(&self) -> Option<usize> {
        if self.program_count == 0 {
            return None;
        }
        let callback = self.descriptor_ref().get_current_program?;
        // SAFETY: the analyser has programs and the callback belongs to this descriptor.
        Some(unsafe { callback(self.handle) } as usize)
    }

    /// Selects program `index`, before the analyser is initialised.
    pub fn select_program(&mut self, index: usize) -> Result<(), Error> {
        if index >= self.program_count {
            return Err(Error::BadProgram(index));
        }
        if self.initialized {
            return Err(Error::AlreadyInitialized("the program"));
        }
        let callback = self
            .descriptor_ref()
            .select_program
            .ok_or(Error::Unsupported("selectProgram"))?;
        // SAFETY: the index is within the program count and the callback belongs to this
        // descriptor.
        unsafe { callback(self.handle, index as c_uint) };
        Ok(())
    }

    /// Initialises the analyser for `channels` input channels, processing `block_size` samples
    /// every `step_size` samples. A step size below the block size makes successive windows
    /// overlap, which most onset and tempo analysers want.
    pub fn initialize(
        &mut self,
        channels: usize,
        step_size: u32,
        block_size: u32,
    ) -> Result<(), Error> {
        if self.initialized {
            return Err(Error::AlreadyInitialized("the analyser"));
        }
        if channels < self.min_channels || channels > self.max_channels {
            return Err(Error::ChannelRange {
                min: self.min_channels,
                max: self.max_channels,
                got: channels,
            });
        }
        if step_size == 0 || block_size == 0 {
            return Err(Error::BadValue);
        }
        let callback = self
            .descriptor_ref()
            .initialise
            .ok_or(Error::Malformed("initialise"))?;
        // SAFETY: the callback belongs to this descriptor; the channel count was checked
        // against the range the analyser reported.
        let ok = unsafe { callback(self.handle, channels as c_uint, step_size, block_size) };
        if ok == 0 {
            return Err(Error::Initialize);
        }
        self.channels = channels;
        self.block_size = block_size as usize;
        self.initialized = true;
        Ok(())
    }

    /// Initialises with the analyser's preferred window and step (or [`DEFAULT_BLOCK_SIZE`]).
    pub fn initialize_preferred(&mut self, channels: usize) -> Result<(), Error> {
        let block = self.preferred_block_size().unwrap_or(DEFAULT_BLOCK_SIZE);
        let step = self.preferred_step_size().unwrap_or(block);
        self.initialize(channels, step, block)
    }

    /// Clears the analyser's state so it can analyse a new piece of audio.
    pub fn reset(&mut self) {
        if let Some(callback) = self.descriptor_ref().reset {
            // SAFETY: the callback belongs to this descriptor and takes its handle.
            unsafe { callback(self.handle) };
        }
    }

    /// Analyses one block. Every slice is one input channel and must hold exactly the block
    /// size passed to [`Instance::initialize`]; pad the final, shorter block with silence.
    /// `sec`/`nsec` are the block's position in the analysed audio.
    pub fn process(
        &mut self,
        channels: &[&[f32]],
        sec: u32,
        nsec: u32,
    ) -> Result<Vec<Feature>, Error> {
        if !self.initialized {
            return Err(Error::NotInitialized);
        }
        if channels.len() != self.channels {
            return Err(Error::ChannelCount {
                expected: self.channels,
                got: channels.len(),
            });
        }
        for channel in channels {
            if channel.len() != self.block_size {
                return Err(Error::BlockSize {
                    expected: self.block_size,
                    got: channel.len(),
                });
            }
        }
        self.channel_ptrs.clear();
        for channel in channels {
            self.channel_ptrs.push(channel.as_ptr());
        }
        let callback = self
            .descriptor_ref()
            .process
            .ok_or(Error::Malformed("process"))?;
        // SAFETY: every channel holds `block_size` samples, `channel_ptrs` is complete for this
        // call, and the timestamps are passed in the documented two-int form.
        let list = unsafe {
            callback(
                self.handle,
                self.channel_ptrs.as_ptr(),
                sec as c_int,
                nsec as c_int,
            )
        };
        // SAFETY: the plug-in returns either NULL or a feature list it owns; it is copied and
        // released below.
        Ok(unsafe { self.copy_features(list, Some(f64::from(sec) + f64::from(nsec) / 1e9)) })
    }

    /// Returns the end-of-stream features the analyser held back, if any.
    pub fn finish(&mut self) -> Result<Vec<Feature>, Error> {
        if !self.initialized {
            return Err(Error::NotInitialized);
        }
        let Some(callback) = self.descriptor_ref().get_remaining_features else {
            return Ok(Vec::new());
        };
        // SAFETY: the callback belongs to this descriptor and takes its handle.
        let list = unsafe { callback(self.handle) };
        // SAFETY: the list is either NULL or owned by the plug-in; it is copied and released.
        Ok(unsafe { self.copy_features(list, None) })
    }

    fn descriptor_ref(&self) -> &RawDescriptor {
        // SAFETY: the descriptor pointer came from `Library::load` and the library is kept
        // mapped through `self._inner`.
        unsafe { &*self.descriptor }
    }

    /// Copies a plug-in feature list into owned features and releases it.
    ///
    /// SAFETY: `list` must be null or a list the plug-in returned for this instance and has
    /// not released; `fallback` is the timestamp for features that carry none.
    unsafe fn copy_features(
        &self,
        list: *mut RawFeatureList,
        fallback: Option<f64>,
    ) -> Vec<Feature> {
        // SAFETY: forwarded from this method's contract.
        unsafe { copy_feature_list(self.descriptor_ref(), self.api_version, list, fallback) }
    }
}

/// Copies a plug-in feature list into owned features and releases it. The counts come from the
/// plug-in, so they are capped before the memory they describe is read; the list is released
/// once, whether or not every entry was copied.
///
/// SAFETY: `list` must be null or a list the plug-in returned for an instance of `descriptor`
/// and has not released; `fallback` is the timestamp for features that carry none.
unsafe fn copy_feature_list(
    descriptor: &RawDescriptor,
    api_version: u32,
    list: *mut RawFeatureList,
    fallback: Option<f64>,
) -> Vec<Feature> {
    if list.is_null() {
        return Vec::new();
    }
    let release = descriptor.release_feature_set;
    let declared = (*list).feature_count as usize;
    let count = declared.min(MAX_FEATURES);
    let mut features = Vec::with_capacity(count);
    if !(*list).features.is_null() {
        for index in 0..count {
            // The Vamp Plugin SDK's `vamp-sdk.h` (`VampFeatureList` and `VampFeatureUnion`)
            // defines a version 2 list as `featureCount` version 1 features followed by
            // `featureCount` version 2 entries, so the duration of feature `i` is at
            // `featureCount + i`.
            let feature = &(*(*list).features.add(index)).v1;
            let timestamp = if feature.has_timestamp != 0 {
                Some(f64::from(feature.sec) + f64::from(feature.nsec) / 1e9)
            } else {
                fallback
            };
            // The value count is the plug-in's; read at most `MAX_VALUES` of them.
            let values = if feature.values.is_null() || feature.value_count == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(
                    feature.values,
                    (feature.value_count as usize).min(MAX_VALUES),
                )
                .to_vec()
            };
            let label = if feature.label.is_null() {
                None
            } else {
                Some(CStr::from_ptr(feature.label).to_string_lossy().into_owned())
            };
            // The version 2 half starts after the *declared* count (see `vamp-sdk.h`, cited
            // above). When that count was capped the second half is past the trusted range, so
            // it is not read.
            let duration = if api_version >= 2 && declared <= MAX_FEATURES {
                let v2 = &(*(*list).features.add(declared + index)).v2;
                (v2.has_duration != 0)
                    .then(|| f64::from(v2.duration_sec) + f64::from(v2.duration_nsec) / 1e9)
            } else {
                None
            };
            features.push(Feature {
                timestamp,
                duration,
                values,
                label,
            });
        }
    }
    if let Some(release) = release {
        // SAFETY: the plug-in owns the list and frees it and its feature data in one call; it
        // is released once, after the copy.
        release(list);
    }
    features
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(cleanup) = self.descriptor_ref().cleanup {
            // SAFETY: `cleanup` frees the instance created by `instantiate`; called once. The
            // gate is the same one `instantiate` uses, for plug-ins that share state.
            let _gate = shared_calls();
            unsafe { cleanup(self.handle) };
        }
    }
}

/// Reads one descriptor into owned metadata.
///
/// SAFETY: `raw` must point to a valid `VampPluginDescriptor` that stays mapped while the call
/// reads it; its parameter, program and string tables must be valid for their counts.
unsafe fn read_info(raw: *const RawDescriptor) -> Result<PluginInfo, Error> {
    let descriptor = &*raw;
    let parameter_count = descriptor.parameter_count as usize;
    if parameter_count > MAX_LISTS {
        return Err(Error::Malformed("parameter table"));
    }
    let mut parameters = Vec::with_capacity(parameter_count);
    if parameter_count > 0 {
        if descriptor.parameters.is_null() {
            return Err(Error::Malformed("parameter table"));
        }
        let table = std::slice::from_raw_parts(descriptor.parameters, parameter_count);
        for pointer in table {
            // A null entry cannot be skipped: the remaining parameters would shift down and
            // `set_parameter(i)` would reach the wrong plug-in parameter.
            if pointer.is_null() {
                return Err(Error::Malformed("parameter entry"));
            }
            let parameter = &**pointer;
            let mut value_names = Vec::new();
            if parameter.is_quantized != 0 && !parameter.value_names.is_null() {
                // A quantized parameter's value-name array ends with a NULL pointer.
                for index in 0..MAX_LISTS {
                    let name = *parameter.value_names.add(index);
                    if name.is_null() {
                        break;
                    }
                    value_names.push(string(name));
                }
            }
            parameters.push(ParameterInfo {
                identifier: string(parameter.identifier),
                name: string(parameter.name),
                description: string(parameter.description),
                unit: string(parameter.unit),
                min: parameter.min_value,
                max: parameter.max_value,
                default: parameter.default_value,
                quantized: parameter.is_quantized != 0,
                quantize_step: parameter.quantize_step,
                value_names,
            });
        }
    }

    let program_count = descriptor.program_count as usize;
    if program_count > MAX_LISTS {
        return Err(Error::Malformed("program table"));
    }
    let programs = if program_count > 0 && !descriptor.programs.is_null() {
        let table = std::slice::from_raw_parts(descriptor.programs, program_count);
        table.iter().map(|pointer| string(*pointer)).collect()
    } else {
        Vec::new()
    };

    Ok(PluginInfo {
        api_version: descriptor.api_version,
        identifier: string(descriptor.identifier),
        name: string(descriptor.name),
        description: string(descriptor.description),
        maker: string(descriptor.maker),
        // `pluginVersion` is an int in the ABI; a negative version is not meaningful.
        plugin_version: descriptor.plugin_version as u32,
        copyright: string(descriptor.copyright),
        input_domain: if descriptor.input_domain == INPUT_TIME_DOMAIN {
            InputDomain::Time
        } else {
            InputDomain::Frequency
        },
        parameters,
        programs,
    })
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
    fn the_feature_union_matches_the_c_layout() {
        // VampFeature: three ints, a count, and two pointers. The union is the larger of that
        // and the three-int VampFeatureV2, so it is pointer-sized and aligned.
        assert_eq!(
            std::mem::size_of::<RawFeature>(),
            std::mem::size_of::<*const c_void>() * 2 + std::mem::size_of::<usize>() * 2
        );
        assert_eq!(
            std::mem::size_of::<RawFeatureUnion>(),
            std::mem::size_of::<RawFeature>()
        );
        assert_eq!(std::mem::size_of::<RawFeatureV2>(), 12);
        // The pointer is aligned, so the count is followed by padding to pointer alignment.
        assert_eq!(
            std::mem::offset_of!(RawFeatureList, features),
            std::mem::size_of::<*mut RawFeatureUnion>()
        );
    }
}

/// Tests on memory built in Rust: no plug-in is loaded and no foreign function is called, so
/// these also run under `cargo +nightly miri test --lib`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod memory_tests {
    use super::*;
    use std::ffi::CString;

    /// A descriptor with no metadata and no callbacks.
    fn descriptor() -> RawDescriptor {
        RawDescriptor {
            api_version: 2,
            identifier: std::ptr::null(),
            name: std::ptr::null(),
            description: std::ptr::null(),
            maker: std::ptr::null(),
            plugin_version: 0,
            copyright: std::ptr::null(),
            parameter_count: 0,
            parameters: std::ptr::null(),
            program_count: 0,
            programs: std::ptr::null(),
            input_domain: 0,
            instantiate: None,
            cleanup: None,
            initialise: None,
            reset: None,
            get_parameter: None,
            set_parameter: None,
            get_current_program: None,
            select_program: None,
            preferred_step_size: None,
            preferred_block_size: None,
            min_channel_count: None,
            max_channel_count: None,
            output_count: None,
            get_output_descriptor: None,
            release_output_descriptor: None,
            process: None,
            get_remaining_features: None,
            release_feature_set: None,
        }
    }

    #[test]
    fn a_feature_list_built_in_rust_is_copied() {
        let label = CString::new("beat").unwrap();
        let mut values = [0.25f32, 0.75];
        let count = 2;
        let mut unions = vec![
            RawFeatureUnion {
                v1: RawFeature {
                    has_timestamp: 1,
                    sec: 3,
                    nsec: 500_000_000,
                    value_count: 2,
                    values: values.as_mut_ptr(),
                    label: label.as_ptr() as *mut c_char,
                },
            },
            RawFeatureUnion {
                v1: RawFeature {
                    has_timestamp: 0,
                    sec: 0,
                    nsec: 0,
                    value_count: 0,
                    values: std::ptr::null_mut(),
                    label: std::ptr::null_mut(),
                },
            },
            // API version 2: the duration half follows the version 1 features.
            RawFeatureUnion {
                v2: RawFeatureV2 {
                    has_duration: 1,
                    duration_sec: 1,
                    duration_nsec: 250_000_000,
                },
            },
            RawFeatureUnion {
                v2: RawFeatureV2 {
                    has_duration: 0,
                    duration_sec: 0,
                    duration_nsec: 0,
                },
            },
        ];
        let mut list = RawFeatureList {
            feature_count: count,
            features: unions.as_mut_ptr(),
        };
        // SAFETY: the list, its unions, values and label all live in this test.
        let copied = unsafe { copy_feature_list(&descriptor(), 2, &mut list, Some(9.5)) };
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].timestamp, Some(3.5));
        assert_eq!(copied[0].duration, Some(1.25));
        assert_eq!(copied[0].values, vec![0.25, 0.75]);
        assert_eq!(copied[0].label.as_deref(), Some("beat"));
        // A feature with no timestamp of its own takes the block's.
        assert_eq!(copied[1].timestamp, Some(9.5));
        assert!(copied[1].values.is_empty());
        assert_eq!(copied[1].duration, None);
    }

    #[test]
    fn descriptor_tables_over_the_cap_are_refused_before_they_are_read() {
        // The tables are NULL: the cap must be checked before anything dereferences them.
        let mut raw = descriptor();
        raw.parameter_count = MAX_LISTS as c_uint + 1;
        // SAFETY: the count is over the cap, so `read_info` returns before reading the table.
        assert_eq!(
            unsafe { read_info(&raw) },
            Err(Error::Malformed("parameter table"))
        );
        raw.parameter_count = 0;
        raw.program_count = MAX_LISTS as c_uint + 1;
        // SAFETY: as above, for the program table.
        assert_eq!(
            unsafe { read_info(&raw) },
            Err(Error::Malformed("program table"))
        );
    }

    #[test]
    fn a_null_parameter_entry_is_refused_instead_of_shifting_the_indices() {
        let parameters = [std::ptr::null::<RawParameterDescriptor>()];
        let raw = RawDescriptor {
            parameter_count: 1,
            parameters: parameters.as_ptr(),
            ..descriptor()
        };
        // SAFETY: the table holds one null entry and its count matches.
        assert_eq!(
            unsafe { read_info(&raw) },
            Err(Error::Malformed("parameter entry"))
        );
    }

    #[test]
    fn metadata_built_in_rust_is_read_into_owned_values() {
        let identifier = CString::new("vamp-test:gain").unwrap();
        let name = CString::new("Gain").unwrap();
        let description = CString::new("").unwrap();
        let unit = CString::new("dB").unwrap();
        let parameter = RawParameterDescriptor {
            identifier: identifier.as_ptr(),
            name: name.as_ptr(),
            description: description.as_ptr(),
            unit: unit.as_ptr(),
            min_value: -60.0,
            max_value: 12.0,
            default_value: 0.0,
            is_quantized: 0,
            quantize_step: 0.1,
            value_names: std::ptr::null(),
        };
        let parameters = [&parameter as *const RawParameterDescriptor];
        let raw = RawDescriptor {
            parameter_count: 1,
            parameters: parameters.as_ptr(),
            ..descriptor()
        };
        // SAFETY: the parameter and its strings live in this test, and the count matches.
        let info = unsafe { read_info(&raw) }.unwrap();
        assert_eq!(info.parameters.len(), 1);
        assert_eq!(info.parameters[0].identifier, "vamp-test:gain");
        assert_eq!(info.parameters[0].name, "Gain");
        assert_eq!(info.parameters[0].unit, "dB");
        assert_eq!(info.parameters[0].min, -60.0);
        assert_eq!(info.parameters[0].max, 12.0);
    }
}
