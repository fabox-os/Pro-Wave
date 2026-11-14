//! The third-party plug-ins of the session (Logic: Audio Units): the list of what is installed,
//! and the ones a channel strip uses.
//!
//! `prowave-plugin-host` does the hosting. This module keeps what the app needs around it:
//!
//! - The **catalog**: every installed CLAP, VST3 and Audio Unit effect, for the Audio FX picker
//!   and the Plug-in Manager. Listing a CLAP or VST3 file runs its code, so each file is scanned
//!   in a child process (`prowave --scan-plugin`): a plug-in that crashes or hangs there costs
//!   one file, not the session. The result is kept in the configuration folder and a file is
//!   scanned again only when it changes.
//! - The **rack**: the plug-ins in use. An `Insert` holds a number; the rack holds the loaded
//!   plug-in behind it. A plug-in is loaded the first time the engine needs it (the engine
//!   knows the sample rate) and stays loaded while the app runs, so Undo can bring a removed
//!   plug-in back with its settings.
use prowave_engine::{ExternalFx, Fx, EXTERNAL_BLOCK};
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
    catalog: Vec<PluginInfo>,
    scan: Scan,
}

fn rack() -> MutexGuard<'static, Rack> {
    static RACK: OnceLock<Mutex<Rack>> = OnceLock::new();
    RACK.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A hosted plug-in as the engine's effect: the engine's trait on the host's processor.
struct Hosted(host::Processor);

impl ExternalFx for Hosted {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.0.process(left, right);
    }

    fn set_param_norm(&mut self, index: usize, norm: f32) {
        self.0.set_param_norm(index, norm);
    }

    fn latency_samples(&self) -> usize {
        self.0.latency_samples()
    }

    fn reset(&mut self) {
        self.0.reset();
    }

    fn boxed_clone(&self) -> Box<dyn ExternalFx> {
        Box::new(Hosted(self.0.clone()))
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
    with_slot(plugin, |slot| match &slot.instance {
        Some(instance) => instance.save_state(),
        None => slot.saved.clone(),
    })
    .flatten()
}

/// The delay plug-in `plugin` puts on the sound, in samples: its own and the block the engine
/// feeds it in (none before it is loaded).
pub fn latency_samples(plugin: u32) -> usize {
    with_slot(plugin, |slot| {
        slot.instance
            .as_ref()
            .map_or(0, |i| EXTERNAL_BLOCK + i.latency_samples())
    })
    .unwrap_or(0)
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
            match host::load(&slot.info, f64::from(sample_rate), EXTERNAL_BLOCK) {
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
    match loaded {
        Some(processor) => Fx::external(Box::new(Hosted(processor)), bypass, sample_rate),
        None => Fx::new(
            prowave_engine::FxKind::External,
            [0.0; prowave_engine::MAX_FX_PARAMS],
            bypass,
            sample_rate,
        ),
    }
}

/// Once a frame, on the UI thread: the main-thread work the loaded plug-ins asked for.
pub fn idle() {
    let instances: Vec<Instance> = rack()
        .slots
        .iter()
        .filter_map(|slot| slot.instance.clone())
        .collect();
    for instance in instances {
        instance.idle();
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
                EXTERNAL_BLOCK + instance.latency_samples(),
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

#[cfg(not(test))]
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

/// Lists the installed plug-ins on a background thread, unless that was done already. With
/// `again`, every file is scanned anew (the Plug-in Manager's Rescan).
pub fn scan(again: bool) {
    {
        let mut rack = rack();
        match rack.scan {
            Scan::Running(..) => return,
            Scan::Done if !again => return,
            _ => rack.scan = Scan::Running(0, 0),
        }
    }
    // Tests have no app program to run as the scanning child; they see an empty catalog.
    #[cfg(test)]
    {
        rack().scan = Scan::Done;
    }
    #[cfg(not(test))]
    std::thread::spawn(move || {
        let known = if again {
            Vec::new()
        } else {
            cache_file()
                .and_then(|file| std::fs::read_to_string(file).ok())
                .map(|text| parse_cache(&text))
                .unwrap_or_default()
        };
        let files: Vec<(Format, PathBuf)> = [Format::Clap, Format::Vst3]
            .into_iter()
            .flat_map(|format| {
                host::installed(format)
                    .into_iter()
                    .map(move |p| (format, p))
            })
            .collect();
        // Audio Units come from the system's own list: no plug-in code runs.
        let mut catalog = host::scan_audio_units();
        let mut scanned = Vec::new();
        for (done, (format, path)) in files.iter().enumerate() {
            let stamp = stamp(path);
            let plugins = known
                .iter()
                .find(|k| k.format == *format && &k.path == path && k.stamp == stamp)
                .map(|k| k.plugins.clone())
                .unwrap_or_else(|| scan_in_child(*format, path));
            catalog.extend(plugins.iter().cloned());
            scanned.push(Scanned {
                format: *format,
                path: path.clone(),
                stamp,
                plugins,
            });
            sort(&mut catalog);
            let mut rack = rack();
            rack.catalog = catalog.clone();
            rack.scan = Scan::Running(done + 1, files.len());
        }
        sort(&mut catalog);
        if let Some(file) = cache_file() {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, cache_text(&scanned));
        }
        let mut rack = rack();
        rack.catalog = catalog;
        rack.scan = Scan::Done;
    });
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
        assert_eq!(fx.process(0.5, -0.5), (0.5, -0.5));
        assert_eq!(fx.latency_samples(), 0);
        assert!(error(plugin).is_some());
        assert!(instance(plugin).is_none());
        assert_eq!(
            state(plugin),
            Some(vec![1, 2, 3]),
            "saving keeps what was loaded"
        );
        assert_eq!(latency_samples(plugin), 0);
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
}
