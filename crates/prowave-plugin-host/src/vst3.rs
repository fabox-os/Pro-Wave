//! VST3 hosting: the COM-style C++ interfaces of VST 3, written out as tables of functions for
//! what an audio effect needs (the module's factory, `IComponent`, `IAudioProcessor`,
//! `IEditController`) and the few objects a host must give back (a stream for state, the
//! parameter moves of a block).
//!
//! A VST3 plug-in is a bundle folder with the library inside (`Contents/MacOS`,
//! `Contents/x86_64-linux`, `Contents/x86_64-win`); the library exports `GetPluginFactory`. As
//! with CLAP, a library is loaded once and never unloaded.

use crate::{Backend, Error, Format, ParamInfo, PluginInfo, MAX_CHANGES};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

type Tuid = [u8; 16];
type TResult = i32;
type Text128 = [u16; 128];

const OK: TResult = 0;
#[cfg(windows)]
const NO_INTERFACE: TResult = 0x8000_4002_u32 as i32;
#[cfg(not(windows))]
const NO_INTERFACE: TResult = -1;
#[cfg(windows)]
const NOT_IMPLEMENTED: TResult = 0x8000_4001_u32 as i32;
#[cfg(not(windows))]
const NOT_IMPLEMENTED: TResult = 3;
const FALSE: TResult = 1;

const AUDIO: i32 = 0;
const INPUT: i32 = 0;
const OUTPUT: i32 = 1;
const PARAM_READ_ONLY: i32 = 1 << 1;
const PARAM_HIDDEN: i32 = 1 << 4;
const PARAM_BYPASS: i32 = 1 << 16;

/// The most classes one module may list, and audio buses of one direction.
const MAX_CLASSES: i32 = 4096;
const MAX_BUSES: i32 = 32;
const MAX_CHANNELS: i32 = 64;
/// The largest state read from a plug-in (64 MiB).
const MAX_STATE: usize = 64 * 1024 * 1024;

/// An interface id from the four numbers the VST3 headers give. Windows stores the first two
/// the way COM does.
const fn iid(a: u32, b: u32, c: u32, d: u32) -> Tuid {
    let (a, b, c, d) = (
        a.to_be_bytes(),
        b.to_be_bytes(),
        c.to_be_bytes(),
        d.to_be_bytes(),
    );
    if cfg!(windows) {
        [
            a[3], a[2], a[1], a[0], b[1], b[0], b[3], b[2], c[0], c[1], c[2], c[3], d[0], d[1],
            d[2], d[3],
        ]
    } else {
        [
            a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3], c[0], c[1], c[2], c[3], d[0], d[1],
            d[2], d[3],
        ]
    }
}

const IID_UNKNOWN: Tuid = iid(0x0000_0000, 0x0000_0000, 0xC000_0000, 0x0000_0046);
const IID_FACTORY2: Tuid = iid(0x0007_B650, 0xF24B_4C0B, 0xA464_EDB9, 0xF00B_2ABB);
const IID_COMPONENT: Tuid = iid(0xE831_FF31, 0xF2D5_4301, 0x928E_BBEE, 0x2569_7802);
const IID_PROCESSOR: Tuid = iid(0x4204_3F99, 0xB7DA_453C, 0xA569_E79D, 0x9AAE_C33D);
const IID_CONTROLLER: Tuid = iid(0xDCD7_BBE3, 0x7742_448D, 0xA874_AACC, 0x979C_759E);
const IID_CONNECTION: Tuid = iid(0x70A4_156F, 0x6E6E_4026, 0x9891_48BF, 0xAA60_D8D1);
const IID_STREAM: Tuid = iid(0xC3BF_6EA2, 0x3099_4752, 0x9B6B_F990, 0x1EE3_3E9B);
const IID_CHANGES: Tuid = iid(0xA477_9663, 0x0BB6_4A56, 0xB443_84A8, 0x466F_EB9D);
const IID_QUEUE: Tuid = iid(0x0126_3A18, 0xED07_4F6F, 0x98C9_D356, 0x4686_F9BA);
const IID_HOST: Tuid = iid(0x58E5_95CC, 0xDB2D_4969, 0x8B6A_AF8C, 0x36A6_64E5);
const IID_HANDLER: Tuid = iid(0x93A0_BEA3, 0x0BD0_45DB, 0x8E89_0B0C, 0xC1E4_6AC6);
const IID_FRAME: Tuid = iid(0x367F_AF01, 0xAFA9_4693, 0x8D4D_A2A0, 0xED08_82A3);
const IID_MESSAGE: Tuid = iid(0x936F_033B, 0xC6C0_47DB, 0xBB08_82F8, 0x13C1_E613);
const IID_ATTRIBUTES: Tuid = iid(0x1E5F_0AEB, 0xCC7F_4533, 0xA254_4011, 0x38AD_5EE4);
/// `Linux::IRunLoop`, which a plug-in queries from `IPlugFrame` on Linux.
#[cfg(target_os = "linux")]
const IID_RUNLOOP: Tuid = iid(0x18C3_5366, 0x9776_4F1A, 0x9C5B_8385, 0x7A87_1389);
/// `kPlatformTypeX11EmbedWindowID` from the VST3 SDK's `iplugview.h`.
#[cfg(target_os = "linux")]
pub(crate) const VST3_X11_PLATFORM: &CStr = c"X11EmbedWindowID";

/// Any VST3 object: a pointer to its table of functions.
type Object = *mut *const c_void;

#[derive(Clone, Copy)]
#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(Object, *const Tuid, *mut Object) -> TResult,
    add_ref: unsafe extern "system" fn(Object) -> u32,
    release: unsafe extern "system" fn(Object) -> u32,
}

#[repr(C)]
struct FactoryTable {
    unknown: Unknown,
    info: unsafe extern "system" fn(Object, *mut FactoryInfo) -> TResult,
    count: unsafe extern "system" fn(Object) -> i32,
    class: unsafe extern "system" fn(Object, i32, *mut ClassInfo) -> TResult,
    create: unsafe extern "system" fn(Object, *const Tuid, *const Tuid, *mut Object) -> TResult,
    class2: unsafe extern "system" fn(Object, i32, *mut ClassInfo2) -> TResult,
}

#[repr(C)]
struct FactoryInfo {
    vendor: [u8; 64],
    url: [u8; 256],
    email: [u8; 128],
    flags: i32,
}

#[repr(C)]
struct ClassInfo {
    cid: Tuid,
    cardinality: i32,
    category: [u8; 32],
    name: [u8; 64],
}

#[repr(C)]
struct ClassInfo2 {
    cid: Tuid,
    cardinality: i32,
    category: [u8; 32],
    name: [u8; 64],
    flags: u32,
    sub_categories: [u8; 128],
    vendor: [u8; 64],
    version: [u8; 64],
    sdk_version: [u8; 64],
}

#[repr(C)]
struct ComponentTable {
    unknown: Unknown,
    initialize: unsafe extern "system" fn(Object, Object) -> TResult,
    terminate: unsafe extern "system" fn(Object) -> TResult,
    controller_class: unsafe extern "system" fn(Object, *mut Tuid) -> TResult,
    set_io_mode: unsafe extern "system" fn(Object, i32) -> TResult,
    bus_count: unsafe extern "system" fn(Object, i32, i32) -> i32,
    bus_info: unsafe extern "system" fn(Object, i32, i32, i32, *mut BusInfo) -> TResult,
    routing: unsafe extern "system" fn(Object, *mut c_void, *mut c_void) -> TResult,
    activate_bus: unsafe extern "system" fn(Object, i32, i32, i32, u8) -> TResult,
    set_active: unsafe extern "system" fn(Object, u8) -> TResult,
    set_state: unsafe extern "system" fn(Object, Object) -> TResult,
    get_state: unsafe extern "system" fn(Object, Object) -> TResult,
}

#[repr(C)]
struct BusInfo {
    media: i32,
    direction: i32,
    channels: i32,
    name: Text128,
    kind: i32,
    flags: u32,
}

#[repr(C)]
struct ProcessorTable {
    unknown: Unknown,
    set_arrangements: unsafe extern "system" fn(Object, *mut u64, i32, *mut u64, i32) -> TResult,
    arrangement: unsafe extern "system" fn(Object, i32, i32, *mut u64) -> TResult,
    can_process: unsafe extern "system" fn(Object, i32) -> TResult,
    latency: unsafe extern "system" fn(Object) -> u32,
    setup: unsafe extern "system" fn(Object, *mut ProcessSetup) -> TResult,
    set_processing: unsafe extern "system" fn(Object, u8) -> TResult,
    process: unsafe extern "system" fn(Object, *mut ProcessData) -> TResult,
    tail: unsafe extern "system" fn(Object) -> u32,
}

#[repr(C)]
struct ProcessSetup {
    mode: i32,
    sample_size: i32,
    max_block: i32,
    rate: f64,
}

#[repr(C)]
struct BusBuffers {
    channels: i32,
    silence: u64,
    buffers: *mut *mut f32,
}

/// `ProcessContext`: the transport. Only the rate and a tempo are given.
#[repr(C)]
struct Context {
    state: u32,
    rate: f64,
    project_samples: i64,
    system_time: i64,
    continuous_samples: i64,
    project_music: f64,
    bar_music: f64,
    cycle_start: f64,
    cycle_end: f64,
    tempo: f64,
    numerator: i32,
    denominator: i32,
    chord: [u8; 4],
    smpte_subframes: i32,
    frame_rate: [u32; 2],
    samples_to_clock: i32,
}

#[repr(C)]
struct ProcessData {
    mode: i32,
    sample_size: i32,
    frames: i32,
    input_count: i32,
    output_count: i32,
    inputs: *mut BusBuffers,
    outputs: *mut BusBuffers,
    in_changes: Object,
    out_changes: Object,
    in_events: Object,
    out_events: Object,
    context: *mut Context,
}

#[repr(C)]
struct ControllerTable {
    unknown: Unknown,
    initialize: unsafe extern "system" fn(Object, Object) -> TResult,
    terminate: unsafe extern "system" fn(Object) -> TResult,
    set_component_state: unsafe extern "system" fn(Object, Object) -> TResult,
    set_state: unsafe extern "system" fn(Object, Object) -> TResult,
    get_state: unsafe extern "system" fn(Object, Object) -> TResult,
    param_count: unsafe extern "system" fn(Object) -> i32,
    param_info: unsafe extern "system" fn(Object, i32, *mut RawParam) -> TResult,
    param_text: unsafe extern "system" fn(Object, u32, f64, *mut Text128) -> TResult,
    param_from_text: unsafe extern "system" fn(Object, u32, *const u16, *mut f64) -> TResult,
    to_plain: unsafe extern "system" fn(Object, u32, f64) -> f64,
    to_norm: unsafe extern "system" fn(Object, u32, f64) -> f64,
    get_norm: unsafe extern "system" fn(Object, u32) -> f64,
    set_norm: unsafe extern "system" fn(Object, u32, f64) -> TResult,
    set_handler: unsafe extern "system" fn(Object, Object) -> TResult,
    create_view: unsafe extern "system" fn(Object, *const c_char) -> Object,
}

#[repr(C)]
struct RawParam {
    id: u32,
    title: Text128,
    short_title: Text128,
    units: Text128,
    steps: i32,
    default: f64,
    unit: i32,
    flags: i32,
}

#[repr(C)]
struct ConnectionTable {
    unknown: Unknown,
    connect: unsafe extern "system" fn(Object, Object) -> TResult,
    disconnect: unsafe extern "system" fn(Object, Object) -> TResult,
    notify: unsafe extern "system" fn(Object, Object) -> TResult,
}

/// The table of functions of `object`, as interface `T`.
///
/// # Safety
/// `object` is a live VST3 object that implements `T` (it came from `queryInterface` or
/// `createInstance` for that interface).
unsafe fn table<'a, T>(object: Object) -> &'a T {
    // SAFETY: a VST3 object starts with a pointer to its interface's table.
    unsafe { &*(*object).cast::<T>() }
}

/// `object` as interface `id`, or null. The caller releases a non-null result.
///
/// # Safety
/// `object` is a live VST3 object.
unsafe fn query(object: Object, id: &Tuid) -> Object {
    let mut out: Object = std::ptr::null_mut();
    // SAFETY: every VST3 object starts with `FUnknown`'s three functions.
    let result = unsafe { (table::<Unknown>(object).query)(object, id, &mut out) };
    if result == OK {
        out
    } else {
        std::ptr::null_mut()
    }
}

/// # Safety
/// `object` is null or a live VST3 object the caller holds a reference to.
unsafe fn release(object: Object) {
    if !object.is_null() {
        // SAFETY: the caller's contract.
        unsafe { (table::<Unknown>(object).release)(object) };
    }
}

/// Keeps `object` alive while the host holds it.
///
/// # Safety
/// `object` is null or a live VST3 object.
#[cfg(target_os = "linux")]
unsafe fn retain(object: Object) {
    if !object.is_null() {
        // SAFETY: the caller's contract.
        unsafe { (table::<Unknown>(object).add_ref)(object) };
    }
}

fn text16(text: &[u16]) -> String {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end]).trim().to_string()
}

// ---- The objects the host gives the plug-in -------------------------------------------------

/// Whether `id` names `wanted` or `FUnknown`; answers `queryInterface` on a host object, which
/// is not counted (it lives as long as its owner).
///
/// # Safety
/// `id` and `out` are the pointers a plug-in passed to `queryInterface`.
unsafe fn host_query(this: Object, id: *const Tuid, out: *mut Object, wanted: &Tuid) -> TResult {
    if id.is_null() || out.is_null() {
        return FALSE;
    }
    // SAFETY: checked not null; an interface id is sixteen readable bytes.
    let id = unsafe { &*id };
    let found = id == wanted || id == &IID_UNKNOWN;
    // SAFETY: `out` is writable for one pointer.
    unsafe { *out = if found { this } else { std::ptr::null_mut() } };
    if found {
        OK
    } else {
        NO_INTERFACE
    }
}

unsafe extern "system" fn host_count(_: Object) -> u32 {
    1
}

#[repr(C)]
struct StreamTable {
    unknown: Unknown,
    read: unsafe extern "system" fn(Object, *mut c_void, i32, *mut i32) -> TResult,
    write: unsafe extern "system" fn(Object, *const c_void, i32, *mut i32) -> TResult,
    seek: unsafe extern "system" fn(Object, i64, i32, *mut i64) -> TResult,
    tell: unsafe extern "system" fn(Object, *mut i64) -> TResult,
}

/// An `IBStream` over bytes in memory: a plug-in's state on its way out or back in.
#[repr(C)]
struct Stream {
    table: *const StreamTable,
    bytes: Vec<u8>,
    at: usize,
}

impl Stream {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            table: &STREAM_TABLE,
            bytes,
            at: 0,
        }
    }

    fn object(&mut self) -> Object {
        std::ptr::from_mut(self).cast()
    }
}

static STREAM_TABLE: StreamTable = StreamTable {
    unknown: Unknown {
        query: stream_query,
        add_ref: host_count,
        release: host_count,
    },
    read: stream_read,
    write: stream_write,
    seek: stream_seek,
    tell: stream_tell,
};

unsafe extern "system" fn stream_query(this: Object, id: *const Tuid, out: *mut Object) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_STREAM) }
}

unsafe extern "system" fn stream_read(
    this: Object,
    data: *mut c_void,
    n: i32,
    done: *mut i32,
) -> TResult {
    // SAFETY: `this` is a `Stream` this module handed to the plug-in for the current call.
    let stream = unsafe { &mut *this.cast::<Stream>() };
    let n = (n.max(0) as usize).min(stream.bytes.len() - stream.at);
    if !data.is_null() {
        // SAFETY: the plug-in gives at least `n` writable bytes; the source range is in bounds.
        unsafe {
            std::ptr::copy_nonoverlapping(stream.bytes[stream.at..].as_ptr(), data.cast(), n);
        }
        stream.at += n;
    }
    if !done.is_null() {
        // SAFETY: `done` is writable for one number.
        unsafe { *done = n as i32 };
    }
    OK
}

unsafe extern "system" fn stream_write(
    this: Object,
    data: *const c_void,
    n: i32,
    done: *mut i32,
) -> TResult {
    // SAFETY: as in `stream_read`.
    let stream = unsafe { &mut *this.cast::<Stream>() };
    let n = n.max(0) as usize;
    if data.is_null() || stream.at.saturating_add(n) > MAX_STATE {
        return FALSE;
    }
    // SAFETY: the plug-in gives `n` readable bytes.
    let source = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), n) };
    let end = stream.at + n;
    if stream.bytes.len() < end {
        stream.bytes.resize(end, 0);
    }
    stream.bytes[stream.at..end].copy_from_slice(source);
    stream.at = end;
    if !done.is_null() {
        // SAFETY: `done` is writable for one number.
        unsafe { *done = n as i32 };
    }
    OK
}

unsafe extern "system" fn stream_seek(this: Object, to: i64, mode: i32, out: *mut i64) -> TResult {
    // SAFETY: as in `stream_read`.
    let stream = unsafe { &mut *this.cast::<Stream>() };
    let base = match mode {
        0 => 0,
        1 => stream.at as i64,
        2 => stream.bytes.len() as i64,
        _ => return FALSE,
    };
    let Some(target) = base.checked_add(to) else {
        return FALSE;
    };
    stream.at = target.clamp(0, stream.bytes.len() as i64) as usize;
    if !out.is_null() {
        // SAFETY: `out` is writable for one number.
        unsafe { *out = stream.at as i64 };
    }
    OK
}

unsafe extern "system" fn stream_tell(this: Object, out: *mut i64) -> TResult {
    if out.is_null() {
        return FALSE;
    }
    // SAFETY: as in `stream_read`; `out` is writable for one number.
    unsafe { *out = (*this.cast::<Stream>()).at as i64 };
    OK
}

#[repr(C)]
struct QueueTable {
    unknown: Unknown,
    id: unsafe extern "system" fn(Object) -> u32,
    count: unsafe extern "system" fn(Object) -> i32,
    point: unsafe extern "system" fn(Object, i32, *mut i32, *mut f64) -> TResult,
    add: unsafe extern "system" fn(Object, i32, f64, *mut i32) -> TResult,
}

/// An `IParamValueQueue` with one point: a parameter's new value at the start of the block.
#[repr(C)]
#[derive(Clone, Copy)]
struct Queue {
    table: *const QueueTable,
    id: u32,
    value: f64,
}

static QUEUE_TABLE: QueueTable = QueueTable {
    unknown: Unknown {
        query: queue_query,
        add_ref: host_count,
        release: host_count,
    },
    id: queue_id,
    count: queue_count,
    point: queue_point,
    add: queue_add,
};

unsafe extern "system" fn queue_query(this: Object, id: *const Tuid, out: *mut Object) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_QUEUE) }
}

unsafe extern "system" fn queue_id(this: Object) -> u32 {
    // SAFETY: `this` is a `Queue` of the block being processed.
    unsafe { (*this.cast::<Queue>()).id }
}

unsafe extern "system" fn queue_count(_: Object) -> i32 {
    1
}

unsafe extern "system" fn queue_point(
    this: Object,
    index: i32,
    offset: *mut i32,
    value: *mut f64,
) -> TResult {
    if index != 0 || offset.is_null() || value.is_null() {
        return FALSE;
    }
    // SAFETY: `this` is a `Queue` of the block being processed; both out pointers are writable.
    unsafe {
        *offset = 0;
        *value = (*this.cast::<Queue>()).value;
    }
    OK
}

unsafe extern "system" fn queue_add(_: Object, _: i32, _: f64, _: *mut i32) -> TResult {
    FALSE
}

#[repr(C)]
struct ChangesTable {
    unknown: Unknown,
    count: unsafe extern "system" fn(Object) -> i32,
    data: unsafe extern "system" fn(Object, i32) -> Object,
    add: unsafe extern "system" fn(Object, *const u32, *mut i32) -> Object,
}

/// An `IParameterChanges`: the parameter moves of one block. The same type with no moves is
/// the list a plug-in may write its own moves to; those are not followed yet, so adding fails.
#[repr(C)]
struct Changes {
    table: *const ChangesTable,
    count: i32,
    queues: [Queue; MAX_CHANGES],
}

impl Changes {
    fn new() -> Self {
        Self {
            table: &CHANGES_TABLE,
            count: 0,
            queues: [Queue {
                table: &QUEUE_TABLE,
                id: 0,
                value: 0.0,
            }; MAX_CHANGES],
        }
    }
}

static CHANGES_TABLE: ChangesTable = ChangesTable {
    unknown: Unknown {
        query: changes_query,
        add_ref: host_count,
        release: host_count,
    },
    count: changes_count,
    data: changes_data,
    add: changes_add,
};

unsafe extern "system" fn changes_query(
    this: Object,
    id: *const Tuid,
    out: *mut Object,
) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_CHANGES) }
}

unsafe extern "system" fn changes_count(this: Object) -> i32 {
    // SAFETY: `this` is a `Changes` of the block being processed.
    unsafe { (*this.cast::<Changes>()).count }
}

unsafe extern "system" fn changes_data(this: Object, index: i32) -> Object {
    // SAFETY: as in `changes_count`.
    let changes = unsafe { &mut *this.cast::<Changes>() };
    if index < 0 || index >= changes.count {
        return std::ptr::null_mut();
    }
    std::ptr::from_mut(&mut changes.queues[index as usize]).cast()
}

unsafe extern "system" fn changes_add(_: Object, _: *const u32, _: *mut i32) -> Object {
    std::ptr::null_mut()
}

#[repr(C)]
struct HostTable {
    unknown: Unknown,
    name: unsafe extern "system" fn(Object, *mut Text128) -> TResult,
    create: unsafe extern "system" fn(Object, *const Tuid, *const Tuid, *mut Object) -> TResult,
}

/// The `IHostApplication` every plug-in is started with: it has a name and makes the messages
/// a plug-in's two halves send each other.
#[repr(C)]
struct HostApp {
    table: *const HostTable,
}

// SAFETY: `HostApp` is a constant pointer to a constant table; nothing in it changes.
unsafe impl Sync for HostApp {}

static HOST_TABLE: HostTable = HostTable {
    unknown: Unknown {
        query: app_query,
        add_ref: host_count,
        release: host_count,
    },
    name: app_name,
    create: app_create,
};

static HOST: HostApp = HostApp { table: &HOST_TABLE };

fn host() -> Object {
    std::ptr::from_ref(&HOST).cast_mut().cast()
}

unsafe extern "system" fn app_query(this: Object, id: *const Tuid, out: *mut Object) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_HOST) }
}

unsafe extern "system" fn app_name(_: Object, out: *mut Text128) -> TResult {
    if out.is_null() {
        return FALSE;
    }
    // SAFETY: `out` is writable for 128 UTF-16 units.
    let out = unsafe { &mut *out };
    *out = [0; 128];
    for (slot, unit) in out.iter_mut().zip("Pro Wave".encode_utf16()) {
        *slot = unit;
    }
    OK
}

unsafe extern "system" fn app_create(
    _: Object,
    class: *const Tuid,
    _: *const Tuid,
    out: *mut Object,
) -> TResult {
    if out.is_null() || class.is_null() {
        return FALSE;
    }
    // SAFETY: `class` is sixteen readable bytes and `out` is writable for one pointer.
    unsafe {
        let class = &*class;
        *out = if class == &IID_MESSAGE {
            Message::create()
        } else if class == &IID_ATTRIBUTES {
            Attributes::create()
        } else {
            std::ptr::null_mut()
        };
        if (*out).is_null() {
            NOT_IMPLEMENTED
        } else {
            OK
        }
    }
}

/// One value of an attribute list.
enum Value {
    Int(i64),
    Float(f64),
    Text(Vec<u16>),
    Binary(Vec<u8>),
}

#[repr(C)]
struct AttributesTable {
    unknown: Unknown,
    set_int: unsafe extern "system" fn(Object, *const c_char, i64) -> TResult,
    get_int: unsafe extern "system" fn(Object, *const c_char, *mut i64) -> TResult,
    set_float: unsafe extern "system" fn(Object, *const c_char, f64) -> TResult,
    get_float: unsafe extern "system" fn(Object, *const c_char, *mut f64) -> TResult,
    set_text: unsafe extern "system" fn(Object, *const c_char, *const u16) -> TResult,
    get_text: unsafe extern "system" fn(Object, *const c_char, *mut u16, u32) -> TResult,
    set_binary: unsafe extern "system" fn(Object, *const c_char, *const c_void, u32) -> TResult,
    get_binary:
        unsafe extern "system" fn(Object, *const c_char, *mut *const c_void, *mut u32) -> TResult,
}

/// An `IAttributeList`: the named values of a message a plug-in sends between its two halves.
/// The plug-in asks the host to make these, and counts its references to them.
#[repr(C)]
struct Attributes {
    table: *const AttributesTable,
    refs: AtomicU32,
    values: Mutex<HashMap<Vec<u8>, Value>>,
}

static ATTRIBUTES_TABLE: AttributesTable = AttributesTable {
    unknown: Unknown {
        query: attributes_query,
        add_ref: attributes_add_ref,
        release: attributes_release,
    },
    set_int: attributes_set_int,
    get_int: attributes_get_int,
    set_float: attributes_set_float,
    get_float: attributes_get_float,
    set_text: attributes_set_text,
    get_text: attributes_get_text,
    set_binary: attributes_set_binary,
    get_binary: attributes_get_binary,
};

impl Attributes {
    fn create() -> Object {
        Box::into_raw(Box::new(Attributes {
            table: &ATTRIBUTES_TABLE,
            refs: AtomicU32::new(1),
            values: Mutex::default(),
        }))
        .cast()
    }

    /// Stores `value` under the plug-in's key.
    ///
    /// # Safety
    /// `this` is a live `Attributes`; `key` is null or a NUL-terminated string.
    unsafe fn set(this: Object, key: *const c_char, value: Value) -> TResult {
        if key.is_null() {
            return FALSE;
        }
        // SAFETY: the caller's contract.
        let (list, key) = unsafe { (&*this.cast::<Attributes>(), CStr::from_ptr(key)) };
        match list.values.lock() {
            Ok(mut values) => {
                values.insert(key.to_bytes().to_vec(), value);
                OK
            }
            Err(_) => FALSE,
        }
    }

    /// Runs `f` on the value under the plug-in's key; `FALSE` when there is none.
    ///
    /// # Safety
    /// As [`Attributes::set`].
    unsafe fn get(this: Object, key: *const c_char, f: impl FnOnce(&Value) -> bool) -> TResult {
        if key.is_null() {
            return FALSE;
        }
        // SAFETY: the caller's contract.
        let (list, key) = unsafe { (&*this.cast::<Attributes>(), CStr::from_ptr(key)) };
        match list.values.lock() {
            Ok(values) if values.get(key.to_bytes()).is_some_and(f) => OK,
            _ => FALSE,
        }
    }
}

unsafe extern "system" fn attributes_query(
    this: Object,
    id: *const Tuid,
    out: *mut Object,
) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments; a found interface is one more
    // reference.
    unsafe {
        let result = host_query(this, id, out, &IID_ATTRIBUTES);
        if result == OK {
            attributes_add_ref(this);
        }
        result
    }
}

unsafe extern "system" fn attributes_add_ref(this: Object) -> u32 {
    // SAFETY: `this` is a live `Attributes`.
    unsafe {
        (*this.cast::<Attributes>())
            .refs
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }
}

unsafe extern "system" fn attributes_release(this: Object) -> u32 {
    // SAFETY: `this` is a live `Attributes` made by `Box::into_raw`; the last reference frees
    // it.
    unsafe {
        let left = (*this.cast::<Attributes>())
            .refs
            .fetch_sub(1, Ordering::AcqRel)
            - 1;
        if left == 0 {
            drop(Box::from_raw(this.cast::<Attributes>()));
        }
        left
    }
}

unsafe extern "system" fn attributes_set_int(this: Object, key: *const c_char, v: i64) -> TResult {
    // SAFETY: the plug-in's arguments to its own attribute list.
    unsafe { Attributes::set(this, key, Value::Int(v)) }
}

unsafe extern "system" fn attributes_get_int(
    this: Object,
    key: *const c_char,
    out: *mut i64,
) -> TResult {
    if out.is_null() {
        return FALSE;
    }
    // SAFETY: the plug-in's arguments; `out` is writable for one number.
    unsafe {
        Attributes::get(this, key, |value| match value {
            Value::Int(v) => {
                *out = *v;
                true
            }
            _ => false,
        })
    }
}

unsafe extern "system" fn attributes_set_float(
    this: Object,
    key: *const c_char,
    v: f64,
) -> TResult {
    // SAFETY: the plug-in's arguments to its own attribute list.
    unsafe { Attributes::set(this, key, Value::Float(v)) }
}

unsafe extern "system" fn attributes_get_float(
    this: Object,
    key: *const c_char,
    out: *mut f64,
) -> TResult {
    if out.is_null() {
        return FALSE;
    }
    // SAFETY: the plug-in's arguments; `out` is writable for one number.
    unsafe {
        Attributes::get(this, key, |value| match value {
            Value::Float(v) => {
                *out = *v;
                true
            }
            _ => false,
        })
    }
}

unsafe extern "system" fn attributes_set_text(
    this: Object,
    key: *const c_char,
    text: *const u16,
) -> TResult {
    if text.is_null() {
        return FALSE;
    }
    let mut units = Vec::new();
    // SAFETY: `text` is a NUL-terminated UTF-16 string; the cap bounds a broken one.
    unsafe {
        while units.len() < 65_536 && *text.add(units.len()) != 0 {
            units.push(*text.add(units.len()));
        }
        Attributes::set(this, key, Value::Text(units))
    }
}

unsafe extern "system" fn attributes_get_text(
    this: Object,
    key: *const c_char,
    out: *mut u16,
    bytes: u32,
) -> TResult {
    let room = bytes as usize / 2;
    if out.is_null() || room == 0 {
        return FALSE;
    }
    // SAFETY: the plug-in's arguments; `out` is writable for `bytes` bytes.
    unsafe {
        Attributes::get(this, key, |value| match value {
            Value::Text(units) => {
                let n = units.len().min(room - 1);
                std::ptr::copy_nonoverlapping(units.as_ptr(), out, n);
                *out.add(n) = 0;
                true
            }
            _ => false,
        })
    }
}

unsafe extern "system" fn attributes_set_binary(
    this: Object,
    key: *const c_char,
    data: *const c_void,
    bytes: u32,
) -> TResult {
    if data.is_null() || bytes as usize > MAX_STATE {
        return FALSE;
    }
    // SAFETY: the plug-in gives `bytes` readable bytes at `data`.
    unsafe {
        let copy = std::slice::from_raw_parts(data.cast::<u8>(), bytes as usize).to_vec();
        Attributes::set(this, key, Value::Binary(copy))
    }
}

unsafe extern "system" fn attributes_get_binary(
    this: Object,
    key: *const c_char,
    out: *mut *const c_void,
    bytes: *mut u32,
) -> TResult {
    if out.is_null() || bytes.is_null() {
        return FALSE;
    }
    // SAFETY: the plug-in's arguments; both out pointers are writable. The bytes stay valid
    // until the key is set again or the list is freed, which is the interface's contract.
    unsafe {
        Attributes::get(this, key, |value| match value {
            Value::Binary(data) => {
                *out = data.as_ptr().cast();
                *bytes = data.len() as u32;
                true
            }
            _ => false,
        })
    }
}

#[repr(C)]
struct MessageTable {
    unknown: Unknown,
    id: unsafe extern "system" fn(Object) -> *const c_char,
    set_id: unsafe extern "system" fn(Object, *const c_char),
    attributes: unsafe extern "system" fn(Object) -> Object,
}

/// An `IMessage`: a name and an attribute list, made by the host when a plug-in asks.
#[repr(C)]
struct Message {
    table: *const MessageTable,
    refs: AtomicU32,
    id: Mutex<CString>,
    attributes: Object,
}

static MESSAGE_TABLE: MessageTable = MessageTable {
    unknown: Unknown {
        query: message_query,
        add_ref: message_add_ref,
        release: message_release,
    },
    id: message_id,
    set_id: message_set_id,
    attributes: message_attributes,
};

impl Message {
    fn create() -> Object {
        Box::into_raw(Box::new(Message {
            table: &MESSAGE_TABLE,
            refs: AtomicU32::new(1),
            id: Mutex::default(),
            attributes: Attributes::create(),
        }))
        .cast()
    }
}

unsafe extern "system" fn message_query(
    this: Object,
    id: *const Tuid,
    out: *mut Object,
) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments; a found interface is one more
    // reference.
    unsafe {
        let result = host_query(this, id, out, &IID_MESSAGE);
        if result == OK {
            message_add_ref(this);
        }
        result
    }
}

unsafe extern "system" fn message_add_ref(this: Object) -> u32 {
    // SAFETY: `this` is a live `Message`.
    unsafe {
        (*this.cast::<Message>())
            .refs
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }
}

unsafe extern "system" fn message_release(this: Object) -> u32 {
    // SAFETY: `this` is a live `Message` made by `Box::into_raw`; the last reference frees it
    // and gives up its attribute list.
    unsafe {
        let left = (*this.cast::<Message>())
            .refs
            .fetch_sub(1, Ordering::AcqRel)
            - 1;
        if left == 0 {
            let message = Box::from_raw(this.cast::<Message>());
            attributes_release(message.attributes);
        }
        left
    }
}

unsafe extern "system" fn message_id(this: Object) -> *const c_char {
    // SAFETY: `this` is a live `Message`. The pointer stays valid until the id is set again,
    // as the interface says.
    match unsafe { (*this.cast::<Message>()).id.lock() } {
        Ok(id) => id.as_ptr(),
        Err(_) => c"".as_ptr(),
    }
}

unsafe extern "system" fn message_set_id(this: Object, id: *const c_char) {
    if id.is_null() {
        return;
    }
    // SAFETY: `this` is a live `Message`; `id` is a NUL-terminated string.
    unsafe {
        if let Ok(mut own) = (*this.cast::<Message>()).id.lock() {
            *own = CStr::from_ptr(id).to_owned();
        }
    }
}

unsafe extern "system" fn message_attributes(this: Object) -> Object {
    // SAFETY: `this` is a live `Message`.
    unsafe { (*this.cast::<Message>()).attributes }
}

// ---- Loading a module -----------------------------------------------------------------------

/// A loaded module's factory, valid for the rest of the program.
#[derive(Clone, Copy)]
struct Loaded(Object);

// SAFETY: the factory belongs to a library that is never unloaded; the registry's lock lets one
// thread at a time call it.
unsafe impl Send for Loaded {}

fn registry() -> &'static Mutex<HashMap<PathBuf, Loaded>> {
    static LOADED: OnceLock<Mutex<HashMap<PathBuf, Loaded>>> = OnceLock::new();
    LOADED.get_or_init(Default::default)
}

/// The library inside a VST3 bundle for this system (a plain file is the library itself).
fn library_file(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return Some(path.to_path_buf());
    }
    if cfg!(target_os = "macos") {
        return crate::bundle_binary(path);
    }
    let stem = path.file_stem()?;
    let arch = std::env::consts::ARCH;
    let (folder, ext) = if cfg!(windows) {
        (format!("{arch}-win"), "vst3")
    } else {
        (format!("{arch}-linux"), "so")
    };
    let dir = path.join("Contents").join(folder);
    let named = dir.join(stem).with_extension(ext);
    if named.is_file() {
        return Some(named);
    }
    // Many bundles name their library differently from the bundle (`Dragonfly.vst3` holding
    // `libdragonfly.so`); the bundle's library is then the one file of that kind in its folder.
    let mut libs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some(ext))
        .collect();
    libs.sort();
    libs.into_iter().next()
}

#[cfg(all(test, target_os = "linux"))]
mod x11_tests {
    use super::{
        frame_query, runloop_register_event_handler, runloop_register_timer,
        runloop_unregister_event_handler, runloop_unregister_timer, EventHandlerTable, Frame,
        RunLoop, TimerHandlerTable, Unknown, IID_FRAME, IID_RUNLOOP, IID_UNKNOWN, OK,
        VST3_X11_PLATFORM,
    };
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn vst3_uses_the_x11_embed_platform() {
        assert_eq!(VST3_X11_PLATFORM.to_bytes(), b"X11EmbedWindowID");
    }

    #[test]
    fn vst3_runloop_iid_is_stable() {
        assert_ne!(IID_RUNLOOP, IID_UNKNOWN);
    }

    static EVENT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TIMER_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn test_query(
        _: super::Object,
        _: *const super::Tuid,
        _: *mut super::Object,
    ) -> super::TResult {
        super::NO_INTERFACE
    }

    unsafe extern "system" fn test_add_ref(_: super::Object) -> u32 {
        2
    }

    unsafe extern "system" fn test_release(_: super::Object) -> u32 {
        1
    }

    unsafe extern "system" fn on_fd(_: super::Object, _: i32) {
        EVENT_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    unsafe extern "system" fn on_timer(_: super::Object) {
        TIMER_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn runloop_pumps_readable_fds_and_due_timers() {
        let unknown = Unknown {
            query: test_query,
            add_ref: test_add_ref,
            release: test_release,
        };
        let event_table = EventHandlerTable {
            unknown,
            on_fd_is_set: on_fd,
        };
        let timer_table = TimerHandlerTable { unknown, on_timer };
        // A VST3 object is a pointer to a variable holding its table pointer.
        let mut event_table_ptr: *const EventHandlerTable = &event_table;
        let mut timer_table_ptr: *const TimerHandlerTable = &timer_table;
        let event = ptr::from_mut(&mut event_table_ptr).cast::<*const c_void>();
        let timer = ptr::from_mut(&mut timer_table_ptr).cast::<*const c_void>();
        let run = RunLoop::new();
        let this = ptr::from_ref(&run).cast::<*const c_void>().cast_mut();
        let mut fds = [0; 2];
        // SAFETY: `pipe` writes two valid descriptors, and both are closed below.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: the write end is open and has room for one byte.
        assert_eq!(unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) }, 1);

        // SAFETY: both objects begin with their live tables; `this` is the `RunLoop`; the
        // descriptors are open and the `RunLoop` outlives both registrations.
        unsafe {
            assert_eq!(runloop_register_event_handler(this, event, fds[0]), OK);
            assert_eq!(runloop_register_timer(this, timer, 0), OK);
        }
        run.pump();
        assert_eq!(EVENT_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(TIMER_CALLS.load(Ordering::Relaxed), 1);

        // SAFETY: the handlers are still registered with this live `RunLoop`.
        unsafe {
            assert_eq!(runloop_unregister_event_handler(this, event), OK);
            assert_eq!(runloop_unregister_timer(this, timer), OK);
        }
        // SAFETY: both descriptors were opened above and are not used again.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn frame_answers_the_runloop_interface() {
        let mut frame = Frame {
            table: &super::FRAME_TABLE,
            wanted: Default::default(),
            run_loop: RunLoop::new(),
        };
        let this = ptr::from_mut(&mut frame).cast::<*const c_void>();
        let mut out: super::Object = ptr::null_mut();
        // SAFETY: `this` is a live `Frame`, and `out` is writable for one pointer.
        assert_eq!(unsafe { frame_query(this, &IID_RUNLOOP, &mut out) }, OK);
        assert_eq!(
            out,
            ptr::from_ref(&frame.run_loop)
                .cast::<*const c_void>()
                .cast_mut()
        );
        let mut out: super::Object = ptr::null_mut();
        // SAFETY: as above.
        assert_eq!(unsafe { frame_query(this, &IID_FRAME, &mut out) }, OK);
        assert_eq!(out, this);
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod bundle_tests {
    use super::library_file;

    #[test]
    fn a_bundle_whose_library_has_another_name_still_finds_its_library() -> std::io::Result<()> {
        let root = std::env::temp_dir().join(format!("prowave-vst3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let arch = std::env::consts::ARCH;
        let bundle = root.join("DejaVu.vst3");
        let folder = bundle.join("Contents").join(format!("{arch}-linux"));
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join("libdejavu.so"), b"not really a library")?;
        assert_eq!(library_file(&bundle), Some(folder.join("libdejavu.so")));

        // The library named after the bundle is preferred when there is one.
        std::fs::write(folder.join("DejaVu.so"), b"x")?;
        assert_eq!(library_file(&bundle), Some(folder.join("DejaVu.so")));

        // A bundle with no library at all is not a plug-in.
        let empty = root.join("Empty.vst3");
        std::fs::create_dir_all(empty.join("Contents"))?;
        assert_eq!(library_file(&empty), None);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        path: *const u8,
        length: isize,
        is_directory: u8,
    ) -> *const c_void;
    fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// Calls the module's start function (`bundleEntry`, `ModuleEntry` or `InitDll`), which the
/// format requires before the factory is used. Returns false when the module refuses.
///
/// # Safety
/// `library` is a VST3 module that was just loaded from `path`.
unsafe fn start_module(library: &libloading::Library, path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: `bytes` is readable for its length. The bundle object is kept for the life
        // of the module (never released), as the module may hold it.
        let bundle = unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                1,
            );
            if url.is_null() {
                return false;
            }
            let bundle = CFBundleCreate(std::ptr::null(), url);
            CFRelease(url);
            bundle
        };
        if bundle.is_null() {
            return false;
        }
        type Entry = unsafe extern "C" fn(*const c_void) -> bool;
        // SAFETY: `bundleEntry` takes the module's CFBundle and returns whether it started.
        match unsafe { library.get::<Entry>(b"bundleEntry\0") } {
            Ok(entry) => unsafe { entry(bundle) },
            Err(_) => false,
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = path;
        type Entry = unsafe extern "C" fn(*mut c_void) -> bool;
        // SAFETY: `ModuleEntry` takes the library handle, which modules do not need to read.
        match unsafe { library.get::<Entry>(b"ModuleEntry\0") } {
            Ok(entry) => unsafe { entry(std::ptr::null_mut()) },
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        let _ = path;
        type Entry = unsafe extern "system" fn() -> bool;
        // SAFETY: `InitDll` is optional and takes nothing.
        match unsafe { library.get::<Entry>(b"InitDll\0") } {
            Ok(entry) => unsafe { entry() },
            Err(_) => true,
        }
    }
}

/// Runs `f` with the factory of the VST3 module at `path`, loading the module the first time.
fn with_factory<T>(path: &Path, f: impl FnOnce(Object) -> T) -> Result<T, Error> {
    let bad = |why: &str| Error::NotAPlugin(format!("{}: {why}", path.display()));
    let mut loaded = registry()
        .lock()
        .map_err(|_| bad("the registry is broken"))?;
    if let Some(Loaded(factory)) = loaded.get(path) {
        return Ok(f(*factory));
    }
    let file = library_file(path).ok_or_else(|| bad("no library for this system"))?;
    // SAFETY: loading a library runs its initialisers; that is what hosting a plug-in is.
    let library = unsafe { libloading::Library::new(&file) }.map_err(|e| bad(&e.to_string()))?;
    // SAFETY: `library` was just loaded from `path`.
    if !unsafe { start_module(&library, path) } {
        return Err(bad("the module did not start"));
    }
    type GetFactory = unsafe extern "system" fn() -> Object;
    // SAFETY: `GetPluginFactory` takes nothing and returns the module's factory or null.
    let factory = match unsafe { library.get::<GetFactory>(b"GetPluginFactory\0") } {
        Ok(get) => unsafe { get() },
        Err(_) => return Err(bad("no GetPluginFactory")),
    };
    if factory.is_null() {
        return Err(bad("no factory"));
    }
    // Never unloaded (see the module comment).
    std::mem::forget(library);
    loaded.insert(path.to_path_buf(), Loaded(factory));
    Ok(f(factory))
}

fn hex(cid: &Tuid) -> String {
    cid.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(id: &str) -> Option<Tuid> {
    if id.len() != 32 || !id.is_ascii() {
        return None;
    }
    let mut cid = [0u8; 16];
    for (i, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(cid)
}

pub(crate) fn scan(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    with_factory(path, |factory| {
        // SAFETY: `factory` is the module's `IPluginFactory`; every structure passed is plain
        // data that all-zero is valid for, and writable.
        unsafe {
            let base = table::<FactoryTable>(factory);
            let mut factory_info: FactoryInfo = std::mem::zeroed();
            (base.info)(factory, &mut factory_info);
            let factory2 = query(factory, &IID_FACTORY2);
            let mut out = Vec::new();
            for i in 0..(base.count)(factory).clamp(0, MAX_CLASSES) {
                let mut info: ClassInfo = std::mem::zeroed();
                if (base.class)(factory, i, &mut info) != OK
                    || crate::buffer_text(&info.category) != "Audio Module Class"
                {
                    continue;
                }
                let mut vendor = crate::buffer_text(&factory_info.vendor);
                if !factory2.is_null() {
                    let mut info2: ClassInfo2 = std::mem::zeroed();
                    if (table::<FactoryTable>(factory2).class2)(factory2, i, &mut info2) == OK {
                        let kinds = crate::buffer_text(&info2.sub_categories);
                        if !kinds.split('|').any(|k| k == "Fx")
                            || kinds.split('|').any(|k| k == "Instrument")
                        {
                            continue;
                        }
                        let own = crate::buffer_text(&info2.vendor);
                        if !own.is_empty() {
                            vendor = own;
                        }
                    }
                }
                out.push(PluginInfo {
                    format: Format::Vst3,
                    id: hex(&info.cid),
                    name: crate::buffer_text(&info.name),
                    vendor,
                    path: path.to_path_buf(),
                });
            }
            release(factory2);
            out
        }
    })
}

// ---- A loaded plug-in -----------------------------------------------------------------------

struct Param {
    id: u32,
    units: String,
}

/// What the audio thread uses in `process`; made once at load.
struct Audio {
    /// The moves the plug-in's own window made (parameter id, position).
    edits: rtrb::Consumer<(u32, f64)>,
    block: usize,
    input: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    silence: Vec<f32>,
    spare: Vec<f32>,
    /// The channel pointers of each bus; `in_buses` and `out_buses` point into these.
    _in_channels: Vec<Vec<*mut f32>>,
    _out_channels: Vec<Vec<*mut f32>>,
    in_buses: Vec<BusBuffers>,
    out_buses: Vec<BusBuffers>,
    in_changes: Box<Changes>,
    out_changes: Box<Changes>,
    context: Context,
    mono_out: bool,
}

#[repr(C)]
struct HandlerTable {
    unknown: Unknown,
    begin: unsafe extern "system" fn(Object, u32) -> TResult,
    perform: unsafe extern "system" fn(Object, u32, f64) -> TResult,
    end: unsafe extern "system" fn(Object, u32) -> TResult,
    restart: unsafe extern "system" fn(Object, i32) -> TResult,
}

/// The `IComponentHandler` a controller reports to: a parameter its own window moved goes on
/// to the audio thread, which hands it to the processor with the next block.
#[repr(C)]
struct Handler {
    table: *const HandlerTable,
    edits: Mutex<rtrb::Producer<(u32, f64)>>,
}

static HANDLER_TABLE: HandlerTable = HandlerTable {
    unknown: Unknown {
        query: handler_query,
        add_ref: host_count,
        release: host_count,
    },
    begin: handler_gesture,
    perform: handler_perform,
    end: handler_gesture,
    restart: handler_restart,
};

unsafe extern "system" fn handler_query(
    this: Object,
    id: *const Tuid,
    out: *mut Object,
) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_HANDLER) }
}

unsafe extern "system" fn handler_gesture(_: Object, _: u32) -> TResult {
    OK
}

unsafe extern "system" fn handler_perform(this: Object, id: u32, value: f64) -> TResult {
    // SAFETY: `this` is the backend's `Handler`, which outlives the controller's use of it.
    let handler = unsafe { &*this.cast::<Handler>() };
    if let Ok(mut edits) = handler.edits.lock() {
        // A full queue drops the move; the next one of the same gesture carries the value.
        let _ = edits.push((id, value));
    }
    OK
}

unsafe extern "system" fn handler_restart(_: Object, _: i32) -> TResult {
    // A changed latency is read again at the next idle; nothing else is restarted.
    OK
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ViewRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `IPlugView`: the plug-in's own view.
#[repr(C)]
struct ViewTable {
    unknown: Unknown,
    supported: unsafe extern "system" fn(Object, *const c_char) -> TResult,
    attached: unsafe extern "system" fn(Object, *mut c_void, *const c_char) -> TResult,
    removed: unsafe extern "system" fn(Object) -> TResult,
    on_wheel: unsafe extern "system" fn(Object, f32) -> TResult,
    on_key_down: unsafe extern "system" fn(Object, u16, i16, i16) -> TResult,
    on_key_up: unsafe extern "system" fn(Object, u16, i16, i16) -> TResult,
    get_size: unsafe extern "system" fn(Object, *mut ViewRect) -> TResult,
    on_size: unsafe extern "system" fn(Object, *mut ViewRect) -> TResult,
    on_focus: unsafe extern "system" fn(Object, u8) -> TResult,
    set_frame: unsafe extern "system" fn(Object, Object) -> TResult,
    can_resize: unsafe extern "system" fn(Object) -> TResult,
    check_size: unsafe extern "system" fn(Object, *mut ViewRect) -> TResult,
}

/// `Linux::IEventHandler`: a plug-in object told when one of its descriptors is readable.
#[cfg(target_os = "linux")]
#[repr(C)]
struct EventHandlerTable {
    unknown: Unknown,
    on_fd_is_set: unsafe extern "system" fn(Object, i32),
}

/// `Linux::ITimerHandler`: a plug-in object told when its interval has passed.
#[cfg(target_os = "linux")]
#[repr(C)]
struct TimerHandlerTable {
    unknown: Unknown,
    on_timer: unsafe extern "system" fn(Object),
}

/// `Linux::IRunLoop`, in the order of the VST3 SDK's `iplugview.h`.
#[cfg(target_os = "linux")]
#[repr(C)]
struct RunLoopTable {
    unknown: Unknown,
    register_event_handler: unsafe extern "system" fn(Object, Object, i32) -> TResult,
    unregister_event_handler: unsafe extern "system" fn(Object, Object) -> TResult,
    register_timer: unsafe extern "system" fn(Object, Object, u64) -> TResult,
    unregister_timer: unsafe extern "system" fn(Object, Object) -> TResult,
}

#[cfg(target_os = "linux")]
struct TimerRegistration {
    handler: Object,
    due: Instant,
    interval: Duration,
}

/// The host's Linux run loop: descriptors the plug-in wants read and timers it wants called.
/// Plug-ins must be able to find it from `IPlugFrame`, so it is the frame's first embedded
/// object and its table begins with `FUnknown` like every other VST3 interface.
#[cfg(target_os = "linux")]
#[repr(C)]
struct RunLoop {
    table: *const RunLoopTable,
    events: Mutex<Vec<(Object, i32)>>,
    timers: Mutex<Vec<TimerRegistration>>,
}

#[cfg(target_os = "linux")]
static RUNLOOP_TABLE: RunLoopTable = RunLoopTable {
    unknown: Unknown {
        query: runloop_query,
        add_ref: host_count,
        release: host_count,
    },
    register_event_handler: runloop_register_event_handler,
    unregister_event_handler: runloop_unregister_event_handler,
    register_timer: runloop_register_timer,
    unregister_timer: runloop_unregister_timer,
};

#[cfg(target_os = "linux")]
unsafe extern "system" fn runloop_query(
    this: Object,
    id: *const Tuid,
    out: *mut Object,
) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_RUNLOOP) }
}

#[cfg(target_os = "linux")]
unsafe extern "system" fn runloop_register_event_handler(
    this: Object,
    handler: Object,
    fd: i32,
) -> TResult {
    if this.is_null() || handler.is_null() || fd < 0 {
        return FALSE;
    }
    // SAFETY: `this` is the `RunLoop` embedded in the frame, held alive by the backend.
    let run = unsafe { &*this.cast::<RunLoop>() };
    let Ok(mut events) = run.events.lock() else {
        return FALSE;
    };
    if events.iter().any(|(h, _)| *h == handler) {
        return FALSE;
    }
    // SAFETY: `handler` is a live plug-in object that the host keeps until unregistration.
    unsafe { retain(handler) };
    events.push((handler, fd));
    OK
}

#[cfg(target_os = "linux")]
unsafe extern "system" fn runloop_unregister_event_handler(
    this: Object,
    handler: Object,
) -> TResult {
    if this.is_null() || handler.is_null() {
        return FALSE;
    }
    // SAFETY: as in `runloop_register_event_handler`.
    let run = unsafe { &*this.cast::<RunLoop>() };
    let entry = {
        let Ok(mut events) = run.events.lock() else {
            return FALSE;
        };
        let Some(index) = events.iter().position(|(h, _)| *h == handler) else {
            return FALSE;
        };
        events.remove(index)
    };
    // SAFETY: the entry was registered above and held one reference for the host.
    unsafe { release(entry.0) };
    OK
}

#[cfg(target_os = "linux")]
unsafe extern "system" fn runloop_register_timer(
    this: Object,
    handler: Object,
    milliseconds: u64,
) -> TResult {
    if this.is_null() || handler.is_null() {
        return FALSE;
    }
    let Some(due) = Instant::now().checked_add(Duration::from_millis(milliseconds)) else {
        return FALSE;
    };
    // SAFETY: as in `runloop_register_event_handler`.
    let run = unsafe { &*this.cast::<RunLoop>() };
    let Ok(mut timers) = run.timers.lock() else {
        return FALSE;
    };
    if timers.iter().any(|t| t.handler == handler) {
        return FALSE;
    }
    // SAFETY: `handler` is a live plug-in object that the host keeps until unregistration.
    unsafe { retain(handler) };
    timers.push(TimerRegistration {
        handler,
        due,
        interval: Duration::from_millis(milliseconds),
    });
    OK
}

#[cfg(target_os = "linux")]
unsafe extern "system" fn runloop_unregister_timer(this: Object, handler: Object) -> TResult {
    if this.is_null() || handler.is_null() {
        return FALSE;
    }
    // SAFETY: as in `runloop_register_event_handler`.
    let run = unsafe { &*this.cast::<RunLoop>() };
    let entry = {
        let Ok(mut timers) = run.timers.lock() else {
            return FALSE;
        };
        let Some(index) = timers.iter().position(|t| t.handler == handler) else {
            return FALSE;
        };
        timers.remove(index)
    };
    // SAFETY: the entry was registered above and held one reference for the host.
    unsafe { release(entry.handler) };
    OK
}

#[cfg(target_os = "linux")]
impl RunLoop {
    fn new() -> Self {
        Self {
            table: &RUNLOOP_TABLE,
            events: Mutex::new(Vec::new()),
            timers: Mutex::new(Vec::new()),
        }
    }

    /// Calls plug-in handlers for readable descriptors and elapsed timers. Called on the
    /// plug-in process's main thread, never from the audio thread.
    pub(crate) fn pump(&self) {
        // Snapshot the registrations: a handler may register or unregister during a callback.
        let events: Vec<(Object, i32)> = match self.events.lock() {
            Ok(events) => events.iter().copied().collect(),
            Err(_) => Vec::new(),
        };
        if !events.is_empty() {
            let mut fds: Vec<libc::pollfd> = events
                .iter()
                .map(|(_, fd)| libc::pollfd {
                    fd: *fd,
                    events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                    revents: 0,
                })
                .collect();
            // SAFETY: `fds` is writable and holds only descriptors the plug-in registered.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
            if ready > 0 {
                for ((handler, fd), polled) in events.iter().zip(&fds) {
                    if polled.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                        // SAFETY: the extra reference keeps the handler alive across its
                        // callback, even if it unregisters itself there.
                        unsafe {
                            retain(*handler);
                            (table::<EventHandlerTable>(*handler).on_fd_is_set)(*handler, *fd);
                            release(*handler);
                        }
                    }
                }
            }
        }

        let now = Instant::now();
        let due: Vec<Object> = match self.timers.lock() {
            Ok(mut timers) => {
                let mut due = Vec::new();
                for timer in timers.iter_mut() {
                    if timer.due <= now {
                        due.push(timer.handler);
                        if let Some(next) = now.checked_add(timer.interval) {
                            timer.due = next;
                        }
                    }
                }
                due
            }
            Err(_) => Vec::new(),
        };
        for handler in due {
            // SAFETY: the extra reference keeps the handler alive across its callback, even if
            // it unregisters itself there.
            unsafe {
                retain(handler);
                (table::<TimerHandlerTable>(handler).on_timer)(handler);
                release(handler);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for RunLoop {
    fn drop(&mut self) {
        // SAFETY: the host holds one reference for each live registration.
        if let Ok(events) = self.events.lock() {
            for (handler, _) in events.iter() {
                unsafe { release(*handler) };
            }
        }
        if let Ok(timers) = self.timers.lock() {
            for timer in timers.iter() {
                unsafe { release(timer.handler) };
            }
        }
    }
}

#[repr(C)]
struct FrameTable {
    unknown: Unknown,
    resize: unsafe extern "system" fn(Object, Object, *mut ViewRect) -> TResult,
}

/// The `IPlugFrame` a view asks for another size: the size is kept for the window (width in
/// the high half, height in the low; 0: nothing asked) and the view is told it has it. On Linux
/// it also carries the `IRunLoop` the view needs for its own event handling.
#[repr(C)]
struct Frame {
    table: *const FrameTable,
    wanted: AtomicU64,
    #[cfg(target_os = "linux")]
    run_loop: RunLoop,
}

static FRAME_TABLE: FrameTable = FrameTable {
    unknown: Unknown {
        query: frame_query,
        add_ref: host_count,
        release: host_count,
    },
    resize: frame_resize,
};

#[cfg(not(target_os = "linux"))]
unsafe extern "system" fn frame_query(this: Object, id: *const Tuid, out: *mut Object) -> TResult {
    // SAFETY: the plug-in's `queryInterface` arguments.
    unsafe { host_query(this, id, out, &IID_FRAME) }
}

#[cfg(target_os = "linux")]
unsafe extern "system" fn frame_query(this: Object, id: *const Tuid, out: *mut Object) -> TResult {
    if this.is_null() || id.is_null() || out.is_null() {
        return FALSE;
    }
    // SAFETY: checked not null; an interface id is sixteen readable bytes.
    let id = unsafe { &*id };
    if *id == IID_FRAME || *id == IID_UNKNOWN {
        // SAFETY: `out` is writable for one pointer.
        unsafe { *out = this };
        return OK;
    }
    if *id == IID_RUNLOOP {
        // SAFETY: `this` is the backend's live `Frame`; its `RunLoop` outlives the call.
        let frame = unsafe { &*this.cast::<Frame>() };
        // SAFETY: `out` is writable for one pointer, and the run loop starts with its table.
        unsafe {
            *out = std::ptr::from_ref(&frame.run_loop)
                .cast::<*const c_void>()
                .cast_mut();
        }
        return OK;
    }
    // SAFETY: `out` is writable for one pointer.
    unsafe { *out = std::ptr::null_mut() };
    NO_INTERFACE
}

unsafe extern "system" fn frame_resize(this: Object, view: Object, size: *mut ViewRect) -> TResult {
    if view.is_null() || size.is_null() {
        return FALSE;
    }
    // SAFETY: `this` is the backend's `Frame`; `view` is the plug-in's own live view and
    // `size` the rectangle it passes, on the main thread.
    unsafe {
        let rect = *size;
        let (width, height) = (
            (rect.right - rect.left).max(1) as u64,
            (rect.bottom - rect.top).max(1) as u64,
        );
        (*this.cast::<Frame>())
            .wanted
            .store((width << 32) | height, Ordering::Release);
        (table::<ViewTable>(view).on_size)(view, size);
    }
    OK
}

#[cfg(target_os = "linux")]
impl Frame {
    /// Works the run loop the plug-in may have registered with this frame.
    fn pump(&self) {
        self.run_loop.pump();
    }
}

pub(crate) struct Vst3Backend {
    handler: Box<Handler>,
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    frame: Box<Frame>,
    /// The plug-in's own view, once made (null before, and when it has none).
    view: std::sync::atomic::AtomicPtr<*const c_void>,
    component: Object,
    processor: Object,
    /// Null when the plug-in has no controller (then it has no parameters here).
    controller: Object,
    /// The controller is its own object, started and stopped apart from the component.
    separate: bool,
    connections: [Object; 2],
    params: Vec<Param>,
    names: Vec<ParamInfo>,
    audio: UnsafeCell<Audio>,
}

/// The channel counts of the component's audio buses in one direction.
///
/// # Safety
/// `component` is a live, initialised `IComponent`.
unsafe fn buses(component: Object, direction: i32) -> Vec<i32> {
    // SAFETY: the caller's contract; `BusInfo` is plain data that all-zero is valid for.
    unsafe {
        let t = table::<ComponentTable>(component);
        (0..(t.bus_count)(component, AUDIO, direction).clamp(0, MAX_BUSES))
            .map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                if (t.bus_info)(component, AUDIO, direction, i, &mut info) == OK {
                    info.channels.clamp(0, MAX_CHANNELS)
                } else {
                    0
                }
            })
            .collect()
    }
}

pub(crate) fn load(path: &Path, id: &str, rate: f64, block: usize) -> Result<Vst3Backend, Error> {
    let cid = unhex(id).ok_or_else(|| Error::NotAPlugin(format!("a bad VST3 class id {id}")))?;
    let create = |class: &Tuid, interface: &Tuid| {
        with_factory(path, |factory| {
            let mut out: Object = std::ptr::null_mut();
            // SAFETY: `factory` is the module's `IPluginFactory`; both ids are sixteen bytes
            // and `out` is writable.
            let result = unsafe {
                (table::<FactoryTable>(factory).create)(factory, class, interface, &mut out)
            };
            if result == OK {
                out
            } else {
                std::ptr::null_mut()
            }
        })
    };
    let component = create(&cid, &IID_COMPONENT)?;
    if component.is_null() {
        return Err(Error::Refused("make the plug-in"));
    }
    // SAFETY: `component` is a live `IComponent` from the factory. Every object taken from it
    // below is released on each way out; structures passed are plain data and writable.
    unsafe {
        let comp = table::<ComponentTable>(component);
        if (comp.initialize)(component, host()) != OK {
            release(component);
            return Err(Error::Refused("start"));
        }
        let stop = |processor: Object, error: Error| {
            release(processor);
            (comp.terminate)(component);
            release(component);
            error
        };
        let processor = query(component, &IID_PROCESSOR);
        if processor.is_null() {
            return Err(stop(processor, Error::NotAnEffect));
        }
        let proc_ = table::<ProcessorTable>(processor);
        let ins = buses(component, INPUT);
        let outs = buses(component, OUTPUT);
        let main = |list: &[i32]| list.first().is_some_and(|c| (1..=2).contains(c));
        if !main(&ins) || !main(&outs) {
            return Err(stop(processor, Error::NotAnEffect));
        }

        // The controller: the component itself, or a class of its own that is told the
        // component's state and connected to it.
        let mut controller = query(component, &IID_CONTROLLER);
        let mut separate = false;
        let mut connections = [std::ptr::null_mut(); 2];
        if controller.is_null() {
            let mut class: Tuid = [0; 16];
            if (comp.controller_class)(component, &mut class) == OK && class != [0; 16] {
                controller = create(&class, &IID_CONTROLLER).unwrap_or(std::ptr::null_mut());
            }
            if !controller.is_null() {
                if (table::<ControllerTable>(controller).initialize)(controller, host()) == OK {
                    separate = true;
                    let a = query(component, &IID_CONNECTION);
                    let b = query(controller, &IID_CONNECTION);
                    if !a.is_null() && !b.is_null() {
                        (table::<ConnectionTable>(a).connect)(a, b);
                        (table::<ConnectionTable>(b).connect)(b, a);
                        connections = [a, b];
                    } else {
                        release(a);
                        release(b);
                    }
                    let mut state = Stream::new(Vec::new());
                    if (comp.get_state)(component, state.object()) == OK {
                        state.at = 0;
                        (table::<ControllerTable>(controller).set_component_state)(
                            controller,
                            state.object(),
                        );
                    }
                } else {
                    release(controller);
                    controller = std::ptr::null_mut();
                }
            }
        }

        let mut setup = ProcessSetup {
            mode: 0,
            sample_size: 0,
            max_block: block as i32,
            rate,
        };
        (comp.activate_bus)(component, AUDIO, INPUT, 0, 1);
        (comp.activate_bus)(component, AUDIO, OUTPUT, 0, 1);
        let started =
            (proc_.setup)(processor, &mut setup) == OK && (comp.set_active)(component, 1) == OK;
        if !started {
            for c in connections {
                release(c);
            }
            if separate {
                (table::<ControllerTable>(controller).terminate)(controller);
            }
            release(controller);
            return Err(stop(processor, Error::Refused("activate")));
        }
        (proc_.set_processing)(processor, 1);

        let (edits_tx, edits_rx) = rtrb::RingBuffer::new(1024);
        let handler = Box::new(Handler {
            table: &HANDLER_TABLE,
            edits: Mutex::new(edits_tx),
        });
        let mut params = Vec::new();
        let mut names = Vec::new();
        if !controller.is_null() {
            let ctl = table::<ControllerTable>(controller);
            (ctl.set_handler)(
                controller,
                std::ptr::from_ref::<Handler>(&handler).cast_mut().cast(),
            );
            let count = (ctl.param_count)(controller).clamp(0, crate::MAX_PARAMS as i32);
            for i in 0..count {
                let mut raw: RawParam = std::mem::zeroed();
                if (ctl.param_info)(controller, i, &mut raw) != OK
                    || raw.flags & (PARAM_READ_ONLY | PARAM_HIDDEN | PARAM_BYPASS) != 0
                {
                    continue;
                }
                let default = raw.default as f32;
                names.push(ParamInfo {
                    name: text16(&raw.title),
                    default: if default.is_finite() {
                        default.clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                });
                params.push(Param {
                    id: raw.id,
                    units: text16(&raw.units),
                });
            }
        }

        let mut audio = Audio {
            block,
            input: [vec![0.0; block], vec![0.0; block]],
            output: [vec![0.0; block], vec![0.0; block]],
            silence: vec![0.0; block],
            spare: vec![0.0; block],
            _in_channels: Vec::new(),
            _out_channels: Vec::new(),
            in_buses: Vec::new(),
            out_buses: Vec::new(),
            in_changes: Box::new(Changes::new()),
            out_changes: Box::new(Changes::new()),
            context: std::mem::zeroed(),
            mono_out: outs.first() == Some(&1),
            edits: edits_rx,
        };
        audio.context.rate = rate;
        audio.context.tempo = 120.0;
        audio.context.numerator = 4;
        audio.context.denominator = 4;
        // The first bus of each direction carries the strip's sound; every other channel is
        // fed silence or written to the spare buffer.
        let mut in_channels: Vec<Vec<*mut f32>> = ins
            .iter()
            .enumerate()
            .map(|(bus, channels)| {
                (0..*channels)
                    .map(|c| match (bus, c) {
                        (0, 0) => audio.input[0].as_mut_ptr(),
                        (0, 1) => audio.input[1].as_mut_ptr(),
                        _ => audio.silence.as_mut_ptr(),
                    })
                    .collect()
            })
            .collect();
        let mut out_channels: Vec<Vec<*mut f32>> = outs
            .iter()
            .enumerate()
            .map(|(bus, channels)| {
                (0..*channels)
                    .map(|c| match (bus, c) {
                        (0, 0) => audio.output[0].as_mut_ptr(),
                        (0, 1) => audio.output[1].as_mut_ptr(),
                        _ => audio.spare.as_mut_ptr(),
                    })
                    .collect()
            })
            .collect();
        let bus = |channels: &mut Vec<*mut f32>| BusBuffers {
            channels: channels.len() as i32,
            silence: 0,
            buffers: channels.as_mut_ptr(),
        };
        audio.in_buses = in_channels.iter_mut().map(bus).collect();
        audio.out_buses = out_channels.iter_mut().map(bus).collect();
        audio._in_channels = in_channels;
        audio._out_channels = out_channels;

        Ok(Vst3Backend {
            handler,
            frame: Box::new(Frame {
                table: &FRAME_TABLE,
                wanted: AtomicU64::new(0),
                #[cfg(target_os = "linux")]
                run_loop: RunLoop::new(),
            }),
            view: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            component,
            processor,
            controller,
            separate,
            connections,
            params,
            names,
            audio: UnsafeCell::new(audio),
        })
    }
}

impl Vst3Backend {
    fn controller(&self) -> Option<&ControllerTable> {
        // SAFETY: a non-null `controller` is a live `IEditController` until `drop`.
        (!self.controller.is_null()).then(|| unsafe { table::<ControllerTable>(self.controller) })
    }
}

impl Backend for Vst3Backend {
    fn params(&self) -> Vec<ParamInfo> {
        self.names.clone()
    }

    fn main_param(&self, index: usize) -> f32 {
        match (self.controller(), self.params.get(index)) {
            (Some(ctl), Some(param)) => {
                // SAFETY: the controller's function on the main thread, with an id it gave.
                let value = unsafe { (ctl.get_norm)(self.controller, param.id) } as f32;
                if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }

    fn main_set_param(&self, index: usize, norm: f32) {
        if let (Some(ctl), Some(param)) = (self.controller(), self.params.get(index)) {
            // SAFETY: the controller's function on the main thread, with an id it gave.
            unsafe { (ctl.set_norm)(self.controller, param.id, f64::from(norm)) };
        }
    }

    fn main_text(&self, index: usize, norm: f32) -> String {
        let (Some(ctl), Some(param)) = (self.controller(), self.params.get(index)) else {
            return String::new();
        };
        let mut text: Text128 = [0; 128];
        // SAFETY: the controller's function on the main thread; `text` is writable.
        let ok = unsafe {
            (ctl.param_text)(self.controller, param.id, f64::from(norm), &mut text) == OK
        };
        let value = if ok {
            text16(&text)
        } else {
            format!("{norm:.2}")
        };
        if param.units.is_empty() {
            value
        } else {
            format!("{value} {}", param.units)
        }
    }

    fn main_latency(&self) -> usize {
        // SAFETY: `processor` is a live `IAudioProcessor` until `drop`.
        unsafe { (table::<ProcessorTable>(self.processor).latency)(self.processor) as usize }
    }

    /// The component's state, then the controller's: the length of the first (four bytes,
    /// little-endian), the first, the second.
    fn main_save(&self) -> Option<Vec<u8>> {
        let mut component = Stream::new(Vec::new());
        // SAFETY: `component` is a live `IComponent`; the stream lives for the call.
        let ok = unsafe {
            (table::<ComponentTable>(self.component).get_state)(self.component, component.object())
        } == OK;
        if !ok {
            return None;
        }
        let mut controller = Stream::new(Vec::new());
        if let Some(ctl) = self.controller() {
            // SAFETY: the controller's function on the main thread; the stream lives for the
            // call.
            unsafe { (ctl.get_state)(self.controller, controller.object()) };
        }
        let mut out = (component.bytes.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&component.bytes);
        out.extend_from_slice(&controller.bytes);
        Some(out)
    }

    fn main_load(&self, state: &[u8]) -> bool {
        let Some((length, rest)) = state.split_first_chunk::<4>() else {
            return false;
        };
        let length = u32::from_le_bytes(*length) as usize;
        if length > rest.len() || state.len() > MAX_STATE {
            return false;
        }
        let (component, controller) = rest.split_at(length);
        let mut stream = Stream::new(component.to_vec());
        // SAFETY: `component` is a live `IComponent`; the stream lives for each call.
        let ok = unsafe {
            (table::<ComponentTable>(self.component).set_state)(self.component, stream.object())
        } == OK;
        if let Some(ctl) = self.controller() {
            stream.at = 0;
            let mut own = Stream::new(controller.to_vec());
            // SAFETY: the controller's functions on the main thread; the streams live for the
            // calls.
            unsafe {
                (ctl.set_component_state)(self.controller, stream.object());
                if !controller.is_empty() {
                    (ctl.set_state)(self.controller, own.object());
                }
            }
        }
        ok
    }

    fn process(&self, left: &mut [f32], right: &mut [f32], changes: &[(u32, f32)]) {
        // SAFETY: only the audio thread calls `process`, one call at a time (see `Shared`).
        let audio = unsafe { &mut *self.audio.get() };
        let frames = left.len().min(right.len()).min(audio.block);
        if frames == 0 {
            return;
        }
        let mut count = 0;
        for (index, norm) in changes {
            if let Some(param) = self.params.get(*index as usize) {
                audio.in_changes.queues[count].id = param.id;
                audio.in_changes.queues[count].value = f64::from(*norm);
                count += 1;
            }
        }
        while count < MAX_CHANGES {
            let Ok((id, value)) = audio.edits.pop() else {
                break;
            };
            audio.in_changes.queues[count].id = id;
            audio.in_changes.queues[count].value = value;
            count += 1;
        }
        audio.in_changes.count = count as i32;
        audio.input[0][..frames].copy_from_slice(&left[..frames]);
        audio.input[1][..frames].copy_from_slice(&right[..frames]);
        let mut data = ProcessData {
            mode: 0,
            sample_size: 0,
            frames: frames as i32,
            input_count: audio.in_buses.len() as i32,
            output_count: audio.out_buses.len() as i32,
            inputs: audio.in_buses.as_mut_ptr(),
            outputs: audio.out_buses.as_mut_ptr(),
            in_changes: std::ptr::from_mut::<Changes>(&mut audio.in_changes).cast(),
            out_changes: std::ptr::from_mut::<Changes>(&mut audio.out_changes).cast(),
            in_events: std::ptr::null_mut(),
            out_events: std::ptr::null_mut(),
            context: &mut audio.context,
        };
        // SAFETY: `processor` is a live, active `IAudioProcessor`; every buffer holds
        // `block >= frames` samples and lives in `audio`.
        let result =
            unsafe { (table::<ProcessorTable>(self.processor).process)(self.processor, &mut data) };
        audio.context.continuous_samples += frames as i64;
        if result != OK {
            return;
        }
        left[..frames].copy_from_slice(&audio.output[0][..frames]);
        right[..frames].copy_from_slice(&audio.output[usize::from(!audio.mono_out)][..frames]);
    }

    fn main_idle(&self) {
        // Plug-ins using an editor register descriptors and timers with the run loop exposed
        // through their frame; this is the plug-in process's main thread.
        #[cfg(target_os = "linux")]
        self.frame.pump();
    }

    fn main_has_editor(&self) -> bool {
        // The view is made when it is first shown (making it needs the main thread's run
        // loop); until then a controller is taken to have one.
        cfg!(any(target_os = "macos", target_os = "linux")) && !self.controller.is_null()
    }

    fn main_editor_open(&self, parent: crate::EditorParent) -> Option<(u32, u32)> {
        let (parent_view, platform): (*mut c_void, &CStr) = match parent {
            #[cfg(target_os = "macos")]
            crate::EditorParent::Cocoa(view) => (view, c"NSView"),
            #[cfg(target_os = "linux")]
            crate::EditorParent::X11 { window, .. } => {
                (window as usize as *mut c_void, VST3_X11_PLATFORM)
            }
            _ => return None,
        };
        let ctl = self.controller()?;
        // SAFETY: the controller's and the view's functions on the main thread, in VST3's
        // order: make the view, ask for its platform, give it its frame (the host's run loop on
        // Linux), put it in the parent view (which the caller keeps alive), measure it.
        unsafe {
            let mut view: Object = self.view.load(Ordering::Relaxed);
            if view.is_null() {
                view = (ctl.create_view)(self.controller, c"editor".as_ptr());
                if view.is_null() {
                    return None;
                }
                let t = table::<ViewTable>(view);
                if (t.supported)(view, platform.as_ptr()) != OK {
                    release(view);
                    return None;
                }
                (t.set_frame)(
                    view,
                    std::ptr::from_ref::<Frame>(&self.frame).cast_mut().cast(),
                );
                if (t.attached)(view, parent_view, platform.as_ptr()) != OK {
                    (t.set_frame)(view, std::ptr::null_mut());
                    release(view);
                    return None;
                }
                self.view.store(view, Ordering::Relaxed);
            }
            let mut rect = ViewRect::default();
            (table::<ViewTable>(view).get_size)(view, &mut rect);
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
            if width > 0 && height > 0 {
                Some((width as u32, height as u32))
            } else {
                Some((480, 320))
            }
        }
    }

    fn main_editor_close(&self) {
        let view: Object = self.view.swap(std::ptr::null_mut(), Ordering::Relaxed);
        if !view.is_null() {
            // SAFETY: `view` is the live view made in `main_editor_open`, taken out of its
            // parent and released once, on the main thread.
            unsafe {
                let t = table::<ViewTable>(view);
                (t.removed)(view);
                (t.set_frame)(view, std::ptr::null_mut());
                release(view);
            }
        }
    }

    fn main_editor_resized(&self) -> Option<(u32, u32)> {
        let size = self.frame.wanted.swap(0, Ordering::AcqRel);
        (size != 0).then_some(((size >> 32) as u32, size as u32))
    }

    fn reset(&self) {
        // VST3 clears a plug-in's memory by stopping and starting it, which belongs to the main
        // thread; the tail is left to ring out instead.
    }
}

impl Drop for Vst3Backend {
    fn drop(&mut self) {
        self.main_editor_close();
        // SAFETY: the last handle is gone, so no thread is in `process`; each object is live
        // and released once, in the reverse order of `load`.
        unsafe {
            if !self.controller.is_null() {
                let _ = &self.handler;
                (table::<ControllerTable>(self.controller).set_handler)(
                    self.controller,
                    std::ptr::null_mut(),
                );
            }
            (table::<ProcessorTable>(self.processor).set_processing)(self.processor, 0);
            let comp = table::<ComponentTable>(self.component);
            (comp.set_active)(self.component, 0);
            let [a, b] = self.connections;
            if !a.is_null() && !b.is_null() {
                (table::<ConnectionTable>(a).disconnect)(a, b);
                (table::<ConnectionTable>(b).disconnect)(b, a);
            }
            release(a);
            release(b);
            if self.separate {
                (table::<ControllerTable>(self.controller).terminate)(self.controller);
            }
            release(self.controller);
            release(self.processor);
            (comp.terminate)(self.component);
            release(self.component);
        }
    }
}
