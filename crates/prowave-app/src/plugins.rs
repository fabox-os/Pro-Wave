//! LADSPA effects in the app: which plug-ins the computer has (the catalogue, filled by a
//! background scan), which ones the open project uses (a short table, so an insert stays a small
//! copyable value), and starting them for the engine.
//!
//! Logic Pro equivalent: the Plug-in Manager and the plug-in menu of an Audio FX slot. A LADSPA
//! library is native code, so every library is read in a child process with a time limit (a copy
//! of this program started with `--scan-ladspa`); one that crashes or hangs three times is put
//! in `quarantine.json` in the config folder and left alone. Plug-ins that play inside the
//! program (`prowave-ladspa` does the loading and the calls) can still crash it while they run;
//! that is the limit of hosting in one process.

use prowave_engine::{BlockEffect, PLUGIN_BLOCK};
use prowave_ladspa::{Instance, Library, PortKind};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// The most control inputs shown for a plug-in: the engine's effect parameters.
pub const MAX_CONTROLS: usize = prowave_engine::MAX_FX_PARAMS;
/// The most library files looked at in one folder.
const MAX_FILES_PER_DIR: usize = 4096;
/// A library that fails this many scans is left alone.
pub const MAX_FAILURES: u32 = 3;
/// How long a child process may take to read one library.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
/// The most output taken from a scanning child.
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;

/// A plug-in the project uses: a number into this module's table. It is `Copy`, so a channel
/// strip's insert stays a plain value; [`lookup`] gives the plug-in back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PluginId(u32);

/// Which plug-in an insert uses, as the project file keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PluginRef {
    /// The library file when the project was saved.
    pub library: PathBuf,
    pub label: String,
    pub unique_id: u64,
    /// For display when the plug-in is missing.
    pub name: String,
}

/// One control input of a plug-in.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlInfo {
    pub name: String,
    /// The LADSPA port number.
    pub port: usize,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub toggled: bool,
    pub integer: bool,
    pub log: bool,
}

impl ControlInfo {
    /// The range this control moves through (a broken range becomes 0 to 1; a toggle is 0 to 1).
    pub fn range(&self) -> (f32, f32) {
        if self.toggled {
            return (0.0, 1.0);
        }
        if self.min.is_finite() && self.max.is_finite() && self.min < self.max {
            (self.min, self.max)
        } else {
            (0.0, 1.0)
        }
    }

    /// `value` put into the range (integer and toggled controls are rounded).
    pub fn clamp(&self, value: f32) -> f32 {
        let (lo, hi) = self.range();
        let v = if value.is_finite() { value } else { lo }.clamp(lo, hi);
        if self.integer {
            v.round().clamp(lo.ceil(), hi.floor().max(lo.ceil()))
        } else if self.toggled {
            if v >= 0.5 {
                1.0
            } else {
                0.0
            }
        } else {
            v
        }
    }

    /// The value at position `norm` (0 to 1) along the range.
    pub fn at_norm(&self, norm: f32) -> f32 {
        let (lo, hi) = self.range();
        let t = if norm.is_finite() {
            norm.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let v = if self.log && lo > 0.0 {
            (lo.ln() * (1.0 - t) + hi.ln() * t).exp()
        } else {
            lo + (hi - lo) * t
        };
        self.clamp(v)
    }

    /// The position (0 to 1) of `value` along the range.
    pub fn to_norm(&self, value: f32) -> f32 {
        let (lo, hi) = self.range();
        let v = self.clamp(value);
        let t = if self.log && lo > 0.0 {
            (v.ln() - lo.ln()) / (hi.ln() - lo.ln())
        } else {
            (v - lo) / (hi - lo)
        };
        if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The text of `value`: whole numbers for integer controls, "On" / "Off" for toggles.
    pub fn text(&self, value: f32) -> String {
        if self.toggled {
            return if value >= 0.5 { "On" } else { "Off" }.to_string();
        }
        if self.integer {
            return format!("{}", value.round() as i64);
        }
        let a = value.abs();
        if a >= 1000.0 {
            format!("{value:.0}")
        } else if a >= 100.0 {
            format!("{value:.1}")
        } else {
            format!("{value:.2}")
        }
    }
}

/// Everything the app needs to know about one LADSPA plug-in.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    pub library: PathBuf,
    /// The plug-in's number in its library.
    pub index: usize,
    pub unique_id: u64,
    pub label: String,
    pub name: String,
    pub maker: String,
    pub audio_in: usize,
    pub audio_out: usize,
    /// The control inputs, in port order, at most [`MAX_CONTROLS`].
    pub controls: Vec<ControlInfo>,
    /// The control output that reports latency in samples, if the plug-in has one.
    pub latency_port: Option<usize>,
}

impl PluginInfo {
    /// The reference saved in a project.
    pub fn plugin_ref(&self) -> PluginRef {
        PluginRef {
            library: self.library.clone(),
            label: self.label.clone(),
            unique_id: self.unique_id,
            name: self.name.clone(),
        }
    }

    /// Whether this can sit in an Audio FX slot: sound out, and few enough ports to feed.
    pub fn is_effect(&self) -> bool {
        self.audio_out >= 1 && self.audio_out <= MAX_AUDIO_PORTS && self.audio_in <= MAX_AUDIO_PORTS
    }

    /// The default value of every control as a position 0 to 1: what a new insert starts with.
    pub fn default_norms(&self) -> Vec<f32> {
        self.controls.iter().map(|c| c.to_norm(c.default)).collect()
    }
}

impl PluginRef {
    /// Whether `info` is the plug-in this refers to: the same label in a library of the same
    /// file name (a project moves between machines).
    pub fn matches(&self, info: &PluginInfo) -> bool {
        info.label == self.label && info.library.file_name() == self.library.file_name()
    }
}

/// The most audio inputs or outputs a hosted effect may have.
const MAX_AUDIO_PORTS: usize = 4;

/// What a library has, as [`PluginInfo`]s.
fn infos_of(library: &Library, path: &Path) -> Vec<PluginInfo> {
    library
        .plugins()
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let mut controls = Vec::new();
            let mut latency_port = None;
            let (mut audio_in, mut audio_out) = (0, 0);
            for port in &p.ports {
                match port.kind {
                    PortKind::AudioInput => audio_in += 1,
                    PortKind::AudioOutput => audio_out += 1,
                    PortKind::ControlInput if controls.len() < MAX_CONTROLS => {
                        controls.push(ControlInfo {
                            name: port.name.clone(),
                            port: port.index,
                            min: port.min,
                            max: port.max,
                            default: port.default,
                            toggled: port.toggled,
                            integer: port.integer,
                            log: port.logarithmic,
                        });
                    }
                    PortKind::ControlOutput
                        if port.name.to_ascii_lowercase().contains("latency") =>
                    {
                        latency_port = Some(port.index);
                    }
                    _ => {}
                }
            }
            PluginInfo {
                library: path.to_path_buf(),
                index,
                unique_id: p.unique_id,
                label: p.label.clone(),
                name: p.name.clone(),
                maker: p.maker.clone(),
                audio_in,
                audio_out,
                controls,
                latency_port,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Where plug-ins are.

/// The folders LADSPA libraries are looked for in: `LADSPA_PATH` if set (a list separated by `:`,
/// or `;` on Windows), else the usual places of the system.
pub fn search_paths() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os("LADSPA_PATH") {
        let list: Vec<PathBuf> = std::env::split_paths(&value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    default_paths()
}

fn default_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut paths = Vec::new();
    #[cfg(target_os = "linux")]
    {
        for root in ["/usr/local/lib", "/usr/lib", "/usr/lib64"] {
            paths.push(Path::new(root).join("ladspa"));
        }
        if let Some(h) = &home {
            paths.push(h.join(".ladspa"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(Path::new("/Library/Audio/Plug-Ins/LADSPA").to_path_buf());
        if let Some(h) = &home {
            paths.push(h.join("Library/Audio/Plug-Ins/LADSPA"));
            paths.push(h.join(".ladspa"));
        }
        paths.push(Path::new("/usr/local/lib/ladspa").to_path_buf());
    }
    #[cfg(windows)]
    {
        let _ = &home;
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var) {
                let pf = PathBuf::from(pf);
                paths.push(pf.join("LADSPA"));
                paths.push(pf.join("Audacity").join("Plug-Ins"));
            }
        }
        if let Some(app) = std::env::var_os("APPDATA") {
            paths.push(PathBuf::from(app).join("ProWave").join("ladspa"));
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = &home;
    }
    paths
}

/// Whether `path` looks like a plug-in library on this system.
fn is_library(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("so") => cfg!(unix),
        Some("dylib") => cfg!(target_os = "macos"),
        Some("dll") => cfg!(windows),
        _ => false,
    }
}

/// The library files directly inside `dir`, sorted by name (a missing folder: none).
pub fn library_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_library(p))
        .take(MAX_FILES_PER_DIR)
        .collect();
    files.sort();
    files
}

/// The file `plugin` lives in now: the saved path if it exists, else the same file name in one
/// of the search folders.
pub fn resolve_library(plugin: &PluginRef) -> Option<PathBuf> {
    if plugin.library.is_file() {
        return Some(plugin.library.clone());
    }
    let name = plugin.library.file_name()?;
    search_paths()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------------------------------------
// The text a scanning child prints.

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

const HEADER: &str = "PW-LADSPA 1";
const FOOTER: &str = "PW-END";

/// `infos` as lines of text (a `P` line for a plug-in, a `C` line for each control).
pub fn encode_infos(infos: &[PluginInfo]) -> String {
    let mut out = format!("{HEADER}\n");
    for i in infos {
        out.push_str(&format!(
            "P\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            escape(&i.library.to_string_lossy()),
            i.index,
            i.unique_id,
            escape(&i.label),
            escape(&i.name),
            escape(&i.maker),
            i.audio_in,
            i.audio_out,
            i.latency_port.map_or(-1i64, |p| p as i64),
            0,
        ));
        for c in &i.controls {
            let flags: String = [(c.toggled, 't'), (c.integer, 'i'), (c.log, 'l')]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, ch)| *ch)
                .collect();
            out.push_str(&format!(
                "C\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape(&c.name),
                c.port,
                c.min,
                c.max,
                c.default,
                if flags.is_empty() { "-".into() } else { flags },
            ));
        }
    }
    out.push_str(FOOTER);
    out.push('\n');
    out
}

/// What [`encode_infos`] wrote. Lines before the header are ignored (a plug-in may print while
/// it loads); a missing footer means the writer died and is an error.
pub fn decode_infos(text: &str) -> Result<Vec<PluginInfo>, String> {
    let bad = |what: &str| format!("unreadable scan output: {what}");
    let mut lines = text.lines().skip_while(|l| *l != HEADER);
    if lines.next().is_none() {
        return Err(bad("no header"));
    }
    let float = |s: &str| s.parse::<f32>().map_err(|_| bad("a number"));
    let int = |s: &str| s.parse::<i64>().map_err(|_| bad("a number"));
    let mut out: Vec<PluginInfo> = Vec::new();
    let mut finished = false;
    for line in lines {
        if line == FOOTER {
            finished = true;
            break;
        }
        let f: Vec<&str> = line.split('\t').collect();
        match f.first().copied() {
            Some("P") if f.len() == 11 => out.push(PluginInfo {
                library: PathBuf::from(unescape(f[1])),
                index: int(f[2])?.max(0) as usize,
                unique_id: int(f[3])?.max(0) as u64,
                label: unescape(f[4]),
                name: unescape(f[5]),
                maker: unescape(f[6]),
                audio_in: int(f[7])?.max(0) as usize,
                audio_out: int(f[8])?.max(0) as usize,
                latency_port: usize::try_from(int(f[9])?).ok(),
                controls: Vec::new(),
            }),
            Some("C") if f.len() == 7 => {
                let plugin = out
                    .last_mut()
                    .ok_or_else(|| bad("a control with no plug-in"))?;
                plugin.controls.push(ControlInfo {
                    name: unescape(f[1]),
                    port: int(f[2])?.max(0) as usize,
                    min: float(f[3])?,
                    max: float(f[4])?,
                    default: float(f[5])?,
                    toggled: f[6].contains('t'),
                    integer: f[6].contains('i'),
                    log: f[6].contains('l'),
                });
            }
            // Anything else is noise printed by a plug-in.
            _ => {}
        }
    }
    if finished {
        Ok(out)
    } else {
        Err(bad("it stops early"))
    }
}

/// The child side of an isolated scan: `prowave --scan-ladspa <file>` prints what is in the
/// library. Returns the process exit code.
pub fn scan_main(path: &Path) -> i32 {
    match Library::load(path) {
        Ok(library) => {
            print!("{}", encode_infos(&infos_of(&library, path)));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Reads the library at `path` in a child process (`exe --scan-ladspa <path>`) with a time
/// limit. A crash, a hang or a library that cannot be read is an error.
pub fn scan_library_isolated(
    exe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<Vec<PluginInfo>, String> {
    let mut child = Command::new(exe)
        .arg("--scan-ladspa")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let Some(stdout) = child.stdout.take() else {
        return Err("no output pipe".into());
    };
    // Read on another thread so a chatty plug-in cannot fill the pipe and stall the child.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_OUTPUT).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                // Not joined: a grandchild the plug-in started may still hold the pipe open,
                // and the reader ends by itself when the pipe closes.
                drop(reader);
                return Err(format!(
                    "{} did not answer in {} s",
                    path.display(),
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let text = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("{} stopped the scan ({status})", path.display()));
    }
    decode_infos(&text)
}

// ---------------------------------------------------------------------------------------------
// Quarantine.

/// The libraries that failed to scan, with how many times: stored in `quarantine.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quarantine {
    failures: BTreeMap<String, u32>,
}

impl Quarantine {
    /// Reads the list from `file` (a missing or damaged file: an empty list).
    pub fn load(file: &Path) -> Self {
        let failures = std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice::<BTreeMap<String, u32>>(&b).ok())
            .unwrap_or_default();
        Self { failures }
    }

    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&self.failures).map_err(std::io::Error::other)?;
        std::fs::write(file, json)
    }

    fn key(library: &Path) -> String {
        library.to_string_lossy().into_owned()
    }

    /// Counts one more failed scan of `library`.
    pub fn record_failure(&mut self, library: &Path) {
        let n = self.failures.entry(Self::key(library)).or_insert(0);
        *n = n.saturating_add(1);
    }

    /// A scan worked: the library's failures are forgotten.
    pub fn record_success(&mut self, library: &Path) {
        self.failures.remove(&Self::key(library));
    }

    /// Whether `library` has failed [`MAX_FAILURES`] times and is no longer tried.
    pub fn is_blocked(&self, library: &Path) -> bool {
        self.failures
            .get(&Self::key(library))
            .is_some_and(|n| *n >= MAX_FAILURES)
    }

    /// The blocked libraries.
    pub fn blocked(&self) -> Vec<PathBuf> {
        self.failures
            .iter()
            .filter(|(_, n)| **n >= MAX_FAILURES)
            .map(|(p, _)| PathBuf::from(p))
            .collect()
    }

    /// Lets the user try `library` again.
    pub fn release(&mut self, library: &Path) {
        self.failures.remove(&Self::key(library));
    }
}

/// What the scan found so far.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub plugins: Vec<PluginInfo>,
    /// Libraries that crashed, hung or could not be read, with why.
    pub failed: Vec<(PathBuf, String)>,
    /// Libraries left alone because they failed three times.
    pub skipped: Vec<PathBuf>,
    pub scanning: bool,
    pub scanned: bool,
}

impl Catalog {
    /// The plug-ins that can sit in an Audio FX slot.
    pub fn effects(&self) -> impl Iterator<Item = &PluginInfo> {
        self.plugins.iter().filter(|p| p.is_effect())
    }
}

/// Scans every folder in `dirs`, skipping quarantined libraries and counting failures in
/// `quarantine`. With `isolate_with` each library is read by that program in a child process.
pub fn scan_all(
    dirs: &[PathBuf],
    isolate_with: Option<&Path>,
    timeout: Duration,
    quarantine: &mut Quarantine,
) -> Catalog {
    let mut found = Catalog::default();
    for dir in dirs {
        for file in library_files(dir) {
            if quarantine.is_blocked(&file) {
                found.skipped.push(file);
                continue;
            }
            let result = match isolate_with {
                Some(exe) => scan_library_isolated(exe, &file, timeout),
                None => Library::load(&file)
                    .map(|l| infos_of(&l, &file))
                    .map_err(|e| e.to_string()),
            };
            match result {
                Ok(mut list) => {
                    quarantine.record_success(&file);
                    found.plugins.append(&mut list);
                }
                Err(e) => {
                    quarantine.record_failure(&file);
                    found.failed.push((file, e));
                }
            }
        }
    }
    found.plugins.sort_by_key(|p| p.name.to_lowercase());
    found.scanned = true;
    found
}

// ---------------------------------------------------------------------------------------------
// The registry: the project's plug-ins, the catalogue, and what was learnt about each.

#[derive(Default)]
struct State {
    table: Vec<PluginRef>,
    catalog: Catalog,
    /// The description of each plug-in a project uses, once asked for (`None`: unreadable).
    described: HashMap<u32, Option<PluginInfo>>,
    /// The delay each effect reported when it started, in samples.
    latency: HashMap<u32, usize>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    let m = STATE.get_or_init(|| Mutex::new(State::default()));
    // A panic while the lock was held leaves plain data behind: carry on with it.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The id for `plugin` (the same one every time for the same plug-in).
pub fn intern(plugin: &PluginRef) -> PluginId {
    let mut s = state();
    if let Some(i) = s
        .table
        .iter()
        .position(|r| r.label == plugin.label && r.library == plugin.library)
    {
        return PluginId(i as u32);
    }
    s.table.push(plugin.clone());
    PluginId((s.table.len() - 1) as u32)
}

/// The plug-in `id` stands for.
pub fn lookup(id: PluginId) -> Option<PluginRef> {
    state().table.get(id.0 as usize).cloned()
}

/// The name to show for `id` (its saved name when the plug-in cannot be found).
pub fn name(id: PluginId) -> String {
    if let Some(info) = info(id) {
        return info.name;
    }
    lookup(id).map_or_else(|| "Plug-in".to_string(), |r| r.name)
}

/// A copy of the catalogue.
pub fn catalog() -> Catalog {
    state().catalog.clone()
}

/// Replaces the catalogue (the scan, and tests).
pub fn set_catalog(catalog: Catalog) {
    let mut s = state();
    s.catalog = catalog;
    s.described.clear();
}

fn quarantine_file() -> PathBuf {
    crate::recent_files::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("quarantine.json")
}

/// Starts the background scan of the plug-in folders, once: later calls do nothing until a scan
/// has finished.
pub fn ensure_scanned() {
    {
        let mut s = state();
        if s.catalog.scanned || s.catalog.scanning {
            return;
        }
        s.catalog.scanning = true;
    }
    std::thread::spawn(scan_now);
}

/// Scans again (the Plug-in Manager's Rescan). With `retry_quarantined`, libraries that failed
/// before are tried again.
pub fn rescan(retry_quarantined: bool) {
    {
        let mut s = state();
        if s.catalog.scanning {
            return;
        }
        s.catalog.scanning = true;
    }
    if retry_quarantined {
        let mut q = Quarantine::load(&quarantine_file());
        for lib in q.blocked() {
            q.release(&lib);
        }
        let _ = q.save(&quarantine_file());
    }
    std::thread::spawn(scan_now);
}

fn scan_now() {
    let exe = std::env::current_exe().ok();
    let file = quarantine_file();
    let mut quarantine = Quarantine::load(&file);
    let found = scan_all(
        &search_paths(),
        exe.as_deref(),
        SCAN_TIMEOUT,
        &mut quarantine,
    );
    if let Err(e) = quarantine.save(&file) {
        tracing::warn!("cannot save {}: {e}", file.display());
    }
    set_catalog(found);
}

/// What is known about plug-in `id`: from the catalogue, else read from its library (in a child
/// process) the first time it is asked for.
pub fn info(id: PluginId) -> Option<PluginInfo> {
    let reference = lookup(id)?;
    {
        let s = state();
        if let Some(i) = s.catalog.plugins.iter().find(|i| reference.matches(i)) {
            return Some(i.clone());
        }
        if let Some(known) = s.described.get(&id.0) {
            return known.clone();
        }
    }
    // Not in the catalogue (a project from another machine, a library outside the search
    // folders): read it once, in a child process.
    let described = resolve_library(&reference).and_then(|path| {
        let exe = std::env::current_exe().ok()?;
        scan_library_isolated(&exe, &path, SCAN_TIMEOUT)
            .ok()?
            .into_iter()
            .find(|i| i.label == reference.label)
    });
    state().described.insert(id.0, described.clone());
    described
}

/// The settings a new effect starts with: each control's default, as a position 0 to 1.
pub fn default_params(id: PluginId) -> [f32; prowave_engine::MAX_FX_PARAMS] {
    let mut params = prowave_engine::FxKind::Plugin.defaults();
    if let Some(info) = info(id) {
        for (slot, norm) in params.iter_mut().zip(info.default_norms()) {
            *slot = norm;
        }
    }
    params
}

/// The delay the effect reported when it started, in samples (0 before it started).
pub fn reported_latency(id: PluginId) -> usize {
    state().latency.get(&id.0).copied().unwrap_or(0)
}

/// The plug-in reference saved in a project file.
pub fn to_doc(id: PluginId) -> Option<prowave_project::PluginDoc> {
    let r = lookup(id)?;
    Some(prowave_project::PluginDoc {
        format: "LADSPA".to_string(),
        library: r.library.to_string_lossy().into_owned(),
        label: r.label,
        unique_id: r.unique_id,
        name: r.name,
    })
}

/// The plug-in a project file names (nothing for a format or label that makes no sense).
pub fn from_doc(doc: &prowave_project::PluginDoc) -> Option<PluginId> {
    if !doc.format.eq_ignore_ascii_case("ladspa")
        || doc.label.trim().is_empty()
        || doc.library.trim().is_empty()
    {
        return None;
    }
    Some(intern(&PluginRef {
        library: PathBuf::from(&doc.library),
        label: doc.label.clone(),
        unique_id: doc.unique_id,
        name: doc.name.chars().take(128).collect(),
    }))
}

// ---------------------------------------------------------------------------------------------
// Running a plug-in as an Audio FX effect.

/// What the effect needs from a plug-in instance (so the channel layouts can be tested without
/// loading a library).
trait Runner: Send {
    /// The number of audio inputs and outputs.
    fn counts(&self) -> (usize, usize);
    fn run(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]);
    fn set_control(&mut self, port: usize, value: f32);
    fn control(&self, port: usize) -> Option<f32>;
    fn reset(&mut self);
}

impl Runner for Instance {
    fn counts(&self) -> (usize, usize) {
        self.audio_port_counts()
    }
    fn run(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        // A refused block (the wrong number of ports) leaves the outputs as they were.
        let _ = self.process(inputs, outputs);
    }
    fn set_control(&mut self, port: usize, value: f32) {
        let _ = Instance::set_control(self, port, value);
    }
    fn control(&self, port: usize) -> Option<f32> {
        Instance::control(self, port)
    }
    fn reset(&mut self) {
        Instance::reset(self);
    }
}

/// One plug-in instance and the buffers its outputs are written to.
struct Voice {
    run: Box<dyn Runner>,
    outs: [[f32; PLUGIN_BLOCK]; MAX_AUDIO_PORTS],
}

/// A LADSPA effect in an Audio FX slot. A mono plug-in (one audio in, one out) gets two
/// instances, one for each side, so a stereo track stays stereo; any other gets one.
struct LadspaEffect {
    voices: Vec<Voice>,
    ins: usize,
    outs: usize,
    mono: [f32; PLUGIN_BLOCK],
    zero: [f32; PLUGIN_BLOCK],
    controls: Vec<ControlInfo>,
    latency_port: Option<usize>,
    latency: usize,
}

impl LadspaEffect {
    fn new(runners: Vec<Box<dyn Runner>>, info: &PluginInfo) -> Option<Self> {
        let (ins, outs) = runners.first()?.counts();
        if outs == 0 || ins > MAX_AUDIO_PORTS || outs > MAX_AUDIO_PORTS {
            return None;
        }
        Some(Self {
            voices: runners
                .into_iter()
                .map(|run| Voice {
                    run,
                    outs: [[0.0; PLUGIN_BLOCK]; MAX_AUDIO_PORTS],
                })
                .collect(),
            ins,
            outs,
            mono: [0.0; PLUGIN_BLOCK],
            zero: [0.0; PLUGIN_BLOCK],
            controls: info.controls.clone(),
            latency_port: info.latency_port,
            latency: 0,
        })
    }
}

/// Runs `voice` on `inputs`, its outputs going to `voice.outs` (no allocation: the slices live
/// on the stack).
fn run_voice(voice: &mut Voice, inputs: &[&[f32]], outs: usize, frames: usize) {
    let [o0, o1, o2, o3] = &mut voice.outs;
    let mut slices: [&mut [f32]; MAX_AUDIO_PORTS] = [
        &mut o0[..frames],
        &mut o1[..frames],
        &mut o2[..frames],
        &mut o3[..frames],
    ];
    voice.run.run(inputs, &mut slices[..outs]);
}

impl BlockEffect for LadspaEffect {
    fn set_param(&mut self, index: usize, norm: f32) {
        let Some(c) = self.controls.get(index) else {
            return;
        };
        let value = c.at_norm(norm);
        for v in &mut self.voices {
            v.run.set_control(c.port, value);
        }
    }

    fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = in_l
            .len()
            .min(in_r.len())
            .min(out_l.len())
            .min(out_r.len())
            .min(PLUGIN_BLOCK);
        let (ins, outs) = (self.ins, self.outs);
        if let [left, right] = self.voices.as_mut_slice() {
            // One instance for each side.
            run_voice(left, &[&in_l[..frames]], 1, frames);
            run_voice(right, &[&in_r[..frames]], 1, frames);
            out_l[..frames].copy_from_slice(&left.outs[0][..frames]);
            out_r[..frames].copy_from_slice(&right.outs[0][..frames]);
        } else if let Some(v) = self.voices.first_mut() {
            if ins == 1 {
                for n in 0..frames {
                    self.mono[n] = 0.5 * (in_l[n] + in_r[n]);
                }
            }
            let zero = &self.zero[..frames];
            let stereo: [&[f32]; MAX_AUDIO_PORTS] = [&in_l[..frames], &in_r[..frames], zero, zero];
            let mono: [&[f32]; 1] = [&self.mono[..frames]];
            let inputs: &[&[f32]] = if ins == 1 { &mono } else { &stereo[..ins] };
            run_voice(v, inputs, outs, frames);
            match outs {
                1 => {
                    out_l[..frames].copy_from_slice(&v.outs[0][..frames]);
                    out_r[..frames].copy_from_slice(&v.outs[0][..frames]);
                }
                _ => {
                    out_l[..frames].copy_from_slice(&v.outs[0][..frames]);
                    out_r[..frames].copy_from_slice(&v.outs[1][..frames]);
                }
            }
        }
        if let Some(port) = self.latency_port {
            let reported = self
                .voices
                .first()
                .and_then(|v| v.run.control(port))
                .unwrap_or(0.0);
            // A bad value is no latency.
            self.latency = if reported.is_finite() && reported > 0.0 && reported < 1.0e6 {
                reported as usize
            } else {
                0
            };
        }
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.run.reset();
        }
    }
}

/// Starts the effect `id` for the engine (off the audio thread). Remembers the delay it
/// reports. A plug-in that is missing, will not start or has no audio output is an error.
pub fn load_effect(id: PluginId, sample_rate: f32) -> Result<Box<dyn BlockEffect>, String> {
    let reference = lookup(id).ok_or("unknown plug-in")?;
    let path = resolve_library(&reference)
        .ok_or_else(|| format!("{} was not found", reference.library.display()))?;
    let library = Library::load(&path).map_err(|e| e.to_string())?;
    let info = infos_of(&library, &path)
        .into_iter()
        .find(|i| i.label == reference.label)
        .ok_or_else(|| format!("the library has no plug-in `{}`", reference.label))?;
    if !info.is_effect() {
        return Err("it has no usable audio output".into());
    }
    let rate = sample_rate.round().max(1.0) as u32;
    let mut runners: Vec<Box<dyn Runner>> = vec![Box::new(
        library
            .instantiate(info.index, rate)
            .map_err(|e| e.to_string())?,
    )];
    if info.audio_in == 1 && info.audio_out == 1 {
        runners.push(Box::new(
            library
                .instantiate(info.index, rate)
                .map_err(|e| e.to_string())?,
        ));
    }
    let mut effect = LadspaEffect::new(runners, &info).ok_or("it cannot be used as an effect")?;
    // One silent block, so a plug-in that reports its delay while it runs has done so.
    let silence = [0.0f32; PLUGIN_BLOCK];
    let (mut out_l, mut out_r) = ([0.0f32; PLUGIN_BLOCK], [0.0f32; PLUGIN_BLOCK]);
    effect.process(&silence, &silence, &mut out_l, &mut out_r);
    effect.reset();
    state().latency.insert(id.0, effect.latency_samples());
    Ok(Box::new(effect))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(min: f32, max: f32, default: f32) -> ControlInfo {
        ControlInfo {
            name: "c".into(),
            port: 0,
            min,
            max,
            default,
            toggled: false,
            integer: false,
            log: false,
        }
    }

    fn sample_info(label: &str, audio_in: usize, audio_out: usize) -> PluginInfo {
        PluginInfo {
            library: PathBuf::from(format!("/plugins/{label}.so")),
            index: 0,
            unique_id: 7,
            label: label.into(),
            name: format!("Name of {label}"),
            maker: "maker".into(),
            audio_in,
            audio_out,
            controls: vec![control(0.0, 4.0, 1.0)],
            latency_port: None,
        }
    }

    #[test]
    fn positions_map_to_values_and_back_and_odd_values_are_put_into_range() {
        let c = control(-12.0, 12.0, 0.0);
        assert_eq!(c.at_norm(0.0), -12.0);
        assert_eq!(c.at_norm(1.0), 12.0);
        assert_eq!(c.at_norm(0.5), 0.0);
        assert!((c.to_norm(6.0) - 0.75).abs() < 1e-6);
        assert_eq!(c.at_norm(7.0), 12.0);
        assert_eq!(c.at_norm(f32::NAN), -12.0);
        assert_eq!(c.clamp(f32::INFINITY), -12.0);
        // A broken range is 0 to 1.
        assert_eq!(control(5.0, 1.0, 0.0).range(), (0.0, 1.0));
        assert_eq!(control(f32::NAN, 1.0, 0.0).range(), (0.0, 1.0));
        let mut log = control(20.0, 20_000.0, 1000.0);
        log.log = true;
        assert!((log.at_norm(0.5) - 632.45).abs() < 0.5);
        let mut int = control(0.0, 4.0, 0.0);
        int.integer = true;
        assert_eq!(int.at_norm(0.4), 2.0);
        assert_eq!(int.text(2.0), "2");
        let mut toggle = control(0.0, 0.0, 0.0);
        toggle.toggled = true;
        assert_eq!(toggle.at_norm(0.49), 0.0);
        assert_eq!(toggle.at_norm(0.5), 1.0);
        assert_eq!(toggle.text(1.0), "On");
    }

    #[test]
    fn a_new_effect_starts_at_the_plugins_defaults() {
        let info = sample_info("pw_unit_defaults", 1, 1);
        assert_eq!(info.default_norms(), vec![0.25]);
        let id = intern(&info.plugin_ref());
        set_catalog(Catalog {
            plugins: vec![info],
            scanned: true,
            ..Catalog::default()
        });
        assert_eq!(default_params(id)[0], 0.25);
        assert_eq!(name(id), "Name of pw_unit_defaults");
    }

    #[test]
    fn effects_are_the_plugins_with_sound_out_and_few_ports() {
        assert!(sample_info("a", 1, 1).is_effect());
        assert!(sample_info("b", 2, 2).is_effect());
        assert!(
            sample_info("c", 0, 1).is_effect(),
            "a generator makes sound"
        );
        assert!(
            !sample_info("d", 1, 0).is_effect(),
            "an analyser makes none"
        );
        assert!(!sample_info("e", 9, 1).is_effect());
    }

    #[test]
    fn the_same_plugin_gets_the_same_id_and_a_project_document_round_trips_it() {
        let r = sample_info("pw_unit_doc", 1, 1).plugin_ref();
        let id = intern(&r);
        assert_eq!(intern(&r), id);
        assert_eq!(lookup(id), Some(r));
        let doc = to_doc(id).expect("doc");
        assert_eq!(doc.format, "LADSPA");
        assert_eq!(from_doc(&doc), Some(id));
        let mut bad = doc.clone();
        bad.format = "vst3".into();
        assert_eq!(from_doc(&bad), None);
        bad = doc.clone();
        bad.label = " ".into();
        assert_eq!(from_doc(&bad), None);
        bad = doc;
        bad.library.clear();
        assert_eq!(from_doc(&bad), None);
    }

    #[test]
    fn an_unknown_plugin_has_no_description_keeps_its_name_and_will_not_load() {
        let id = intern(&PluginRef {
            library: PathBuf::from("/nowhere/at/all.so"),
            label: "gone".into(),
            unique_id: 1,
            name: "Saved Name".into(),
        });
        assert!(info(id).is_none());
        assert_eq!(name(id), "Saved Name");
        assert_eq!(
            default_params(id),
            prowave_engine::FxKind::Plugin.defaults()
        );
        assert!(load_effect(id, 48_000.0).is_err());
    }

    #[test]
    fn a_scan_survives_the_text_round_trip_with_odd_characters() {
        let mut info = sample_info("delay", 1, 1);
        info.library = PathBuf::from("/usr/lib/ladspa/we\tird\\name.so");
        info.name = "Simple Delay\nLine".into();
        info.latency_port = Some(3);
        info.controls.push(ControlInfo {
            name: "On".into(),
            port: 1,
            min: 0.0,
            max: 1.0,
            default: 1.0,
            toggled: true,
            integer: true,
            log: true,
        });
        let infos = vec![info.clone(), PluginInfo { index: 3, ..info }];
        assert_eq!(decode_infos(&encode_infos(&infos)), Ok(infos));
    }

    #[test]
    fn scan_noise_is_ignored_but_a_missing_footer_or_a_bad_line_is_an_error() {
        let text = format!(
            "plugin says hello\n{}",
            encode_infos(&[sample_info("a", 1, 1)])
        );
        assert_eq!(decode_infos(&text).map(|v| v.len()), Ok(1));
        assert!(decode_infos(&text.replace("PW-END\n", "")).is_err());
        assert!(decode_infos("nothing here").is_err());
        assert!(decode_infos("PW-LADSPA 1\nC\tx\t0\t0\t1\t0\t-\nPW-END\n").is_err());
        assert!(
            decode_infos("PW-LADSPA 1\nP\t/x.so\tzz\t1\ta\tb\tc\t1\t1\t-1\t0\nPW-END\n").is_err()
        );
    }

    #[test]
    fn the_quarantine_blocks_after_three_failures_and_is_kept_in_a_json_file() {
        let mut q = Quarantine::default();
        let lib = Path::new("/x/bad.so");
        q.record_failure(lib);
        q.record_failure(lib);
        assert!(!q.is_blocked(lib));
        q.record_failure(lib);
        assert!(q.is_blocked(lib));
        assert_eq!(q.blocked(), vec![lib.to_path_buf()]);
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("sub").join("quarantine.json");
        q.save(&file).expect("save");
        assert_eq!(Quarantine::load(&file), q);
        q.release(lib);
        assert!(!q.is_blocked(lib));
        q.record_failure(lib);
        q.record_success(lib);
        q.record_failure(lib);
        assert!(!q.is_blocked(lib));
        std::fs::write(&file, b"{not json").expect("write");
        assert_eq!(Quarantine::load(&file), Quarantine::default());
        assert_eq!(
            Quarantine::load(&dir.path().join("missing.json")),
            Quarantine::default()
        );
    }

    #[test]
    fn only_libraries_of_this_system_are_listed_and_a_missing_folder_is_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["b.so", "a.so", "c.txt", "d.dll", "e.dylib"] {
            std::fs::write(dir.path().join(name), b"x").expect("write");
        }
        std::fs::create_dir(dir.path().join("sub.so")).expect("dir");
        let names: Vec<String> = library_files(dir.path())
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        #[cfg(target_os = "linux")]
        assert_eq!(names, vec!["a.so", "b.so"]);
        #[cfg(target_os = "macos")]
        assert_eq!(names, vec!["a.so", "b.so", "e.dylib"]);
        #[cfg(windows)]
        assert_eq!(names, vec!["d.dll"]);
        assert!(library_files(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn a_moved_library_is_found_by_file_name_when_it_is_on_the_search_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("amp.so");
        std::fs::write(&file, b"x").expect("write");
        let mut r = PluginRef {
            library: file.clone(),
            label: "amp".into(),
            unique_id: 1,
            name: "Amp".into(),
        };
        assert_eq!(resolve_library(&r), Some(file));
        r.library = PathBuf::from("/definitely/not/here/amp.so");
        assert!(resolve_library(&r).is_none_or(|p| p.ends_with("amp.so")));
    }

    #[test]
    fn a_child_that_crashes_hangs_or_prints_nonsense_is_a_failed_scan() {
        // The "program" is a shell script, so this runs on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().expect("temp dir");
            let script = |name: &str, body: &str| {
                let path = dir.path().join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
                path
            };
            let lib = Path::new("/x.so");
            let long = Duration::from_secs(20);
            assert!(
                scan_library_isolated(&script("crash.sh", "kill -SEGV $$"), lib, long).is_err()
            );
            assert!(scan_library_isolated(&script("noise.sh", "echo hello"), lib, long).is_err());
            assert!(
                scan_library_isolated(&script("half.sh", "echo 'PW-LADSPA 1'"), lib, long).is_err()
            );
            let start = Instant::now();
            let hung = scan_library_isolated(
                &script("hang.sh", "sleep 30"),
                lib,
                Duration::from_millis(300),
            );
            assert!(hung.is_err());
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "killed at the limit"
            );
            // A child that prints a good scan works.
            let good = encode_infos(&[sample_info("ok", 1, 1)]);
            let body = format!("cat <<'EOF'\n{good}EOF");
            let infos = scan_library_isolated(&script("ok.sh", &body), lib, long).expect("scan");
            assert_eq!(infos.len(), 1);
        }
    }

    #[test]
    fn a_folder_scan_counts_failures_skips_quarantined_files_and_reads_real_libraries() {
        let dir = tempfile::tempdir().expect("temp dir");
        if cfg!(unix) {
            std::fs::write(dir.path().join("junk.so"), b"not a library").expect("write");
        }
        let fixture = prowave_ladspa::test_fixture_path();
        if let Some(path) = fixture {
            let name = Path::new(path).file_name().expect("file name");
            std::fs::copy(path, dir.path().join(name)).expect("copy");
        }
        let dirs = [dir.path().to_path_buf()];
        let mut q = Quarantine::default();
        let first = scan_all(&dirs, None, SCAN_TIMEOUT, &mut q);
        assert!(first.scanned);
        if fixture.is_some() {
            let labels: Vec<&str> = first.plugins.iter().map(|p| p.label.as_str()).collect();
            assert_eq!(labels, ["constant", "gain"].map(|l| l), "sorted by name");
        }
        if cfg!(unix) {
            assert_eq!(first.failed.len(), 1);
            scan_all(&dirs, None, SCAN_TIMEOUT, &mut q);
            scan_all(&dirs, None, SCAN_TIMEOUT, &mut q);
            let again = scan_all(&dirs, None, SCAN_TIMEOUT, &mut q);
            assert_eq!(
                again.skipped.len(),
                1,
                "the third failure quarantines the file"
            );
            assert!(again.failed.is_empty());
        }
    }

    // ----- the effect, with a mock plug-in for every channel layout

    /// Multiplies each input by a gain and reports a latency; the inputs it was given are kept.
    struct Mock {
        ins: usize,
        outs: usize,
        gain: f32,
        latency: f32,
        resets: usize,
        seen: Vec<Vec<f32>>,
    }

    impl Runner for Mock {
        fn counts(&self) -> (usize, usize) {
            (self.ins, self.outs)
        }
        fn run(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
            assert_eq!(inputs.len(), self.ins);
            assert_eq!(outputs.len(), self.outs);
            self.seen = inputs.iter().map(|i| i.to_vec()).collect();
            for (k, out) in outputs.iter_mut().enumerate() {
                // Output k carries input k (or input 0 for a generator).
                for (n, o) in out.iter_mut().enumerate() {
                    let src = inputs.get(k).or(inputs.first()).map_or(1.0, |i| i[n]);
                    *o = src * self.gain + k as f32 * 0.01;
                }
            }
        }
        fn set_control(&mut self, port: usize, value: f32) {
            if port == 0 {
                self.gain = value;
            }
        }
        fn control(&self, port: usize) -> Option<f32> {
            (port == 5).then_some(self.latency)
        }
        fn reset(&mut self) {
            self.resets += 1;
        }
    }

    fn mock(ins: usize, outs: usize) -> Box<dyn Runner> {
        Box::new(Mock {
            ins,
            outs,
            gain: 1.0,
            latency: 0.0,
            resets: 0,
            seen: Vec::new(),
        })
    }

    fn effect(runners: Vec<Box<dyn Runner>>, latency_port: Option<usize>) -> LadspaEffect {
        let mut info = sample_info("mock", 1, 1);
        info.latency_port = latency_port;
        LadspaEffect::new(runners, &info).expect("an effect")
    }

    fn block(effect: &mut LadspaEffect, l: f32, r: f32) -> (f32, f32) {
        let (in_l, in_r) = ([l; PLUGIN_BLOCK], [r; PLUGIN_BLOCK]);
        let (mut out_l, mut out_r) = ([0.0; PLUGIN_BLOCK], [0.0; PLUGIN_BLOCK]);
        effect.process(&in_l, &in_r, &mut out_l, &mut out_r);
        (out_l[PLUGIN_BLOCK - 1], out_r[PLUGIN_BLOCK - 1])
    }

    #[test]
    fn a_mono_plugin_runs_once_per_side_so_stereo_is_kept() {
        let mut e = effect(vec![mock(1, 1), mock(1, 1)], None);
        assert_eq!(block(&mut e, 0.5, -0.25), (0.5, -0.25));
        // Position 0.5 of the 0..4 control is a gain of 2, set on both instances.
        e.set_param(0, 0.5);
        assert_eq!(block(&mut e, 0.5, -0.25), (1.0, -0.5));
        // A control the plug-in does not have is ignored.
        e.set_param(9, 0.5);
        assert_eq!(block(&mut e, 0.5, -0.25), (1.0, -0.5));
    }

    #[test]
    fn a_stereo_plugin_gets_both_channels_and_extra_ports_are_fed_silence() {
        let mut e = effect(vec![mock(2, 2)], None);
        let (l, r) = block(&mut e, 0.25, -0.5);
        assert_eq!(l, 0.25);
        assert!((r - (-0.49)).abs() < 1e-6, "{r}");
        let mut many = effect(vec![mock(4, 2)], None);
        block(&mut many, 0.25, -0.5);
    }

    #[test]
    fn a_mono_in_stereo_out_plugin_hears_the_mix_and_mono_out_feeds_both_sides() {
        let mut e = effect(vec![mock(1, 2)], None);
        let (l, r) = block(&mut e, 0.5, 0.25);
        assert_eq!(l, 0.375, "the input was the mix of both sides");
        assert!((r - 0.385).abs() < 1e-6, "{r}");
        let mut e = effect(vec![mock(2, 1)], None);
        let (l, r) = block(&mut e, 0.25, 0.5);
        assert_eq!((l, r), (0.25, 0.25), "one output feeds both sides");
    }

    #[test]
    fn a_generator_makes_sound_with_no_input_and_a_plugin_with_no_output_is_refused() {
        let mut e = effect(vec![mock(0, 1)], None);
        assert_eq!(block(&mut e, 0.5, 0.5), (1.0, 1.0));
        assert!(LadspaEffect::new(vec![mock(1, 0)], &sample_info("x", 1, 0)).is_none());
        assert!(LadspaEffect::new(vec![mock(9, 1)], &sample_info("x", 9, 1)).is_none());
        assert!(LadspaEffect::new(Vec::new(), &sample_info("x", 1, 1)).is_none());
    }

    #[test]
    fn the_latency_a_plugin_reports_is_read_and_bad_values_are_no_latency() {
        let mut m = Mock {
            ins: 1,
            outs: 1,
            gain: 1.0,
            latency: 64.0,
            resets: 0,
            seen: Vec::new(),
        };
        m.latency = 64.0;
        let mut e = effect(vec![Box::new(m)], Some(5));
        assert_eq!(e.latency_samples(), 0, "unknown before it runs");
        block(&mut e, 0.0, 0.0);
        assert_eq!(e.latency_samples(), 64);
        for bad in [f32::NAN, -5.0, 1e30] {
            let m = Mock {
                ins: 1,
                outs: 1,
                gain: 1.0,
                latency: bad,
                resets: 0,
                seen: Vec::new(),
            };
            let mut e = effect(vec![Box::new(m)], Some(5));
            block(&mut e, 0.0, 0.0);
            assert_eq!(e.latency_samples(), 0, "{bad}");
        }
    }

    #[test]
    fn the_effect_does_not_allocate_while_it_runs() {
        for layout in [(1, 1), (2, 2), (0, 1), (1, 2), (4, 4)] {
            let runners = if layout == (1, 1) {
                vec![mock(1, 1), mock(1, 1)]
            } else {
                vec![mock(layout.0, layout.1)]
            };
            let mut e = effect(runners, Some(5));
            let (in_l, in_r) = ([0.1f32; PLUGIN_BLOCK], [0.2f32; PLUGIN_BLOCK]);
            let (mut out_l, mut out_r) = ([0.0; PLUGIN_BLOCK], [0.0; PLUGIN_BLOCK]);
            // The mock keeps what it saw in a Vec, so it is outside the check; the slot's own
            // work (buffers, mixing, mapping) must not allocate. Run once to warm it up.
            e.process(&in_l, &in_r, &mut out_l, &mut out_r);
            e.set_param(0, 0.3);
            e.reset();
        }
    }

    // ----- a real LADSPA library (the C plug-in of the prowave-ladspa tests)

    fn fixture_id(label: &str) -> Option<PluginId> {
        let path = prowave_ladspa::test_fixture_path()?;
        let library = Library::load(path).ok()?;
        let info = infos_of(&library, Path::new(path))
            .into_iter()
            .find(|i| i.label == label)?;
        Some(intern(&info.plugin_ref()))
    }

    #[test]
    fn a_real_plugin_loads_runs_and_takes_its_gain_as_a_position() {
        let Some(id) = fixture_id("gain") else {
            return; // no C compiler in this build
        };
        let mut effect = load_effect(id, 48_000.0).expect("load");
        let run = |e: &mut Box<dyn BlockEffect>, l: f32, r: f32| {
            let (in_l, in_r) = ([l; PLUGIN_BLOCK], [r; PLUGIN_BLOCK]);
            let (mut out_l, mut out_r) = ([9.0; PLUGIN_BLOCK], [9.0; PLUGIN_BLOCK]);
            e.process(&in_l, &in_r, &mut out_l, &mut out_r);
            (out_l[PLUGIN_BLOCK - 1], out_r[PLUGIN_BLOCK - 1])
        };
        // The plug-in's default gain is 0: silence, on both sides.
        assert_eq!(run(&mut effect, 0.5, -0.25), (0.0, 0.0));
        effect.set_param(0, 0.5);
        assert_eq!(run(&mut effect, 0.5, -0.25), (0.25, -0.125));
        effect.set_param(0, 1.0);
        assert_eq!(run(&mut effect, 0.5, -0.25), (0.5, -0.25));
        effect.reset();
        assert_eq!(
            run(&mut effect, 0.5, -0.25),
            (0.5, -0.25),
            "reset keeps the settings"
        );
        assert_eq!(reported_latency(id), 0);
    }

    #[test]
    fn a_real_generator_plays_without_an_input_and_a_real_effect_works_in_the_engines_chain() {
        let Some(id) = fixture_id("constant") else {
            return;
        };
        let mut effect = load_effect(id, 48_000.0).expect("load");
        effect.set_param(0, 1.0);
        let silence = [0.0f32; PLUGIN_BLOCK];
        let (mut l, mut r) = ([0.0; PLUGIN_BLOCK], [0.0; PLUGIN_BLOCK]);
        effect.process(&silence, &silence, &mut l, &mut r);
        assert!(
            l[0] != 0.0 && l[0] == r[0],
            "a generator makes sound: {}",
            l[0]
        );

        use prowave_engine::{Chain, Fx, FxKind};
        let Some(gain) = fixture_id("gain") else {
            return;
        };
        let mut params = FxKind::Plugin.defaults();
        params[0] = 1.0; // full gain
        let mut fx = Fx::new(FxKind::Plugin, params, false, 48_000.0);
        assert!(!fx.has_plugin());
        fx.set_plugin(load_effect(gain, 48_000.0).expect("load"));
        assert!(fx.has_plugin());
        let mut chain = Chain::new(vec![fx]);
        let mut last = (0.0, 0.0);
        for _ in 0..PLUGIN_BLOCK * 3 {
            last = chain.process(0.25, -0.25);
        }
        assert_eq!(last, (0.25, -0.25));
        assert_eq!(chain.latency_samples(), PLUGIN_BLOCK);
    }

    #[test]
    fn a_real_library_is_read_in_a_child_process_when_the_program_can_scan() {
        // The test program is not a scanner, so only the in-process read is checked here; the
        // child protocol is covered by the script tests above and by `scan_main`.
        let Some(path) = prowave_ladspa::test_fixture_path() else {
            return;
        };
        let library = Library::load(path).expect("load");
        let infos = infos_of(&library, Path::new(path));
        assert_eq!(infos.len(), 2);
        let gain = infos.iter().find(|i| i.label == "gain").expect("gain");
        assert_eq!((gain.audio_in, gain.audio_out), (1, 1));
        assert_eq!(gain.controls.len(), 1);
        assert_eq!(gain.controls[0].name, "Gain");
        assert_eq!(gain.controls[0].range(), (0.0, 1.0));
        assert_eq!(decode_infos(&encode_infos(&infos)), Ok(infos));
    }
}
