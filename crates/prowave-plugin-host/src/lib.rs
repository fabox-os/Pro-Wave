//! Third-party plug-in hosting: CLAP, VST3 and (on macOS) Audio Units, as audio effects.
//!
//! Logic Pro hosts Audio Units in a channel strip's Audio FX slots and lists them in its
//! Plug-in Manager. This crate is that for Pro Wave: [`scan_file`] and [`scan_audio_units`] list
//! what is installed, [`load`] makes an [`Instance`] of one, and the instance's [`Processor`] is
//! what the audio thread runs, one block at a time.
//!
//! The three formats are C interfaces, written out here by hand (no SDK is linked): `clap.rs`,
//! `vst3.rs` and `au.rs`. Each is a `Backend`; the rest of the crate does not know which.
//!
//! A plug-in is native code, so every `unsafe` block here calls into it or reads what it
//! returned, and has a SAFETY comment. [`load`] runs the plug-in inside the calling process,
//! where a crash takes that process down. [`load_sandboxed`] gives it a process of its own
//! (`sandbox.rs`): the sound crosses in shared memory, and a plug-in that crashes or hangs
//! while playing leaves the sound passing through its slot. That process also shows the
//! plug-in's own window (`mac.rs`). The app scans in a child process too
//! (`prowave --scan-plugin`), since a scan runs every plug-in once.
//!
//! Threads: an [`Instance`] belongs to the main (UI) thread, a [`Processor`] to the audio
//! thread. A parameter moved on the main thread travels to the audio thread through a lock-free
//! queue and reaches the plug-in at the start of the next block.
//!
//! Not done: instruments (a plug-in must take and give audio), side-chain inputs (they get
//! silence), surround; a process of its own on Windows ([`load_sandboxed`] loads in-process
//! there); the plug-ins' own windows on Windows and Linux.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
mod au;
mod clap;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(unix)]
mod sandbox;
mod vst3;

/// The most parameter moves handed to a plug-in in one block; later ones wait for the next.
const MAX_CHANGES: usize = 128;
/// The parameter moves that can wait between the main thread and the audio thread.
const QUEUE: usize = 1024;
/// The most parameters read from one plug-in.
pub const MAX_PARAMS: usize = 4096;

/// A plug-in format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Clap,
    Vst3,
    AudioUnit,
}

impl Format {
    /// The name the Plug-in Manager shows.
    pub fn name(self) -> &'static str {
        match self {
            Format::Clap => "CLAP",
            Format::Vst3 => "VST3",
            Format::AudioUnit => "Audio Unit",
        }
    }

    /// The format for a name written by [`Format::name`].
    pub fn from_name(name: &str) -> Option<Format> {
        [Format::Clap, Format::Vst3, Format::AudioUnit]
            .into_iter()
            .find(|f| f.name() == name)
    }
}

/// One installed plug-in, as a scan found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    pub format: Format,
    /// What names the plug-in inside its file: a CLAP id, a VST3 class id in hexadecimal, or an
    /// Audio Unit's `type:subtype:manufacturer` codes.
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// The plug-in's file or bundle (empty for an Audio Unit, which the system finds).
    pub path: PathBuf,
}

/// One parameter of a loaded plug-in.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    pub name: String,
    /// The default position along the range, 0..=1.
    pub default: f32,
}

/// Why a plug-in could not be scanned or loaded.
#[derive(Debug)]
pub enum Error {
    /// The file is not there or is not a plug-in of its format.
    NotAPlugin(String),
    /// The plug-in refused a step of its start (the step is named).
    Refused(&'static str),
    /// The plug-in has no stereo or mono audio input and output (an instrument, a MIDI effect).
    NotAnEffect,
    /// The format does not exist on this system (an Audio Unit off macOS).
    Unsupported,
    /// The plug-in's own process did not start or did not answer (the reason is given).
    Process(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAPlugin(why) => write!(f, "not a plug-in: {why}"),
            Error::Refused(step) => write!(f, "the plug-in refused to {step}"),
            Error::NotAnEffect => write!(f, "the plug-in is not an audio effect"),
            Error::Unsupported => write!(f, "this plug-in format is not available here"),
            Error::Process(why) => write!(f, "the plug-in's process failed: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// One format's side of a loaded plug-in. The `main_` methods are called on the main thread
/// only, `process` and `reset` on the audio thread only; the two may run at the same time, as
/// each format allows.
// The window methods are used where plug-in windows are hosted (macOS, for now).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) trait Backend {
    fn params(&self) -> Vec<ParamInfo>;
    /// The position (0..=1) of parameter `index` now.
    fn main_param(&self, index: usize) -> f32;
    /// Tells the plug-in's main-thread side about a move (the audio side gets it in `process`).
    fn main_set_param(&self, _index: usize, _norm: f32) {}
    /// How the plug-in writes parameter `index` at position `norm` ("-3.0 dB").
    fn main_text(&self, index: usize, norm: f32) -> String;
    fn main_latency(&self) -> usize;
    fn main_save(&self) -> Option<Vec<u8>>;
    fn main_load(&self, state: &[u8]) -> bool;
    /// Work the plug-in asked to do on the main thread.
    fn main_idle(&self) {}
    /// One block in place, after applying `changes` (parameter index, position).
    fn process(&self, left: &mut [f32], right: &mut [f32], changes: &[(u32, f32)]);
    fn reset(&self);
    /// Whether the plug-in has a window of its own.
    fn main_has_editor(&self) -> bool {
        false
    }
    /// Puts the plug-in's own view inside `parent` (macOS: an `NSView`) and returns its size
    /// in points, or `None` if it has no view or refused.
    fn main_editor_open(&self, _parent: *mut std::ffi::c_void) -> Option<(u32, u32)> {
        None
    }
    fn main_editor_close(&self) {}
    /// The size the plug-in asked its window to take since the last call, if it asked.
    fn main_editor_resized(&self) -> Option<(u32, u32)> {
        None
    }
}

/// What the audio thread keeps for one plug-in.
struct AudioSide {
    moves: rtrb::Consumer<(u32, f32)>,
    changes: [(u32, f32); MAX_CHANGES],
    /// Moves made on the audio thread itself (automation), waiting for the next block.
    pending: usize,
}

struct Shared {
    backend: Box<dyn Backend>,
    info: PluginInfo,
    params: Vec<ParamInfo>,
    /// The positions as the main thread last knew them.
    values: Mutex<Vec<f32>>,
    to_audio: Mutex<rtrb::Producer<(u32, f32)>>,
    audio: UnsafeCell<AudioSide>,
    latency: AtomicUsize,
}

// SAFETY: `backend` is used from two threads the way its format allows (see `Backend`), and
// `audio` only by the one `Processor` being played: the engine plays one handle at a time, and
// `Processor::process` takes `&mut self`.
unsafe impl Send for Shared {}
// SAFETY: as above; everything else in `Shared` is a `Mutex`, an atomic or immutable.
unsafe impl Sync for Shared {}

/// A loaded plug-in, for the main thread: its parameters, its state, its latency.
#[derive(Clone)]
pub(crate) struct LocalInstance(Arc<Shared>);

/// A loaded plug-in, for the audio thread. Cloning gives another handle on the same plug-in;
/// only one may be played at a time.
#[derive(Clone)]
pub(crate) struct LocalProcessor(Arc<Shared>);

impl LocalInstance {
    fn new(backend: Box<dyn Backend>, info: PluginInfo) -> Self {
        let params = backend.params();
        let values = (0..params.len()).map(|i| backend.main_param(i)).collect();
        let (tx, rx) = rtrb::RingBuffer::new(QUEUE);
        let latency = backend.main_latency();
        Self(Arc::new(Shared {
            backend,
            info,
            params,
            values: Mutex::new(values),
            to_audio: Mutex::new(tx),
            audio: UnsafeCell::new(AudioSide {
                moves: rx,
                changes: [(0, 0.0); MAX_CHANGES],
                pending: 0,
            }),
            latency: AtomicUsize::new(latency),
        }))
    }

    pub fn info(&self) -> &PluginInfo {
        &self.0.info
    }

    /// The parameters, in the plug-in's order.
    pub fn params(&self) -> &[ParamInfo] {
        &self.0.params
    }

    /// The position (0..=1) of parameter `index` (0 when there is none).
    pub fn param(&self, index: usize) -> f32 {
        self.0
            .values
            .lock()
            .ok()
            .and_then(|v| v.get(index).copied())
            .unwrap_or(0.0)
    }

    /// Moves parameter `index` to position `norm` (0..=1). The sound follows at the next block.
    pub fn set_param(&self, index: usize, norm: f32) {
        if index >= self.0.params.len() || !norm.is_finite() {
            return;
        }
        let norm = norm.clamp(0.0, 1.0);
        if let Ok(mut values) = self.0.values.lock() {
            values[index] = norm;
        }
        self.0.backend.main_set_param(index, norm);
        if let Ok(mut tx) = self.0.to_audio.lock() {
            // A full queue drops the move: the audio thread is not running, and the next move
            // of the same knob will carry the value.
            let _ = tx.push((index as u32, norm));
        }
    }

    /// How the plug-in writes parameter `index` at its position now.
    pub fn param_text(&self, index: usize) -> String {
        if index >= self.0.params.len() {
            return String::new();
        }
        self.0.backend.main_text(index, self.param(index))
    }

    /// The plug-in's own delay in samples.
    pub fn latency_samples(&self) -> usize {
        self.0.latency.load(Ordering::Relaxed)
    }

    /// Everything the plug-in wants in a project file, or `None` if it has no state to give.
    pub fn save_state(&self) -> Option<Vec<u8>> {
        self.0.backend.main_save()
    }

    /// Puts back a state made by [`Instance::save_state`]. Returns whether the plug-in took it.
    pub fn load_state(&self, state: &[u8]) -> bool {
        let ok = self.0.backend.main_load(state);
        if let Ok(mut values) = self.0.values.lock() {
            for (i, v) in values.iter_mut().enumerate() {
                *v = self.0.backend.main_param(i);
            }
        }
        self.0
            .latency
            .store(self.0.backend.main_latency(), Ordering::Relaxed);
        ok
    }

    /// Call often on the main thread (once a frame): the plug-in's main-thread work.
    pub fn idle(&self) {
        self.0.backend.main_idle();
        self.0
            .latency
            .store(self.0.backend.main_latency(), Ordering::Relaxed);
    }

    /// The parameters whose position is not what the main thread last knew (the plug-in's own
    /// window moved them, a state was loaded, automation played), with the new position.
    /// Parameters for which `skip` is true are left for a later call.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn refresh(&self, skip: impl Fn(usize) -> bool) -> Vec<(usize, f32)> {
        let mut moved = Vec::new();
        if let Ok(mut values) = self.0.values.lock() {
            for (i, known) in values.iter_mut().enumerate() {
                if skip(i) {
                    continue;
                }
                let now = self.0.backend.main_param(i);
                if (now - *known).abs() > 1e-6 {
                    *known = now;
                    moved.push((i, now));
                }
            }
        }
        moved
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn backend(&self) -> &dyn Backend {
        &*self.0.backend
    }

    /// The handle the audio thread plays.
    pub fn processor(&self) -> LocalProcessor {
        LocalProcessor(Arc::clone(&self.0))
    }
}

impl LocalProcessor {
    fn audio(&mut self) -> &mut AudioSide {
        // SAFETY: only the audio thread reaches `audio`, through the one handle being played
        // (see `Shared`), and `&mut self` keeps this handle to one caller.
        unsafe { &mut *self.0.audio.get() }
    }

    /// One block in place. `left` and `right` must have the same length, at most the block
    /// size given to [`load`]. No allocation, no lock.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let shared = Arc::as_ptr(&self.0);
        let audio = self.audio();
        let mut n = audio.pending;
        while n < MAX_CHANGES {
            match audio.moves.pop() {
                Ok(change) => {
                    audio.changes[n] = change;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        audio.pending = 0;
        let frames = left.len().min(right.len());
        // SAFETY: `shared` points into the `Arc` this handle holds.
        let backend = unsafe { &(*shared).backend };
        backend.process(
            &mut left[..frames],
            &mut right[..frames],
            &audio.changes[..n],
        );
    }

    /// Automation, on the audio thread: parameter `index` goes to position `norm` at the next
    /// block.
    pub fn set_param_norm(&mut self, index: usize, norm: f32) {
        if index >= self.0.params.len() || !norm.is_finite() {
            return;
        }
        let audio = self.audio();
        if audio.pending < MAX_CHANGES {
            audio.changes[audio.pending] = (index as u32, norm.clamp(0.0, 1.0));
            audio.pending += 1;
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.0.latency.load(Ordering::Relaxed)
    }

    /// Clears the plug-in's memory (tails, filter state).
    pub fn reset(&mut self) {
        self.0.backend.reset();
    }
}

#[derive(Clone)]
enum Kind {
    Local(LocalInstance),
    #[cfg(unix)]
    Remote(Arc<sandbox::Remote>),
}

/// A loaded plug-in, for the main thread: its parameters, its state, its latency, its window.
/// It runs inside Pro Wave ([`load`]) or in a process of its own ([`load_sandboxed`]).
#[derive(Clone)]
pub struct Instance(Kind);

enum ProcessorKind {
    Local(LocalProcessor),
    #[cfg(unix)]
    Remote(sandbox::RemoteProcessor),
}

/// A loaded plug-in, for the audio thread. Cloning gives another handle on the same plug-in;
/// only one may be played at a time.
pub struct Processor(ProcessorKind);

impl Clone for Processor {
    fn clone(&self) -> Self {
        Processor(match &self.0 {
            ProcessorKind::Local(p) => ProcessorKind::Local(p.clone()),
            #[cfg(unix)]
            ProcessorKind::Remote(p) => ProcessorKind::Remote(p.clone()),
        })
    }
}

macro_rules! each {
    ($self:expr, $x:ident => $body:expr) => {
        match &$self.0 {
            Kind::Local($x) => $body,
            #[cfg(unix)]
            Kind::Remote($x) => $body,
        }
    };
}

impl Instance {
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn local(&self) -> Option<&LocalInstance> {
        match &self.0 {
            Kind::Local(local) => Some(local),
            #[cfg(unix)]
            Kind::Remote(_) => None,
        }
    }

    pub fn info(&self) -> &PluginInfo {
        each!(self, x => x.info())
    }

    /// The parameters, in the plug-in's order.
    pub fn params(&self) -> &[ParamInfo] {
        each!(self, x => x.params())
    }

    /// The position (0..=1) of parameter `index` (0 when there is none).
    pub fn param(&self, index: usize) -> f32 {
        each!(self, x => x.param(index))
    }

    /// Moves parameter `index` to position `norm` (0..=1). The sound follows at the next block.
    pub fn set_param(&self, index: usize, norm: f32) {
        each!(self, x => x.set_param(index, norm))
    }

    /// How the plug-in writes parameter `index` at its position now.
    pub fn param_text(&self, index: usize) -> String {
        each!(self, x => x.param_text(index))
    }

    /// The plug-in's own delay in samples.
    pub fn latency_samples(&self) -> usize {
        each!(self, x => x.latency_samples())
    }

    /// Everything the plug-in wants in a project file, or `None` if it has no state to give.
    pub fn save_state(&self) -> Option<Vec<u8>> {
        each!(self, x => x.save_state())
    }

    /// Puts back a state made by [`Instance::save_state`]. Returns whether the plug-in took it.
    pub fn load_state(&self, state: &[u8]) -> bool {
        each!(self, x => x.load_state(state))
    }

    /// Call often on the main thread (once a frame): the plug-in's main-thread work.
    pub fn idle(&self) {
        each!(self, x => x.idle())
    }

    /// The handle the audio thread plays.
    pub fn processor(&self) -> Processor {
        Processor(match &self.0 {
            Kind::Local(x) => ProcessorKind::Local(x.processor()),
            #[cfg(unix)]
            Kind::Remote(x) => ProcessorKind::Remote(sandbox::RemoteProcessor::new(x)),
        })
    }

    /// Whether the plug-in's process ended on its own (it crashed). The sound then passes
    /// through its slot unchanged. Never true for a plug-in loaded with [`load`]: a crash
    /// there ends Pro Wave.
    pub fn crashed(&self) -> bool {
        match &self.0 {
            Kind::Local(_) => false,
            #[cfg(unix)]
            Kind::Remote(x) => x.crashed(),
        }
    }

    /// Whether the plug-in's own window can be shown (macOS, in a process of its own).
    pub fn has_editor(&self) -> bool {
        match &self.0 {
            Kind::Local(_) => false,
            #[cfg(unix)]
            Kind::Remote(x) => x.has_editor(),
        }
    }

    /// Shows the plug-in's own window, titled `title` (Logic: the channel strip's name), with
    /// the power button lit unless `bypassed`.
    pub fn open_editor(&self, title: &str, bypassed: bool) {
        match &self.0 {
            Kind::Local(_) => {}
            #[cfg(unix)]
            Kind::Remote(x) => x.open_editor(title, bypassed),
        }
        let _ = (title, bypassed);
    }

    pub fn close_editor(&self) {
        match &self.0 {
            Kind::Local(_) => {}
            #[cfg(unix)]
            Kind::Remote(x) => x.close_editor(),
        }
    }

    /// Whether the plug-in's own window is on screen.
    pub fn editor_open(&self) -> bool {
        match &self.0 {
            Kind::Local(_) => false,
            #[cfg(unix)]
            Kind::Remote(x) => x.editor_open(),
        }
    }

    /// Whether the power button of the plug-in's own window was clicked since the last call
    /// (an odd number of times): the caller toggles Bypass.
    pub fn take_power_click(&self) -> bool {
        match &self.0 {
            Kind::Local(_) => false,
            #[cfg(unix)]
            Kind::Remote(x) => x.take_power_click(),
        }
    }
}

impl Processor {
    /// One block in place. `left` and `right` must have the same length, at most the block
    /// size given at load. No allocation, no lock.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        match &mut self.0 {
            ProcessorKind::Local(p) => p.process(left, right),
            #[cfg(unix)]
            ProcessorKind::Remote(p) => p.process(left, right),
        }
    }

    /// Automation, on the audio thread: parameter `index` goes to position `norm` at the next
    /// block.
    pub fn set_param_norm(&mut self, index: usize, norm: f32) {
        match &mut self.0 {
            ProcessorKind::Local(p) => p.set_param_norm(index, norm),
            #[cfg(unix)]
            ProcessorKind::Remote(p) => p.set_param_norm(index, norm),
        }
    }

    pub fn latency_samples(&self) -> usize {
        match &self.0 {
            ProcessorKind::Local(p) => p.latency_samples(),
            #[cfg(unix)]
            ProcessorKind::Remote(p) => p.latency_samples(),
        }
    }

    /// Clears the plug-in's memory (tails, filter state).
    pub fn reset(&mut self) {
        match &mut self.0 {
            ProcessorKind::Local(p) => p.reset(),
            #[cfg(unix)]
            ProcessorKind::Remote(p) => p.reset(),
        }
    }

    /// Offline rendering (not in real time): a plug-in in its own process is waited for, block
    /// by block, instead of its late blocks passing dry. A plug-in in this process runs at the
    /// engine's own pace, so nothing changes for it.
    pub fn set_offline(&mut self, offline: bool) {
        // Only a process-isolated plug-in (Unix) has anything to wait for.
        #[cfg(not(unix))]
        let _ = offline;
        match &mut self.0 {
            ProcessorKind::Local(_) => {}
            #[cfg(unix)]
            ProcessorKind::Remote(p) => p.set_offline(offline),
        }
    }
}

/// The folders plug-ins of `format` are installed in on this system (existing or not).
pub fn folders(format: Format) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let under_home = |rest: &str| home.as_ref().map(|h| h.join(rest));
    let (dir, env) = match format {
        Format::Clap => ("CLAP", "CLAP_PATH"),
        Format::Vst3 => ("VST3", "VST3_PATH"),
        Format::AudioUnit => return Vec::new(),
    };
    let mut out: Vec<PathBuf> = std::env::var_os(env)
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        out.push(PathBuf::from(format!("/Library/Audio/Plug-Ins/{dir}")));
        out.extend(under_home(&format!("Library/Audio/Plug-Ins/{dir}")));
    } else if cfg!(windows) {
        for var in ["COMMONPROGRAMFILES", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                out.push(if var == "LOCALAPPDATA" {
                    base.join("Programs").join("Common").join(dir)
                } else {
                    base.join(dir)
                });
            }
        }
    } else {
        let lower = dir.to_ascii_lowercase();
        out.extend(under_home(&format!(".{lower}")));
        out.push(PathBuf::from(format!("/usr/lib/{lower}")));
        out.push(PathBuf::from(format!("/usr/local/lib/{lower}")));
    }
    out
}

/// The plug-in files and bundles of `format` under [`folders`] (two levels of sub-folders,
/// since vendors make their own), sorted.
pub fn installed(format: Format) -> Vec<PathBuf> {
    fn walk(dir: &Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                out.push(path);
            } else if depth > 0 && path.is_dir() {
                walk(&path, ext, depth - 1, out);
            }
        }
    }
    let ext = match format {
        Format::Clap => "clap",
        Format::Vst3 => "vst3",
        Format::AudioUnit => return Vec::new(),
    };
    let mut out = Vec::new();
    for dir in folders(format) {
        walk(&dir, ext, 2, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// The audio effects inside one CLAP or VST3 file. This loads the file and runs its code.
pub fn scan_file(format: Format, path: &Path) -> Result<Vec<PluginInfo>, Error> {
    match format {
        Format::Clap => clap::scan(path),
        Format::Vst3 => vst3::scan(path),
        Format::AudioUnit => Err(Error::Unsupported),
    }
}

/// The Audio Unit effects the system knows (none off macOS). This runs no plug-in code.
pub fn scan_audio_units() -> Vec<PluginInfo> {
    #[cfg(target_os = "macos")]
    {
        au::scan()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Loads `info` to play stereo sound at `sample_rate` in blocks of at most `max_block` samples.
/// Call on the main thread.
pub fn load(info: &PluginInfo, sample_rate: f64, max_block: usize) -> Result<Instance, Error> {
    let block = max_block.clamp(1, 8192);
    let rate = if sample_rate.is_finite() {
        sample_rate.clamp(8_000.0, 384_000.0)
    } else {
        48_000.0
    };
    let backend: Box<dyn Backend> = match info.format {
        Format::Clap => Box::new(clap::load(&info.path, &info.id, rate, block)?),
        Format::Vst3 => Box::new(vst3::load(&info.path, &info.id, rate, block)?),
        #[cfg(target_os = "macos")]
        Format::AudioUnit => Box::new(au::load(&info.id, rate, block)?),
        #[cfg(not(target_os = "macos"))]
        Format::AudioUnit => return Err(Error::Unsupported),
    };
    Ok(Instance(Kind::Local(LocalInstance::new(
        backend,
        info.clone(),
    ))))
}

/// The command-line flag of a plug-in's own process: `<program> --plugin-host <arguments>`.
pub const HOST_ARG: &str = "--plugin-host";

/// Loads `info` in a process of its own, started from `program` (the app itself, or any program
/// that calls [`serve_if_requested`] first thing). The plug-in's sound crosses in shared memory;
/// if the plug-in crashes or hangs, its slot passes the sound through and Pro Wave goes on.
/// Where a process of its own is not available (Windows, for now) this is [`load`].
pub fn load_sandboxed(
    info: &PluginInfo,
    sample_rate: f64,
    max_block: usize,
    program: &Path,
) -> Result<Instance, Error> {
    #[cfg(unix)]
    {
        let block = max_block.clamp(1, sandbox::MAX_BLOCK);
        sandbox::Remote::start(info, sample_rate, block, program)
            .map(|remote| Instance(Kind::Remote(remote)))
    }
    #[cfg(not(unix))]
    {
        let _ = program;
        load(info, sample_rate, max_block)
    }
}

/// When the arguments are those of a plug-in's own process ([`HOST_ARG`]), runs it and never
/// returns; otherwise returns at once. A program passed to [`load_sandboxed`] calls this first.
pub fn serve_if_requested(args: &[std::ffi::OsString]) {
    #[cfg(unix)]
    if let Some(at) = args.iter().position(|a| a == HOST_ARG) {
        sandbox::serve(&args[at + 1..]);
    }
    let _ = args;
}

/// The program inside a macOS bundle (`Contents/MacOS/<name>`): the one `Info.plist` names,
/// else the one named like the bundle, else the only file there.
pub(crate) fn bundle_binary(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents").join("MacOS");
    let named = std::fs::read_to_string(bundle.join("Contents").join("Info.plist"))
        .ok()
        .and_then(|plist| {
            let rest = plist.split("<key>CFBundleExecutable</key>").nth(1)?;
            let value = rest.split("<string>").nth(1)?.split("</string>").next()?;
            Some(dir.join(value.trim()))
        })
        .filter(|p| p.is_file());
    let by_stem = bundle
        .file_stem()
        .map(|s| dir.join(s))
        .filter(|p| p.is_file());
    named.or(by_stem).or_else(|| {
        let mut files = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path());
        let first = files.next()?;
        files.next().is_none().then_some(first)
    })
}

/// A C string from a plug-in as text (empty for null).
///
/// # Safety
/// `ptr` is null or points to a NUL-terminated string that lives for the call.
pub(crate) unsafe fn c_text(ptr: *const std::os::raw::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .trim()
        .to_string()
}

/// The text in a fixed C buffer, up to its first NUL.
pub(crate) fn buffer_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}
