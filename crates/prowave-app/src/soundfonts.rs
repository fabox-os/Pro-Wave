//! SoundFont (`.sf2`) files the project's instrument tracks use. Each file is read and parsed
//! once and shared by every track that plays it; the engine plays the presets (see
//! `prowave_engine::SfPlayer`).
//!
//! A General MIDI SoundFont is about 150 MB, so a file is read on a background thread
//! ([`request`]) and the screen shows how far the read is ([`progress`]) instead of freezing.
//! The file is parsed as it is read, so it is never held twice in memory.

use prowave_engine::{SoundFontBank, MAX_SOUNDFONT_BYTES};
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Cache = Mutex<HashMap<String, Arc<SoundFontBank>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A read in progress: how many bytes of the file have been read.
struct Reading {
    done: Arc<AtomicU64>,
    total: u64,
    started: Instant,
}

/// What is happening to the files asked for: reads in progress and reads that failed.
#[derive(Default)]
struct Jobs {
    reading: HashMap<String, Reading>,
    failed: HashMap<String, String>,
}

fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Jobs::default()))
}

/// Where a SoundFont is: ready to play, being read, or unreadable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Loading,
    Failed(String),
}

/// How far a read has got.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    /// 0.0 to 1.0.
    pub fraction: f32,
    pub elapsed: Duration,
    /// Time left, once enough has been read to tell.
    pub remaining: Option<Duration>,
}

/// Counts the bytes that pass through it.
struct Counting<R> {
    inner: R,
    done: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.done.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Estimates the progress of a read of `total` bytes of which `done` were read in `elapsed`.
fn estimate(done: u64, total: u64, elapsed: Duration) -> Progress {
    let fraction = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0) as f32
    };
    // Too early to tell in the first moments: the rate is not steady yet.
    let remaining = (fraction >= 0.02 && elapsed >= Duration::from_millis(300))
        .then(|| elapsed.mul_f64((1.0 / f64::from(fraction) - 1.0).max(0.0)));
    Progress {
        fraction,
        elapsed,
        remaining,
    }
}

/// Reads and parses the file at `path`, counting the bytes read into `done`.
fn read_bank(path: &str, done: &Arc<AtomicU64>) -> Result<Arc<SoundFontBank>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let reader = BufReader::with_capacity(
        1 << 20,
        Counting {
            inner: file.take(MAX_SOUNDFONT_BYTES as u64),
            done: done.clone(),
        },
    );
    let mut reader = reader;
    SoundFontBank::load_reader(&mut reader)
        .map(Arc::new)
        .map_err(|e| format!("{path}: {e}"))
}

fn size_of(path: &str) -> Result<u64, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > MAX_SOUNDFONT_BYTES as u64 {
        return Err(format!(
            "{path}: larger than {} MB",
            MAX_SOUNDFONT_BYTES >> 20
        ));
    }
    Ok(meta.len())
}

/// Starts reading the SoundFont at `path` on a background thread, unless it is loaded or being
/// read already, and returns where it is. A failed file stays failed until [`forget`] is called
/// for it. `repaint` is called when a read ends, so the screen shows the result.
pub fn request(path: &str, repaint: impl FnOnce() + Send + 'static) -> Status {
    if loaded(path).is_some() {
        return Status::Ready;
    }
    let Ok(mut j) = jobs().lock() else {
        return Status::Failed("the SoundFont list is unavailable".into());
    };
    if j.reading.contains_key(path) {
        return Status::Loading;
    }
    if let Some(e) = j.failed.get(path) {
        return Status::Failed(e.clone());
    }
    let total = match size_of(path) {
        Ok(total) => total,
        Err(e) => {
            j.failed.insert(path.to_string(), e.clone());
            return Status::Failed(e);
        }
    };
    let done = Arc::new(AtomicU64::new(0));
    j.reading.insert(
        path.to_string(),
        Reading {
            done: done.clone(),
            total,
            started: Instant::now(),
        },
    );
    let name = path.to_string();
    let spawned = std::thread::Builder::new()
        .name("soundfont-load".into())
        .spawn(move || {
            let result = read_bank(&name, &done);
            if let Ok(mut j) = jobs().lock() {
                j.reading.remove(&name);
                match &result {
                    Ok(bank) => {
                        if let Ok(mut map) = cache().lock() {
                            map.insert(name.clone(), Arc::clone(bank));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{e}");
                        j.failed.insert(name.clone(), e.clone());
                    }
                }
            }
            repaint();
        });
    if spawned.is_err() {
        j.reading.remove(path);
        let e = format!("{path}: cannot start the loading thread");
        j.failed.insert(path.to_string(), e.clone());
        return Status::Failed(e);
    }
    Status::Loading
}

/// How far the read of `path` has got, while one is running.
pub fn progress(path: &str) -> Option<Progress> {
    let j = jobs().lock().ok()?;
    let r = j.reading.get(path)?;
    Some(estimate(
        r.done.load(Ordering::Relaxed),
        r.total,
        r.started.elapsed(),
    ))
}

/// The SoundFont at `path`, read and parsed on first use, waiting for it. Errors (a missing or
/// unreadable file, a file that is not a SoundFont, or one larger than [`MAX_SOUNDFONT_BYTES`])
/// are returned, not cached, so a fixed file is picked up the next time. The program uses
/// [`request`] instead, which does not wait.
pub fn load(path: &str) -> Result<Arc<SoundFontBank>, String> {
    if let Some(bank) = loaded(path) {
        return Ok(bank);
    }
    size_of(path)?;
    let bank = read_bank(path, &Arc::new(AtomicU64::new(0)))?;
    if let Ok(mut map) = cache().lock() {
        map.insert(path.to_string(), Arc::clone(&bank));
    }
    Ok(bank)
}

/// The SoundFont at `path` if it is already loaded (never reads the file).
pub fn loaded(path: &str) -> Option<Arc<SoundFontBank>> {
    cache().lock().ok()?.get(path).map(Arc::clone)
}

/// Forgets the file at `path` (and a failed read of it), so the next read starts again (after the
/// file was replaced).
pub fn forget(path: &str) {
    if let Ok(mut map) = cache().lock() {
        map.remove(path);
    }
    if let Ok(mut j) = jobs().lock() {
        j.failed.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let path = "/nonexistent/prowave-test-missing.sf2";
        assert!(load(path).is_err());
        assert!(loaded(path).is_none());
    }

    #[test]
    fn a_file_that_is_not_a_soundfont_is_an_error() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("not-a-font.sf2");
        assert!(std::fs::write(&path, b"this is not a SoundFont").is_ok());
        let Some(path) = path.to_str() else {
            panic!("a UTF-8 path");
        };
        assert!(load(path).is_err());
        assert!(loaded(path).is_none());
    }

    /// The General MIDI SoundFont named by `PROWAVE_TEST_SF2`, if set (it is not in the repo).
    #[test]
    fn a_soundfont_is_loaded_once_and_then_shared() {
        let Ok(path) = std::env::var("PROWAVE_TEST_SF2") else {
            return;
        };
        let Ok(first) = load(&path) else {
            panic!("load {path}");
        };
        let Ok(second) = load(&path) else {
            panic!("load again");
        };
        assert!(Arc::ptr_eq(&first, &second), "one copy in memory");
        assert!(loaded(&path).is_some());
        forget(&path);
        assert!(loaded(&path).is_none());
    }

    #[test]
    fn progress_is_the_share_of_bytes_read_and_the_time_left_follows_the_pace() {
        let p = estimate(0, 100, Duration::from_millis(100));
        assert_eq!(p.fraction, 0.0);
        assert!(p.remaining.is_none(), "nothing to go by yet");
        let p = estimate(25, 100, Duration::from_secs(2));
        assert!((p.fraction - 0.25).abs() < 1e-6);
        let left = p.remaining.map(|d| d.as_secs_f64());
        assert!(left.is_some_and(|s| (s - 6.0).abs() < 1e-6), "{left:?}");
        let p = estimate(100, 100, Duration::from_secs(8));
        assert_eq!(p.remaining, Some(Duration::ZERO));
        assert_eq!(estimate(5, 0, Duration::from_secs(1)).fraction, 0.0);
    }

    #[test]
    fn a_bad_file_is_read_in_the_background_and_reported_as_failed() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("bad.sf2");
        assert!(std::fs::write(&path, b"this is not a SoundFont").is_ok());
        let Some(path) = path.to_str() else {
            panic!("a UTF-8 path");
        };
        request(path, || {});
        let until = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match request(path, || {}) {
                Status::Loading if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => break other,
            }
        };
        assert!(matches!(status, Status::Failed(_)), "{status:?}");
        assert!(progress(path).is_none());
        // Failed stays failed (no read every frame) until it is forgotten.
        assert!(matches!(request(path, || {}), Status::Failed(_)));
        forget(path);
        assert!(!matches!(request(path, || {}), Status::Ready));
        forget(path);
    }
}
