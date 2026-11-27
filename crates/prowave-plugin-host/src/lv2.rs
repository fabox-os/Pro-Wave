// LV2 plug-in hosting on Linux, via liblilv.
#![cfg(target_os = "linux")]

use crate::{Backend, EditorParent, Error, Format, ParamInfo, PluginInfo};
use libloading::Library;
use lilv::World;
use lv2_raw::core::LV2Feature;
use lv2_raw::urid::LV2Urid;
use lv2_raw::urid::LV2UridMap;
use lv2_sys::{LV2UI_Descriptor, LV2_Atom_Sequence};
use std::collections::HashMap;
use std::ffi::CStr;
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use x11_dl::xlib::{self, Xlib};

// The URID map is process-wide so the option values passed to one instance use the same IDs the
// plug-in receives from its `urid:map` feature. The map callback ignores its handle, so no
// per-instance state is needed here.
static GLOBAL_URID_MAP: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
static GLOBAL_URID_NEXT: AtomicU32 = AtomicU32::new(1);

/// Enough room for the atom sequences JUCE plug-ins exchange with their UI (including time
/// position messages); larger than the 9848-byte minimum advertised by typical plug-ins.
const ATOM_BUFFER_BYTES: usize = 16384;

fn global_urid(uri: &str) -> u32 {
    let mut g = GLOBAL_URID_MAP.lock().unwrap_or_else(|p| p.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(uri) {
        return *v;
    }
    let mut v = GLOBAL_URID_NEXT.fetch_add(1, Ordering::Relaxed);
    if v == 0 {
        v = 1;
    }
    m.insert(uri.to_string(), v);
    v
}

extern "C" fn urid_map_fn(_handle: *mut std::os::raw::c_void, uri: *const c_char) -> LV2Urid {
    if uri.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(uri) }.to_str().unwrap_or("");
    global_urid(s)
}

// One world for every LV2 instance. Creating and dropping a world per instance dlopen/dlcloses the
// same plugins repeatedly; Calf Organ leaves global state behind and crashes when Calf Vinyl is
// later loaded from a fresh world. A `Plugin` keeps the world's `Life` alive, so instances remain
// valid after the short-lived mutex guard is released.
static LV2_WORLD: std::sync::OnceLock<Mutex<World>> = std::sync::OnceLock::new();

fn shared_world() -> std::sync::MutexGuard<'static, World> {
    let world = LV2_WORLD.get_or_init(|| Mutex::new(World::with_load_all()));
    world
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Initialises an LV2 atom sequence buffer. Output buffers advertise their free body capacity;
/// input buffers start empty so the plug-in's event iterator sees no messages.
fn init_atom_sequence(buffer: &mut [u64], output: bool) {
    let bytes = std::mem::size_of_val(buffer);
    let sequence = buffer.as_mut_ptr() as *mut LV2_Atom_Sequence;
    unsafe {
        (*sequence).atom.size = if output {
            (bytes - std::mem::size_of::<lv2_sys::LV2_Atom>()) as u32
        } else {
            0
        };
        (*sequence).atom.type_ = global_urid("http://lv2plug.in/ns/ext/atom#Sequence");
        (*sequence).body.unit = global_urid("http://lv2plug.in/ns/ext/time#frames");
        (*sequence).body.pad = 0;
    }
}

/// Reads an LV2 numeric literal. `Node::as_float()` only accepts decimal literals, while many
/// plug-ins (including Calf) write integer literals such as `lv2:default 1`, so ints must be
/// accepted too. Without this, those ports get a bogus range and Calf Crusher produces NaNs.
fn numeric_node(node: Option<&lilv::node::Node>) -> Option<f32> {
    let node = node?;
    node.as_float().or_else(|| node.as_int().map(|v| v as f32))
}

/// Stable storage for the `opts:options` array passed to the plug-in and its UI. Values live here
/// for the instance's whole lifetime; plug-ins keep the option pointers after instantiating.
#[repr(C)]
struct Lv2Options {
    sample_rate: f32,
    block_length: i32,
    values: [lv2_sys::LV2_Options_Option; 3],
}

impl Lv2Options {
    fn new(rate: f64, block: usize) -> Box<Self> {
        let mut options = Box::new(Self {
            sample_rate: rate as f32,
            block_length: block as i32,
            values: [
                lv2_sys::LV2_Options_Option {
                    context: lv2_sys::LV2_Options_Context_LV2_OPTIONS_INSTANCE,
                    subject: 0,
                    key: global_urid("http://lv2plug.in/ns/ext/parameters#sampleRate"),
                    size: std::mem::size_of::<f32>() as u32,
                    type_: global_urid("http://lv2plug.in/ns/ext/atom#Float"),
                    value: std::ptr::null(),
                },
                lv2_sys::LV2_Options_Option {
                    context: lv2_sys::LV2_Options_Context_LV2_OPTIONS_INSTANCE,
                    subject: 0,
                    key: global_urid("http://lv2plug.in/ns/ext/buf-size#maxBlockLength"),
                    size: std::mem::size_of::<i32>() as u32,
                    type_: global_urid("http://lv2plug.in/ns/ext/atom#Int"),
                    value: std::ptr::null(),
                },
                // SAFETY: zero is a valid value for every field of the FFI option struct.
                unsafe { std::mem::zeroed() },
            ],
        });
        let this: *const Self = &*options;
        // SAFETY: `this` points at a live boxed value that is not moved afterwards.
        unsafe {
            options.values[0].value = std::ptr::addr_of!((*this).sample_rate).cast();
            options.values[1].value = std::ptr::addr_of!((*this).block_length).cast();
        }
        options
    }

    fn as_mut_ptr(&mut self) -> *mut lv2_sys::LV2_Options_Option {
        self.values.as_mut_ptr()
    }
}

pub(crate) struct Lv2Backend {
    // Rust drops struct fields in declaration order. The plug-in instance must be dropped first,
    // while its port buffers, options, URID map, and lilv world/plugin are still alive.
    instance: std::cell::UnsafeCell<lilv::instance::ActiveInstance>,

    // UI state: the library must outlive the UI handle that was loaded from it.
    has_ui: bool,
    ui_binary: Option<std::path::PathBuf>,
    ui_bundle: Option<String>,
    ui_lib: std::cell::UnsafeCell<Option<Library>>,
    ui_handle: std::cell::UnsafeCell<*mut std::os::raw::c_void>,
    ui_widget: std::cell::UnsafeCell<usize>,
    ui_desc: std::cell::UnsafeCell<*const LV2UI_Descriptor>,
    ui_idle: std::cell::UnsafeCell<Option<unsafe extern "C" fn(*mut std::os::raw::c_void) -> i32>>,
    ui_size: std::cell::UnsafeCell<(u32, u32)>,
    ui_resize_pending: AtomicBool,
    ui_resize_feature: std::cell::UnsafeCell<Option<Box<lv2_sys::LV2UI_Resize>>>,

    port_values: std::cell::UnsafeCell<Vec<f32>>,
    audio_in_ports: Vec<usize>,
    audio_out_ports: Vec<usize>,
    atom_in_ports: Vec<usize>,
    // One aligned buffer per atom port, input ports first, then output ports.
    atom_buffers: std::cell::UnsafeCell<Vec<Vec<u64>>>,
    in_buf: std::cell::UnsafeCell<Vec<Vec<f32>>>,
    out_buf: std::cell::UnsafeCell<Vec<Vec<f32>>>,
    block: usize,
    options: Box<Lv2Options>,
    _urid_map: std::mem::ManuallyDrop<Box<LV2UridMap>>,

    plugin_uri: String,
    params: Vec<ParamInfo>,
    control_ports: Vec<usize>,
    ranges: Vec<(f32, f32)>,
    #[allow(dead_code)]
    plugin: lilv::plugin::Plugin,
}

unsafe impl Send for Lv2Backend {}
unsafe impl Sync for Lv2Backend {}

pub(crate) fn scan(path: &std::path::Path) -> Result<Vec<PluginInfo>, Error> {
    let world = shared_world();
    let bundle_str = path.to_str().unwrap_or("");
    let mut out = Vec::new();
    for plugin in world.plugins().iter() {
        if !plugin.verify() {
            continue;
        }
        let bu = plugin.bundle_uri();
        let bu_str = bu.as_uri().unwrap_or("");
        // Match by decoded path or raw substring (handle %20).
        let matches = bu_str.contains(bundle_str)
            || bu_str.replace("%20", " ").contains(bundle_str)
            || (bu.path().map(|(_, p)| p).unwrap_or_default()).contains(bundle_str);
        if !matches {
            continue;
        }
        let uri = plugin.uri();
        let id = uri
            .as_uri()
            .or_else(|| uri.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let name = plugin
            .name()
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| id.clone());
        let vendor = plugin
            .author_name()
            .and_then(|n| n.as_str().map(str::to_string))
            .unwrap_or_default();
        out.push(PluginInfo {
            format: Format::Lv2,
            id,
            name,
            vendor,
            path: path.to_path_buf(),
        });
    }
    Ok(out)
}

pub(crate) fn load(
    _path: &std::path::Path,
    id: &str,
    rate: f64,
    block: usize,
) -> Result<Lv2Backend, Error> {
    let world = shared_world();
    let uri_node = world.new_uri(id);
    let plugin = world
        .plugins()
        .plugin(&uri_node)
        .ok_or_else(|| Error::NotAPlugin("plugin not found".into()))?;

    let audio_class = world.new_uri("http://lv2plug.in/ns/lv2core#AudioPort");
    let atom_class = world.new_uri("http://lv2plug.in/ns/ext/atom#AtomPort");
    let control_class = world.new_uri("http://lv2plug.in/ns/lv2core#ControlPort");
    let input_class = world.new_uri("http://lv2plug.in/ns/lv2core#InputPort");
    let output_class = world.new_uri("http://lv2plug.in/ns/lv2core#OutputPort");

    let mut audio_in_ports = Vec::new();
    let mut audio_out_ports = Vec::new();
    let mut atom_in_ports = Vec::new();
    let mut atom_out_ports = Vec::new();
    let mut control_ports = Vec::new();
    for i in 0..plugin.ports_count() {
        let Some(port) = plugin.port_by_index(i) else {
            continue;
        };
        let is_audio = port.is_a(&audio_class);
        let is_atom = port.is_a(&atom_class);
        let is_control = port.is_a(&control_class);
        let is_input = port.is_a(&input_class);
        let is_output = port.is_a(&output_class);
        if is_audio {
            if is_input {
                audio_in_ports.push(i);
            } else if is_output {
                audio_out_ports.push(i);
            }
        } else if is_atom {
            if is_input {
                atom_in_ports.push(i);
            } else if is_output {
                atom_out_ports.push(i);
            }
        } else if is_control {
            control_ports.push(i);
        }
    }
    if audio_in_ports.is_empty() || audio_out_ports.is_empty() {
        return Err(Error::NotAnEffect);
    }
    // Connect every audio port. Several plug-ins (for example Calf Vocoder) dereference all of
    // their inputs unconditionally, so leaving extra ports at nullptr crashes in `run()`. Only
    // the first two channels are audible; the rest are fed silence and their output is discarded.

    // Ranges + defaults + names for control ports.
    let mut params = Vec::new();
    let mut ranges = Vec::new();
    let _port_ranges = plugin.port_ranges_float();
    for &pi in &control_ports {
        let Some(port) = plugin.port_by_index(pi) else {
            continue;
        };
        let range = port.range();
        let min = numeric_node(range.minimum.as_ref()).unwrap_or(0.0);
        let max = numeric_node(range.maximum.as_ref()).unwrap_or(1.0);
        let def = numeric_node(range.default.as_ref()).unwrap_or(min);
        let name = port
            .name()
            .and_then(|n| n.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("Param {}", pi));
        let norm_def = if max > min {
            ((def - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.5
        };
        params.push(ParamInfo {
            name,
            default: norm_def,
        });
        ranges.push((min, max));
    }

    let plugin_uri = id.to_string();

    // URID map feature. The map callback is process-wide, so a null handle is sufficient.
    let mut urid_map = Box::new(LV2UridMap {
        handle: std::ptr::null_mut(),
        map: urid_map_fn,
    });
    let feature = LV2Feature {
        uri: c"http://lv2plug.in/ns/ext/urid#map".as_ptr(),
        data: urid_map.as_mut() as *mut LV2UridMap as *mut std::os::raw::c_void,
    };

    // `opts:options` carries a zero-terminated array, not an options interface. JUCE-based
    // plug-ins read `bufs:maxBlockLength` from it while instantiating.
    let mut options = Lv2Options::new(rate, block);
    let opts_feature = LV2Feature {
        uri: c"http://lv2plug.in/ns/ext/options#options".as_ptr(),
        data: options.as_mut_ptr() as *mut std::os::raw::c_void,
    };
    // boundedBlockLength feature (no data).
    let bounded_feature = LV2Feature {
        uri: c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength".as_ptr(),
        data: std::ptr::null_mut(),
    };

    // Instantiate.
    let mut instance =
        unsafe { plugin.instantiate(rate, [&feature, &opts_feature, &bounded_feature]) }
            .ok_or(Error::Refused("instantiate"))?;
    let _ = &mut instance;

    // Port values array (one per port).
    let mut port_values = vec![0.0f32; plugin.ports_count()];
    // Set control defaults.
    for (idx, &pi) in control_ports.iter().enumerate() {
        let (min, max) = ranges[idx];
        let def_norm = params[idx].default;
        port_values[pi] = min + def_norm * (max - min);
    }

    // Buffers.
    let in_buf: Vec<Vec<f32>> = audio_in_ports.iter().map(|_| vec![0.0f32; block]).collect();
    let mut out_buf: Vec<Vec<f32>> = audio_out_ports
        .iter()
        .map(|_| vec![0.0f32; block])
        .collect();

    // Atom sequence buffers. JUCE's LV2 wrapper dereferences its sequence output on every
    // `run()`, so leaving that port unconnected crashes the plug-in with a null pointer.
    let mut atom_buffers: Vec<Vec<u64>> = (0..atom_in_ports.len() + atom_out_ports.len())
        .map(|_| vec![0u64; ATOM_BUFFER_BYTES / std::mem::size_of::<u64>()])
        .collect();
    for (i, buffer) in atom_buffers.iter_mut().enumerate() {
        init_atom_sequence(buffer, i >= atom_in_ports.len());
    }

    // Connect ports.
    unsafe {
        // audio in
        for (k, &pi) in audio_in_ports.iter().enumerate() {
            instance.connect_port(pi, in_buf[k].as_ptr());
        }
        // audio out
        for (k, &pi) in audio_out_ports.iter().enumerate() {
            instance.connect_port_mut(pi, out_buf[k].as_mut_ptr());
        }
        // atom in
        for (k, &pi) in atom_in_ports.iter().enumerate() {
            instance.connect_port(pi, atom_buffers[k].as_ptr() as *const std::os::raw::c_void);
        }
        // atom out
        for (k, &pi) in atom_out_ports.iter().enumerate() {
            let buffer = &mut atom_buffers[atom_in_ports.len() + k];
            instance.connect_port_mut(pi, buffer.as_mut_ptr() as *mut std::os::raw::c_void);
        }
        // control
        for &pi in &control_ports {
            instance.connect_port_mut(pi, &mut port_values[pi] as *mut f32);
        }
    }

    let active = unsafe { instance.activate() };

    // UI discovery via lilv.
    let mut has_ui = false;
    let mut ui_binary: Option<std::path::PathBuf> = None;
    let mut ui_bundle: Option<String> = None;
    if let Some(uis) = plugin.uis() {
        for ui in uis.iter() {
            let is_x11 = {
                let x11_uri = world.new_uri("http://lv2plug.in/ns/extensions/ui#X11UI");
                ui.is_a(&x11_uri)
            };
            if is_x11 {
                has_ui = true;
                if let Some(bu) = ui.bundle_uri() {
                    if let Some((_, p)) = bu.path() {
                        ui_bundle = Some(p);
                    }
                }
                if let Some(bi) = ui.binary_uri() {
                    if let Some((_, p)) = bi.path() {
                        ui_binary = Some(std::path::PathBuf::from(p));
                    }
                }
                break;
            }
        }
    }

    Ok(Lv2Backend {
        plugin,
        instance: std::cell::UnsafeCell::new(active),
        params: params.clone(),
        control_ports,
        ranges,
        port_values: std::cell::UnsafeCell::new(port_values),
        audio_in_ports,
        audio_out_ports,
        atom_in_ports,
        atom_buffers: std::cell::UnsafeCell::new(atom_buffers),
        in_buf: std::cell::UnsafeCell::new(in_buf),
        out_buf: std::cell::UnsafeCell::new(out_buf),
        block,
        options,
        _urid_map: std::mem::ManuallyDrop::new(urid_map),
        plugin_uri,
        has_ui,
        ui_binary,
        ui_bundle,
        ui_lib: std::cell::UnsafeCell::new(None),
        ui_handle: std::cell::UnsafeCell::new(std::ptr::null_mut()),
        ui_widget: std::cell::UnsafeCell::new(0),
        ui_desc: std::cell::UnsafeCell::new(std::ptr::null()),
        ui_idle: std::cell::UnsafeCell::new(None),
        ui_size: std::cell::UnsafeCell::new((480, 320)),
        ui_resize_pending: AtomicBool::new(false),
        ui_resize_feature: std::cell::UnsafeCell::new(None),
    })
}

impl Backend for Lv2Backend {
    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    fn main_param(&self, index: usize) -> f32 {
        let Some(&pi) = self.control_ports.get(index) else {
            return 0.0;
        };
        let pv = unsafe { &*self.port_values.get() };
        let val = pv[pi];
        let _ = pi;
        let (min, max) = self.ranges[index];
        let range = max - min;
        if range <= 0.0 {
            return 0.5;
        }
        ((val - min) / range).clamp(0.0, 1.0)
    }

    fn main_set_param(&self, index: usize, norm: f32) {
        let Some(&pi) = self.control_ports.get(index) else {
            return;
        };
        let (min, max) = self.ranges[index];
        // port_values is behind &self; mutate via unsafe pointer.
        let ptr = self.port_values.get() as *mut f32;
        unsafe {
            *ptr.add(pi) = min + norm.clamp(0.0, 1.0) * (max - min);
        }
    }

    fn main_text(&self, index: usize, norm: f32) -> String {
        let Some(&_pi) = self.control_ports.get(index) else {
            return String::new();
        };
        let (min, max) = self.ranges[index];
        let val = min + norm.clamp(0.0, 1.0) * (max - min);
        format!("{val:.2}")
    }

    fn main_latency(&self) -> usize {
        0
    }

    fn main_save(&self) -> Option<Vec<u8>> {
        None
    }

    fn main_load(&self, _state: &[u8]) -> bool {
        false
    }

    fn process(&self, left: &mut [f32], right: &mut [f32], changes: &[(u32, f32)]) {
        let frames = left.len().min(right.len()).min(self.block);
        if frames == 0 {
            return;
        }
        // Apply changes.
        for (idx, norm) in changes {
            let i = *idx as usize;
            if i >= self.control_ports.len() {
                continue;
            }
            let pi = self.control_ports[i];
            let (min, max) = self.ranges[i];
            let ptr = self.port_values.get() as *mut f32;
            unsafe {
                *ptr.add(pi) = min + norm.clamp(0.0, 1.0) * (max - min);
            }
        }
        // Copy input. Extra audio inputs are connected but silent; the host's two-channel API
        // has nothing to feed them.
        let in_buf = unsafe { &mut *self.in_buf.get() };
        let out_buf = unsafe { &mut *self.out_buf.get() };
        for buffer in in_buf.iter_mut() {
            buffer[..frames].fill(0.0);
        }
        if !self.audio_in_ports.is_empty() {
            in_buf[0][..frames].copy_from_slice(&left[..frames]);
        }
        if self.audio_in_ports.len() >= 2 {
            in_buf[1][..frames].copy_from_slice(&right[..frames]);
        }
        // Reset atom sequence headers each block: output buffers advertise free space and input
        // buffers carry no messages from Pro Wave yet.
        let atom_buffers = unsafe { &mut *self.atom_buffers.get() };
        for (i, buffer) in atom_buffers.iter_mut().enumerate() {
            init_atom_sequence(buffer, i >= self.atom_in_ports.len());
        }
        // Run.
        let inst = unsafe { &mut *self.instance.get() };
        unsafe {
            inst.run(frames);
        }
        // Copy output.
        if !self.audio_out_ports.is_empty() {
            left[..frames].copy_from_slice(&out_buf[0][..frames]);
        }
        if self.audio_out_ports.len() >= 2 {
            right[..frames].copy_from_slice(&out_buf[1][..frames]);
        } else if !self.audio_out_ports.is_empty() {
            right[..frames].copy_from_slice(&out_buf[0][..frames]);
        }
    }

    fn reset(&self) {}

    fn main_has_editor(&self) -> bool {
        self.has_ui && self.ui_binary.is_some()
    }

    fn main_editor_open(&self, parent: EditorParent) -> Option<(u32, u32)> {
        let (display, parent_window) = match parent {
            EditorParent::X11 { _display, window } => (_display, window as usize),
            #[cfg(target_os = "macos")]
            _ => return None,
        };
        let binary = self.ui_binary.as_ref()?;
        // SAFETY: the path is a shared object; libloading validates it can be opened.
        let lib = unsafe { Library::new(binary) }.ok()?;
        let desc_fn: libloading::Symbol<unsafe extern "C" fn(usize) -> *const LV2UI_Descriptor> =
            unsafe { lib.get(b"lv2ui_descriptor") }.ok()?;
        let desc_ptr = unsafe { desc_fn(0) };
        if desc_ptr.is_null() {
            return None;
        }
        let instantiate = unsafe { (*desc_ptr).instantiate? };

        let urid_map_ptr = &**self._urid_map as *const LV2UridMap as *mut LV2UridMap;
        let feat_urid = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/ext/urid#map".as_ptr(),
            data: urid_map_ptr as *mut std::os::raw::c_void,
        };
        let feat_parent = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/extensions/ui#parent".as_ptr(),
            data: parent_window as *mut std::os::raw::c_void,
        };
        // instance-access gives the UI the LV2_Handle of the audio instance, like Logic's
        // plug-in UI talking directly to its DSP state.
        let instance_handle = unsafe { (&*self.instance.get()).instance().handle() };
        let feat_instance = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/ext/instance-access".as_ptr(),
            data: instance_handle as *mut std::os::raw::c_void,
        };
        // The host promises to call the UI's idle interface on its main thread.
        let feat_idle = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/extensions/ui#idleInterface".as_ptr(),
            data: std::ptr::null_mut(),
        };
        // The UI may advertise its size through this host feature. Keep the feature alive for as
        // long as the UI is instantiated.
        let mut resize_feature = Box::new(lv2_sys::LV2UI_Resize {
            handle: self as *const Self as *mut std::os::raw::c_void,
            ui_resize: Some(host_ui_resize),
        });
        let resize_ptr =
            resize_feature.as_mut() as *mut lv2_sys::LV2UI_Resize as *mut std::os::raw::c_void;
        let feat_resize = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/extensions/ui#resize".as_ptr(),
            data: resize_ptr,
        };
        let feat_options = lv2_sys::LV2_Feature {
            URI: c"http://lv2plug.in/ns/ext/options#options".as_ptr(),
            data: self.options.values.as_ptr() as *mut std::os::raw::c_void,
        };
        let features: [*const lv2_sys::LV2_Feature; 7] = [
            &feat_urid,
            &feat_parent,
            &feat_instance,
            &feat_idle,
            &feat_resize,
            &feat_options,
            std::ptr::null(),
        ];

        let plugin_uri = CString::new(self.plugin_uri.clone()).ok()?;
        let bundle = self.ui_bundle.clone().unwrap_or_else(|| "/".to_string());
        let bundle = CString::new(format!("{}/", bundle.trim_end_matches('/'))).ok()?;

        let mut widget: *mut std::os::raw::c_void = std::ptr::null_mut();
        let ui_handle = unsafe {
            instantiate(
                desc_ptr,
                plugin_uri.as_ptr(),
                bundle.as_ptr(),
                Some(ui_write),
                self as *const Self as *mut std::os::raw::c_void,
                &mut widget,
                features.as_ptr(),
            )
        };
        if ui_handle.is_null() {
            return None;
        }

        // XEmbed is handled by x11::Window, which sees the plug-in create its child under the
        // parent view and sends EMBEDDED_NOTIFY from the same display connection on every pump.
        // Doing it here too races that setup and leaves the view unmapped.
        let mut size = unsafe { *self.ui_size.get() };
        let requested = self.ui_resize_pending.swap(false, Ordering::AcqRel);
        if !requested {
            if let Ok(xlib) = Xlib::open() {
                if let Some(geometry) =
                    widget_geometry(&xlib, display as *mut xlib::Display, widget as usize)
                {
                    size = geometry;
                }
            }
        }
        unsafe {
            if let Some(ext_fn) = (*desc_ptr).extension_data {
                let idle_ptr = ext_fn(c"http://lv2plug.in/ns/extensions/ui#idleInterface".as_ptr());
                if !idle_ptr.is_null() {
                    let idle = &*(idle_ptr as *const lv2_sys::LV2UI_Idle_Interface);
                    *self.ui_idle.get() = idle.idle;
                }
            }
            *self.ui_size.get() = size;
            *self.ui_lib.get() = Some(lib);
            *self.ui_handle.get() = ui_handle;
            *self.ui_desc.get() = desc_ptr;
            *self.ui_widget.get() = widget as usize;
            *self.ui_resize_feature.get() = Some(resize_feature);
        }
        Some(size)
    }

    fn main_editor_close(&self) {
        unsafe {
            let handle = *self.ui_handle.get();
            if !handle.is_null() {
                let desc = *self.ui_desc.get();
                if !desc.is_null() {
                    if let Some(cleanup) = (*desc).cleanup {
                        cleanup(handle);
                    }
                }
            }
            *self.ui_handle.get() = std::ptr::null_mut();
            *self.ui_desc.get() = std::ptr::null();
            *self.ui_widget.get() = 0;
            *self.ui_lib.get() = None;
            *self.ui_idle.get() = None;
            *self.ui_resize_feature.get() = None;
        }
        self.ui_resize_pending.store(false, Ordering::Release);
    }

    fn main_idle(&self) {
        unsafe {
            if let Some(idle) = *self.ui_idle.get() {
                let handle = *self.ui_handle.get();
                if !handle.is_null() {
                    let _ = idle(handle);
                }
            }
        }
    }

    fn main_editor_resized(&self) -> Option<(u32, u32)> {
        if self.ui_resize_pending.swap(false, Ordering::AcqRel) {
            Some(unsafe { *self.ui_size.get() })
        } else {
            None
        }
    }
}

/// Reads the size of the X11 window the UI created, used when the UI does not request one.
fn widget_geometry(xlib: &Xlib, display: *mut xlib::Display, widget: usize) -> Option<(u32, u32)> {
    if display.is_null() || widget == 0 {
        return None;
    }
    let mut root: xlib::Window = 0;
    let mut x = 0;
    let mut y = 0;
    let mut width = 0;
    let mut height = 0;
    let mut border = 0;
    let mut depth = 0;
    // SAFETY: the display belongs to the host window and `widget` is the X11 window returned by
    // the UI; all out parameters are valid for the duration of the call.
    let ok = unsafe {
        (xlib.XGetGeometry)(
            display,
            widget as xlib::Window,
            &mut root,
            &mut x,
            &mut y,
            &mut width,
            &mut height,
            &mut border,
            &mut depth,
        )
    };
    (ok != 0 && width > 0 && height > 0).then_some((width, height))
}

unsafe extern "C" fn host_ui_resize(
    handle: *mut std::os::raw::c_void,
    width: i32,
    height: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let backend = handle as *mut Lv2Backend;
    if width > 0 && height > 0 {
        unsafe {
            *(*backend).ui_size.get() = (width as u32, height as u32);
        }
        (*backend).ui_resize_pending.store(true, Ordering::Release);
    }
    0
}

unsafe extern "C" fn ui_write(
    controller: *mut std::os::raw::c_void,
    port_index: u32,
    buffer_size: u32,
    port_protocol: u32,
    buffer: *const std::os::raw::c_void,
) {
    if controller.is_null() || buffer.is_null() || buffer_size < 4 {
        return;
    }
    if port_protocol != 0 {
        return;
    }
    let backend = controller as *mut Lv2Backend;
    let val = *(buffer as *const f32);
    let pv = (*backend).port_values.get();
    let pv = &mut *pv;
    if (port_index as usize) < pv.len() {
        pv[port_index as usize] = val;
    }
}

impl Drop for Lv2Backend {
    fn drop(&mut self) {
        self.main_editor_close();
        // Instance drops automatically (it is the first field); nothing extra needed.
        let _ = &mut self.instance;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_node_reads_float_and_int_literals() {
        let world = World::new();
        let float = world.new_float(0.5);
        let int = world.new_int(4);
        let string = world.new_string("not a number");
        assert_eq!(numeric_node(Some(&float)), Some(0.5));
        assert_eq!(numeric_node(Some(&int)), Some(4.0));
        assert_eq!(numeric_node(Some(&string)), None);
        assert_eq!(numeric_node(None), None);
    }
}
