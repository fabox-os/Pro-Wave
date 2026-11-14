//! Audio Unit hosting (macOS): the AudioToolbox C interface for effect units (`aufx`, `aumf`).
//!
//! The system keeps the list of Audio Units, so scanning runs no plug-in code. An effect unit
//! pulls its input through a callback and is asked for its output with `AudioUnitRender`.

use crate::{Backend, Error, Format, ParamInfo, PluginInfo};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

type OsStatus = i32;
type Component = *mut c_void;
type Unit = *mut c_void;
type CfRef = *const c_void;

const TYPE_EFFECT: u32 = fourcc(b"aufx");
const TYPE_MUSIC_EFFECT: u32 = fourcc(b"aumf");
const FORMAT_LPCM: u32 = fourcc(b"lpcm");
/// Float, packed, one buffer for each channel.
const FORMAT_FLAGS: u32 = 1 | 8 | 32;

const SCOPE_GLOBAL: u32 = 0;
const SCOPE_INPUT: u32 = 1;
const SCOPE_OUTPUT: u32 = 2;

const PROP_CLASS_INFO: u32 = 0;
const PROP_PARAMETER_LIST: u32 = 3;
const PROP_PARAMETER_INFO: u32 = 4;
const PROP_STREAM_FORMAT: u32 = 8;
const PROP_LATENCY: u32 = 12;
const PROP_MAX_FRAMES: u32 = 14;
const PROP_RENDER_CALLBACK: u32 = 23;
const PROP_COCOA_UI: u32 = 31;

const PARAM_CF_NAME_RELEASE: u32 = 1 << 4;
const PARAM_HAS_CF_NAME: u32 = 1 << 27;
const PARAM_IS_WRITABLE: u32 = 1 << 31;

const TIME_SAMPLE_VALID: u32 = 1;
const CF_UTF8: u32 = 0x0800_0100;
const PLIST_BINARY: isize = 200;
/// The largest state read back from a project (64 MiB).
const MAX_STATE: usize = 64 * 1024 * 1024;

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Description {
    kind: u32,
    sub_kind: u32,
    manufacturer: u32,
    flags: u32,
    flags_mask: u32,
}

#[repr(C)]
#[derive(Default)]
struct StreamFormat {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Buffer {
    channels: u32,
    byte_size: u32,
    data: *mut c_void,
}

/// An `AudioBufferList` of two buffers (stereo, not interleaved).
#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [Buffer; 2],
}

/// `AudioTimeStamp`: only the sample time is used; the rest is zero.
#[repr(C)]
struct TimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte: [u8; 24],
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct RenderCallback {
    proc_: unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const TimeStamp,
        u32,
        u32,
        *mut BufferList,
    ) -> OsStatus,
    refcon: *mut c_void,
}

#[repr(C)]
struct RawParam {
    name: [u8; 52],
    unit_name: CfRef,
    clump: u32,
    cf_name: CfRef,
    unit: u32,
    min: f32,
    max: f32,
    default: f32,
    flags: u32,
}

// AUGenericView, the system's view for a unit that has none of its own.
#[link(name = "CoreAudioKit", kind = "framework")]
extern "C" {}

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioComponentFindNext(after: Component, desc: *const Description) -> Component;
    fn AudioComponentGetDescription(component: Component, out: *mut Description) -> OsStatus;
    fn AudioComponentCopyName(component: Component, out: *mut CfRef) -> OsStatus;
    fn AudioComponentInstanceNew(component: Component, out: *mut Unit) -> OsStatus;
    fn AudioComponentInstanceDispose(unit: Unit) -> OsStatus;
    fn AudioUnitInitialize(unit: Unit) -> OsStatus;
    fn AudioUnitUninitialize(unit: Unit) -> OsStatus;
    fn AudioUnitReset(unit: Unit, scope: u32, element: u32) -> OsStatus;
    fn AudioUnitGetPropertyInfo(
        unit: Unit,
        id: u32,
        scope: u32,
        element: u32,
        size: *mut u32,
        writable: *mut u8,
    ) -> OsStatus;
    fn AudioUnitGetProperty(
        unit: Unit,
        id: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> OsStatus;
    fn AudioUnitSetProperty(
        unit: Unit,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> OsStatus;
    fn AudioUnitGetParameter(
        unit: Unit,
        id: u32,
        scope: u32,
        element: u32,
        value: *mut f32,
    ) -> OsStatus;
    fn AudioUnitSetParameter(
        unit: Unit,
        id: u32,
        scope: u32,
        element: u32,
        value: f32,
        offset: u32,
    ) -> OsStatus;
    fn AudioUnitRender(
        unit: Unit,
        flags: *mut u32,
        time: *const TimeStamp,
        bus: u32,
        frames: u32,
        data: *mut BufferList,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(object: CfRef);
    fn CFStringGetCString(string: CfRef, buffer: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFDataCreate(allocator: CfRef, bytes: *const u8, length: isize) -> CfRef;
    fn CFDataGetLength(data: CfRef) -> isize;
    fn CFDataGetBytePtr(data: CfRef) -> *const u8;
    fn CFPropertyListCreateData(
        allocator: CfRef,
        plist: CfRef,
        format: isize,
        options: usize,
        error: *mut CfRef,
    ) -> CfRef;
    fn CFPropertyListCreateWithData(
        allocator: CfRef,
        data: CfRef,
        options: usize,
        format: *mut isize,
        error: *mut CfRef,
    ) -> CfRef;
}

/// The text of a CFString (empty for null or text that does not fit).
///
/// # Safety
/// `string` is null or a valid CFString.
unsafe fn cf_text(string: CfRef) -> String {
    if string.is_null() {
        return String::new();
    }
    let mut buffer = [0u8; 512];
    // SAFETY: `buffer` is writable for the size given; the call NUL-terminates on success.
    if unsafe { CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, CF_UTF8) }
        == 0
    {
        return String::new();
    }
    crate::buffer_text(&buffer)
}

fn code_text(code: u32) -> String {
    code.to_be_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The three codes of an id written by [`scan`] (`type:subtype:manufacturer`, in hexadecimal).
fn parse_id(id: &str) -> Option<Description> {
    let mut codes = id.split(':').map(|c| u32::from_str_radix(c, 16).ok());
    let desc = Description {
        kind: codes.next()??,
        sub_kind: codes.next()??,
        manufacturer: codes.next()??,
        flags: 0,
        flags_mask: 0,
    };
    codes.next().is_none().then_some(desc)
}

pub(crate) fn scan() -> Vec<PluginInfo> {
    let mut out = Vec::new();
    for kind in [TYPE_EFFECT, TYPE_MUSIC_EFFECT] {
        let wanted = Description {
            kind,
            ..Description::default()
        };
        let mut component: Component = std::ptr::null_mut();
        // The system's list is finite; the cap only guards against a broken one.
        for _ in 0..10_000 {
            // SAFETY: `component` is null or the previous result; `wanted` lives for the call.
            component = unsafe { AudioComponentFindNext(component, &wanted) };
            if component.is_null() {
                break;
            }
            let mut desc = Description::default();
            let mut name: CfRef = std::ptr::null();
            // SAFETY: `component` is a valid component; both out pointers are writable. The
            // copied name is released below.
            let full = unsafe {
                if AudioComponentGetDescription(component, &mut desc) != 0 {
                    continue;
                }
                AudioComponentCopyName(component, &mut name);
                let text = cf_text(name);
                if !name.is_null() {
                    CFRelease(name);
                }
                text
            };
            // The system writes "Vendor: Name".
            let (vendor, name) = match full.split_once(": ") {
                Some((vendor, name)) => (vendor.to_string(), name.to_string()),
                None => (String::new(), full),
            };
            if name.is_empty() {
                continue;
            }
            out.push(PluginInfo {
                format: Format::AudioUnit,
                id: format!(
                    "{}:{}:{}",
                    code_text(desc.kind),
                    code_text(desc.sub_kind),
                    code_text(desc.manufacturer)
                ),
                name,
                vendor,
                path: PathBuf::new(),
            });
        }
    }
    out
}

struct Param {
    id: u32,
    min: f32,
    max: f32,
}

/// What the audio thread uses in `process`; boxed so the unit's callback can point to it.
struct Audio {
    block: usize,
    input: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    sample_time: f64,
}

pub(crate) struct AuBackend {
    unit: Unit,
    params: Vec<Param>,
    names: Vec<ParamInfo>,
    rate: f64,
    audio: Box<UnsafeCell<Audio>>,
    /// The unit's own view once made (null before), and its size when last looked at.
    view: AtomicPtr<c_void>,
    view_size: AtomicU64,
}

/// The unit asks for its input: the block `process` put in `Audio::input`.
unsafe extern "C" fn pull_input(
    refcon: *mut c_void,
    _flags: *mut u32,
    _time: *const TimeStamp,
    _bus: u32,
    frames: u32,
    data: *mut BufferList,
) -> OsStatus {
    if refcon.is_null() || data.is_null() {
        return -50;
    }
    // SAFETY: `refcon` is the backend's `Audio`, which outlives the unit; the unit calls this
    // inside `AudioUnitRender`, on the thread that is in `process`.
    let audio = unsafe { &mut *refcon.cast::<UnsafeCell<Audio>>().cast::<Audio>() };
    // SAFETY: the unit gives a buffer list with `count` buffers (two were asked for).
    let list = unsafe { &mut *data };
    let frames = (frames as usize).min(audio.block);
    for (channel, buffer) in list
        .buffers
        .iter_mut()
        .enumerate()
        .take(list.count as usize)
    {
        let source = &mut audio.input[channel.min(1)];
        if buffer.data.is_null() {
            buffer.data = source.as_mut_ptr().cast();
        } else {
            let n = frames.min(buffer.byte_size as usize / 4);
            // SAFETY: the unit's buffer holds `byte_size` bytes; `n` samples fit both sides.
            unsafe {
                std::ptr::copy_nonoverlapping(source.as_ptr(), buffer.data.cast::<f32>(), n);
            }
        }
    }
    0
}

pub(crate) fn load(id: &str, rate: f64, block: usize) -> Result<AuBackend, Error> {
    let desc =
        parse_id(id).ok_or_else(|| Error::NotAPlugin(format!("a bad Audio Unit id {id}")))?;
    // SAFETY: `desc` lives for the call; a null start searches from the beginning.
    let component = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
    if component.is_null() {
        return Err(Error::NotAPlugin(format!("no Audio Unit {id}")));
    }
    let mut unit: Unit = std::ptr::null_mut();
    // SAFETY: `component` is valid and `unit` writable.
    if unsafe { AudioComponentInstanceNew(component, &mut unit) } != 0 || unit.is_null() {
        return Err(Error::Refused("make the Audio Unit"));
    }
    let audio = Box::new(UnsafeCell::new(Audio {
        block,
        input: [vec![0.0; block], vec![0.0; block]],
        output: [vec![0.0; block], vec![0.0; block]],
        sample_time: 0.0,
    }));
    let fail = |step| {
        // SAFETY: a unit that failed to start is disposed of; nothing else holds it.
        unsafe { AudioComponentInstanceDispose(unit) };
        Error::Refused(step)
    };
    let set = |id: u32, scope: u32, data: *const c_void, size: usize| {
        // SAFETY: `data` points to `size` readable bytes of the property's type.
        unsafe { AudioUnitSetProperty(unit, id, scope, 0, data, size as u32) == 0 }
    };
    let format = StreamFormat {
        sample_rate: rate,
        format_id: FORMAT_LPCM,
        format_flags: FORMAT_FLAGS,
        bytes_per_packet: 4,
        frames_per_packet: 1,
        bytes_per_frame: 4,
        channels: 2,
        bits_per_channel: 32,
        reserved: 0,
    };
    let format_ptr = std::ptr::from_ref(&format).cast::<c_void>();
    let format_size = std::mem::size_of::<StreamFormat>();
    if !set(PROP_STREAM_FORMAT, SCOPE_INPUT, format_ptr, format_size)
        || !set(PROP_STREAM_FORMAT, SCOPE_OUTPUT, format_ptr, format_size)
    {
        fail("");
        return Err(Error::NotAnEffect);
    }
    let frames = block as u32;
    set(
        PROP_MAX_FRAMES,
        SCOPE_GLOBAL,
        std::ptr::from_ref(&frames).cast(),
        4,
    );
    let callback = RenderCallback {
        proc_: pull_input,
        refcon: std::ptr::from_ref::<UnsafeCell<Audio>>(&audio)
            .cast_mut()
            .cast(),
    };
    if !set(
        PROP_RENDER_CALLBACK,
        SCOPE_INPUT,
        std::ptr::from_ref(&callback).cast(),
        std::mem::size_of::<RenderCallback>(),
    ) {
        return Err(fail("take an input"));
    }
    // SAFETY: `unit` is a valid, configured unit.
    if unsafe { AudioUnitInitialize(unit) } != 0 {
        return Err(fail("start"));
    }

    let mut params = Vec::new();
    let mut names = Vec::new();
    let mut size = 0u32;
    // SAFETY: `unit` is valid; `size` is writable and the writable flag may be null.
    let listed = unsafe {
        AudioUnitGetPropertyInfo(
            unit,
            PROP_PARAMETER_LIST,
            SCOPE_GLOBAL,
            0,
            &mut size,
            std::ptr::null_mut(),
        )
    } == 0;
    if listed && size >= 4 {
        let mut ids = vec![0u32; (size as usize / 4).min(crate::MAX_PARAMS)];
        let mut bytes = (ids.len() * 4) as u32;
        // SAFETY: `ids` is writable for `bytes` bytes.
        let ok = unsafe {
            AudioUnitGetProperty(
                unit,
                PROP_PARAMETER_LIST,
                SCOPE_GLOBAL,
                0,
                ids.as_mut_ptr().cast(),
                &mut bytes,
            )
        } == 0;
        ids.truncate(if ok { bytes as usize / 4 } else { 0 });
        for id in ids {
            // SAFETY: `RawParam` is plain data; all-zero is a valid value.
            let mut raw: RawParam = unsafe { std::mem::zeroed() };
            let mut raw_size = std::mem::size_of::<RawParam>() as u32;
            // SAFETY: `raw` is writable for `raw_size` bytes; the element is the parameter id.
            let ok = unsafe {
                AudioUnitGetProperty(
                    unit,
                    PROP_PARAMETER_INFO,
                    SCOPE_GLOBAL,
                    id,
                    std::ptr::from_mut(&mut raw).cast(),
                    &mut raw_size,
                )
            } == 0;
            if !ok {
                continue;
            }
            let name = if raw.flags & PARAM_HAS_CF_NAME != 0 && !raw.cf_name.is_null() {
                // SAFETY: the flag says `cf_name` is a CFString; it is released when the unit
                // says the caller owns it.
                unsafe {
                    let text = cf_text(raw.cf_name);
                    if raw.flags & PARAM_CF_NAME_RELEASE != 0 {
                        CFRelease(raw.cf_name);
                    }
                    text
                }
            } else {
                crate::buffer_text(&raw.name)
            };
            let usable = raw.flags & PARAM_IS_WRITABLE != 0
                && raw.min.is_finite()
                && raw.max.is_finite()
                && raw.max > raw.min;
            if !usable {
                continue;
            }
            let default = ((raw.default - raw.min) / (raw.max - raw.min)).clamp(0.0, 1.0);
            names.push(ParamInfo {
                name,
                default: if default.is_finite() { default } else { 0.0 },
            });
            params.push(Param {
                id,
                min: raw.min,
                max: raw.max,
            });
        }
    }
    Ok(AuBackend {
        unit,
        params,
        names,
        rate,
        audio,
        view: AtomicPtr::new(std::ptr::null_mut()),
        view_size: AtomicU64::new(0),
    })
}

impl Backend for AuBackend {
    fn params(&self) -> Vec<ParamInfo> {
        self.names.clone()
    }

    fn main_param(&self, index: usize) -> f32 {
        let Some(param) = self.params.get(index) else {
            return 0.0;
        };
        let mut value = 0.0f32;
        // SAFETY: `unit` is valid until `drop`; reading a parameter is allowed on any thread.
        if unsafe { AudioUnitGetParameter(self.unit, param.id, SCOPE_GLOBAL, 0, &mut value) } != 0
            || !value.is_finite()
        {
            return 0.0;
        }
        ((value - param.min) / (param.max - param.min)).clamp(0.0, 1.0)
    }

    fn main_text(&self, index: usize, norm: f32) -> String {
        match self.params.get(index) {
            Some(param) => format!("{:.2}", param.min + (param.max - param.min) * norm),
            None => String::new(),
        }
    }

    fn main_latency(&self) -> usize {
        let mut seconds = 0.0f64;
        let mut size = 8u32;
        // SAFETY: `seconds` is writable for the eight bytes of the latency property.
        let ok = unsafe {
            AudioUnitGetProperty(
                self.unit,
                PROP_LATENCY,
                SCOPE_GLOBAL,
                0,
                std::ptr::from_mut(&mut seconds).cast(),
                &mut size,
            )
        } == 0;
        if ok && seconds.is_finite() && seconds > 0.0 {
            (seconds * self.rate).round().min(1e7) as usize
        } else {
            0
        }
    }

    fn main_save(&self) -> Option<Vec<u8>> {
        let mut plist: CfRef = std::ptr::null();
        let mut size = std::mem::size_of::<CfRef>() as u32;
        // SAFETY: the class info property is one CFPropertyList the caller owns; `plist` is
        // writable for a pointer. Every object made here is released.
        unsafe {
            if AudioUnitGetProperty(
                self.unit,
                PROP_CLASS_INFO,
                SCOPE_GLOBAL,
                0,
                std::ptr::from_mut(&mut plist).cast(),
                &mut size,
            ) != 0
                || plist.is_null()
            {
                return None;
            }
            let data = CFPropertyListCreateData(
                std::ptr::null(),
                plist,
                PLIST_BINARY,
                0,
                std::ptr::null_mut(),
            );
            CFRelease(plist);
            if data.is_null() {
                return None;
            }
            let length = CFDataGetLength(data).max(0) as usize;
            let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(data), length).to_vec();
            CFRelease(data);
            Some(bytes)
        }
    }

    fn main_load(&self, state: &[u8]) -> bool {
        if state.is_empty() || state.len() > MAX_STATE {
            return false;
        }
        // SAFETY: `state` is readable for its length; the data and the property list made from
        // it are released here, and the unit keeps its own reference to what it needs.
        unsafe {
            let data = CFDataCreate(std::ptr::null(), state.as_ptr(), state.len() as isize);
            if data.is_null() {
                return false;
            }
            let plist = CFPropertyListCreateWithData(
                std::ptr::null(),
                data,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CFRelease(data);
            if plist.is_null() {
                return false;
            }
            let ok = AudioUnitSetProperty(
                self.unit,
                PROP_CLASS_INFO,
                SCOPE_GLOBAL,
                0,
                std::ptr::from_ref(&plist).cast(),
                std::mem::size_of::<CfRef>() as u32,
            ) == 0;
            CFRelease(plist);
            ok
        }
    }

    fn process(&self, left: &mut [f32], right: &mut [f32], changes: &[(u32, f32)]) {
        for (index, norm) in changes {
            if let Some(param) = self.params.get(*index as usize) {
                let value = param.min + (param.max - param.min) * norm;
                // SAFETY: `unit` is valid until `drop`; setting a parameter is real-time safe
                // by the Audio Unit contract.
                unsafe { AudioUnitSetParameter(self.unit, param.id, SCOPE_GLOBAL, 0, value, 0) };
            }
        }
        // SAFETY: only the audio thread calls `process`, one call at a time (see `Shared`).
        // The input callback reaches the same `Audio` from inside `AudioUnitRender` below, so
        // no reference to it is held across that call.
        let audio = self.audio.get();
        let frames = unsafe {
            let audio = &mut *audio;
            let frames = left.len().min(right.len()).min(audio.block);
            audio.input[0][..frames].copy_from_slice(&left[..frames]);
            audio.input[1][..frames].copy_from_slice(&right[..frames]);
            frames
        };
        if frames == 0 {
            return;
        }
        // SAFETY: as above; the pointers stay valid because the vectors are never resized.
        let (out_l, out_r, sample_time) = unsafe {
            let audio = &mut *audio;
            (
                audio.output[0].as_mut_ptr(),
                audio.output[1].as_mut_ptr(),
                audio.sample_time,
            )
        };
        let buffer = |data: *mut f32| Buffer {
            channels: 1,
            byte_size: (frames * 4) as u32,
            data: data.cast(),
        };
        let mut list = BufferList {
            count: 2,
            buffers: [buffer(out_l), buffer(out_r)],
        };
        let time = TimeStamp {
            sample_time,
            host_time: 0,
            rate_scalar: 0.0,
            word_clock_time: 0,
            smpte: [0; 24],
            flags: TIME_SAMPLE_VALID,
            reserved: 0,
        };
        let mut flags = 0u32;
        // SAFETY: `unit` is initialised; `list` has two buffers of `frames` samples; the call
        // is real-time safe by the Audio Unit contract.
        let status =
            unsafe { AudioUnitRender(self.unit, &mut flags, &time, 0, frames as u32, &mut list) };
        // SAFETY: back from the render call, this is the only reference to `Audio`.
        unsafe { (*audio).sample_time += frames as f64 };
        if status != 0 {
            return;
        }
        // The unit may have pointed the list at its own buffers.
        for (channel, out) in [left, right].into_iter().enumerate() {
            let data = list.buffers[channel].data.cast::<f32>();
            if !data.is_null() && list.buffers[channel].byte_size as usize >= frames * 4 {
                // SAFETY: the buffer holds at least `frames` samples (checked above).
                out[..frames].copy_from_slice(unsafe { std::slice::from_raw_parts(data, frames) });
            }
        }
    }

    fn main_has_editor(&self) -> bool {
        // A unit without a view of its own gets the system's generic one.
        true
    }

    fn main_editor_open(&self, parent: *mut c_void) -> Option<(u32, u32)> {
        use crate::mac::{class, frame, msg, Id, Rect, Size};
        // SAFETY: AppKit and AudioToolbox calls on the main thread, each with its own argument
        // types. The Cocoa UI property is a bundle URL followed by class names, all owned by
        // the caller; the first class makes the view.
        unsafe {
            let mut view: Id = self.view.load(Ordering::Relaxed);
            if view.is_null() {
                let mut size = 0u32;
                let listed = AudioUnitGetPropertyInfo(
                    self.unit,
                    PROP_COCOA_UI,
                    SCOPE_GLOBAL,
                    0,
                    &mut size,
                    std::ptr::null_mut(),
                ) == 0;
                let pointers = size as usize / std::mem::size_of::<CfRef>();
                if listed && (2..=64).contains(&pointers) {
                    let mut info: Vec<CfRef> = vec![std::ptr::null(); pointers];
                    if AudioUnitGetProperty(
                        self.unit,
                        PROP_COCOA_UI,
                        SCOPE_GLOBAL,
                        0,
                        info.as_mut_ptr().cast(),
                        &mut size,
                    ) == 0
                    {
                        let bundle = msg!(Id, class("NSBundle"), "bundleWithURL:",
                            Id: info[0].cast_mut());
                        if !bundle.is_null() {
                            let made = msg!(Id, bundle, "classNamed:", Id: info[1].cast_mut());
                            if !made.is_null() {
                                let factory = msg!(Id, msg!(Id, made, "alloc"), "init");
                                view = msg!(Id, factory, "uiViewForAudioUnit:withSize:",
                                    *mut c_void: self.unit, Size: Size { w: 0.0, h: 0.0 });
                            }
                        }
                        for object in info.into_iter().filter(|o| !o.is_null()) {
                            CFRelease(object);
                        }
                    }
                }
                if view.is_null() {
                    let generic = class("AUGenericView");
                    if generic.is_null() {
                        return None;
                    }
                    view = msg!(Id, msg!(Id, generic, "alloc"), "initWithAudioUnit:",
                        *mut c_void: self.unit);
                }
                if view.is_null() {
                    return None;
                }
                let at = frame(view);
                msg!((), view, "setFrame:",
                    Rect: Rect { x: 0.0, y: 0.0, w: at.w, h: at.h });
                msg!((), parent.cast::<c_void>(), "addSubview:", Id: view);
                self.view.store(view, Ordering::Relaxed);
            }
            let at = frame(view);
            let (width, height) = (at.w.max(1.0) as u32, at.h.max(1.0) as u32);
            self.view_size.store(
                (u64::from(width) << 32) | u64::from(height),
                Ordering::Relaxed,
            );
            Some((width, height))
        }
    }

    fn main_editor_close(&self) {
        let view = self.view.swap(std::ptr::null_mut(), Ordering::Relaxed);
        if !view.is_null() {
            // SAFETY: `view` is the live NSView made in `main_editor_open`.
            unsafe { crate::mac::msg!((), view, "removeFromSuperview") };
        }
    }

    /// An Audio Unit's view sizes itself; the window follows when its size has changed.
    fn main_editor_resized(&self) -> Option<(u32, u32)> {
        let view = self.view.load(Ordering::Relaxed);
        if view.is_null() {
            return None;
        }
        // SAFETY: `view` is the live NSView made in `main_editor_open`.
        let at = unsafe { crate::mac::frame(view) };
        let (width, height) = (at.w.max(1.0) as u32, at.h.max(1.0) as u32);
        let size = (u64::from(width) << 32) | u64::from(height);
        (self.view_size.swap(size, Ordering::Relaxed) != size).then_some((width, height))
    }

    fn reset(&self) {
        // SAFETY: `unit` is valid until `drop`.
        unsafe { AudioUnitReset(self.unit, SCOPE_GLOBAL, 0) };
    }
}

impl Drop for AuBackend {
    fn drop(&mut self) {
        self.main_editor_close();
        // SAFETY: the last handle is gone, so no thread is rendering; the unit is ours.
        unsafe {
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
        }
    }
}
