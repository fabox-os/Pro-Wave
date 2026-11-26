// LV2 plug-in hosting on Linux, via liblilv.
#![cfg(target_os = "linux")]

use crate::{Backend, Error, Format, ParamInfo, PluginInfo};
use lilv::World;
use lv2_raw::core::LV2Feature;
use lv2_raw::urid::LV2Urid;
use lv2_raw::urid::LV2UridMap;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

struct UridState {
    map: Mutex<HashMap<String, u32>>,
    next: AtomicU32,
}

extern "C" fn urid_map_fn(handle: *mut std::os::raw::c_void, uri: *const c_char) -> LV2Urid {
    if uri.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(uri) }.to_str().unwrap_or("");
    let state = unsafe { &*(handle as *const UridState) };
    let mut m = state.map.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(v) = m.get(s) {
        return *v;
    }
    let mut v = state.next.fetch_add(1, Ordering::Relaxed);
    if v == 0 {
        v = 1;
    }
    m.insert(s.to_string(), v);
    v
}

pub(crate) struct Lv2Backend {
    #[allow(dead_code)]
    plugin: lilv::plugin::Plugin,
    #[allow(dead_code)]
    world: World,
    params: Vec<ParamInfo>,
    control_ports: Vec<usize>,
    ranges: Vec<(f32, f32)>,
    port_values: std::cell::UnsafeCell<Vec<f32>>,
    audio_in_ports: Vec<usize>,
    audio_out_ports: Vec<usize>,
    in_buf: std::cell::UnsafeCell<Vec<Vec<f32>>>,
    out_buf: std::cell::UnsafeCell<Vec<Vec<f32>>>,
    block: usize,
    _urid_state: std::mem::ManuallyDrop<Box<UridState>>,
    _urid_map: std::mem::ManuallyDrop<Box<LV2UridMap>>,
    // Manually dropped in `Drop` while world/plugin still alive (lilv_instance_free needs them).
    instance: std::mem::ManuallyDrop<std::cell::UnsafeCell<lilv::instance::ActiveInstance>>,
}

unsafe impl Send for Lv2Backend {}
unsafe impl Sync for Lv2Backend {}

pub(crate) fn scan(path: &std::path::Path) -> Result<Vec<PluginInfo>, Error> {
    let world = World::with_load_all();
    let bundle_str = path.to_str().unwrap_or("");
    let mut out = Vec::new();
    for plugin in world.plugins().iter() {
        if !plugin.verify() {
            continue;
        }
        let bu = plugin.bundle_uri();
        let bu_str = bu.as_uri().unwrap_or("");
        if !bu_str.contains(bundle_str) {
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
    let world = World::with_load_all();
    let uri_node = world.new_uri(id);
    let plugin = world
        .plugins()
        .plugin(&uri_node)
        .ok_or_else(|| Error::NotAPlugin("plugin not found".into()))?;

    let audio_class = world.new_uri("http://lv2plug.in/ns/lv2core#AudioPort");
    let control_class = world.new_uri("http://lv2plug.in/ns/lv2core#ControlPort");
    let input_class = world.new_uri("http://lv2plug.in/ns/lv2core#InputPort");
    let output_class = world.new_uri("http://lv2plug.in/ns/lv2core#OutputPort");

    let mut audio_in_ports = Vec::new();
    let mut audio_out_ports = Vec::new();
    let mut control_ports = Vec::new();
    for i in 0..plugin.ports_count() {
        let Some(port) = plugin.port_by_index(i) else {
            continue;
        };
        let is_audio = port.is_a(&audio_class);
        let is_control = port.is_a(&control_class);
        let is_input = port.is_a(&input_class);
        let is_output = port.is_a(&output_class);
        if is_audio {
            if is_input {
                audio_in_ports.push(i);
            } else if is_output {
                audio_out_ports.push(i);
            }
        } else if is_control {
            control_ports.push(i);
        }
    }
    if audio_in_ports.is_empty() || audio_out_ports.is_empty() {
        return Err(Error::NotAnEffect);
    }
    // Support up to 2 channels each.
    if audio_in_ports.len() > 2 {
        audio_in_ports.truncate(2);
    }
    if audio_out_ports.len() > 2 {
        audio_out_ports.truncate(2);
    }

    // Ranges + defaults + names for control ports.
    let mut params = Vec::new();
    let mut ranges = Vec::new();
    let _port_ranges = plugin.port_ranges_float();
    for &pi in &control_ports {
        let Some(port) = plugin.port_by_index(pi) else {
            continue;
        };
        let range = port.range();
        let min = range
            .minimum
            .as_ref()
            .and_then(|n| n.as_float())
            .unwrap_or(0.0);
        let max = range
            .maximum
            .as_ref()
            .and_then(|n| n.as_float())
            .unwrap_or(1.0);
        let def = range
            .default
            .as_ref()
            .and_then(|n| n.as_float())
            .unwrap_or(min);
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

    // URID map feature.
    let urid_state = Box::new(UridState {
        map: Mutex::new(HashMap::new()),
        next: AtomicU32::new(1),
    });
    let handle = Box::into_raw(urid_state) as *mut std::os::raw::c_void; // leak-ish but stored
                                                                         // We keep urid_state in backend; but handle points to same. Re-box carefully: store box and derive handle from it.
                                                                         // Actually we need handle before moving. Redo below.
    let _ = handle;

    // Redo urid state handling properly.
    let urid_state = Box::new(UridState {
        map: Mutex::new(HashMap::new()),
        next: AtomicU32::new(1),
    });
    let handle_ptr = (&*urid_state as *const UridState) as *mut std::os::raw::c_void;
    let mut urid_map = Box::new(LV2UridMap {
        handle: handle_ptr,
        map: urid_map_fn,
    });
    let feature_uri = c"http://lv2plug.in/ns/ext/urid#map".as_ptr();
    let feature = LV2Feature {
        uri: feature_uri,
        data: urid_map.as_mut() as *mut LV2UridMap as *mut std::os::raw::c_void,
    };

    // Instantiate.
    let mut instance = unsafe {
        plugin
            .instantiate(rate, std::iter::once(&feature))
            .ok_or(Error::Refused("instantiate"))?
    };
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
        // control
        for &pi in &control_ports {
            instance.connect_port_mut(pi, &mut port_values[pi] as *mut f32);
        }
    }

    let active = unsafe { instance.activate() };

    Ok(Lv2Backend {
        world,
        plugin,
        instance: std::mem::ManuallyDrop::new(std::cell::UnsafeCell::new(active)),
        params: params.clone(),
        control_ports,
        ranges,
        port_values: std::cell::UnsafeCell::new(port_values),
        audio_in_ports,
        audio_out_ports,
        in_buf: std::cell::UnsafeCell::new(in_buf),
        out_buf: std::cell::UnsafeCell::new(out_buf),
        block,
        _urid_state: std::mem::ManuallyDrop::new(urid_state),
        _urid_map: std::mem::ManuallyDrop::new(urid_map),
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
        // Copy input.
        let in_buf = unsafe { &mut *self.in_buf.get() };
        let out_buf = unsafe { &mut *self.out_buf.get() };
        if !self.audio_in_ports.is_empty() {
            in_buf[0][..frames].copy_from_slice(&left[..frames]);
        }
        if self.audio_in_ports.len() >= 2 {
            in_buf[1][..frames].copy_from_slice(&right[..frames]);
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
}

impl Drop for Lv2Backend {
    fn drop(&mut self) {
        // SAFETY: instance must be dropped while world/plugin still alive (they follow in drop order).
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.instance);
        }
    }
}
