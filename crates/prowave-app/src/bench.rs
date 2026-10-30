//! `prowave --bench <file>`: measures the lightweight targets from LINUX-MACOS-LIGHTWEIGHT.md
//! in a release build and writes them to `<file>` as `key=value` lines, for `cargo xtask bench`:
//!
//! - `startup_ms`: from `main` to the first frame on screen
//! - `idle_rss_mb`: memory in use with an empty project, once the window has settled
//! - `dsp24_load`: time to render 24 playing software-instrument tracks, as a share of the
//!   audio's own length (0.15 = 15 % of one CPU core)
//! - `dsp24_rss_mb`: memory in use after that render
//!
//! `prowave --layout-shots <folder>`: opens the demo song, resizes the window to each size in
//! [`LAYOUT_SIZES`] and saves a picture of what it drew as `layout-<w>x<h>.png`, to check the
//! layout on small screens on any system (a build server included).
//!
//! In bench mode the app touches none of the user's settings (recent projects,
//! interface size). When no window can be opened (a build server without a display), only the
//! DSP numbers are written, with `window_error`.
use crate::model::{Project, TrackKind};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const ARG: &str = "--bench";
pub const SHOTS_ARG: &str = "--layout-shots";

/// Window sizes (inner, in points) for `--layout-shots`: a large display, a 13-inch MacBook
/// Air, a common laptop, and a small window.
pub const LAYOUT_SIZES: [(f32, f32); 4] = [
    (1920.0, 1080.0),
    (1470.0, 920.0),
    (1280.0, 800.0),
    (1024.0, 700.0),
];
/// Frames to let the layout settle at each size before taking its picture.
const SHOT_FRAMES: u32 = 20;

/// Frames to wait after the first one before measuring idle memory (fonts, GPU buffers and the
/// audio device are all set up by then).
const SETTLE_FRAMES: u32 = 60;
/// Tracks, sample rate, callback size and length of the DSP measurement.
const DSP_TRACKS: usize = 24;
const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 256;
const RENDER_SECS: f64 = 20.0;

static START: OnceLock<Instant> = OnceLock::new();

/// Time spent building the interface, summed over the frames since the app settled.
static UI_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Times the frame it lives in (what `ui_ms` in the bench report averages).
pub struct FrameTimer(Instant);

pub fn frame_timer() -> FrameTimer {
    FrameTimer(Instant::now())
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        UI_MICROS.fetch_add(self.0.elapsed().as_micros() as u64, Relaxed);
        UI_FRAMES.fetch_add(1, Relaxed);
    }
}

/// A project of `n` tracks cloned from the demo's, each with its regions: to see how the
/// interface copes with a big song (`PROWAVE_BENCH_TRACKS=1000`).
pub fn many_tracks(n: usize) -> Project {
    let mut p = Project::demo();
    let base: Vec<_> = p
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Output)
        .cloned()
        .collect();
    p.tracks = (0..n)
        .map(|i| {
            let mut t = base[i % base.len()].clone();
            t.name = format!("Track {}", i + 1);
            t
        })
        .collect();
    p.position_beats = 0.0;
    p
}

/// Marks the start of `main`; `startup_ms` counts from here.
pub fn mark_start() {
    let _ = START.set(Instant::now());
}

/// The report file given after `--bench`, when the app runs as a benchmark.
pub fn requested(args: &[std::ffi::OsString]) -> Option<PathBuf> {
    value_after(args, ARG)
}

/// The folder given after `--layout-shots`.
pub fn shots_requested(args: &[std::ffi::OsString]) -> Option<PathBuf> {
    value_after(args, SHOTS_ARG)
}

fn value_after(args: &[std::ffi::OsString], flag: &str) -> Option<PathBuf> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(PathBuf::from)
}

/// Resident memory of this process, in MiB.
pub fn rss_mb() -> Option<f64> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    Some(sys.process(pid)?.memory() as f64 / (1024.0 * 1024.0))
}

/// A project with [`DSP_TRACKS`] software-instrument tracks, each playing a 12-bar region of
/// the demo song's notes.
pub fn dsp_project() -> Project {
    let mut p = Project::demo();
    let midi: Vec<_> = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Instrument || t.kind == TrackKind::Drummer)
        .cloned()
        .collect();
    p.tracks = (0..DSP_TRACKS)
        .map(|i| {
            let mut t = midi[i % midi.len()].clone();
            t.name = format!("Track {}", i + 1);
            t
        })
        .collect();
    p.position_beats = 0.0;
    p
}

/// What an offline render produced.
pub struct Render {
    /// Time spent in the engine, and the length of the audio it made.
    pub busy: Duration,
    pub audio_secs: f64,
    /// Loudest output sample.
    pub peak: f32,
    /// Output samples that were NaN or infinite.
    pub non_finite: usize,
    /// Tracks whose peak meter went NaN or infinite at some point.
    pub bad_meters: Vec<usize>,
}

/// Plays `p` from the start for `secs` through the engine, offline (no sound card), with each
/// track's Library patch, the way the app sends them.
pub fn render(p: &Project, secs: f64) -> Render {
    use prowave_engine::{Command, EngineCore, Shared};
    use std::sync::Arc;

    let (mut tx, rx) = rtrb::RingBuffer::<Command>::new(1024);
    let (trash_tx, mut trash_rx) = rtrb::RingBuffer::new(64);
    let shared = Arc::new(Shared::new());
    let mut core = EngineCore::new(SAMPLE_RATE, rx, trash_tx, shared.clone());
    let mut state = core.take_state_reader().expect("state reader");
    let mut commands = vec![
        Command::SetSong(Box::new(crate::engine_sync::offline_song(p))),
        Command::SetTempoMap(Box::new(p.tempo_map())),
        Command::SetCycle {
            on: true,
            start_beat: 0.0,
            end_beat: p.bar_to_beats(12.0),
        },
    ];
    for (i, t) in p.tracks.iter().enumerate() {
        commands.push(Command::SetTrackPatch {
            track: i as u16,
            patch: crate::patches::synth_patch(t),
        });
    }
    commands.push(Command::SetPosition { beat: 0.0 });
    commands.push(Command::Play);
    for cmd in commands {
        if tx.push(cmd).is_err() {
            tracing::warn!("bench: engine command queue full");
        }
    }

    let mut out = vec![0.0f32; BLOCK_FRAMES * 2];
    let blocks = (secs * SAMPLE_RATE as f64 / BLOCK_FRAMES as f64).ceil() as usize;
    let mut r = Render {
        busy: Duration::ZERO,
        audio_secs: (blocks * BLOCK_FRAMES) as f64 / SAMPLE_RATE as f64,
        peak: 0.0,
        non_finite: 0,
        bad_meters: Vec::new(),
    };
    for _ in 0..blocks {
        let t0 = Instant::now();
        core.process(&mut out);
        r.busy += t0.elapsed();
        for s in &out {
            if s.is_finite() {
                r.peak = r.peak.max(s.abs());
            } else {
                r.non_finite += 1;
            }
        }
        for i in 0..p.tracks.len() {
            let (l, rr) = state.read().track_peaks(i);
            if !(l.is_finite() && rr.is_finite()) && !r.bad_meters.contains(&i) {
                r.bad_meters.push(i);
            }
        }
        while trash_rx.pop().is_ok() {}
    }
    r
}

/// Renders [`RENDER_SECS`] of [`dsp_project`] and returns the render time as a share of the
/// audio's length, and the loudest sample (to prove it played).
pub fn dsp_load() -> (f64, f32) {
    let r = render(&dsp_project(), RENDER_SECS);
    (r.busy.as_secs_f64() / r.audio_secs, r.peak)
}

/// Collects the numbers while the app runs and writes the report.
pub struct Probe {
    out: PathBuf,
    frames: u32,
    startup_ms: Option<f64>,
}

impl Probe {
    pub fn new(out: PathBuf) -> Self {
        Self {
            out,
            frames: 0,
            startup_ms: None,
        }
    }

    /// Call once per frame. Returns true when the report is written and the app should close.
    pub fn frame(&mut self, ctx: &egui::Context) -> bool {
        self.frames += 1;
        if self.startup_ms.is_none() {
            self.startup_ms = START.get().map(|s| s.elapsed().as_secs_f64() * 1000.0);
        }
        ctx.request_repaint();
        if self.frames < SETTLE_FRAMES {
            return false;
        }
        let mut report = String::new();
        if let Some(ms) = self.startup_ms {
            let _ = writeln!(report, "startup_ms={ms:.1}");
        }
        if let Some(mb) = rss_mb() {
            let _ = writeln!(report, "idle_rss_mb={mb:.1}");
        }
        {
            use std::sync::atomic::Ordering::Relaxed;
            let frames = UI_FRAMES.load(Relaxed).max(1);
            let _ = writeln!(
                report,
                "ui_ms={:.2}",
                UI_MICROS.load(Relaxed) as f64 / frames as f64 / 1000.0
            );
        }
        dsp_lines(&mut report);
        write_report(&self.out, &report);
        true
    }
}

fn dsp_lines(report: &mut String) {
    let (load, peak) = dsp_load();
    let _ = writeln!(report, "dsp24_load={load:.4}");
    let _ = writeln!(report, "dsp24_peak={peak:.3}");
    if let Some(mb) = rss_mb() {
        let _ = writeln!(report, "dsp24_rss_mb={mb:.1}");
    }
}

fn write_report(out: &std::path::Path, report: &str) {
    if let Err(e) = std::fs::write(out, report) {
        tracing::error!("bench: cannot write {}: {e}", out.display());
    }
}

/// Steps through [`LAYOUT_SIZES`], saving a picture at each (see the module docs).
pub struct LayoutShots {
    dir: PathBuf,
    size: usize,
    frames: u32,
    /// Set by the screenshot callback once the picture of the current size is saved.
    saved: std::sync::Arc<std::sync::atomic::AtomicBool>,
    requested: bool,
}

impl LayoutShots {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            size: 0,
            frames: 0,
            saved: Default::default(),
            requested: false,
        }
    }

    /// Call once per frame. Returns true when every picture is saved and the app should close.
    pub fn frame(&mut self, ctx: &egui::Context) -> bool {
        use std::sync::atomic::Ordering;
        ctx.request_repaint();
        let Some(&(w, h)) = LAYOUT_SIZES.get(self.size) else {
            return true;
        };
        if self.frames == 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
        }
        self.frames += 1;
        if self.frames == SHOT_FRAMES && !self.requested {
            self.requested = true;
            let file = self.dir.join(format!("layout-{w}x{h}.png"));
            let saved = self.saved.clone();
            ctx.request_screenshot(move |image| {
                save_png(&file, &image);
                saved.store(true, Ordering::Release);
            });
        }
        if self.saved.swap(false, Ordering::Acquire) {
            self.size += 1;
            self.frames = 0;
            self.requested = false;
        }
        false
    }
}

fn save_png(file: &std::path::Path, image: &egui::ColorImage) {
    let [w, h] = image.size;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    let result = std::fs::create_dir_all(file.parent().unwrap_or(std::path::Path::new(".")))
        .map_err(|e| e.to_string())
        .and_then(|_| {
            image::save_buffer(file, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .map_err(|e| e.to_string())
        });
    match result {
        Ok(()) => tracing::info!("layout shot saved: {}", file.display()),
        Err(e) => tracing::error!("cannot save {}: {e}", file.display()),
    }
}

/// The window could not be opened: report the DSP numbers and why there are no others.
pub fn report_without_window(out: &std::path::Path, error: &str) {
    let mut report = format!("window_error={}\n", error.replace('\n', " "));
    dsp_lines(&mut report);
    write_report(out, &report);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bench_flag_takes_the_report_file() {
        let args: Vec<std::ffi::OsString> = vec!["--play".into(), ARG.into(), "out.txt".into()];
        assert_eq!(requested(&args), Some(PathBuf::from("out.txt")));
        assert_eq!(requested(&[ARG.into()]), None, "no file, no bench");
        assert_eq!(requested(&[]), None);
        let args: Vec<std::ffi::OsString> = vec![SHOTS_ARG.into(), "shots".into()];
        assert_eq!(shots_requested(&args), Some(PathBuf::from("shots")));
        assert_eq!(requested(&args), None);
    }

    #[test]
    fn the_dsp_project_has_24_playing_instrument_tracks() {
        let p = dsp_project();
        assert_eq!(p.tracks.len(), DSP_TRACKS);
        assert!(p
            .tracks
            .iter()
            .all(|t| t.kind != TrackKind::Audio && !t.regions.is_empty()));
        assert!(!crate::engine_sync::offline_song(&p).notes.is_empty());
    }
}
