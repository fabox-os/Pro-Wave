//! Vamp analysis plug-ins in the app: the Plug-in Manager's catalogue of installed analysers.
//!
//! Vamp libraries are native code, so each one is listed by a copy of this program started with
//! `--scan-vamp <file>` and a 15-second time limit. A library that crashes, hangs or cannot be
//! read three times is left alone in `vamp-quarantine.json`, the same rule LADSPA uses. This phase
//! is catalogue-only: analysers are shown in the Plug-in Manager but are not loaded into a track.

use crate::plugins;
use prowave_vamp::{InputDomain, Library, PluginInfo};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// The most library files looked at in one folder.
const MAX_FILES_PER_DIR: usize = 4096;
/// The most output taken from a scanning child.
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
const HEADER: &str = "PW-VAMP 1";
const FOOTER: &str = "PW-END";

/// The folders Vamp libraries are looked for in: `VAMP_PATH` if set, else the system's places.
pub fn search_paths() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os("VAMP_PATH") {
        let list: Vec<PathBuf> = std::env::split_paths(&value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut paths = Vec::new();
    #[cfg(target_os = "linux")]
    {
        for root in ["/usr/local/lib", "/usr/lib", "/usr/lib64"] {
            paths.push(Path::new(root).join("vamp"));
        }
        if let Some(h) = &home {
            paths.push(h.join(".vamp"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(Path::new("/Library/Audio/Plug-Ins/Vamp").to_path_buf());
        if let Some(h) = &home {
            paths.push(h.join("Library/Audio/Plug-Ins/Vamp"));
            paths.push(h.join(".vamp"));
        }
        paths.push(Path::new("/usr/local/lib/vamp").to_path_buf());
        paths.push(Path::new("/opt/homebrew/lib/vamp").to_path_buf());
    }
    #[cfg(windows)]
    {
        let _ = &home;
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var) {
                paths.push(PathBuf::from(pf).join("Vamp"));
            }
        }
        if let Some(app) = std::env::var_os("APPDATA") {
            paths.push(PathBuf::from(app).join("ProWave").join("vamp"));
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = &home;
    }
    paths
}

/// Whether `path` looks like a shared library on this system.
fn is_library(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("so") if cfg!(unix)
    ) || matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("dylib") if cfg!(target_os = "macos")
    ) || matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("dll") if cfg!(windows)
    )
}

/// The library files directly inside `dir`, sorted by name.
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

/// The catalog fields as lines of text, for the scanning child to print.
pub fn encode_infos(infos: &[PluginInfo]) -> String {
    let mut out = format!("{HEADER}\n");
    for i in infos {
        out.push_str(&format!(
            "V\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            escape(&i.identifier),
            escape(&i.name),
            escape(&i.maker),
            escape(&i.description),
            escape(&i.copyright),
            i.api_version,
            i.plugin_version,
            if i.input_domain == InputDomain::Time {
                0
            } else {
                1
            },
        ));
    }
    out.push_str(FOOTER);
    out.push('\n');
    out
}

/// Reads the lines [`encode_infos`] wrote. Lines before the header are ignored, because a
/// plug-in may print while it loads; a missing footer means the writer died.
pub fn decode_infos(text: &str) -> Result<Vec<PluginInfo>, String> {
    let bad = |what: &str| format!("unreadable Vamp scan output: {what}");
    let mut lines = text.lines().skip_while(|l| *l != HEADER);
    if lines.next().is_none() {
        return Err(bad("no header"));
    }
    let mut out = Vec::new();
    let mut finished = false;
    for line in lines {
        if line == FOOTER {
            finished = true;
            break;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.first().copied() != Some("V") || f.len() != 9 {
            continue; // noise printed by a plug-in
        }
        let domain = match f[8] {
            "0" => InputDomain::Time,
            "1" => InputDomain::Frequency,
            _ => return Err(bad("an input domain")),
        };
        out.push(PluginInfo {
            api_version: f[6].parse().map_err(|_| bad("a number"))?,
            identifier: unescape(f[1]),
            name: unescape(f[2]),
            description: unescape(f[4]),
            maker: unescape(f[3]),
            copyright: unescape(f[5]),
            plugin_version: f[7].parse().map_err(|_| bad("a number"))?,
            input_domain: domain,
            // Parameters, outputs and programs need a live instance; they are not part of a
            // catalogue scan. The app does not show them yet.
            parameters: Vec::new(),
            programs: Vec::new(),
        });
    }
    if finished {
        Ok(out)
    } else {
        Err(bad("it stops early"))
    }
}

/// The child side of an isolated scan: `prowave --scan-vamp <file>`.
pub fn scan_main(path: &Path) -> i32 {
    match Library::load(path) {
        Ok(library) => {
            print!("{}", encode_infos(library.plugins()));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Reads one library in a child process with a time limit; a crash, hang or unreadable file is
/// an error. The reader runs on another thread so a chatty plug-in cannot stall the child.
pub fn scan_library_isolated(
    exe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<Vec<PluginInfo>, String> {
    let mut child = Command::new(exe)
        .arg("--scan-vamp")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let Some(stdout) = child.stdout.take() else {
        return Err("no output pipe".into());
    };
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

/// Libraries that failed to scan, with how many times; shared shape with LADSPA's quarantine but
/// kept in its own file so a broken Vamp analyser cannot stop LADSPA effects being scanned.
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

    /// Writes the list back to `file`.
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

    /// Whether `library` has failed `plugins::MAX_FAILURES` times and is no longer tried.
    pub fn is_blocked(&self, library: &Path) -> bool {
        self.failures
            .get(&Self::key(library))
            .is_some_and(|n| *n >= plugins::MAX_FAILURES)
    }

    /// The blocked libraries.
    pub fn blocked(&self) -> Vec<PathBuf> {
        self.failures
            .iter()
            .filter(|(_, n)| **n >= plugins::MAX_FAILURES)
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

/// Scans every folder in `dirs` in the child `exe`, skipping quarantined libraries.
pub fn scan_all(
    dirs: &[PathBuf],
    exe: &Path,
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
            match scan_library_isolated(exe, &file, timeout) {
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

#[derive(Default)]
struct State {
    catalog: Catalog,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    let m = STATE.get_or_init(|| Mutex::new(State::default()));
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A copy of the catalogue.
pub fn catalog() -> Catalog {
    state().catalog.clone()
}

/// Replaces the catalogue (the scan, and tests).
pub fn set_catalog(found: Catalog) {
    state().catalog = found;
}

fn quarantine_file() -> PathBuf {
    crate::recent_files::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("vamp-quarantine.json")
}

/// Starts the background scan of the Vamp folders, once.
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

/// Scans again (the Plug-in Manager's Rescan); `retry_quarantined` releases blocked files.
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
    let Ok(exe) = std::env::current_exe() else {
        set_catalog(Catalog::default());
        return;
    };
    let file = quarantine_file();
    let mut quarantine = Quarantine::load(&file);
    let found = scan_all(
        &search_paths(),
        &exe,
        plugins::SCAN_TIMEOUT,
        &mut quarantine,
    );
    if let Err(e) = quarantine.save(&file) {
        tracing::warn!("cannot save {}: {e}", file.display());
    }
    set_catalog(found);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyser(name: &str) -> PluginInfo {
        PluginInfo {
            api_version: 2,
            identifier: format!("vamp-example\t{name}"),
            name: name.into(),
            description: "A test\nanalyser".into(),
            maker: "Pro Wave".into(),
            plugin_version: 1,
            copyright: "CC0".into(),
            input_domain: InputDomain::Frequency,
            parameters: Vec::new(),
            programs: Vec::new(),
        }
    }

    #[test]
    fn scan_output_round_trips_escaped_text() {
        let one = analyser("RMS\tlevel");
        let text = encode_infos(std::slice::from_ref(&one));
        assert!(text.starts_with(HEADER));
        assert!(text.trim_end().ends_with(FOOTER));
        let read = decode_infos(&format!("noise from the library\n{text}")).expect("decode");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, one.name);
        assert_eq!(read[0].identifier, one.identifier);
        assert_eq!(read[0].description, one.description);
        assert_eq!(read[0].input_domain, InputDomain::Frequency);
    }

    #[test]
    fn a_cut_short_scan_is_an_error() {
        let text = encode_infos(&[analyser("RMS")]);
        let cut = text.trim_end().trim_end_matches(FOOTER);
        assert!(decode_infos(cut).is_err());
    }

    #[test]
    fn only_shared_libraries_are_listed() {
        // Each platform's shared-library extension is listed, and nothing else.
        let lib = if cfg!(windows) {
            "vamp.dll"
        } else if cfg!(target_os = "macos") {
            "libvamp.dylib"
        } else {
            "libvamp.so"
        };
        assert!(is_library(Path::new(lib)), "{lib} is a library here");
        assert!(!is_library(Path::new("readme.txt")));
    }

    #[test]
    fn a_real_fixture_round_trips_through_the_scan_protocol() {
        let Some(path) = prowave_vamp::test_fixture_path() else {
            return; // a platform without a C compiler builds no fixture
        };
        let library = Library::load(path).expect("the test analyser loads");
        let read = decode_infos(&encode_infos(library.plugins())).expect("decode real metadata");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].identifier, "vamp_test_rms");
        assert_eq!(read[0].name, "Test RMS");
        assert_eq!(read[0].maker, "Pro Wave");
        assert_eq!(read[0].description, "Mean absolute level");
    }
}
