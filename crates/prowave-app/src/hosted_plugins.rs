//! The third-party plug-ins of the session (Logic: Audio Units): the list of what is installed,
//! and the ones a channel strip uses.
//!
//! `prowave-plugin-host` does the hosting. This module keeps what the app needs around it:
//!
//! - The **catalog**: every installed CLAP, VST3 and Audio Unit effect, for the Audio FX picker
//!   and the Plug-in Manager. Listing a CLAP or VST3 file runs its code, so each file is scanned
//!   in a child process (`prowave --scan-plugin`): a plug-in that crashes or hangs there costs
//!   one file, not the session. The result is kept in the configuration folder and a file is
//!   scanned again only when it changes. Every start reads that cache at once, so the menus
//!   list the last run's plug-ins before the scan has looked at anything, then the scan adds
//!   the new files, redoes the changed ones and drops the removed ones.
//! - A plug-in runs in a **process of its own** (`prowave --plugin-host`, served by the host
//!   crate): one that crashes while playing leaves its slot passing the sound, and [`again`]
//!   starts it afresh with the state put aside a few seconds before.
//! - The **rack**: the plug-ins in use. An `Insert` holds a number; the rack holds the loaded
//!   plug-in behind it. A plug-in is loaded the first time the engine needs it (the engine
//!   knows the sample rate) and stays loaded while the app runs, so Undo can bring a removed
//!   plug-in back with its settings.
use prowave_engine::{BlockEffect, Fx, PLUGIN_BLOCK};
use prowave_plugin_host as host;
pub use prowave_plugin_host::{Format, Instance, PluginInfo};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The command-line flag of the scanning child process: `--scan-plugin <format> <path>`.
pub const SCAN_ARG: &str = "--scan-plugin";
/// The longest one file may take to list its plug-ins.
#[cfg(not(test))]
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One plug-in a channel strip uses (or used: see the module comment).
struct Slot {
    info: PluginInfo,
    /// The name the Mixer shows, kept for the life of the app (an `Insert` gives its name as
    /// `&'static str`, like the built-in effects).
    name: &'static str,
    instance: Option<Instance>,
    /// The sample rate `instance` was loaded at.
    rate: f32,
    /// The state from a project file, until the plug-in is loaded (and for good if it is not
    /// installed here, so saving the project keeps it).
    saved: Option<Vec<u8>>,
    /// Why the plug-in could not be loaded.
    error: Option<String>,
    /// The catalog generation (see [`Rack::generation`]) at which a missing plug-in was last
    /// found installed and tried: it is not tried again until the catalog changes.
    checked: Option<u64>,
}

/// How far the scan of the installed plug-ins is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scan {
    #[default]
    NotStarted,
    /// Files done, files in all.
    Running(usize, usize),
    Done,
}

#[derive(Default)]
struct Rack {
    slots: Vec<Slot>,
    /// The installed plug-ins the menus list, sorted (see [`Rack::rebuild`]).
    catalog: Vec<PluginInfo>,
    /// The plug-in files the catalog is built from: the cache's, then each one as it is scanned.
    files: Vec<Scanned>,
    /// The Audio Units of the system's own list, as the last scan found them.
    audio_units: Vec<PluginInfo>,
    /// Counts the changes to `catalog`: a missing plug-in is looked for again after one.
    generation: u64,
    /// Counts the slots that became loadable again: the engine rebuilds the chains that use
    /// them (see [`revision`]).
    revision: u64,
    scan: Scan,
    /// Asks for a new picture when the catalog changes, since the scan runs on another thread.
    repaint: Option<egui::Context>,
}

impl Rack {
    /// Puts the catalog together again from the Audio Units and the files, and when it changed,
    /// counts the change and asks for a new picture.
    fn rebuild(&mut self) {
        let mut catalog = self.audio_units.clone();
        catalog.extend(self.files.iter().flat_map(|f| f.plugins.iter().cloned()));
        sort(&mut catalog);
        if catalog != self.catalog {
            self.catalog = catalog;
            self.generation += 1;
            self.wake();
        }
    }

    fn wake(&self) {
        if let Some(ctx) = &self.repaint {
            ctx.request_repaint();
        }
    }

    /// Whether `slot` is in use but not installed here. Only once the scan has finished: while it
    /// still looks, a plug-in that is about to be found is not shown as missing.
    fn is_missing(&self, slot: &Slot) -> bool {
        self.scan == Scan::Done
            && slot.error.is_some()
            && installed_in(&self.catalog, &slot.info).is_none()
    }

    /// Finds again the missing plug-ins that are installed now: such a slot takes the installed
    /// entry and is loaded afresh by the engine, with the state the project gave it. A plug-in
    /// that was found but would not load is not tried again until the catalog changes, so that
    /// a broken one is not loaded at every frame.
    fn retry(&mut self) {
        let generation = self.generation;
        let mut resolved = false;
        for slot in &mut self.slots {
            if slot.instance.is_some() || slot.error.is_none() || slot.checked == Some(generation) {
                continue;
            }
            let Some(found) = installed_in(&self.catalog, &slot.info) else {
                continue;
            };
            slot.checked = Some(generation);
            slot.info = found;
            slot.error = None;
            resolved = true;
        }
        if resolved {
            self.revision += 1;
            self.wake();
        }
    }
}

fn rack() -> MutexGuard<'static, Rack> {
    static RACK: OnceLock<Mutex<Rack>> = OnceLock::new();
    RACK.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The installed plug-in that `info` names: the catalog's entry with the same format and id,
/// else (for a CLAP or VST3 file) the file itself when it is still at its path. An Audio Unit
/// is only found in the catalog: the system finds it, not a path.
fn installed_in(catalog: &[PluginInfo], info: &PluginInfo) -> Option<PluginInfo> {
    catalog
        .iter()
        .find(|c| c.format == info.format && c.id == info.id)
        .cloned()
        .or_else(|| (info.format != Format::AudioUnit && info.path.exists()).then(|| info.clone()))
}

/// A hosted plug-in as the engine's effect: the engine's trait on the host's processor.
struct Hosted(host::Processor);

impl BlockEffect for Hosted {
    fn set_param(&mut self, index: usize, norm: f32) {
        self.0.set_param_norm(index, norm);
    }

    fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        // The plug-in works in place.
        out_l.copy_from_slice(in_l);
        out_r.copy_from_slice(in_r);
        self.0.process(out_l, out_r);
    }

    fn latency_samples(&self) -> usize {
        self.0.latency_samples()
    }

    fn reset(&mut self) {
        self.0.reset();
    }

    /// A hosted plug-in keeps its own settings; only automation moves them.
    fn takes_settings(&self) -> bool {
        false
    }

    fn boxed_clone(&self) -> Option<Box<dyn BlockEffect>> {
        Some(Box::new(Hosted(self.0.clone())))
    }

    fn set_offline(&mut self, offline: bool) {
        self.0.set_offline(offline);
    }
}

/// Puts `info` in the rack, with the state a project file had for it, and returns its number
/// for an `Insert`. The plug-in is loaded when the engine first plays it.
pub fn add(info: &PluginInfo, saved: Option<Vec<u8>>) -> u32 {
    let mut rack = rack();
    rack.slots.push(Slot {
        info: info.clone(),
        name: Box::leak(info.name.clone().into_boxed_str()),
        instance: None,
        rate: 0.0,
        saved,
        error: None,
        checked: None,
    });
    rack.slots.len() as u32
}

fn with_slot<T>(plugin: u32, f: impl FnOnce(&mut Slot) -> T) -> Option<T> {
    let index = (plugin as usize).checked_sub(1)?;
    rack().slots.get_mut(index).map(f)
}

/// The name of plug-in `plugin` ("Plug-in" for a number the rack does not have).
pub fn name(plugin: u32) -> &'static str {
    with_slot(plugin, |slot| slot.name).unwrap_or("Plug-in")
}

pub fn info(plugin: u32) -> Option<PluginInfo> {
    with_slot(plugin, |slot| slot.info.clone())
}

/// The loaded plug-in, for its window (none until the engine played it, or if it failed).
pub fn instance(plugin: u32) -> Option<Instance> {
    with_slot(plugin, |slot| slot.instance.clone()).flatten()
}

/// Why plug-in `plugin` could not be loaded, if it could not.
pub fn error(plugin: u32) -> Option<String> {
    with_slot(plugin, |slot| slot.error.clone()).flatten()
}

/// The plug-in's state for a project file: from the plug-in when it is loaded, else what the
/// project file it came from had.
pub fn state(plugin: u32) -> Option<Vec<u8>> {
    // A plug-in that crashed has no answer: what was last put aside stands in.
    let (instance, saved) = with_slot(plugin, |slot| (slot.instance.clone(), slot.saved.clone()))?;
    instance.and_then(|i| i.save_state()).or(saved)
}

/// The delay plug-in `plugin` puts on the sound, in samples: its own and the block the engine
/// feeds it in (the block counts before the plug-in is loaded, and when it cannot be).
pub fn latency_samples(plugin: u32) -> usize {
    let own = with_slot(plugin, |slot| {
        slot.instance.as_ref().map_or(0, |i| i.latency_samples())
    })
    .unwrap_or(0);
    PLUGIN_BLOCK + own
}

/// Plug-in `plugin` as an effect the engine can run at `sample_rate`, loading it the first time
/// (and again, keeping its settings, when the sample rate changed). A plug-in that cannot be
/// loaded gives an effect that passes the sound through.
pub fn fx(plugin: u32, bypass: bool, sample_rate: f32) -> Fx {
    let loaded = with_slot(plugin, |slot| {
        if slot.instance.is_none() || slot.rate != sample_rate {
            let state = match &slot.instance {
                Some(old) => old.save_state(),
                None => slot.saved.clone(),
            };
            match load(&slot.info, sample_rate) {
                Ok(instance) => {
                    if let Some(state) = &state {
                        instance.load_state(state);
                    }
                    slot.instance = Some(instance);
                    slot.error = None;
                }
                Err(error) => {
                    tracing::warn!(plugin = %slot.info.name, %error, "plug-in not loaded");
                    slot.instance = None;
                    slot.saved = state;
                    slot.error = Some(error.to_string());
                }
            }
            slot.rate = sample_rate;
        }
        slot.instance.as_ref().map(Instance::processor)
    })
    .flatten();
    let mut fx = Fx::new(
        prowave_engine::FxKind::Plugin,
        [0.0; prowave_engine::MAX_FX_PARAMS],
        bypass,
        sample_rate,
    );
    if let Some(processor) = loaded {
        fx.set_plugin(Box::new(Hosted(processor)));
    }
    fx
}

/// Loads a plug-in in a process of its own, started from this program (`main` hands such a
/// start to the host first thing), so that a plug-in that crashes while playing takes its
/// process with it and not Pro Wave. Tests have no app program to start: they load in-process.
fn load(info: &PluginInfo, sample_rate: f32) -> Result<Instance, host::Error> {
    let rate = f64::from(sample_rate);
    #[cfg(not(test))]
    if let Ok(program) = std::env::current_exe() {
        return host::load_sandboxed(info, rate, PLUGIN_BLOCK, &program);
    }
    host::load(info, rate, PLUGIN_BLOCK)
}

/// How often a running plug-in's state is put aside, so that one that crashes can be loaded
/// again with nearly the settings it had.
const KEEP_STATE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
/// How often the missing plug-ins are looked for again (see [`retry_missing`]).
const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether `every` has passed since `last` last said yes; a yes starts the wait again.
fn due(last: &Mutex<Option<std::time::Instant>>, every: std::time::Duration) -> bool {
    last.lock().is_ok_and(|mut last| {
        let due = last.is_none_or(|at| at.elapsed() >= every);
        if due {
            *last = Some(std::time::Instant::now());
        }
        due
    })
}

/// Looks again at the plug-ins that were not installed: one that is installed now is loaded by
/// the engine at its next rebuild (see [`revision`]), with the state the project gave it.
pub fn retry_missing() {
    rack().retry();
}

/// Counts the slots that became loadable again. The engine compares it with the last value it
/// built its chains for, and rebuilds the strips that use hosted plug-ins when it changed.
pub fn revision() -> u64 {
    rack().revision
}

/// Whether plug-in `plugin` is in use but not installed here: the scan has finished, it is not
/// in the catalog nor at its file, and it could not be loaded. It keeps its slot and passes the
/// sound through.
pub fn missing(plugin: u32) -> bool {
    let rack = rack();
    (plugin as usize)
        .checked_sub(1)
        .and_then(|index| rack.slots.get(index))
        .is_some_and(|slot| rack.is_missing(slot))
}

/// The frame to ask for a new picture when the catalog changes during the scan. The app gives
/// it once at start.
pub fn set_repaint(ctx: &egui::Context) {
    rack().repaint = Some(ctx.clone());
}

/// Once a frame, on the UI thread: the main-thread work the loaded plug-ins asked for, and now
/// and then a copy of their state.
pub fn idle() {
    static LAST_KEPT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    static LAST_RETRY: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    if due(&LAST_RETRY, RETRY_EVERY) {
        retry_missing();
    }
    let instances: Vec<(usize, Instance)> = rack()
        .slots
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| Some((i, slot.instance.clone()?)))
        .collect();
    for (_, instance) in &instances {
        instance.idle();
    }
    if due(&LAST_KEPT, KEEP_STATE_EVERY) {
        // Asked without the rack's lock held: the answer comes from another process.
        for (index, instance) in instances {
            if let Some(state) = instance.save_state() {
                if let Some(slot) = rack().slots.get_mut(index) {
                    slot.saved = Some(state);
                }
            }
        }
    }
}

/// Whether plug-in `plugin`'s process ended on its own: it crashed while it was loaded.
pub fn crashed(plugin: u32) -> bool {
    with_slot(plugin, |slot| {
        slot.instance.as_ref().is_some_and(Instance::crashed)
    })
    .unwrap_or(false)
}

/// A new place in the rack for the plug-in of `plugin`, with the state last put aside for it:
/// the number to put in the `Insert` instead, which makes the engine load it afresh. This is
/// how a plug-in that crashed is started again.
pub fn again(plugin: u32) -> Option<u32> {
    let (info, saved) = with_slot(plugin, |slot| (slot.info.clone(), slot.saved.clone()))?;
    Some(add(&info, saved))
}

/// The number of parameters of plug-in `plugin` (none before it is loaded).
pub fn param_count(plugin: u32) -> usize {
    instance(plugin).map_or(0, |i| i.params().len())
}

/// The name of parameter `index` of plug-in `plugin` ("?" when there is none).
pub fn param_name(plugin: u32, index: usize) -> String {
    instance(plugin)
        .and_then(|i| i.params().get(index).map(|p| p.name.clone()))
        .unwrap_or_else(|| "?".to_string())
}

/// The position (0..=1) of parameter `index` of plug-in `plugin`.
pub fn param(plugin: u32, index: usize) -> f32 {
    instance(plugin).map_or(0.0, |i| i.param(index))
}

/// Moves parameter `index` of plug-in `plugin` to position `norm` (0..=1).
pub fn set_param(plugin: u32, index: usize, norm: f32) {
    if let Some(instance) = instance(plugin) {
        instance.set_param(index, norm);
    }
}

/// The plug-ins in use that are loaded, for the Plug-in Manager: name, format, delay in
/// samples.
pub fn loaded() -> Vec<(String, Format, usize)> {
    rack()
        .slots
        .iter()
        .filter_map(|slot| {
            let instance = slot.instance.as_ref()?;
            Some((
                slot.info.name.clone(),
                slot.info.format,
                PLUGIN_BLOCK + instance.latency_samples(),
            ))
        })
        .collect()
}

/// The installed plug-ins found so far, sorted by vendor and name.
pub fn catalog() -> Vec<PluginInfo> {
    rack().catalog.clone()
}

pub fn scan_status() -> Scan {
    rack().scan
}

/// The catalog entry of a plug-in a project file names: the same format and id.
pub fn find(format: Format, id: &str) -> Option<PluginInfo> {
    rack()
        .catalog
        .iter()
        .find(|info| info.format == format && info.id == id)
        .cloned()
}

fn sort(catalog: &mut [PluginInfo]) {
    catalog.sort_by_cached_key(|info| {
        (
            info.vendor.to_lowercase(),
            info.name.to_lowercase(),
            info.format.name(),
        )
    });
}

/// One line of the scan's output and of the cache: tabs and line ends in a name become spaces.
fn field(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn plugin_line(info: &PluginInfo) -> String {
    format!(
        "plugin\t{}\t{}\t{}",
        field(&info.id),
        field(&info.name),
        field(&info.vendor)
    )
}

fn parse_plugin_line(line: &str, format: Format, path: &Path) -> Option<PluginInfo> {
    let mut parts = line.split('\t');
    if parts.next() != Some("plugin") {
        return None;
    }
    Some(PluginInfo {
        format,
        id: parts.next()?.to_string(),
        name: parts.next()?.to_string(),
        vendor: parts.next().unwrap_or_default().to_string(),
        path: path.to_path_buf(),
    })
}

/// The scanning child process: when the arguments are `--scan-plugin <format> <path>`, lists
/// that file's plug-ins on standard output and returns true (the caller then exits).
pub fn run_scan_if_requested(args: &[std::ffi::OsString]) -> bool {
    let Some(at) = args.iter().position(|a| a == SCAN_ARG) else {
        return false;
    };
    let format = args
        .get(at + 1)
        .and_then(|f| Format::from_name(&f.to_string_lossy()));
    if let (Some(format), Some(path)) = (format, args.get(at + 2)) {
        match host::scan_file(format, Path::new(path)) {
            Ok(found) => {
                for info in &found {
                    println!("{}", plugin_line(info));
                }
                println!("done");
            }
            Err(error) => eprintln!("{error}"),
        }
    }
    true
}

#[cfg(not(test))]
/// When `path` was last changed, in seconds (0 when unknown), to tell a new version of a
/// plug-in from the one in the cache. A bundle is a folder: its program is what changes.
fn stamp(path: &Path) -> u64 {
    let of = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let inner = path.join("Contents");
    of(path).max(if inner.is_dir() { of(&inner) } else { 0 })
}

/// One scanned file as the cache keeps it.
#[derive(Clone, Debug, PartialEq)]
struct Scanned {
    format: Format,
    path: PathBuf,
    stamp: u64,
    plugins: Vec<PluginInfo>,
}

/// A plug-in file on disk now, with the stamp that tells whether it changed.
#[derive(Clone, Debug, PartialEq)]
struct Found {
    format: Format,
    path: PathBuf,
    stamp: u64,
}

/// What a scan does with the cache and the files on disk: the files to list anew (the new ones,
/// and those whose stamp changed, or all of them for a rescan) and the cached files that are gone.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    scan: Vec<Found>,
    forget: Vec<(Format, PathBuf)>,
}

fn plan(known: &[Scanned], on_disk: &[Found], rescan_all: bool) -> Plan {
    let scan = on_disk
        .iter()
        .filter(|f| {
            rescan_all
                || !known
                    .iter()
                    .any(|k| k.format == f.format && k.path == f.path && k.stamp == f.stamp)
        })
        .cloned()
        .collect();
    let forget = known
        .iter()
        .filter(|k| {
            !on_disk
                .iter()
                .any(|f| f.format == k.format && f.path == k.path)
        })
        .map(|k| (k.format, k.path.clone()))
        .collect();
    Plan { scan, forget }
}

fn cache_text(files: &[Scanned]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(&format!(
            "file\t{}\t{}\t{}\n",
            file.format.name(),
            file.stamp,
            field(&file.path.to_string_lossy())
        ));
        for info in &file.plugins {
            out.push_str(&plugin_line(info));
            out.push('\n');
        }
    }
    out
}

fn parse_cache(text: &str) -> Vec<Scanned> {
    let mut files: Vec<Scanned> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        if parts.next() == Some("file") {
            let format = parts.next().and_then(Format::from_name);
            let stamp = parts.next().and_then(|s| s.parse().ok());
            if let (Some(format), Some(stamp), Some(path)) = (format, stamp, parts.next()) {
                files.push(Scanned {
                    format,
                    path: PathBuf::from(path),
                    stamp,
                    plugins: Vec::new(),
                });
            }
        } else if let Some(file) = files.last_mut() {
            if let Some(info) = parse_plugin_line(line, file.format, &file.path) {
                file.plugins.push(info);
            }
        }
    }
    files
}

#[cfg(not(test))]
fn cache_file() -> Option<PathBuf> {
    Some(crate::recent_files::config_dir()?.join("plugins.txt"))
}

/// The cache of the last scan, as far as it reads. Tests never touch the configuration folder.
#[cfg(not(test))]
fn read_cache() -> Vec<Scanned> {
    cache_file()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .map(|text| parse_cache(&text))
        .unwrap_or_default()
}

#[cfg(test)]
fn read_cache() -> Vec<Scanned> {
    Vec::new()
}

/// The CLAP and VST3 files on disk now, with their stamps. Audio Units are not files.
#[cfg(not(test))]
fn on_disk_files() -> Vec<Found> {
    [Format::Clap, Format::Vst3, Format::Lv2]
        .into_iter()
        .flat_map(|format| {
            host::installed(format).into_iter().map(move |path| Found {
                format,
                stamp: stamp(&path),
                path,
            })
        })
        .collect()
}

/// Lists one file's plug-ins in a child process. A crash, a refusal or a file that takes
/// longer than [`SCAN_TIMEOUT`] gives no plug-ins.
#[cfg(not(test))]
fn scan_in_child(format: Format, path: &Path) -> Vec<PluginInfo> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let child = Command::new(exe)
        .arg(SCAN_ARG)
        .arg(format.name())
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    // The output is read on its own thread so a chatty plug-in cannot fill the pipe and stall.
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            text
        })
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SCAN_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                tracing::warn!(file = %path.display(), "plug-in scan timed out");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let text = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    // Without the last line the child did not finish: what it listed is not trusted.
    if !text.lines().any(|line| line == "done") {
        tracing::warn!(file = %path.display(), "plug-in scan failed");
        return Vec::new();
    }
    text.lines()
        .filter_map(|line| parse_plugin_line(line, format, path))
        .collect()
}

/// Starts the scan of the installed plug-ins on a background thread, unless one runs or (without
/// `again`) one has finished. Without `again` the cache of the last run is read here first, so
/// the menus list its plug-ins at once. With `again`, every file is scanned anew (the Plug-in
/// Manager's Rescan), and the catalog stays as it is until each file is done.
pub fn scan(again: bool) {
    {
        let mut rack = rack();
        match rack.scan {
            Scan::Running(..) => return,
            Scan::Done if !again => return,
            _ => rack.scan = Scan::Running(0, 0),
        }
    }
    if !again {
        // The cache is small, so reading it here is quick. A file that is gone is left out now.
        let cached: Vec<Scanned> = read_cache()
            .into_iter()
            .filter(|f| f.path.exists())
            .collect();
        let mut rack = rack();
        rack.files = cached;
        rack.rebuild();
    }
    // Tests have no app program to run as the scanning child; they see an empty catalog.
    #[cfg(test)]
    {
        rack().scan = Scan::Done;
    }
    #[cfg(not(test))]
    std::thread::spawn(move || run_scan(again));
}

/// The scan on its thread: the plug-in files on disk that are new or changed since the cache was
/// written are listed in child processes, and the catalog follows as each one is done. The
/// cache is written at the end, without the files that are gone.
#[cfg(not(test))]
fn run_scan(again: bool) {
    let found = on_disk_files();
    let known = rack().files.clone();
    let plan = plan(&known, &found, again);
    // Audio Units come from the system's own list: no plug-in code runs.
    let audio_units = host::scan_audio_units();
    {
        let mut rack = rack();
        rack.audio_units = audio_units;
        rack.files
            .retain(|f| !plan.forget.contains(&(f.format, f.path.clone())));
        rack.rebuild();
    }
    let total = plan.scan.len();
    for (done, file) in plan.scan.into_iter().enumerate() {
        // Listed outside the rack's lock: a child process can take a while.
        let plugins = scan_in_child(file.format, &file.path);
        let entry = Scanned {
            format: file.format,
            path: file.path,
            stamp: file.stamp,
            plugins,
        };
        let mut rack = rack();
        match rack
            .files
            .iter()
            .position(|f| f.format == entry.format && f.path == entry.path)
        {
            Some(at) => rack.files[at] = entry,
            None => rack.files.push(entry),
        }
        rack.rebuild();
        rack.scan = Scan::Running(done + 1, total);
    }
    let files = rack().files.clone();
    if let Some(file) = cache_file() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, cache_text(&files));
    }
    let mut rack = rack();
    rack.scan = Scan::Done;
    // A plug-in that was missing may be installed now, and the menus show the final list.
    rack.retry();
    rack.wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str) -> PluginInfo {
        PluginInfo {
            format: Format::Clap,
            id: format!("org.test.{name}"),
            name: name.to_string(),
            vendor: "Test\tVendor".to_string(),
            path: PathBuf::from("/plug-ins/test.clap"),
        }
    }

    #[test]
    fn the_cache_reads_back_what_it_wrote() {
        let files = vec![
            Scanned {
                format: Format::Clap,
                path: PathBuf::from("/plug-ins/test.clap"),
                stamp: 42,
                plugins: vec![info("One"), info("Two")],
            },
            Scanned {
                format: Format::Vst3,
                path: PathBuf::from("/plug-ins/broken.vst3"),
                stamp: 7,
                plugins: Vec::new(),
            },
        ];
        let read = parse_cache(&cache_text(&files));
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].plugins.len(), 2);
        assert_eq!(read[0].plugins[1].name, "Two");
        assert_eq!(
            read[0].plugins[0].vendor, "Test Vendor",
            "a tab becomes a space"
        );
        assert_eq!(read[0].plugins[0].path, files[0].path);
        assert_eq!((read[1].format, read[1].stamp), (Format::Vst3, 7));
        assert!(read[1].plugins.is_empty(), "a file that failed stays known");
    }

    #[test]
    fn a_damaged_cache_is_read_as_far_as_it_makes_sense() {
        let read = parse_cache("plugin\tlost\tLost\n\nfile\tNo Format\t1\t/x\nfile\tCLAP\tx\t/y\n");
        assert!(read.is_empty());
    }

    #[test]
    fn a_plugin_that_is_not_installed_passes_the_sound_and_keeps_its_state() {
        let mut missing = info("Missing");
        missing.path = PathBuf::from("/nonexistent/missing.clap");
        let plugin = add(&missing, Some(vec![1, 2, 3]));
        assert_eq!(name(plugin), "Missing");
        let mut fx = fx(plugin, false, 48_000.0);
        // Without its plug-in the slot passes the sound, one block late like a loaded one.
        let last = (0..=PLUGIN_BLOCK).map(|_| fx.process(0.5, -0.5)).last();
        assert_eq!(last, Some((0.5, -0.5)));
        assert_eq!(fx.latency_samples(), PLUGIN_BLOCK);
        assert!(error(plugin).is_some());
        assert!(instance(plugin).is_none());
        assert_eq!(
            state(plugin),
            Some(vec![1, 2, 3]),
            "saving keeps what was loaded"
        );
        assert_eq!(latency_samples(plugin), PLUGIN_BLOCK);
    }

    #[test]
    fn an_unknown_number_is_a_plain_plug_in() {
        assert_eq!(name(0), "Plug-in");
        assert_eq!(name(u32::MAX), "Plug-in");
        assert!(info_of_none());
    }

    fn info_of_none() -> bool {
        super::info(u32::MAX).is_none() && state(0).is_none()
    }

    fn found(format: Format, path: &str, stamp: u64) -> Found {
        Found {
            format,
            path: PathBuf::from(path),
            stamp,
        }
    }

    fn cached(format: Format, path: &str, stamp: u64, plugins: Vec<PluginInfo>) -> Scanned {
        Scanned {
            format,
            path: PathBuf::from(path),
            stamp,
            plugins,
        }
    }

    /// A rack as the app has it after a scan: the given slots and the catalog they are built from.
    fn rack_with(slots: Vec<Slot>, catalog: Vec<PluginInfo>) -> Rack {
        Rack {
            slots,
            catalog,
            scan: Scan::Done,
            ..Rack::default()
        }
    }

    fn slot_for(info: PluginInfo, error: Option<&str>) -> Slot {
        Slot {
            info,
            name: "Test",
            instance: None,
            rate: 0.0,
            saved: Some(vec![9]),
            error: error.map(str::to_string),
            checked: None,
        }
    }

    #[test]
    fn a_launch_lists_new_and_changed_files_and_forgets_removed_ones() {
        let known = vec![
            cached(Format::Clap, "/plug-ins/same.clap", 1, vec![info("Same")]),
            cached(Format::Clap, "/plug-ins/changed.clap", 2, vec![info("Old")]),
            cached(Format::Vst3, "/plug-ins/gone.vst3", 3, vec![info("Gone")]),
        ];
        let on_disk = vec![
            found(Format::Clap, "/plug-ins/same.clap", 1),
            found(Format::Clap, "/plug-ins/changed.clap", 5),
            found(Format::Clap, "/plug-ins/new.clap", 4),
        ];
        let plan = plan(&known, &on_disk, false);
        assert_eq!(
            plan.scan,
            vec![
                found(Format::Clap, "/plug-ins/changed.clap", 5),
                found(Format::Clap, "/plug-ins/new.clap", 4),
            ],
            "the unchanged file is read from the cache"
        );
        assert_eq!(
            plan.forget,
            vec![(Format::Vst3, PathBuf::from("/plug-ins/gone.vst3"))]
        );
    }

    #[test]
    fn a_rescan_lists_every_file_again_and_still_forgets_removed_ones() {
        let known = vec![
            cached(Format::Clap, "/plug-ins/a.clap", 1, vec![info("A")]),
            cached(Format::Clap, "/plug-ins/b.clap", 2, vec![info("B")]),
        ];
        let on_disk = vec![found(Format::Clap, "/plug-ins/a.clap", 1)];
        let plan = plan(&known, &on_disk, true);
        assert_eq!(plan.scan, on_disk);
        assert_eq!(
            plan.forget,
            vec![(Format::Clap, PathBuf::from("/plug-ins/b.clap"))]
        );
    }

    #[test]
    fn an_empty_cache_and_an_empty_folder_plan_nothing() {
        assert_eq!(plan(&[], &[], false), Plan::default());
    }

    #[test]
    fn the_catalog_lists_the_cache_and_the_audio_units_and_counts_its_changes() {
        let mut rack = Rack {
            scan: Scan::Running(0, 2),
            ..Rack::default()
        };
        rack.files = vec![cached(
            Format::Clap,
            "/plug-ins/a.clap",
            1,
            vec![info("Zed")],
        )];
        rack.rebuild();
        assert_eq!(rack.catalog.len(), 1, "the cache is listed before any scan");
        let generation = rack.generation;
        rack.rebuild();
        assert_eq!(rack.generation, generation, "no change, no new generation");
        rack.audio_units = vec![info("Alpha")];
        rack.rebuild();
        assert_eq!(rack.generation, generation + 1);
        let names: Vec<&str> = rack.catalog.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Zed"], "sorted by name within a vendor");
    }

    #[test]
    fn a_plug_in_is_missing_only_after_the_scan_and_when_it_is_not_installed() {
        let missing = info("Missing");
        let mut rack = rack_with(vec![slot_for(missing.clone(), Some("no file"))], Vec::new());
        assert!(rack.is_missing(&rack.slots[0]));
        rack.scan = Scan::Running(1, 3);
        assert!(
            !rack.is_missing(&rack.slots[0]),
            "not yet: the scan may still find it"
        );
        rack.scan = Scan::Done;
        rack.catalog = vec![missing.clone()];
        assert!(!rack.is_missing(&rack.slots[0]), "listed: installed");
        rack.catalog.clear();
        rack.slots[0].error = None;
        assert!(
            !rack.is_missing(&rack.slots[0]),
            "a plug-in that loads is not missing"
        );
    }

    #[test]
    fn a_plug_in_whose_file_is_still_there_is_not_missing() {
        // The crate's own manifest is a file that is certainly there.
        let mut here = info("Here");
        here.path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let rack = rack_with(vec![slot_for(here, Some("not listed"))], Vec::new());
        assert!(!rack.is_missing(&rack.slots[0]));
    }

    #[test]
    fn a_missing_plug_in_is_found_again_once_and_loaded_with_its_state() {
        let mut rack = rack_with(vec![slot_for(info("Back"), Some("no file"))], Vec::new());
        rack.retry();
        assert_eq!(rack.revision, 0, "still not installed: nothing to rebuild");
        assert!(rack.slots[0].error.is_some());

        // Reinstalled: the catalog lists it and the next look takes it over.
        let mut back = info("Back");
        back.path = PathBuf::from("/plug-ins/reinstalled.clap");
        rack.catalog = vec![back.clone()];
        rack.generation = 1;
        rack.retry();
        assert_eq!(rack.revision, 1, "the engine must rebuild its chains");
        assert!(rack.slots[0].error.is_none());
        assert_eq!(rack.slots[0].info, back, "the installed entry is used");
        assert_eq!(
            rack.slots[0].saved,
            Some(vec![9]),
            "the state from the project is kept for the load"
        );

        // Loading failed again (it is in the catalog but would not start): no retry at the same
        // catalog, so no rebuild loop.
        rack.slots[0].error = Some("would not start".into());
        rack.retry();
        assert_eq!(rack.revision, 1);
        // A new catalog gives it another try.
        rack.generation = 2;
        rack.retry();
        assert_eq!(rack.revision, 2);
    }

    #[test]
    fn a_plug_in_that_is_loaded_is_never_retried() {
        let mut rack = rack_with(vec![slot_for(info("Loaded"), None)], vec![info("Loaded")]);
        rack.retry();
        assert_eq!(rack.revision, 0);
    }
}
