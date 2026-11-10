//! Updates: Pro Wave looks for a newer release on GitHub, tells the user, and can install it.
//!
//! * The check asks GitHub's public releases list for the newest release (one small request,
//!   on a background thread, nothing about the user is sent) and compares its version with
//!   the running one. It runs when the program starts, at most every six hours, unless the
//!   user turned it off in Help; Help > Check for Updates… always checks.
//! * Installing happens in two steps. First the new version is downloaded, checked against the
//!   SHA-256 that GitHub publishes for the release file, and made ready next to the current one
//!   (`….new`) while Pro Wave keeps running. Then, when the user says so, Pro Wave closes, and
//!   a helper (this same program started with `--apply-update`) waits until it has closed,
//!   swaps the new version in with two renames, and opens it. The old version stays as `….old`
//!   until the new one has been used once, so a bad update can be undone by hand, and the swap
//!   is undone by itself if it fails half way.
//! * Each kind of install gets the file made for it: the `.exe` zip on Windows, the `.app` zip on
//!   macOS, the AppImage, the tarball's program, and for `.deb` and `.rpm` installs that very
//!   package, installed with the system's package manager (the system asks for the password).
//!   A Flatpak bundle, and packages when the system cannot ask for a password, are downloaded
//!   and checked, and the command that installs them is shown.
//! * Folders the user cannot write to only get the notice and a button to the download page.

use crate::i18n::{tr, trf};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The GitHub repository releases come from.
pub const REPO: &str = "fabox-os/Pro-Wave";
/// How long to wait between two automatic checks.
const CHECK_EVERY_SECS: u64 = 6 * 60 * 60;
/// The most a release file may weigh (a safety limit; the packages are about 6 to 12 MB).
const MAX_DOWNLOAD: u64 = 400 * 1024 * 1024;

/// A file of a release.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The SHA-256 GitHub publishes for the file, in lower case hex.
    pub sha256: Option<String>,
}

/// A release newer than the running program.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    /// The file for this system that can be installed from the program, if there is one.
    pub asset: Option<Asset>,
}

// ------------------------------------------------------------------ versions and releases

/// `1.2.3`, `v1.2.3` or `1.2.3-beta` as numbers.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `candidate` is a newer version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The kinds of release file that can be installed from the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Windows,
    Mac,
    AppImage,
    Tarball,
    Deb,
    Rpm,
    Flatpak,
}

impl Kind {
    /// Whether `name` is the release file of this kind.
    pub fn matches(self, name: &str) -> bool {
        match self {
            Kind::Windows => name == "prowave-windows-x86_64.zip",
            Kind::Mac => name == "prowave-macos-universal.zip",
            Kind::AppImage => name.starts_with("Pro_Wave-") && name.ends_with("-x86_64.AppImage"),
            Kind::Tarball => name == "prowave-linux-x86_64.tar.gz",
            Kind::Deb => name.starts_with("prowave-studio_") && name.ends_with("_amd64.deb"),
            Kind::Rpm => name.starts_with("prowave-studio-") && name.ends_with(".x86_64.rpm"),
            Kind::Flatpak => {
                name.starts_with("prowave-studio-") && name.ends_with("-x86_64.flatpak")
            }
        }
    }

    /// A packaged install: a package manager owns the files.
    fn is_package(self) -> bool {
        matches!(self, Kind::Deb | Kind::Rpm | Kind::Flatpak)
    }

    /// The word the helper is told.
    fn word(self) -> &'static str {
        match self {
            Kind::Windows => "windows",
            Kind::Mac => "mac",
            Kind::AppImage => "appimage",
            Kind::Tarball => "tarball",
            Kind::Deb => "deb",
            Kind::Rpm => "rpm",
            Kind::Flatpak => "flatpak",
        }
    }

    fn from_word(word: &str) -> Option<Kind> {
        [
            Kind::Windows,
            Kind::Mac,
            Kind::AppImage,
            Kind::Tarball,
            Kind::Deb,
            Kind::Rpm,
            Kind::Flatpak,
        ]
        .into_iter()
        .find(|k| k.word() == word)
    }
}

/// What an update replaces: how this copy of Pro Wave was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    /// The thing that is replaced or started again: the `.exe`, the `.app` folder, the AppImage,
    /// the program of the tarball, or, for packages, the program the package installed.
    pub path: PathBuf,
}

impl Target {
    fn new(kind: Kind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }
}

/// The `.app` folder a macOS program lives in.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|a| a.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// How a copy of Pro Wave at `exe` can be updated, from the facts about it: the operating
/// system, the AppImage that is running (if any), whether it is a Flatpak and which package
/// manager owns the program (`Kind::Deb` or `Kind::Rpm`). `None` when it is not a layout this
/// knows.
pub fn detect(
    os: &str,
    exe: &Path,
    appimage: Option<&Path>,
    flatpak: bool,
    package: Option<Kind>,
) -> Option<Target> {
    match os {
        "windows" => Some(Target::new(Kind::Windows, exe)),
        "macos" => app_bundle(exe).map(|app| Target::new(Kind::Mac, app)),
        "linux" => {
            if flatpak {
                return Some(Target::new(Kind::Flatpak, exe));
            }
            if let Some(image) = appimage {
                return Some(Target::new(Kind::AppImage, image));
            }
            // The system's own folders belong to a package manager, if one owns the program.
            let system = ["/usr/", "/opt/", "/app/", "/snap/", "/bin/", "/sbin/"];
            if system.iter().any(|s| exe.starts_with(s)) {
                return match package {
                    Some(kind @ (Kind::Deb | Kind::Rpm)) => Some(Target::new(kind, exe)),
                    _ => None,
                };
            }
            exe.file_name()
                .is_some_and(|n| n == "prowave")
                .then(|| Target::new(Kind::Tarball, exe))
        }
        _ => None,
    }
}

/// Which package manager owns the program at `exe` (Linux): the one that knows the file.
fn owning_package(exe: &Path) -> Option<Kind> {
    let owns = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .arg(exe)
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("prowave-studio")
            })
    };
    if owns("dpkg", &["-S"]) {
        Some(Kind::Deb)
    } else if owns("rpm", &["-qf", "--qf", "%{NAME}\n"]) {
        Some(Kind::Rpm)
    } else {
        None
    }
}

/// Whether files can be made next to `path`.
fn folder_is_writable(path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    let probe = dir.join(".prowave-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// What an update would replace here, if this copy can be updated by the program.
pub fn current_target() -> Option<Target> {
    static TARGET: OnceLock<Option<Target>> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let image = std::env::var_os("APPIMAGE")
                .map(PathBuf::from)
                .filter(|p| p.is_file());
            let package = (std::env::consts::OS == "linux")
                .then(|| owning_package(&exe))
                .flatten();
            let target = detect(
                std::env::consts::OS,
                &exe,
                image.as_deref(),
                std::env::var_os("FLATPAK_ID").is_some(),
                package,
            )?;
            // A package is installed by the package manager, which does not need the folder to
            // be writable; everything else is replaced by this program.
            (target.kind.is_package() || folder_is_writable(&target.path)).then_some(target)
        })
        .clone()
}

/// The newest release in GitHub's answer when it is newer than `current`.
pub fn parse_releases(
    json: &str,
    current: &str,
    kind: Option<Kind>,
) -> Result<Option<Release>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = value
        .as_array()
        .ok_or_else(|| "unexpected answer from GitHub".to_string())?;
    // The list is newest first; drafts and prereleases are not offered.
    let newest = list
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            // Only plain `1.2.3` tags: a `-rc1` build is not the version it is a candidate for.
            if tag.contains('-') {
                return None;
            }
            parse_version(tag).map(|v| (v, r))
        })
        .max_by_key(|(v, _)| *v);
    let Some((_, release)) = newest else {
        return Ok(None);
    };
    let version = release["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let asset = release["assets"].as_array().and_then(|assets| {
        assets
            .iter()
            .find(|a| kind.is_some_and(|k| a["name"].as_str().is_some_and(|name| k.matches(name))))
            .and_then(|a| {
                Some(Asset {
                    name: a["name"].as_str()?.to_string(),
                    url: a["browser_download_url"].as_str()?.to_string(),
                    size: a["size"].as_u64().unwrap_or(0),
                    sha256: a["digest"]
                        .as_str()
                        .and_then(|d| d.strip_prefix("sha256:"))
                        .map(str::to_lowercase),
                })
            })
    });
    Ok(Some(Release {
        notes: release["body"].as_str().unwrap_or_default().to_string(),
        page: release["html_url"].as_str().map_or_else(
            || format!("https://github.com/{REPO}/releases"),
            str::to_string,
        ),
        version,
        asset,
    }))
}

// ------------------------------------------------------------------ network

fn agent(timeout: Duration) -> ureq::Agent {
    // The system's own list of trusted certificate authorities, not a built-in one: antivirus
    // programs that scan HTTPS (Norton, Kaspersky, ESET...) install their own authority there,
    // and a built-in list would call every connection to GitHub untrusted.
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .user_agent(format!("ProWave/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Asks GitHub for the newest release; `Some` when it is newer than the running program.
fn fetch_latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=10");
    // A slow or flaky connection (or a slow first connection through a proxy or antivirus)
    // gets three tries of 30 seconds before the check gives up.
    let mut last = String::new();
    let mut text = None;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let got = agent(Duration::from_secs(30))
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string()));
        match got {
            Ok(t) => {
                text = Some(t);
                break;
            }
            Err(e) => last = e,
        }
    }
    let text = text.ok_or(last)?;
    parse_releases(
        &text,
        env!("CARGO_PKG_VERSION"),
        current_target().map(|t| t.kind),
    )
}

/// Download progress, shared with the window.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// 0 downloading, 1 checking the file, 2 installing.
    pub step: AtomicU64,
}

fn download(asset: &Asset, to: &Path, progress: &Progress) -> Result<String, String> {
    let mut response = agent(Duration::from_secs(300))
        .get(&asset.url)
        .call()
        .map_err(|e| e.to_string())?;
    progress.total.store(asset.size, Ordering::Relaxed);
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let mut file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", to.display()))?;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------------ installing

/// Unpacks the program from the release zip into `dest`: the entry called `name`, or ending
/// in `/name`.
fn unpack_file(zip_path: &Path, name: &str, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let suffix = format!("/{name}");
    let index = (0..zip.len())
        .find(|&i| {
            zip.by_index(i).is_ok_and(|f| {
                let entry = f.name().map(|n| n.replace('\\', "/")).unwrap_or_default();
                entry == name || entry.ends_with(&suffix)
            })
        })
        .ok_or_else(|| format!("the update does not contain {name}"))?;
    let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
    if entry.size() > MAX_DOWNLOAD {
        return Err("the program in the update is too big".into());
    }
    copy_program(&mut entry, dest)
}

/// Unpacks `prowave.exe` from the release zip into `dest`.
pub fn unpack_exe(zip_path: &Path, dest: &Path) -> Result<(), String> {
    unpack_file(zip_path, "prowave.exe", dest)
}

/// Writes a program from `from` to `dest`, refusing one that is cut short.
fn copy_program(from: &mut dyn Read, dest: &Path) -> Result<(), String> {
    let mut out = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let copied = std::io::copy(&mut from.take(MAX_DOWNLOAD), &mut out)
        .map_err(|e| format!("{}: {e}", dest.display()))?;
    out.flush().map_err(|e| e.to_string())?;
    // A real Pro Wave is megabytes; this catches a truncated or empty file.
    if copied < 1_000_000 {
        let _ = std::fs::remove_file(dest);
        return Err("the program in the update is too small".into());
    }
    Ok(())
}

/// Unpacks the Linux program `prowave-linux-x86_64/prowave` from the release tarball.
pub fn unpack_from_tarball(tarball: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(tarball).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let is_program = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches("./")
            == "prowave-linux-x86_64/prowave";
        if is_program && entry.header().entry_type().is_file() {
            return copy_program(&mut entry, dest);
        }
    }
    Err("the update does not contain the program".into())
}

/// Unpacks the macOS release zip and puts the `.app` it holds at `dest`.
pub fn unpack_app(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let stage = with_suffix(dest, ".stage");
    let _ = remove_path(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    let result = (|| {
        let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        // Permissions and links inside the app are kept; names that point out of the folder
        // are refused by the zip library.
        zip.extract(&stage).map_err(|e| e.to_string())?;
        let app = std::fs::read_dir(&stage)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir() && p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| "the update does not contain the app".to_string())?;
        let program = app.join("Contents").join("MacOS").join("prowave");
        if !app.join("Contents").join("Info.plist").is_file() {
            return Err("the update does not contain a complete app".into());
        }
        let size = std::fs::metadata(&program).map_or(0, |m| m.len());
        if size < 1_000_000 {
            return Err("the program in the update is missing or too small".into());
        }
        let _ = remove_path(dest);
        std::fs::rename(&app, dest).map_err(|e| format!("{}: {e}", dest.display()))
    })();
    let _ = remove_path(&stage);
    result
}

/// Lets the file be run.
fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Deletes a file or a folder.
fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    }
}

/// Renames `from` to `to`, trying again briefly: Windows antivirus and SmartScreen can hold a
/// file for a moment after it was written or closed.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt < 4 {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    match last {
        Some(e) => Err(e),
        None => Err(std::io::Error::other("rename failed")),
    }
}

/// Swaps the new version in: the current one is renamed to `old`, then the new one takes its
/// name. If the second rename fails, the first is undone, so there is always a program.
pub fn swap_in(current: &Path, new: &Path, old: &Path) -> Result<(), String> {
    let _ = remove_path(old);
    rename_with_retry(current, old).map_err(|e| format!("{}: {e}", current.display()))?;
    if let Err(e) = rename_with_retry(new, current) {
        let undo = rename_with_retry(old, current);
        return Err(match undo {
            Ok(()) => format!("{}: {e}", current.display()),
            Err(u) => format!(
                "{}: {e} (and the old program is at {}: {u})",
                current.display(),
                old.display()
            ),
        });
    }
    Ok(())
}

/// Makes sure a program is in place after a failed swap: if the swap was cut short between its
/// two renames, the old version is put back.
fn ensure_program(target: &Target) -> Result<(), String> {
    if target.path.exists() {
        return Ok(());
    }
    let old = with_suffix(&target.path, ".old");
    if !old.exists() {
        return Ok(());
    }
    rename_with_retry(&old, &target.path).map_err(|e| format!("{}: {e}", target.path.display()))
}

/// `prowave.exe` becomes `prowave.exe.new` and so on.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// What is ready after the download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Staged {
    /// The new version is ready; the helper puts it in place once Pro Wave has closed.
    Ready(Plan),
    /// The file was downloaded and checked but must be installed by hand: this is the command.
    Saved { path: PathBuf, command: String },
}

/// What the helper does once Pro Wave has closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub target: Target,
    /// The new version next to the current one (`….new`), or the package file.
    pub staged: PathBuf,
}

/// Downloads and checks `release` and gets it ready for this copy of Pro Wave.
fn stage(release: &Release, progress: &Progress) -> Result<Staged, String> {
    let target = current_target().ok_or_else(|| "this copy cannot be updated here".to_string())?;
    stage_for(release, progress, &target, &have_program)
}

/// Whether `name` is a program on the search path.
fn have_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// [`stage`] for the copy described by `target`; `have` says which programs exist.
fn stage_for(
    release: &Release,
    progress: &Progress,
    target: &Target,
    have: &dyn Fn(&str) -> bool,
) -> Result<Staged, String> {
    let asset = release
        .asset
        .as_ref()
        .filter(|a| target.kind.matches(&a.name))
        .ok_or_else(|| "there is no file for this system".to_string())?;
    if asset.size > MAX_DOWNLOAD {
        return Err(format!("the update file is too big ({} bytes)", asset.size));
    }
    let expected = asset
        .sha256
        .as_deref()
        .ok_or_else(|| "GitHub has no checksum for this file".to_string())?;
    let path = &target.path;
    let new = with_suffix(path, ".new");
    let package = target.kind.is_package();
    if !package && !folder_is_writable(path) {
        return Err(format!("cannot write next to {}", path.display()));
    }
    // The AppImage is downloaded straight to its place beside the current one; the other
    // files are archives or packages that go to the temporary folder.
    let download_to = if target.kind == Kind::AppImage {
        new.clone()
    } else {
        std::env::temp_dir().join(format!("prowave-update-{}-{}", release.version, asset.name))
    };
    let result = (|| {
        let got = download(asset, &download_to, progress)?;
        progress.step.store(1, Ordering::Relaxed);
        if got != expected {
            return Err("the downloaded file does not match its checksum".to_string());
        }
        progress.step.store(2, Ordering::Relaxed);
        match target.kind {
            Kind::Windows => unpack_exe(&download_to, &new)?,
            Kind::Tarball => {
                unpack_from_tarball(&download_to, &new)?;
                make_executable(&new)?;
            }
            Kind::AppImage => {
                if std::fs::metadata(&new).map_or(0, |m| m.len()) < 1_000_000 {
                    return Err("the program in the update is too small".into());
                }
                make_executable(&new)?;
            }
            Kind::Mac => unpack_app(&download_to, &new)?,
            Kind::Deb | Kind::Rpm | Kind::Flatpak => {}
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = remove_path(&download_to);
        let _ = remove_path(&new);
        return Err(e);
    }
    match target.kind {
        // A package is installed by the package manager, with the system's password prompt.
        Kind::Deb | Kind::Rpm if have("pkexec") => Ok(Staged::Ready(Plan {
            target: target.clone(),
            staged: download_to,
        })),
        Kind::Deb | Kind::Rpm | Kind::Flatpak => {
            let saved = save_download(&download_to)?;
            let command = manual_command(target.kind, &saved, have);
            Ok(Staged::Saved {
                path: saved,
                command,
            })
        }
        _ => {
            if download_to != new {
                let _ = remove_path(&download_to);
            }
            Ok(Staged::Ready(Plan {
                target: target.clone(),
                staged: new,
            }))
        }
    }
}

/// Moves a downloaded file to the user's Downloads folder.
fn save_download(file: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = home
        .as_ref()
        .map(|h| h.join("Downloads"))
        .filter(|d| d.is_dir())
        .or(home)
        .ok_or_else(|| "no folder to save the update in".to_string())?;
    let dest = dir.join(file.file_name().ok_or("no file name")?);
    if std::fs::rename(file, &dest).is_err() {
        std::fs::copy(file, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        let _ = std::fs::remove_file(file);
    }
    Ok(dest)
}

/// The command a package manager runs to install `file` (the part after `pkexec`).
pub fn package_command(kind: Kind, file: &Path, have: &dyn Fn(&str) -> bool) -> Vec<OsString> {
    let file = file.as_os_str().to_owned();
    let words = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
    let mut command = match kind {
        Kind::Deb => words(&[
            "env",
            "DEBIAN_FRONTEND=noninteractive",
            "apt-get",
            "install",
            "-y",
        ]),
        Kind::Rpm if have("dnf") => words(&["dnf", "install", "-y"]),
        Kind::Rpm if have("zypper") => {
            words(&["zypper", "--non-interactive", "--no-gpg-checks", "install"])
        }
        Kind::Rpm => words(&["rpm", "-U", "--nosignature"]),
        _ => Vec::new(),
    };
    command.push(file);
    command
}

/// The command to show when the user has to install the downloaded file themselves.
pub fn manual_command(kind: Kind, file: &Path, have: &dyn Fn(&str) -> bool) -> String {
    let file = file.display();
    match kind {
        Kind::Deb => format!("sudo apt install \"{file}\""),
        Kind::Flatpak => format!("flatpak install --user --bundle \"{file}\""),
        _ => {
            let command = package_command(kind, Path::new(""), have);
            let words: Vec<String> = command
                .iter()
                .take(command.len() - 1)
                .map(|w| w.to_string_lossy().into_owned())
                .collect();
            format!("sudo {} \"{file}\"", words.join(" "))
        }
    }
}

/// Notes that an update was installed: the old version is kept for the first run of the new
/// one.
fn remember_update() {
    if cfg!(test) {
        return;
    }
    if let Some(file) = config_file("update-installed") {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(file, b""));
        }
    }
}

/// Removes the leftovers of an update. `just_updated` is the first run of the new version: the
/// old one is kept until the new one has started once.
fn clean_up(target: &Target, just_updated: bool) {
    for suffix in [".new", ".stage"] {
        let _ = remove_path(&with_suffix(&target.path, suffix));
    }
    // An unpacked macOS update works in `<target>.new.stage`.
    let _ = remove_path(&with_suffix(&with_suffix(&target.path, ".new"), ".stage"));
    if !just_updated {
        let _ = remove_path(&with_suffix(&target.path, ".old"));
    }
}

/// The leftover of the previous update is deleted when the new version is used for the second
/// time (the first time after an update the old one is kept, in case the new one is bad).
pub fn clean_up_after_update() {
    let marker = config_file("update-installed");
    let just_updated = marker.as_ref().is_some_and(|m| m.exists());
    if just_updated {
        if let Some(marker) = marker {
            let _ = std::fs::remove_file(marker);
        }
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let image = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let Some(target) = detect(
        std::env::consts::OS,
        &exe,
        image.as_deref(),
        std::env::var_os("FLATPAK_ID").is_some(),
        None,
    ) else {
        return;
    };
    clean_up(&target, just_updated);
}

// ------------------------------------------------------------------ the helper

/// The word that starts the helper.
const HELPER_FLAG: &str = "--apply-update";

/// Starts Pro Wave again as its own helper, which puts the update in place once this program
/// has closed and then opens the new version. The caller closes this program.
fn launch_helper(plan: &Plan) -> Result<(), String> {
    // An AppImage's program lives in a mount that goes away when the AppImage closes, so the
    // new AppImage (which has the same helper in it) does the job instead.
    let exe = if plan.target.kind == Kind::AppImage {
        plan.staged.clone()
    } else {
        std::env::current_exe().map_err(|e| e.to_string())?
    };
    let mut command = std::process::Command::new(exe);
    command
        .arg(HELPER_FLAG)
        .arg(std::process::id().to_string())
        .arg(plan.target.kind.word())
        .arg(&plan.target.path)
        .arg(&plan.staged);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window, and not tied to this program's.
        command.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// The helper's job, from its command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Job {
    pub wait_for: u32,
    pub plan: Plan,
}

/// `--apply-update <pid> <kind> <target> <staged>`.
pub fn parse_helper_args(args: &[OsString]) -> Option<Job> {
    let [flag, pid, kind, path, staged] = args else {
        return None;
    };
    if flag != HELPER_FLAG {
        return None;
    }
    let kind = Kind::from_word(kind.to_str()?)?;
    let target = PathBuf::from(path);
    let staged = PathBuf::from(staged);
    // Only the paths this program stages itself: an update is the program or folder with
    // `.new` on its name, and a package is a file in a temporary folder.
    if !target.is_absolute() || !staged.is_absolute() {
        return None;
    }
    if !kind.is_package() && staged != with_suffix(&target, ".new") {
        return None;
    }
    Some(Job {
        wait_for: pid.to_str()?.parse().ok()?,
        plan: Plan {
            target: Target::new(kind, target),
            staged,
        },
    })
}

/// Runs the helper if this program was started as one; true when it did (the caller exits).
pub fn run_helper_if_requested() -> bool {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some(job) = parse_helper_args(&args) else {
        return false;
    };
    wait_for_exit(job.wait_for);
    if let Err(e) = apply(&job.plan, &have_program) {
        // Put the old version back if the swap was cut short, then open it: it tells what
        // went wrong.
        let _ = ensure_program(&job.plan.target);
        if let Some(file) = config_file("update-error") {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(file, e));
            }
        }
    }
    open_again(&job.plan.target);
    true
}

/// Waits (at most half a minute) until the program with this process number has closed.
fn wait_for_exit(pid: u32) {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    for _ in 0..150 {
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        if system.process(pid).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The error shown when a package manager could not install the update. The downloaded file is
/// kept so the user can install it themselves.
fn package_error(kind: Kind, staged: &Path, have: &dyn Fn(&str) -> bool, detail: &str) -> String {
    format!(
        "{detail}\nThe downloaded file is kept at {}.\nTo install it yourself, run: {}",
        staged.display(),
        manual_command(kind, staged, have)
    )
}

/// Runs the package manager as the administrator; a failure says why.
fn run_pkexec(command: &[OsString]) -> Result<(), String> {
    let output = std::process::Command::new("pkexec")
        .args(command)
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    // 126 and 127: the password prompt was dismissed or not allowed.
    Err(match output.status.code() {
        Some(126 | 127) => "the installation was not allowed".to_string(),
        _ => String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("the package manager failed")
            .to_string(),
    })
}

/// Puts the update in place. Files and folders are swapped; packages are installed by the
/// package manager.
pub fn apply(plan: &Plan, have: &dyn Fn(&str) -> bool) -> Result<(), String> {
    apply_with(plan, have, &run_pkexec)
}

fn apply_with(
    plan: &Plan,
    have: &dyn Fn(&str) -> bool,
    run: &dyn Fn(&[OsString]) -> Result<(), String>,
) -> Result<(), String> {
    let (path, staged) = (&plan.target.path, &plan.staged);
    match plan.target.kind {
        Kind::Deb | Kind::Rpm | Kind::Flatpak => {
            let command = package_command(plan.target.kind, staged, have);
            // The file stays where it is if the package manager fails, so it can be installed
            // by hand; the error says where it is and what to run.
            run(&command).map_err(|e| package_error(plan.target.kind, staged, have, &e))?;
            let _ = std::fs::remove_file(staged);
            remember_update();
            Ok(())
        }
        _ => {
            swap_in(path, staged, &with_suffix(path, ".old"))?;
            remember_update();
            Ok(())
        }
    }
}

/// Opens Pro Wave again (the new version after an update, the old one if it failed).
fn open_again(target: &Target) {
    let mut command = match target.kind {
        Kind::Mac => {
            let mut c = std::process::Command::new("open");
            c.arg("-n").arg(&target.path);
            c
        }
        Kind::AppImage => {
            let mut c = std::process::Command::new(&target.path);
            // The new AppImage sets these up for itself.
            for var in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
                c.env_remove(var);
            }
            c
        }
        _ => std::process::Command::new(&target.path),
    };
    let _ = command.spawn();
}

/// The error the helper left, if it could not install the update (read once).
fn take_helper_error() -> Option<String> {
    let file = config_file("update-error")?;
    let text = std::fs::read_to_string(&file).ok()?;
    let _ = std::fs::remove_file(file);
    (!text.trim().is_empty()).then_some(text)
}

// ------------------------------------------------------------------ settings

fn config_file(name: &str) -> Option<PathBuf> {
    crate::recent_files::config_dir().map(|d| d.join(name))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether the program looks for updates by itself (Help > Check for Updates Automatically).
pub fn auto_enabled() -> bool {
    config_file("updates-off").is_none_or(|f| !f.exists())
}

pub fn set_auto_enabled(on: bool) {
    // The tests of the menus must not change the settings of whoever runs them.
    if cfg!(test) {
        return;
    }
    let Some(file) = config_file("updates-off") else {
        return;
    };
    if on {
        let _ = std::fs::remove_file(file);
    } else if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(file, b""));
    }
}

fn skipped_version() -> Option<String> {
    let text = std::fs::read_to_string(config_file("update-skipped")?).ok()?;
    Some(text.trim().to_string())
}

fn skip_version(version: &str) {
    if let Some(file) = config_file("update-skipped") {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(file, version));
        }
    }
}

/// True when the last automatic check was long enough ago; remembers this one.
fn due_for_automatic_check() -> bool {
    let Some(file) = config_file("update-checked") else {
        return true;
    };
    let last: u64 = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    let now = unix_now();
    if now.saturating_sub(last) < CHECK_EVERY_SECS {
        return false;
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&file, now.to_string()));
    }
    true
}

// ------------------------------------------------------------------ the window

type CheckResult = Result<Option<Release>, String>;

enum State {
    Idle,
    Checking(Receiver<CheckResult>),
    Available(Release),
    Installing(Release, Arc<Progress>, Receiver<Result<Staged, String>>),
    /// Downloaded and checked: waiting for the user to close Pro Wave and update.
    Ready(Release, Plan),
    /// Downloaded and checked, to be installed by hand.
    Saved(Release, PathBuf, String),
    UpToDate,
    Failed(String),
}

/// What the app does after the window ran.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// The helper was started: close this program so it can update.
    Quit,
}

pub struct Updater {
    state: State,
    /// The check was asked for by the user (Help > Check for Updates…): say so when there is
    /// nothing new or it failed.
    manual: bool,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            manual: false,
        }
    }

    /// At startup: say what went wrong if the last update could not be installed, otherwise
    /// do the automatic check.
    pub fn check_at_startup(&mut self) {
        if let Some(error) = take_helper_error() {
            self.state = State::Failed(error);
        } else if auto_enabled() && due_for_automatic_check() {
            self.start_check(false);
        }
    }

    /// Looks for a newer release on a background thread.
    pub fn start_check(&mut self, manual: bool) {
        if matches!(self.state, State::Checking(_) | State::Installing(..)) {
            return;
        }
        let (tx, rx) = channel();
        self.manual = manual;
        self.state = State::Checking(rx);
        let spawned = std::thread::Builder::new()
            .name("prowave-update-check".into())
            .spawn(move || {
                let _ = tx.send(fetch_latest());
            });
        if let Err(e) = spawned {
            self.state = State::Failed(e.to_string());
        }
    }

    fn start_install(&mut self, release: Release) {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let (job_release, job_progress) = (release.clone(), progress.clone());
        let spawned = std::thread::Builder::new()
            .name("prowave-update-install".into())
            .spawn(move || {
                let _ = tx.send(stage(&job_release, &job_progress));
            });
        self.state = match spawned {
            Ok(_) => State::Installing(release, progress, rx),
            Err(e) => State::Failed(e.to_string()),
        };
    }

    /// Moves the check or the download along. Call once per frame.
    fn poll(&mut self) {
        match &self.state {
            State::Checking(rx) => match rx.try_recv() {
                Ok(Ok(Some(release))) => {
                    // A version the user skipped is not offered again by the automatic check.
                    self.state = if !self.manual
                        && skipped_version().as_deref() == Some(release.version.as_str())
                    {
                        State::Idle
                    } else {
                        State::Available(release)
                    };
                }
                Ok(Ok(None)) => {
                    self.state = if self.manual {
                        State::UpToDate
                    } else {
                        State::Idle
                    };
                }
                Ok(Err(e)) => {
                    // The automatic check stays quiet when there is no network.
                    self.state = if self.manual {
                        State::Failed(e)
                    } else {
                        State::Idle
                    };
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.state = State::Idle,
            },
            State::Installing(release, _, rx) => match rx.try_recv() {
                Ok(Ok(Staged::Ready(plan))) => self.state = State::Ready(release.clone(), plan),
                Ok(Ok(Staged::Saved { path, command })) => {
                    self.state = State::Saved(release.clone(), path, command);
                }
                Ok(Err(e)) => self.state = State::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.state = State::Failed("the update stopped".into());
                }
            },
            _ => {}
        }
    }

    /// Draws the update window when there is something to say. `unsaved` says whether the
    /// project has changes that were not saved (the program is not closed over them).
    pub fn show(&mut self, ctx: &egui::Context, unsaved: &dyn Fn() -> bool) -> Action {
        self.poll();
        if matches!(self.state, State::Checking(_) | State::Installing(..)) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let mut next: Option<State> = None;
        let mut install_release: Option<Release> = None;
        let mut action = Action::None;
        let current = env!("CARGO_PKG_VERSION");
        let title = match &self.state {
            State::Idle | State::Checking(_) => return Action::None,
            _ => tr("Update"),
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                match &self.state {
                    State::Available(release) => {
                        ui.heading(trf("Pro Wave {} is available", &[&release.version]));
                        ui.label(trf("You have {}.", &[&current]));
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .show(ui, |ui| {
                                ui.label(clean_notes(&release.notes));
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if release.asset.is_some() {
                                if ui.button(tr("Install update")).clicked() {
                                    install_release = Some(release.clone());
                                }
                            } else if ui.button(tr("Download page")).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                            }
                            if release.asset.is_some() && ui.button(tr("Download page")).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                            }
                            if ui.button(tr("Later")).clicked() {
                                next = Some(State::Idle);
                            }
                            if ui.button(tr("Skip this version")).clicked() {
                                skip_version(&release.version);
                                next = Some(State::Idle);
                            }
                        });
                    }
                    State::Installing(_, progress, _) => {
                        let step = progress.step.load(Ordering::Relaxed);
                        let done = progress.done.load(Ordering::Relaxed) as f32;
                        let total = progress.total.load(Ordering::Relaxed).max(1) as f32;
                        let (text, fraction) = match step {
                            0 => (
                                trf("Downloading… {}%", &[&((done / total * 100.0) as u32)]),
                                done / total,
                            ),
                            1 => (tr("Checking the download…").to_string(), 1.0),
                            _ => (tr("Preparing…").to_string(), 1.0),
                        };
                        ui.label(text);
                        ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)));
                    }
                    State::Ready(release, plan) => {
                        let unsaved = unsaved();
                        ui.heading(trf("Pro Wave {} is ready to install", &[&release.version]));
                        ui.label(tr("Pro Wave will close, update and open again."));
                        if unsaved {
                            ui.label(tr("Save your project before updating"));
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!unsaved, egui::Button::new(tr("Close and update")))
                                .clicked()
                            {
                                match launch_helper(plan) {
                                    Ok(()) => action = Action::Quit,
                                    Err(e) => next = Some(State::Failed(e)),
                                }
                            }
                            if ui.button(tr("Later")).clicked() {
                                next = Some(State::Idle);
                            }
                        });
                    }
                    State::Saved(release, path, command) => {
                        ui.label(trf(
                            "Pro Wave {} was downloaded and checked: {}",
                            &[&release.version, &path.display()],
                        ));
                        ui.label(tr("To install it, run this in a terminal:"));
                        ui.add(
                            egui::Label::new(egui::RichText::new(command).monospace())
                                .wrap()
                                .selectable(true),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr("Copy command")).clicked() {
                                ctx.copy_text(command.clone());
                            }
                            if ui.button(tr("Close")).clicked() {
                                next = Some(State::Idle);
                            }
                        });
                    }
                    State::UpToDate => {
                        ui.label(trf("Pro Wave is up to date ({}).", &[&current]));
                        if ui.button(tr("Close")).clicked() {
                            next = Some(State::Idle);
                        }
                    }
                    State::Failed(e) => {
                        ui.label(trf("The update did not work: {}", &[e]));
                        if ui.button(tr("Close")).clicked() {
                            next = Some(State::Idle);
                        }
                    }
                    State::Idle | State::Checking(_) => {}
                }
            });
        if let Some(release) = install_release {
            self.start_install(release);
        } else if let Some(state) = next {
            self.state = state;
        }
        action
    }
}

/// Release notes as plain text: no Markdown marks.
fn clean_notes(notes: &str) -> String {
    notes
        .lines()
        .map(|l| l.trim_start_matches('#').trim().replace("**", ""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse_version("v0.1.10"), Some((0, 1, 10)));
        assert_eq!(parse_version("0.2"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.0.0-beta.1"), Some((1, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("0.1.10", "0.1.9"), "10 is more than 9");
        assert!(is_newer("v0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.7", "0.1.7"));
        assert!(!is_newer("0.1.6", "0.1.7"));
        assert!(!is_newer("nonsense", "0.1.7"));
    }

    const SAMPLE: &str = r###"[
      {"tag_name":"v0.1.9","draft":true,"body":"not public","html_url":"https://x/9","assets":[]},
      {"tag_name":"v0.1.8","draft":false,"body":"## 0.1.8\n**Fixed** things","html_url":"https://github.com/fabox-os/Pro-Wave/releases/tag/v0.1.8",
       "assets":[
         {"name":"prowave-windows-x86_64.zip","browser_download_url":"https://github.com/dl/win.zip","size":6420076,"digest":"sha256:ABCDEF0123"},
         {"name":"prowave-macos-universal.zip","browser_download_url":"https://github.com/dl/mac.zip","size":1,"digest":"sha256:00"},
         {"name":"Pro_Wave-0.1.8-x86_64.AppImage","browser_download_url":"https://github.com/dl/app.AppImage","size":2,"digest":"sha256:11"},
         {"name":"prowave-linux-x86_64.tar.gz","browser_download_url":"https://github.com/dl/linux.tar.gz","size":3,"digest":"sha256:22"},
         {"name":"prowave-studio_0.1.8_amd64.deb","browser_download_url":"https://github.com/dl/x.deb","size":4,"digest":"sha256:33"},
         {"name":"prowave-studio-0.1.8-1.x86_64.rpm","browser_download_url":"https://github.com/dl/x.rpm","size":5,"digest":"sha256:44"},
         {"name":"prowave-studio-0.1.8-x86_64.flatpak","browser_download_url":"https://github.com/dl/x.flatpak","size":6,"digest":"sha256:55"}]},
      {"tag_name":"v0.1.7","draft":false,"body":"old","html_url":"https://x/7","assets":[]}
    ]"###;

    #[test]
    fn release_candidates_are_not_offered() {
        let json = r#"[{"tag_name":"v0.2.0-rc1","draft":false,"body":"","html_url":"x","assets":[]},
                       {"tag_name":"v0.1.8","draft":false,"body":"","html_url":"y","assets":[]}]"#;
        let release = parse_releases(json, "0.1.7", None).unwrap().unwrap();
        assert_eq!(release.version, "0.1.8");
        let only_rc =
            r#"[{"tag_name":"v0.2.0-rc1","draft":false,"body":"","html_url":"x","assets":[]}]"#;
        assert_eq!(parse_releases(only_rc, "0.1.7", None).unwrap(), None);
    }

    #[test]
    fn the_newest_public_release_is_offered_when_it_is_newer() {
        let release = parse_releases(SAMPLE, "0.1.7", None).unwrap().unwrap();
        assert_eq!(release.version, "0.1.8", "drafts are skipped");
        assert!(release.page.ends_with("/v0.1.8"));
        assert!(release.notes.contains("Fixed"));
        assert!(
            release.asset.is_none(),
            "this copy cannot install it itself"
        );
        assert_eq!(parse_releases(SAMPLE, "0.1.8", None).unwrap(), None);
        assert_eq!(parse_releases(SAMPLE, "0.2.0", None).unwrap(), None);
        assert_eq!(parse_releases("[]", "0.1.7", None).unwrap(), None);
        assert!(parse_releases("{}", "0.1.7", None).is_err());
        assert!(parse_releases("not json", "0.1.7", None).is_err());
    }

    const ALL_KINDS: [Kind; 7] = [
        Kind::Windows,
        Kind::Mac,
        Kind::AppImage,
        Kind::Tarball,
        Kind::Deb,
        Kind::Rpm,
        Kind::Flatpak,
    ];

    #[test]
    fn each_install_gets_the_file_made_for_it_with_its_checksum() {
        let file = |kind| {
            parse_releases(SAMPLE, "0.1.7", Some(kind))
                .unwrap()
                .unwrap()
                .asset
                .unwrap()
        };
        let win = file(Kind::Windows);
        assert_eq!(win.name, "prowave-windows-x86_64.zip");
        assert_eq!(win.url, "https://github.com/dl/win.zip");
        assert_eq!(win.sha256.as_deref(), Some("abcdef0123"), "lower case");
        assert_eq!(file(Kind::Mac).name, "prowave-macos-universal.zip");
        assert_eq!(file(Kind::AppImage).name, "Pro_Wave-0.1.8-x86_64.AppImage");
        assert_eq!(file(Kind::Tarball).name, "prowave-linux-x86_64.tar.gz");
        assert_eq!(file(Kind::Deb).name, "prowave-studio_0.1.8_amd64.deb");
        assert_eq!(file(Kind::Rpm).name, "prowave-studio-0.1.8-1.x86_64.rpm");
        assert_eq!(
            file(Kind::Flatpak).name,
            "prowave-studio-0.1.8-x86_64.flatpak"
        );
        assert_eq!(file(Kind::Deb).sha256.as_deref(), Some("33"));
        // No file is taken for two kinds.
        for name in [
            "prowave-windows-x86_64.zip",
            "prowave-macos-universal.zip",
            "Pro_Wave-0.1.8-x86_64.AppImage",
            "prowave-linux-x86_64.tar.gz",
            "prowave-studio_0.1.8_amd64.deb",
            "prowave-studio-0.1.8-1.x86_64.rpm",
            "prowave-studio-0.1.8-x86_64.flatpak",
        ] {
            assert_eq!(
                ALL_KINDS.iter().filter(|k| k.matches(name)).count(),
                1,
                "{name}"
            );
        }
    }

    #[test]
    fn the_way_pro_wave_was_installed_decides_what_an_update_replaces() {
        let p = Path::new;
        let t = |kind, path: &str| Some(Target::new(kind, path));
        assert_eq!(
            detect("windows", p("C:/Pro Wave/prowave.exe"), None, false, None),
            t(Kind::Windows, "C:/Pro Wave/prowave.exe")
        );
        assert_eq!(
            detect(
                "macos",
                p("/Applications/Pro Wave.app/Contents/MacOS/prowave"),
                None,
                false,
                None
            ),
            t(Kind::Mac, "/Applications/Pro Wave.app")
        );
        assert_eq!(detect("macos", p("/tmp/prowave"), None, false, None), None);
        let image = p("/home/me/Pro_Wave-0.1.7-x86_64.AppImage");
        assert_eq!(
            detect(
                "linux",
                p("/tmp/.mount_ProWav/usr/bin/prowave"),
                Some(image),
                false,
                None
            ),
            t(Kind::AppImage, "/home/me/Pro_Wave-0.1.7-x86_64.AppImage")
        );
        assert_eq!(
            detect(
                "linux",
                p("/home/me/prowave-linux-x86_64/prowave"),
                None,
                false,
                None
            ),
            t(Kind::Tarball, "/home/me/prowave-linux-x86_64/prowave")
        );
        // Installed by a package manager: that package is the update.
        assert_eq!(
            detect("linux", p("/usr/bin/prowave"), None, false, Some(Kind::Deb)),
            t(Kind::Deb, "/usr/bin/prowave")
        );
        assert_eq!(
            detect("linux", p("/usr/bin/prowave"), None, false, Some(Kind::Rpm)),
            t(Kind::Rpm, "/usr/bin/prowave")
        );
        // A Flatpak gets its bundle.
        assert_eq!(
            detect("linux", p("/app/bin/prowave"), None, true, None),
            t(Kind::Flatpak, "/app/bin/prowave")
        );
        // A program in a system folder that no package manager owns, or something else.
        assert_eq!(
            detect("linux", p("/usr/local/bin/prowave"), None, false, None),
            None
        );
        assert_eq!(
            detect("linux", p("/home/me/other-program"), None, false, None),
            None
        );
        assert_eq!(
            detect("freebsd", p("/home/me/prowave"), None, false, None),
            None
        );
    }

    #[test]
    fn packages_are_installed_with_the_systems_package_manager() {
        let file = Path::new("/tmp/prowave.pkg");
        let args = |kind, have: &[&str]| -> Vec<String> {
            let have = have.to_vec();
            package_command(kind, file, &move |n| have.contains(&n))
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(Kind::Deb, &[]),
            [
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "apt-get",
                "install",
                "-y",
                "/tmp/prowave.pkg"
            ]
        );
        assert_eq!(
            args(Kind::Rpm, &["dnf", "zypper"])[..3],
            ["dnf", "install", "-y"]
        );
        assert_eq!(args(Kind::Rpm, &["zypper"])[0], "zypper");
        assert_eq!(
            args(Kind::Rpm, &[]),
            ["rpm", "-U", "--nosignature", "/tmp/prowave.pkg"]
        );

        let none = |_: &str| false;
        assert_eq!(
            manual_command(Kind::Deb, file, &none),
            "sudo apt install \"/tmp/prowave.pkg\""
        );
        assert_eq!(
            manual_command(Kind::Rpm, file, &|n| n == "dnf"),
            "sudo dnf install -y \"/tmp/prowave.pkg\""
        );
        assert_eq!(
            manual_command(Kind::Flatpak, file, &none),
            "flatpak install --user --bundle \"/tmp/prowave.pkg\""
        );
    }

    #[test]
    fn the_helper_is_told_what_to_do_on_its_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("Pro Wave.AppImage");
        let staged = with_suffix(&image, ".new");
        let words = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let job = parse_helper_args(&[
            OsString::from("--apply-update"),
            OsString::from("1234"),
            OsString::from("appimage"),
            image.clone().into_os_string(),
            staged.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(job.wait_for, 1234);
        assert_eq!(job.plan.target.kind, Kind::AppImage);
        assert_eq!(job.plan.target.path, image);
        assert_eq!(job.plan.staged, staged);
        for kind in ALL_KINDS {
            assert_eq!(Kind::from_word(kind.word()), Some(kind));
        }
        // Ordinary starts, and nonsense, are not the helper.
        assert_eq!(parse_helper_args(&[]), None);
        assert_eq!(parse_helper_args(&words(&["project.prowave"])), None);
        assert_eq!(
            parse_helper_args(&words(&["--apply-update", "x", "deb", "a", "b"])),
            None
        );
        assert_eq!(
            parse_helper_args(&words(&["--apply-update", "1", "nope", "a", "b"])),
            None
        );
    }

    #[test]
    fn the_helper_swaps_the_new_version_in_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("prowave");
        let new = with_suffix(&exe, ".new");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        let plan = Plan {
            target: Target::new(Kind::Tarball, &exe),
            staged: new,
        };
        apply(&plan, &|_| false).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(with_suffix(&exe, ".old")).unwrap(), b"old");
        // Nothing staged any more: the swap fails and the program stays as it is.
        assert!(apply(&plan, &|_| false).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[test]
    fn notes_lose_their_markdown_marks() {
        assert_eq!(clean_notes("## 0.1.8\n**Fixed** it"), "0.1.8\nFixed it");
    }

    fn zip_with(dir: &Path, entries: &[(&str, usize)]) -> PathBuf {
        let path = dir.join("update.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        for (name, size) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(&vec![7u8; *size]).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn the_program_is_unpacked_from_the_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip = zip_with(
            dir.path(),
            &[
                ("prowave-windows-x86_64/README.txt", 10),
                ("prowave-windows-x86_64/prowave.exe", 1_500_000),
            ],
        );
        let out = dir.path().join("prowave.exe.new");
        unpack_exe(&zip, &out).unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 1_500_000);
    }

    #[test]
    fn a_zip_without_the_program_or_with_a_tiny_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("new");
        let zip = zip_with(dir.path(), &[("readme.txt", 10)]);
        assert!(unpack_exe(&zip, &out).is_err());
        let zip = zip_with(dir.path(), &[("prowave.exe", 100)]);
        assert!(unpack_exe(&zip, &out).unwrap_err().contains("too small"));
        assert!(!out.exists(), "the tiny file is not left behind");
    }

    fn tarball_with(dir: &Path, entries: &[(&str, usize)]) -> PathBuf {
        let path = dir.join("update.tar.gz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (name, size) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(*size as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, &vec![9u8; *size][..])
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn the_linux_program_is_unpacked_from_the_tarball() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = tarball_with(
            dir.path(),
            &[
                ("prowave-linux-x86_64/install.sh", 20),
                ("prowave-linux-x86_64/prowave", 1_200_000),
            ],
        );
        let out = dir.path().join("prowave.new");
        unpack_from_tarball(&tgz, &out).unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 1_200_000);

        let other = tarball_with(dir.path(), &[("somewhere/else/prowave", 1_200_000)]);
        assert!(unpack_from_tarball(&other, &dir.path().join("x")).is_err());
        let tiny = tarball_with(dir.path(), &[("prowave-linux-x86_64/prowave", 50)]);
        assert!(unpack_from_tarball(&tiny, &dir.path().join("y")).is_err());
    }

    #[test]
    fn the_mac_app_is_unpacked_and_swapped_in_as_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let zip = zip_with(
            dir.path(),
            &[
                ("Pro Wave.app/Contents/Info.plist", 200),
                ("Pro Wave.app/Contents/MacOS/prowave", 1_500_000),
                ("__MACOSX/._Pro Wave.app", 10),
            ],
        );
        let app = dir.path().join("Pro Wave.app");
        let (new, old) = (with_suffix(&app, ".new"), with_suffix(&app, ".old"));
        // The app that is installed now.
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), b"old app").unwrap();

        unpack_app(&zip, &new).unwrap();
        assert!(new.join("Contents/MacOS/prowave").is_file());
        assert!(
            !with_suffix(&new, ".stage").exists(),
            "the work folder is gone"
        );
        swap_in(&app, &new, &old).unwrap();
        assert_eq!(
            std::fs::metadata(app.join("Contents/MacOS/prowave"))
                .unwrap()
                .len(),
            1_500_000
        );
        assert_eq!(
            std::fs::read(old.join("Contents/Info.plist")).unwrap(),
            b"old app"
        );
        assert!(!new.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(app.join("Contents/MacOS/prowave"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "the program can still be run");
        }

        // A zip with no app, or with a program that is missing, is refused.
        let none = zip_with(dir.path(), &[("readme.txt", 10)]);
        assert!(unpack_app(&none, &dir.path().join("a.new")).is_err());
        let empty = zip_with(dir.path(), &[("Pro Wave.app/Contents/Info.plist", 10)]);
        assert!(unpack_app(&empty, &dir.path().join("b.new")).is_err());
        let no_plist = zip_with(
            dir.path(),
            &[("Pro Wave.app/Contents/MacOS/prowave", 1_500_000)],
        );
        assert!(unpack_app(&no_plist, &dir.path().join("c.new")).is_err());
    }

    #[test]
    fn the_swap_keeps_the_old_program_and_undoes_itself_when_it_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, new, old) = (
            dir.path().join("prowave.exe"),
            dir.path().join("prowave.exe.new"),
            dir.path().join("prowave.exe.old"),
        );
        std::fs::write(&exe, b"old program").unwrap();
        std::fs::write(&new, b"new program").unwrap();
        std::fs::write(&old, b"leftover").unwrap();
        swap_in(&exe, &new, &old).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new program");
        assert_eq!(std::fs::read(&old).unwrap(), b"old program");
        assert!(!new.exists());

        // The new file is missing: the swap fails and the program is where it was.
        let err = swap_in(&exe, &new, &old);
        assert!(err.is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new program");
    }

    #[cfg(unix)]
    #[test]
    fn a_downloaded_program_can_be_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prowave");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        make_executable(&file).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn prereleases_are_not_offered() {
        let json = r#"[
          {"tag_name":"v0.2.0","draft":false,"prerelease":true,"body":"beta","html_url":"x","assets":[]},
          {"tag_name":"v0.1.9","draft":false,"prerelease":false,"body":"stable","html_url":"y","assets":[]}
        ]"#;
        let release = parse_releases(json, "0.1.8", None).unwrap().unwrap();
        assert_eq!(release.version, "0.1.9", "the beta is skipped");
        let only_beta = r#"[{"tag_name":"v0.2.0","draft":false,"prerelease":true,"body":"","html_url":"x","assets":[]}]"#;
        assert_eq!(parse_releases(only_beta, "0.1.8", None).unwrap(), None);
    }

    #[test]
    fn an_asset_bigger_than_the_limit_is_refused_before_downloading() {
        let release = Release {
            version: "9.9.9".into(),
            notes: String::new(),
            page: String::new(),
            asset: Some(Asset {
                name: "prowave-windows-x86_64.zip".into(),
                url: "https://example.invalid/update.zip".into(),
                size: MAX_DOWNLOAD + 1,
                sha256: None,
            }),
        };
        let dir = tempfile::tempdir().unwrap();
        let target = Target::new(Kind::Windows, dir.path().join("prowave.exe"));
        let err = stage_for(&release, &Progress::default(), &target, &|_| false).unwrap_err();
        assert!(err.contains("too big"), "{err}");
        assert!(!with_suffix(&target.path, ".new").exists());
    }

    #[test]
    fn the_helper_ignores_a_staged_file_that_is_not_the_expected_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Pro Wave").join("prowave.exe");
        let staged = with_suffix(&exe, ".new");
        let parse = |kind: &str, target: &Path, staged: &Path| {
            parse_helper_args(&[
                OsString::from("--apply-update"),
                OsString::from("1"),
                OsString::from(kind),
                target.as_os_str().to_owned(),
                staged.as_os_str().to_owned(),
            ])
        };
        let job = parse("windows", &exe, &staged).expect("the expected staged file");
        assert_eq!(job.wait_for, 1);
        assert_eq!(job.plan.target.kind, Kind::Windows);
        assert_eq!(job.plan.target.path, exe);
        assert_eq!(job.plan.staged, staged);
        // A path this program would not have staged is not a job.
        assert_eq!(parse("windows", &exe, &dir.path().join("passwd")), None);
        assert_eq!(
            parse(
                "windows",
                Path::new("prowave.exe"),
                Path::new("prowave.exe.new")
            ),
            None,
            "relative paths are not accepted"
        );
        // A package lives in a temporary folder, so only its path being absolute counts.
        assert!(parse(
            "deb",
            &dir.path().join("usr/bin/prowave"),
            &dir.path().join("prowave-studio_1_amd64.deb")
        )
        .is_some());
    }

    #[test]
    fn a_failed_package_install_keeps_the_downloaded_file() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("prowave-studio_1.2.3_amd64.deb");
        std::fs::write(&staged, b"package").unwrap();
        let plan = Plan {
            target: Target::new(Kind::Deb, dir.path().join("usr/bin/prowave")),
            staged: staged.clone(),
        };
        let err = apply_with(&plan, &|_| false, &|_| {
            Err("the installation was not allowed".to_string())
        })
        .unwrap_err();
        assert!(staged.exists(), "the file is kept for a manual install");
        assert!(err.contains("the installation was not allowed"));
        assert!(err.contains("sudo apt install"), "{err}");
    }

    #[test]
    fn a_failed_package_install_keeps_the_file_and_says_how_to_retry() {
        let file = Path::new("/tmp/prowave-studio_1.2.3_amd64.deb");
        let none = |_: &str| false;
        let message = package_error(Kind::Deb, file, &none, "the installation was not allowed");
        assert!(message.contains("the installation was not allowed"));
        assert!(message.contains("/tmp/prowave-studio_1.2.3_amd64.deb"));
        assert!(message.contains("sudo apt install"));
    }

    #[test]
    fn the_first_run_after_an_update_keeps_the_old_version_and_drops_stale_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("prowave");
        std::fs::write(&exe, b"new").unwrap();
        for suffix in [".old", ".new", ".new.stage", ".stage"] {
            std::fs::write(with_suffix(&exe, suffix), suffix.as_bytes()).unwrap();
        }
        let target = Target::new(Kind::Tarball, &exe);
        clean_up(&target, true);
        assert!(with_suffix(&exe, ".old").exists(), "kept for one run");
        assert!(!with_suffix(&exe, ".new").exists());
        assert!(
            !with_suffix(&with_suffix(&exe, ".new"), ".stage").exists(),
            "the folder an unpacked macOS update works in"
        );
        assert!(!with_suffix(&exe, ".stage").exists());
        clean_up(&target, false);
        assert!(
            !with_suffix(&exe, ".old").exists(),
            "gone after the first run"
        );
    }

    #[test]
    fn a_half_finished_swap_is_undone_before_the_old_version_opens() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("prowave");
        let old = with_suffix(&exe, ".old");
        std::fs::write(&old, b"old").unwrap();
        let target = Target::new(Kind::Tarball, &exe);
        ensure_program(&target).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!old.exists(), "the old version moved back into place");
        // With a program in place nothing changes.
        std::fs::write(&exe, b"new").unwrap();
        ensure_program(&target).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[test]
    fn a_rename_that_keeps_failing_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let dest = dir.path().join("dest");
        assert!(rename_with_retry(&missing, &dest).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn suffixes_are_added_to_the_whole_name() {
        let p = with_suffix(Path::new("C:/x/prowave.exe"), ".new");
        assert!(p.to_string_lossy().ends_with("prowave.exe.new"));
    }

    #[test]
    fn hashes_are_lower_case_hex() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex(&h.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Talks to GitHub: `cargo test -p prowave-app live -- --ignored`.
    #[test]
    #[ignore]
    fn live_check_against_github() {
        let text = {
            let mut r = agent(Duration::from_secs(15))
                .get(&format!(
                    "https://api.github.com/repos/{REPO}/releases?per_page=10"
                ))
                .call()
                .unwrap();
            r.body_mut().read_to_string().unwrap()
        };
        // Every kind of file the program installs is in the real release, with its checksum.
        for kind in ALL_KINDS {
            let release = parse_releases(&text, "0.0.1", Some(kind))
                .unwrap()
                .expect("a newer release");
            assert!(parse_version(&release.version).is_some());
            assert!(release.page.starts_with("https://github.com/"));
            let asset = release.asset.expect("the file of this kind");
            assert!(kind.matches(&asset.name), "{}", asset.name);
            assert!(asset.sha256.is_some(), "GitHub publishes the checksum");
        }
        let get = |kind| parse_releases(&text, "0.0.1", Some(kind)).unwrap().unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Windows: the whole path, over a stand-in for the running program.
        let release = get(Kind::Windows);
        let asset = release.asset.clone().unwrap();
        let zip = dir.path().join("update.zip");
        let got = download(&asset, &zip, &Progress::default()).unwrap();
        assert_eq!(Some(got), asset.sha256);
        unpack_exe(&zip, &dir.path().join("prowave.exe.new")).unwrap();
        let exe = dir.path().join("prowave.exe");
        std::fs::write(&exe, b"the running program").unwrap();
        let target = Target::new(Kind::Windows, &exe);
        let staged = stage_for(&release, &Progress::default(), &target, &|_| false).unwrap();
        let Staged::Ready(plan) = staged else {
            panic!("the Windows update is ready to apply");
        };
        assert_eq!(plan.staged, with_suffix(&exe, ".new"));
        apply(&plan, &|_| false).unwrap();
        assert!(std::fs::metadata(&exe).unwrap().len() > 1_000_000);
        assert_eq!(
            std::fs::read(with_suffix(&exe, ".old")).unwrap(),
            b"the running program"
        );

        // macOS and Linux: the real files unpack the way the installer expects.
        let mac = get(Kind::Mac).asset.unwrap();
        let mac_zip = dir.path().join("mac.zip");
        assert_eq!(
            Some(download(&mac, &mac_zip, &Progress::default()).unwrap()),
            mac.sha256
        );
        let app = dir.path().join("Pro Wave.app.new");
        unpack_app(&mac_zip, &app).unwrap();
        assert!(app.join("Contents/MacOS/prowave").is_file());
        let linux = get(Kind::Tarball).asset.unwrap();
        let tgz = dir.path().join("linux.tar.gz");
        assert_eq!(
            Some(download(&linux, &tgz, &Progress::default()).unwrap()),
            linux.sha256
        );
        let program = dir.path().join("prowave.new");
        unpack_from_tarball(&tgz, &program).unwrap();
        assert!(std::fs::metadata(&program).unwrap().len() > 1_000_000);

        // A package is downloaded whole and checked, ready for the package manager.
        let deb_release = get(Kind::Deb);
        let deb_target = Target::new(Kind::Deb, "/usr/bin/prowave");
        let staged = stage_for(&deb_release, &Progress::default(), &deb_target, &|n| {
            n == "pkexec"
        })
        .unwrap();
        let Staged::Ready(plan) = staged else {
            panic!("a .deb is ready for pkexec");
        };
        let deb_asset = deb_release.asset.unwrap();
        assert!(plan.staged.to_string_lossy().ends_with(&deb_asset.name));
        let bytes = std::fs::read(&plan.staged).unwrap();
        assert_eq!(bytes.len() as u64, deb_asset.size);
        assert_eq!(&bytes[..7], b"!<arch>", "a real .deb");
        std::fs::remove_file(&plan.staged).unwrap();
    }
}
