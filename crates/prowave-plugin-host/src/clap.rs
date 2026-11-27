//! CLAP hosting (CLever Audio Plug-in, <https://cleveraudio.org>): the C interface of CLAP 1.x,
//! written out for what an audio effect needs: the factory, audio ports, parameters, latency
//! and state.
//!
//! A CLAP file is a shared library exporting `clap_entry` (on macOS it is the program of a
//! bundle). A library is loaded once and never unloaded: unloading code that may still have
//! threads or static data is where hosts crash.

use crate::{
    buffer_text, c_text, Backend, EditorParent, Error, Format, ParamInfo, PluginInfo, MAX_CHANGES,
};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The most plug-ins one file may list.
const MAX_PLUGINS: u32 = 4096;
/// The most audio ports of one direction, and channels of one port.
const MAX_PORTS: u32 = 32;
const MAX_CHANNELS: u32 = 64;
/// The largest state read from a plug-in (64 MiB).
const MAX_STATE: usize = 64 * 1024 * 1024;

const CORE_EVENT_SPACE: u16 = 0;
const EVENT_PARAM_VALUE: u16 = 5;
const PARAM_IS_STEPPED: u32 = 1 << 0;
const PARAM_IS_HIDDEN: u32 = 1 << 2;
const PARAM_IS_READONLY: u32 = 1 << 3;
const PARAM_IS_BYPASS: u32 = 1 << 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct Version {
    major: u32,
    minor: u32,
    revision: u32,
}

const VERSION: Version = Version {
    major: 1,
    minor: 2,
    revision: 0,
};

#[repr(C)]
struct Entry {
    version: Version,
    init: Option<unsafe extern "C" fn(*const c_char) -> bool>,
    deinit: Option<unsafe extern "C" fn()>,
    get_factory: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
struct Factory {
    count: Option<unsafe extern "C" fn(*const Factory) -> u32>,
    descriptor: Option<unsafe extern "C" fn(*const Factory, u32) -> *const Descriptor>,
    create:
        Option<unsafe extern "C" fn(*const Factory, *const Host, *const c_char) -> *const Plugin>,
}

#[repr(C)]
struct Descriptor {
    version: Version,
    id: *const c_char,
    name: *const c_char,
    vendor: *const c_char,
    url: *const c_char,
    manual_url: *const c_char,
    support_url: *const c_char,
    plugin_version: *const c_char,
    description: *const c_char,
    features: *const *const c_char,
}

#[repr(C)]
struct Host {
    version: Version,
    data: *mut c_void,
    name: *const c_char,
    vendor: *const c_char,
    url: *const c_char,
    host_version: *const c_char,
    get_extension: unsafe extern "C" fn(*const Host, *const c_char) -> *const c_void,
    request_restart: unsafe extern "C" fn(*const Host),
    request_process: unsafe extern "C" fn(*const Host),
    request_callback: unsafe extern "C" fn(*const Host),
}

#[repr(C)]
struct Plugin {
    desc: *const Descriptor,
    data: *mut c_void,
    init: Option<unsafe extern "C" fn(*const Plugin) -> bool>,
    destroy: Option<unsafe extern "C" fn(*const Plugin)>,
    activate: Option<unsafe extern "C" fn(*const Plugin, f64, u32, u32) -> bool>,
    deactivate: Option<unsafe extern "C" fn(*const Plugin)>,
    start_processing: Option<unsafe extern "C" fn(*const Plugin) -> bool>,
    stop_processing: Option<unsafe extern "C" fn(*const Plugin)>,
    reset: Option<unsafe extern "C" fn(*const Plugin)>,
    process: Option<unsafe extern "C" fn(*const Plugin, *const Process) -> i32>,
    get_extension: Option<unsafe extern "C" fn(*const Plugin, *const c_char) -> *const c_void>,
    on_main_thread: Option<unsafe extern "C" fn(*const Plugin)>,
}

#[repr(C)]
struct AudioBuffer {
    data32: *mut *mut f32,
    data64: *mut *mut f64,
    channel_count: u32,
    latency: u32,
    constant_mask: u64,
}

#[repr(C)]
struct Process {
    steady_time: i64,
    frames: u32,
    transport: *const c_void,
    inputs: *const AudioBuffer,
    outputs: *mut AudioBuffer,
    input_count: u32,
    output_count: u32,
    in_events: *const InputEvents,
    out_events: *const OutputEvents,
}

#[repr(C)]
struct InputEvents {
    ctx: *mut c_void,
    size: unsafe extern "C" fn(*const InputEvents) -> u32,
    get: unsafe extern "C" fn(*const InputEvents, u32) -> *const EventHeader,
}

#[repr(C)]
struct OutputEvents {
    ctx: *mut c_void,
    try_push: unsafe extern "C" fn(*const OutputEvents, *const EventHeader) -> bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHeader {
    size: u32,
    time: u32,
    space: u16,
    kind: u16,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ParamValueEvent {
    header: EventHeader,
    param_id: u32,
    cookie: *mut c_void,
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    value: f64,
}

#[repr(C)]
struct ParamsExt {
    count: Option<unsafe extern "C" fn(*const Plugin) -> u32>,
    get_info: Option<unsafe extern "C" fn(*const Plugin, u32, *mut RawParam) -> bool>,
    get_value: Option<unsafe extern "C" fn(*const Plugin, u32, *mut f64) -> bool>,
    value_to_text: Option<unsafe extern "C" fn(*const Plugin, u32, f64, *mut c_char, u32) -> bool>,
    text_to_value:
        Option<unsafe extern "C" fn(*const Plugin, u32, *const c_char, *mut f64) -> bool>,
    flush: Option<unsafe extern "C" fn(*const Plugin, *const InputEvents, *const OutputEvents)>,
}

#[repr(C)]
struct RawParam {
    id: u32,
    flags: u32,
    cookie: *mut c_void,
    name: [u8; 256],
    module: [u8; 1024],
    min: f64,
    max: f64,
    default: f64,
}

#[repr(C)]
struct LatencyExt {
    get: Option<unsafe extern "C" fn(*const Plugin) -> u32>,
}

#[repr(C)]
struct StateExt {
    save: Option<unsafe extern "C" fn(*const Plugin, *const OutStream) -> bool>,
    load: Option<unsafe extern "C" fn(*const Plugin, *const InStream) -> bool>,
}

#[repr(C)]
struct OutStream {
    ctx: *mut c_void,
    write: unsafe extern "C" fn(*const OutStream, *const c_void, u64) -> i64,
}

#[repr(C)]
struct InStream {
    ctx: *mut c_void,
    read: unsafe extern "C" fn(*const InStream, *mut c_void, u64) -> i64,
}

#[repr(C)]
struct AudioPortsExt {
    count: Option<unsafe extern "C" fn(*const Plugin, bool) -> u32>,
    get: Option<unsafe extern "C" fn(*const Plugin, u32, bool, *mut RawPort) -> bool>,
}

#[repr(C)]
struct RawPort {
    id: u32,
    name: [u8; 256],
    flags: u32,
    channel_count: u32,
    port_type: *const c_char,
    in_place_pair: u32,
}

/// A loaded CLAP library: its factory, valid for the rest of the program.
#[derive(Clone, Copy)]
struct Loaded(*const Factory);

// SAFETY: the factory belongs to a library that is never unloaded, and CLAP's factory functions
// may be called from any one thread at a time; the registry's lock gives that.
unsafe impl Send for Loaded {}

fn registry() -> &'static Mutex<HashMap<PathBuf, Loaded>> {
    static LOADED: OnceLock<Mutex<HashMap<PathBuf, Loaded>>> = OnceLock::new();
    LOADED.get_or_init(Default::default)
}

/// The library file of a CLAP path: the program inside a macOS bundle, else the path itself.
fn library_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        crate::bundle_binary(path).unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Runs `f` with the factory of the CLAP file at `path`, loading the file the first time.
fn with_factory<T>(path: &Path, f: impl FnOnce(&Factory) -> T) -> Result<T, Error> {
    let bad = |why: &str| Error::NotAPlugin(format!("{}: {why}", path.display()));
    let mut loaded = registry()
        .lock()
        .map_err(|_| bad("the registry is broken"))?;
    if let Some(Loaded(factory)) = loaded.get(path) {
        // SAFETY: the pointer came from `get_factory` of a library that stays loaded.
        return Ok(f(unsafe { &**factory }));
    }
    let file = library_file(path);
    // SAFETY: loading a library runs its initialisers; that is what hosting a plug-in is.
    let library = unsafe { libloading::Library::new(&file) }.map_err(|e| bad(&e.to_string()))?;
    // SAFETY: `clap_entry` is a `clap_plugin_entry_t` object in every CLAP library.
    let entry = unsafe { library.get::<*const Entry>(b"clap_entry\0") }
        .map_err(|_| bad("no clap_entry"))?;
    let entry: *const Entry = *entry;
    if entry.is_null() {
        return Err(bad("clap_entry is null"));
    }
    // SAFETY: `entry` points to the library's static entry, checked not null.
    let entry = unsafe { &*entry };
    if entry.version.major < 1 {
        return Err(bad("a CLAP draft older than 1.0"));
    }
    let (Some(init), Some(get_factory)) = (entry.init, entry.get_factory) else {
        return Err(bad("an incomplete clap_entry"));
    };
    let c_path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| bad("a bad path"))?;
    // SAFETY: `init` gets the plug-in's path as a NUL-terminated string, once per library.
    if !unsafe { init(c_path.as_ptr()) } {
        return Err(bad("clap_entry.init failed"));
    }
    // SAFETY: the factory id is a NUL-terminated string; the result is null or a factory that
    // lives as long as the library.
    let factory = unsafe { get_factory(c"clap.plugin-factory".as_ptr()) }.cast::<Factory>();
    if factory.is_null() {
        return Err(bad("no plug-in factory"));
    }
    // Never unloaded (see the module comment).
    std::mem::forget(library);
    loaded.insert(path.to_path_buf(), Loaded(factory));
    // SAFETY: checked not null above; lives as long as the library.
    Ok(f(unsafe { &*factory }))
}

/// Whether the NULL-terminated `features` list names `wanted`.
///
/// # Safety
/// `features` is null or a NULL-terminated array of NUL-terminated strings.
unsafe fn has_feature(features: *const *const c_char, wanted: &CStr) -> bool {
    if features.is_null() {
        return false;
    }
    for i in 0..64 {
        // SAFETY: the array ends at its first null, which stops this loop.
        let item = unsafe { *features.add(i) };
        if item.is_null() {
            break;
        }
        // SAFETY: a non-null item is a NUL-terminated string.
        if unsafe { CStr::from_ptr(item) } == wanted {
            return true;
        }
    }
    false
}

pub(crate) fn scan(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    with_factory(path, |factory| {
        let (Some(count), Some(descriptor)) = (factory.count, factory.descriptor) else {
            return Vec::new();
        };
        // SAFETY: the factory's own functions, called with the factory.
        let n = unsafe { count(factory) }.min(MAX_PLUGINS);
        let mut out = Vec::new();
        for i in 0..n {
            // SAFETY: `i` is below the count the factory gave.
            let desc = unsafe { descriptor(factory, i) };
            if desc.is_null() {
                continue;
            }
            // SAFETY: a non-null descriptor is static data of the library.
            let desc = unsafe { &*desc };
            // SAFETY: descriptor strings are null or NUL-terminated; `features` is a
            // NULL-terminated list.
            let (id, name, vendor, effect) = unsafe {
                (
                    c_text(desc.id),
                    c_text(desc.name),
                    c_text(desc.vendor),
                    has_feature(desc.features, c"audio-effect"),
                )
            };
            if effect && !id.is_empty() {
                out.push(PluginInfo {
                    format: Format::Clap,
                    name: if name.is_empty() { id.clone() } else { name },
                    id,
                    vendor,
                    path: path.to_path_buf(),
                });
            }
        }
        out
    })
}

/// One parameter as the plug-in describes it.
#[derive(Clone, Copy)]
struct Param {
    id: u32,
    cookie: *mut c_void,
    min: f64,
    max: f64,
    stepped: bool,
}

impl Param {
    fn plain(&self, norm: f32) -> f64 {
        let v = self.min + (self.max - self.min) * f64::from(norm.clamp(0.0, 1.0));
        if self.stepped {
            v.round()
        } else {
            v
        }
    }

    fn norm(&self, plain: f64) -> f32 {
        if self.max > self.min && plain.is_finite() {
            (((plain - self.min) / (self.max - self.min)) as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// The buffers one port's channels point to.
struct Port {
    channels: Vec<*mut f32>,
}

/// What the audio thread uses in `process`; made once at load.
struct Audio {
    started: bool,
    failed: bool,
    block: usize,
    /// Input: the block's sound (the plug-in may not write here). Output: what it made.
    input: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    /// Silence for the inputs nobody feeds, and a place for the outputs nobody hears.
    silence: Vec<f32>,
    spare: Vec<f32>,
    in_ports: Vec<Port>,
    out_ports: Vec<Port>,
    in_buffers: Vec<AudioBuffer>,
    out_buffers: Vec<AudioBuffer>,
    events: [ParamValueEvent; MAX_CHANGES],
    event_count: u32,
}

pub(crate) struct ClapBackend {
    plugin: *const Plugin,
    /// Kept for the plug-in, which holds a pointer to it.
    _host: Box<Host>,
    /// What the plug-in asked of the host; the host's `data` points here.
    state: Box<HostState>,
    gui_ext: *const GuiExt,
    /// The plug-in's view was made (`clap_plugin_gui.create`) and not destroyed yet.
    editor: AtomicBool,
    params_ext: *const ParamsExt,
    latency_ext: *const LatencyExt,
    state_ext: *const StateExt,
    params: Vec<Param>,
    names: Vec<ParamInfo>,
    audio: UnsafeCell<Audio>,
}

/// What a plug-in asks of its host between calls.
struct HostState {
    /// It asked for `on_main_thread`.
    callback: AtomicBool,
    /// It asked its window to take this size (width in the high half, height in the low; 0:
    /// nothing asked).
    resize: AtomicU64,
}

/// `clap_plugin_gui`: the plug-in's own view.
#[repr(C)]
struct GuiExt {
    is_api_supported: Option<unsafe extern "C" fn(*const Plugin, *const c_char, bool) -> bool>,
    get_preferred_api:
        Option<unsafe extern "C" fn(*const Plugin, *mut *const c_char, *mut bool) -> bool>,
    create: Option<unsafe extern "C" fn(*const Plugin, *const c_char, bool) -> bool>,
    destroy: Option<unsafe extern "C" fn(*const Plugin)>,
    set_scale: Option<unsafe extern "C" fn(*const Plugin, f64) -> bool>,
    get_size: Option<unsafe extern "C" fn(*const Plugin, *mut u32, *mut u32) -> bool>,
    can_resize: Option<unsafe extern "C" fn(*const Plugin) -> bool>,
    get_resize_hints: Option<unsafe extern "C" fn(*const Plugin, *mut c_void) -> bool>,
    adjust_size: Option<unsafe extern "C" fn(*const Plugin, *mut u32, *mut u32) -> bool>,
    set_size: Option<unsafe extern "C" fn(*const Plugin, u32, u32) -> bool>,
    set_parent: Option<unsafe extern "C" fn(*const Plugin, *const GuiWindow) -> bool>,
    set_transient: Option<unsafe extern "C" fn(*const Plugin, *const GuiWindow) -> bool>,
    suggest_title: Option<unsafe extern "C" fn(*const Plugin, *const c_char)>,
    show: Option<unsafe extern "C" fn(*const Plugin) -> bool>,
    hide: Option<unsafe extern "C" fn(*const Plugin) -> bool>,
}

#[repr(C)]
struct GuiWindow {
    api: *const c_char,
    handle: *mut c_void,
}

/// `clap_host_gui`: what a plug-in's view asks of the window it is in.
#[repr(C)]
struct HostGui {
    resize_hints_changed: unsafe extern "C" fn(*const Host),
    request_resize: unsafe extern "C" fn(*const Host, u32, u32) -> bool,
    request_show: unsafe extern "C" fn(*const Host) -> bool,
    request_hide: unsafe extern "C" fn(*const Host) -> bool,
    closed: unsafe extern "C" fn(*const Host, bool),
}

static HOST_GUI: HostGui = HostGui {
    resize_hints_changed: host_ignore,
    request_resize: host_resize,
    request_show: host_refuse,
    request_hide: host_refuse,
    closed: host_closed,
};

/// CLAP's X11 window API id, from `CLAP_WINDOW_API_X11` in `clap/ext/gui.h`.
#[cfg(target_os = "linux")]
pub(crate) const CLAP_X11_API: &CStr = c"x11";

/// CLAP's `clap_xwnd`: the X11 window id, with no display pointer in the ABI handle.
#[cfg(target_os = "linux")]
#[repr(transparent)]
pub(crate) struct ClapX11Window(pub(crate) std::os::raw::c_ulong);

#[cfg(target_os = "linux")]
impl ClapX11Window {
    /// The pointer-sized value CLAP reads as its `clap_window_t.x11` union member.
    fn handle(&self) -> *mut c_void {
        self.0 as usize as *mut c_void
    }
}

/// The window API this system's plug-in views use (none on other systems).
#[cfg(target_os = "macos")]
const WINDOW_API: Option<&CStr> = Some(c"cocoa");
#[cfg(target_os = "linux")]
const WINDOW_API: Option<&CStr> = Some(CLAP_X11_API);
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const WINDOW_API: Option<&CStr> = None;

/// # Safety
/// `host` is null or the `Host` this module made, whose `data` is the backend's `HostState`.
unsafe fn host_state<'a>(host: *const Host) -> Option<&'a HostState> {
    if host.is_null() {
        return None;
    }
    // SAFETY: the caller's contract; the state lives as long as the plug-in.
    unsafe { (*host).data.cast::<HostState>().as_ref() }
}

unsafe extern "C" fn host_resize(host: *const Host, width: u32, height: u32) -> bool {
    // SAFETY: the plug-in passes the host it was made with.
    match unsafe { host_state(host) } {
        Some(state) if width > 0 && height > 0 => {
            let size = (u64::from(width) << 32) | u64::from(height);
            state.resize.store(size, Ordering::Release);
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn host_refuse(_: *const Host) -> bool {
    false
}

unsafe extern "C" fn host_closed(_: *const Host, _: bool) {}

unsafe extern "C" fn host_extension(_: *const Host, name: *const c_char) -> *const c_void {
    // SAFETY: the plug-in passes a NUL-terminated extension id.
    if !name.is_null() && unsafe { CStr::from_ptr(name) } == c"clap.gui" {
        return std::ptr::from_ref(&HOST_GUI).cast();
    }
    std::ptr::null()
}

unsafe extern "C" fn host_ignore(_: *const Host) {}

unsafe extern "C" fn host_callback(host: *const Host) {
    // SAFETY: the plug-in passes the host it was made with.
    if let Some(state) = unsafe { host_state(host) } {
        state.callback.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn events_size(list: *const InputEvents) -> u32 {
    // SAFETY: `ctx` is the `Audio` of the block being processed (set in `process`).
    unsafe { (*(*list).ctx.cast::<Audio>()).event_count }
}

unsafe extern "C" fn events_get(list: *const InputEvents, index: u32) -> *const EventHeader {
    // SAFETY: as in `events_size`.
    let audio = unsafe { &*(*list).ctx.cast::<Audio>() };
    if index < audio.event_count {
        &audio.events[index as usize].header
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn events_push(_: *const OutputEvents, _: *const EventHeader) -> bool {
    // What the plug-in reports back (its own parameter moves) is not followed yet.
    true
}

unsafe extern "C" fn stream_write(stream: *const OutStream, data: *const c_void, n: u64) -> i64 {
    // SAFETY: `ctx` is the `Vec<u8>` of the `main_save` call in progress.
    let out = unsafe { &mut *(*stream).ctx.cast::<Vec<u8>>() };
    let n = n as usize;
    if data.is_null() || out.len().saturating_add(n) > MAX_STATE {
        return -1;
    }
    // SAFETY: the plug-in gives `n` readable bytes at `data`.
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), n) });
    n as i64
}

unsafe extern "C" fn stream_read(stream: *const InStream, data: *mut c_void, n: u64) -> i64 {
    // SAFETY: `ctx` is the `(&[u8], usize)` cursor of the `main_load` call in progress.
    let (bytes, at) = unsafe { &mut *(*stream).ctx.cast::<(&[u8], usize)>() };
    let n = (n as usize).min(bytes.len() - *at);
    if data.is_null() {
        return -1;
    }
    // SAFETY: the plug-in gives `n` writable bytes at `data`; the source range is in `bytes`.
    unsafe { std::ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), data.cast::<u8>(), n) };
    *at += n;
    n as i64
}

/// The channel counts of the plug-in's audio ports in one direction (none without the
/// audio-ports extension: CLAP gives such a plug-in no audio).
fn ports(plugin: *const Plugin, ext: *const AudioPortsExt, input: bool) -> Vec<u32> {
    if ext.is_null() {
        return Vec::new();
    }
    // SAFETY: a non-null extension is static data of the plug-in.
    let ext = unsafe { &*ext };
    let (Some(count), Some(get)) = (ext.count, ext.get) else {
        return Vec::new();
    };
    // SAFETY: the extension's functions, called with the plug-in on the main thread.
    let n = unsafe { count(plugin, input) }.min(MAX_PORTS);
    (0..n)
        .map(|i| {
            // SAFETY: `RawPort` is plain data; all-zero is a valid value.
            let mut raw: RawPort = unsafe { std::mem::zeroed() };
            // SAFETY: `i` is below the count; `raw` is a writable `clap_audio_port_info`.
            if unsafe { get(plugin, i, input, &mut raw) } {
                raw.channel_count.min(MAX_CHANNELS)
            } else {
                0
            }
        })
        .collect()
}

pub(crate) fn load(path: &Path, id: &str, rate: f64, block: usize) -> Result<ClapBackend, Error> {
    let c_id = CString::new(id).map_err(|_| Error::NotAPlugin("a bad plug-in id".into()))?;
    let state = Box::new(HostState {
        callback: AtomicBool::new(false),
        resize: AtomicU64::new(0),
    });
    let host = Box::new(Host {
        version: VERSION,
        data: std::ptr::from_ref::<HostState>(&state).cast_mut().cast(),
        name: c"Pro Wave".as_ptr(),
        vendor: c"Pro Wave".as_ptr(),
        url: c"https://github.com/fabox-os/Pro-Wave".as_ptr(),
        host_version: c"0.1.0".as_ptr(),
        get_extension: host_extension,
        request_restart: host_ignore,
        request_process: host_ignore,
        request_callback: host_callback,
    });
    let plugin = with_factory(path, |factory| match factory.create {
        // SAFETY: the factory's own function; `host` lives as long as the plug-in (it is kept
        // in the backend) and `c_id` for the call.
        Some(create) => unsafe { create(factory, &*host, c_id.as_ptr()) },
        None => std::ptr::null(),
    })?;
    if plugin.is_null() {
        return Err(Error::Refused("make the plug-in"));
    }
    // SAFETY: checked not null; the plug-in lives until `destroy`.
    let p = unsafe { &*plugin };
    let destroy = |step| {
        if let Some(destroy) = p.destroy {
            // SAFETY: a plug-in that failed to start is destroyed, as CLAP asks.
            unsafe { destroy(plugin) };
        }
        Error::Refused(step)
    };
    // SAFETY: `init` is the first call on a new plug-in, on the main thread.
    if !p.init.is_some_and(|init| unsafe { init(plugin) }) {
        return Err(destroy("start"));
    }
    let extension = |name: &CStr| match p.get_extension {
        // SAFETY: the plug-in's own function with a NUL-terminated id, after `init`.
        Some(get) => unsafe { get(plugin, name.as_ptr()) },
        None => std::ptr::null(),
    };
    let params_ext = extension(c"clap.params").cast::<ParamsExt>();
    let latency_ext = extension(c"clap.latency").cast::<LatencyExt>();
    let state_ext = extension(c"clap.state").cast::<StateExt>();
    let gui_ext = extension(c"clap.gui").cast::<GuiExt>();
    let ports_ext = extension(c"clap.audio-ports").cast::<AudioPortsExt>();

    let ins = ports(plugin, ports_ext, true);
    let outs = ports(plugin, ports_ext, false);
    let main = |list: &[u32]| list.first().is_some_and(|c| (1..=2).contains(c));
    if !main(&ins) || !main(&outs) {
        destroy("");
        return Err(Error::NotAnEffect);
    }

    let mut params = Vec::new();
    let mut names = Vec::new();
    // SAFETY: a non-null extension is static data of the plug-in.
    if let Some(ext) = unsafe { params_ext.as_ref() } {
        if let (Some(count), Some(get_info)) = (ext.count, ext.get_info) {
            // SAFETY: the extension's function, on the main thread.
            let n = unsafe { count(plugin) }.min(crate::MAX_PARAMS as u32);
            for i in 0..n {
                // SAFETY: `RawParam` is plain data; all-zero is a valid value.
                let mut raw: RawParam = unsafe { std::mem::zeroed() };
                // SAFETY: `i` is below the count; `raw` is a writable `clap_param_info`.
                if !unsafe { get_info(plugin, i, &mut raw) } {
                    continue;
                }
                if raw.flags & (PARAM_IS_HIDDEN | PARAM_IS_READONLY | PARAM_IS_BYPASS) != 0
                    || !(raw.min.is_finite() && raw.max.is_finite() && raw.max > raw.min)
                {
                    continue;
                }
                let param = Param {
                    id: raw.id,
                    cookie: raw.cookie,
                    min: raw.min,
                    max: raw.max,
                    stepped: raw.flags & PARAM_IS_STEPPED != 0,
                };
                names.push(ParamInfo {
                    name: buffer_text(&raw.name),
                    default: param.norm(raw.default),
                });
                params.push(param);
            }
        }
    }

    // SAFETY: `activate` on the main thread, after `init`, with the engine's rate and block.
    if !p
        .activate
        .is_some_and(|activate| unsafe { activate(plugin, rate, 1, block as u32) })
    {
        return Err(destroy("activate"));
    }

    let mut audio = Audio {
        started: false,
        failed: false,
        block,
        input: [vec![0.0; block], vec![0.0; block]],
        output: [vec![0.0; block], vec![0.0; block]],
        silence: vec![0.0; block],
        spare: vec![0.0; block],
        in_ports: Vec::new(),
        out_ports: Vec::new(),
        in_buffers: Vec::new(),
        out_buffers: Vec::new(),
        events: [ParamValueEvent {
            header: EventHeader {
                size: std::mem::size_of::<ParamValueEvent>() as u32,
                time: 0,
                space: CORE_EVENT_SPACE,
                kind: EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: 0,
            cookie: std::ptr::null_mut(),
            note_id: -1,
            port: -1,
            channel: -1,
            key: -1,
            value: 0.0,
        }; MAX_CHANGES],
        event_count: 0,
    };
    // The first port of each direction carries the strip's sound; a mono port gets the left
    // channel. Every other channel is fed silence or written to the spare buffer.
    for (index, channels) in ins.iter().enumerate() {
        let list = (0..*channels)
            .map(|c| match (index, c) {
                (0, 0) => audio.input[0].as_mut_ptr(),
                (0, 1) => audio.input[1].as_mut_ptr(),
                _ => audio.silence.as_mut_ptr(),
            })
            .collect();
        audio.in_ports.push(Port { channels: list });
    }
    for (index, channels) in outs.iter().enumerate() {
        let list = (0..*channels)
            .map(|c| match (index, c) {
                (0, 0) => audio.output[0].as_mut_ptr(),
                (0, 1) => audio.output[1].as_mut_ptr(),
                _ => audio.spare.as_mut_ptr(),
            })
            .collect();
        audio.out_ports.push(Port { channels: list });
    }
    let buffer = |port: &mut Port| AudioBuffer {
        data32: port.channels.as_mut_ptr(),
        data64: std::ptr::null_mut(),
        channel_count: port.channels.len() as u32,
        latency: 0,
        constant_mask: 0,
    };
    audio.in_buffers = audio.in_ports.iter_mut().map(buffer).collect();
    audio.out_buffers = audio.out_ports.iter_mut().map(buffer).collect();

    Ok(ClapBackend {
        plugin,
        _host: host,
        state,
        gui_ext,
        editor: AtomicBool::new(false),
        params_ext,
        latency_ext,
        state_ext,
        params,
        names,
        audio: UnsafeCell::new(audio),
    })
}

impl ClapBackend {
    fn plugin(&self) -> &Plugin {
        // SAFETY: `plugin` was checked at load and lives until `drop`.
        unsafe { &*self.plugin }
    }
}

impl Backend for ClapBackend {
    fn params(&self) -> Vec<ParamInfo> {
        self.names.clone()
    }

    fn main_param(&self, index: usize) -> f32 {
        let Some(param) = self.params.get(index) else {
            return 0.0;
        };
        // SAFETY: a non-null extension is static data of the plug-in.
        let get = unsafe { self.params_ext.as_ref() }.and_then(|e| e.get_value);
        let mut value = 0.0;
        // SAFETY: the extension's function on the main thread, with a parameter id it gave.
        if get.is_some_and(|get| unsafe { get(self.plugin, param.id, &mut value) }) {
            param.norm(value)
        } else {
            0.0
        }
    }

    fn main_text(&self, index: usize, norm: f32) -> String {
        let Some(param) = self.params.get(index) else {
            return String::new();
        };
        let plain = param.plain(norm);
        let mut text = [0u8; 128];
        // SAFETY: a non-null extension is static data of the plug-in.
        let to_text = unsafe { self.params_ext.as_ref() }.and_then(|e| e.value_to_text);
        // SAFETY: the extension's function on the main thread; `text` is writable for the
        // length given, and the plug-in NUL-terminates what it writes.
        let ok = to_text.is_some_and(|f| unsafe {
            f(
                self.plugin,
                param.id,
                plain,
                text.as_mut_ptr().cast(),
                text.len() as u32 - 1,
            )
        });
        if ok {
            buffer_text(&text)
        } else {
            format!("{plain:.2}")
        }
    }

    fn main_latency(&self) -> usize {
        // SAFETY: a non-null extension is static data of the plug-in.
        match unsafe { self.latency_ext.as_ref() }.and_then(|e| e.get) {
            // SAFETY: the extension's function on the main thread.
            Some(get) => (unsafe { get(self.plugin) }) as usize,
            None => 0,
        }
    }

    fn main_save(&self) -> Option<Vec<u8>> {
        // SAFETY: a non-null extension is static data of the plug-in.
        let save = unsafe { self.state_ext.as_ref() }.and_then(|e| e.save)?;
        let mut out: Vec<u8> = Vec::new();
        let stream = OutStream {
            ctx: std::ptr::from_mut(&mut out).cast(),
            write: stream_write,
        };
        // SAFETY: the extension's function on the main thread; `stream` and `out` live for the
        // call.
        unsafe { save(self.plugin, &stream) }.then_some(out)
    }

    fn main_load(&self, state: &[u8]) -> bool {
        // SAFETY: a non-null extension is static data of the plug-in.
        let Some(load) = unsafe { self.state_ext.as_ref() }.and_then(|e| e.load) else {
            return false;
        };
        let mut cursor: (&[u8], usize) = (state, 0);
        let stream = InStream {
            ctx: std::ptr::from_mut(&mut cursor).cast(),
            read: stream_read,
        };
        // SAFETY: the extension's function on the main thread; `stream` and `cursor` live for
        // the call.
        unsafe { load(self.plugin, &stream) }
    }

    fn main_idle(&self) {
        if self.state.callback.swap(false, Ordering::AcqRel) {
            if let Some(on_main) = self.plugin().on_main_thread {
                // SAFETY: the plug-in asked for this call, on the main thread.
                unsafe { on_main(self.plugin) };
            }
        }
    }

    fn main_has_editor(&self) -> bool {
        // SAFETY: a non-null extension is static data of the plug-in.
        let supported = unsafe { self.gui_ext.as_ref() }.and_then(|e| e.is_api_supported);
        match (supported, WINDOW_API) {
            // SAFETY: the extension's function on the main thread, with a NUL-terminated id.
            (Some(supported), Some(api)) => unsafe { supported(self.plugin, api.as_ptr(), false) },
            _ => false,
        }
    }

    fn main_editor_open(&self, parent: EditorParent) -> Option<(u32, u32)> {
        let handle = match parent {
            #[cfg(target_os = "macos")]
            EditorParent::Cocoa(view) => view,
            #[cfg(target_os = "linux")]
            EditorParent::X11 { window, .. } => {
                ClapX11Window(window as std::os::raw::c_ulong).handle()
            }
        };
        let api = WINDOW_API?;
        // SAFETY: a non-null extension is static data of the plug-in.
        let gui = unsafe { self.gui_ext.as_ref() }?;
        let window = GuiWindow {
            api: api.as_ptr(),
            handle,
        };
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: the extension's functions on the main thread, in CLAP's order: create, put
        // in the parent view (which the caller keeps alive), measure, show.
        unsafe {
            if !self.editor.load(Ordering::Relaxed) {
                if !gui.create?(self.plugin, api.as_ptr(), false) {
                    return None;
                }
                self.editor.store(true, Ordering::Relaxed);
                if !gui.set_parent?(self.plugin, &window) {
                    self.main_editor_close();
                    return None;
                }
            }
            if !gui.get_size?(self.plugin, &mut width, &mut height) {
                (width, height) = (480, 320);
            }
            if let Some(show) = gui.show {
                show(self.plugin);
            }
        }
        Some((width, height))
    }

    fn main_editor_close(&self) {
        // SAFETY: a non-null extension is static data of the plug-in.
        let destroy = unsafe { self.gui_ext.as_ref() }.and_then(|e| e.destroy);
        if let (true, Some(destroy)) = (self.editor.swap(false, Ordering::Relaxed), destroy) {
            // SAFETY: the view was made by `create` and is destroyed once, on the main thread.
            unsafe { destroy(self.plugin) };
        }
    }

    fn main_editor_resized(&self) -> Option<(u32, u32)> {
        let size = self.state.resize.swap(0, Ordering::AcqRel);
        if size == 0 {
            return None;
        }
        let (width, height) = ((size >> 32) as u32, size as u32);
        // SAFETY: a non-null extension is static data of the plug-in.
        if let Some(set_size) = unsafe { self.gui_ext.as_ref() }.and_then(|e| e.set_size) {
            // SAFETY: the extension's function on the main thread; the plug-in asked for this
            // size and is told it has it.
            unsafe { set_size(self.plugin, width, height) };
        }
        Some((width, height))
    }

    fn process(&self, left: &mut [f32], right: &mut [f32], changes: &[(u32, f32)]) {
        // SAFETY: only the audio thread calls `process`, one call at a time (see `Shared`).
        let audio = unsafe { &mut *self.audio.get() };
        let frames = left.len().min(right.len()).min(audio.block);
        if audio.failed || frames == 0 {
            return;
        }
        let plugin = self.plugin();
        if !audio.started {
            // SAFETY: `start_processing` on the audio thread, after `activate`.
            let ok = plugin
                .start_processing
                .is_none_or(|start| unsafe { start(self.plugin) });
            if !ok {
                audio.failed = true;
                return;
            }
            audio.started = true;
        }
        audio.event_count = 0;
        for (index, norm) in changes {
            if let Some(param) = self.params.get(*index as usize) {
                let event = &mut audio.events[audio.event_count as usize];
                event.param_id = param.id;
                event.cookie = param.cookie;
                event.value = param.plain(*norm);
                audio.event_count += 1;
            }
        }
        audio.input[0][..frames].copy_from_slice(&left[..frames]);
        audio.input[1][..frames].copy_from_slice(&right[..frames]);
        let in_events = InputEvents {
            ctx: std::ptr::from_mut(audio).cast(),
            size: events_size,
            get: events_get,
        };
        let out_events = OutputEvents {
            ctx: std::ptr::null_mut(),
            try_push: events_push,
        };
        let process = Process {
            steady_time: -1,
            frames: frames as u32,
            transport: std::ptr::null(),
            inputs: audio.in_buffers.as_ptr(),
            outputs: audio.out_buffers.as_mut_ptr(),
            input_count: audio.in_buffers.len() as u32,
            output_count: audio.out_buffers.len() as u32,
            in_events: &in_events,
            out_events: &out_events,
        };
        let Some(run) = plugin.process else {
            return;
        };
        // SAFETY: `process` on the audio thread between `start_processing` and
        // `stop_processing`; every buffer holds `block >= frames` samples and lives in `audio`.
        let status = unsafe { run(self.plugin, &process) };
        if status == 0 {
            // CLAP_PROCESS_ERROR: the output is not to be used; the sound passes dry.
            return;
        }
        let mono = audio
            .out_ports
            .first()
            .is_some_and(|p| p.channels.len() == 1);
        left[..frames].copy_from_slice(&audio.output[0][..frames]);
        right[..frames].copy_from_slice(&audio.output[usize::from(!mono)][..frames]);
    }

    fn reset(&self) {
        if let Some(reset) = self.plugin().reset {
            // SAFETY: `reset` on the audio thread, on an activated plug-in.
            unsafe { reset(self.plugin) };
        }
    }
}

impl Drop for ClapBackend {
    fn drop(&mut self) {
        self.main_editor_close();
        let started = self.audio.get_mut().started;
        let plugin = self.plugin();
        // SAFETY: the last handle is gone, so no thread is in `process`. CLAP wants
        // `stop_processing` on the audio thread; with that thread no longer playing the
        // plug-in, this thread is the only one that can call it.
        unsafe {
            if started {
                if let Some(stop) = plugin.stop_processing {
                    stop(self.plugin);
                }
            }
            if let Some(deactivate) = plugin.deactivate {
                deactivate(self.plugin);
            }
            if let Some(destroy) = plugin.destroy {
                destroy(self.plugin);
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{ClapX11Window, CLAP_X11_API};
    use std::os::raw::c_ulong;

    #[test]
    fn clap_uses_the_x11_window_api() {
        assert_eq!(CLAP_X11_API.to_bytes(), b"x11");
    }

    #[test]
    fn clap_x11_window_handle_has_the_abi_layout() {
        assert_eq!(
            std::mem::size_of::<ClapX11Window>(),
            std::mem::size_of::<c_ulong>()
        );
        let window = ClapX11Window(42);
        assert_eq!(window.handle() as usize, 42);
    }
}
