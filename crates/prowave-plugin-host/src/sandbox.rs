//! A plug-in in a process of its own (Logic does this for Audio Units on Apple silicon): if the
//! plug-in crashes or hangs, the process dies or falls behind, and Pro Wave goes on with the
//! sound passing through the slot.
//!
//! Two halves, both here. [`Remote`] is the side inside Pro Wave. [`serve`] is the plug-in's
//! process: it loads the plug-in the ordinary way ([`crate::load`]) and answers.
//!
//! **Sound** crosses in shared memory. The audio thread writes a block, raises a semaphore and
//! spins until the other process has answered (about the time of a thread switch). If the
//! answer does not come in [`WAIT`], that block passes dry and no more are sent until the
//! plug-in has caught up, so a plug-in that hangs costs one late block and is then out of the
//! way. Nothing on the audio thread allocates or takes a lock; raising the semaphore is its one
//! system call for each block.
//!
//! **Everything else** (parameters, state, the window) is lines of text on the process's pipes:
//! a request gets one answer (`r` lines), and the process reports what changes on its own (`e`
//! lines: a parameter its window moved, its latency, its window closing). The process ends when
//! its input closes, so it never outlives Pro Wave.
use crate::{Error, ParamInfo, PluginInfo, MAX_CHANGES};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The largest block that crosses in one exchange.
pub(crate) const MAX_BLOCK: usize = 1024;
/// How long the audio thread waits for a block before it passes dry.
const WAIT: Duration = Duration::from_millis(40);
/// How long an offline render waits for one block. Offline nothing is real time, so the wait
/// is as long as the plug-in needs; this cap only stops a plug-in that has hung (without dying)
/// from holding the render forever. A plug-in that died is given up at once.
const OFFLINE_WAIT: Duration = Duration::from_secs(30);
/// How long a plug-in may take to load in its process.
const START: Duration = Duration::from_secs(30);
/// How long a request on the pipes may take.
const ANSWER: Duration = Duration::from_secs(3);
/// How often the plug-in's process looks for parameters that moved, and runs its idle work.
const TICK: Duration = Duration::from_millis(33);

/// What both processes map. Only atomics and plain numbers: either side may stop at any point.
#[repr(C)]
struct Shared {
    /// The number of the block the audio thread asked for, and of the last one done.
    asked: AtomicU32,
    done: AtomicU32,
    frames: AtomicU32,
    reset: AtomicU32,
    quit: AtomicU32,
    change_count: AtomicU32,
    /// Parameter index, position as `f32` bits.
    changes: [[AtomicU32; 2]; MAX_CHANGES],
    /// The block: sound in, the plug-in's sound out, as `f32` bits.
    audio: [[AtomicU32; MAX_BLOCK]; 2],
}

/// The shared memory, mapped. `owner` made it and takes its name away at the end.
struct Map {
    shared: *mut Shared,
    name: CString,
    owner: bool,
}

// SAFETY: `shared` points to a mapping that lives as long as `Map`, and `Shared` is all atomics.
unsafe impl Send for Map {}
// SAFETY: as above.
unsafe impl Sync for Map {}

impl Map {
    fn open(name: &str, create: bool) -> Option<Map> {
        let name = CString::new(name).ok()?;
        let size = std::mem::size_of::<Shared>();
        let flags = if create {
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR
        } else {
            libc::O_RDWR
        };
        // SAFETY: `name` is NUL-terminated; the mode is the variadic argument `shm_open` reads
        // when it creates. A new object is zero-filled by `ftruncate`, and all-zero is a valid
        // `Shared`.
        unsafe {
            let fd = libc::shm_open(name.as_ptr(), flags, 0o600 as libc::c_uint);
            if fd < 0 {
                return None;
            }
            if create && libc::ftruncate(fd, size as libc::off_t) != 0 {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
                return None;
            }
            let at = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            libc::close(fd);
            if at == libc::MAP_FAILED {
                if create {
                    libc::shm_unlink(name.as_ptr());
                }
                return None;
            }
            Some(Map {
                shared: at.cast(),
                name,
                owner: create,
            })
        }
    }

    fn get(&self) -> &Shared {
        // SAFETY: the mapping lives as long as `self` and is at least one `Shared` long.
        unsafe { &*self.shared }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: the mapping was made by `open` with this size; the name is ours to remove.
        unsafe {
            libc::munmap(self.shared.cast(), std::mem::size_of::<Shared>());
            if self.owner {
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }
}

/// The named semaphore that wakes the plug-in's audio thread.
struct Wake {
    sem: *mut libc::sem_t,
    name: CString,
    owner: bool,
}

// SAFETY: a POSIX semaphore may be posted and waited on from any thread.
unsafe impl Send for Wake {}
// SAFETY: as above.
unsafe impl Sync for Wake {}

impl Wake {
    fn open(name: &str, create: bool) -> Option<Wake> {
        let name = CString::new(name).ok()?;
        // SAFETY: `name` is NUL-terminated; the mode and first value are the variadic arguments
        // `sem_open` reads when it creates.
        let sem = unsafe {
            if create {
                libc::sem_open(
                    name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL,
                    0o600 as libc::c_uint,
                    0 as libc::c_uint,
                )
            } else {
                libc::sem_open(name.as_ptr(), 0)
            }
        };
        (sem != libc::SEM_FAILED).then_some(Wake {
            sem,
            name,
            owner: create,
        })
    }

    fn post(&self) {
        // SAFETY: `sem` is open until `drop`.
        unsafe { libc::sem_post(self.sem) };
    }

    /// Waits until the other side posts (an interrupted wait starts again).
    fn wait(&self) {
        // SAFETY: `sem` is open until `drop`.
        while unsafe { libc::sem_wait(self.sem) } != 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: `sem` came from `sem_open`; the name is ours to remove.
        unsafe {
            libc::sem_close(self.sem);
            if self.owner {
                libc::sem_unlink(self.name.as_ptr());
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// A text for one field of a line: tabs and line ends become spaces.
fn field(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

// ---- Inside Pro Wave ------------------------------------------------------------------------

/// What the thread that reads the process's output shares with [`Remote`].
struct Mirror {
    values: Mutex<Vec<f32>>,
    latency: AtomicUsize,
    dead: AtomicBool,
    editor_open: AtomicBool,
    power_clicks: AtomicU32,
}

struct Pipes {
    child: Child,
    input: ChildStdin,
    answers: Receiver<String>,
}

/// What the audio thread keeps for the plug-in.
struct AudioSide {
    /// A block was not answered in time; none is sent until the plug-in has caught up.
    behind: bool,
    pending: usize,
    changes: [(u32, f32); MAX_CHANGES],
    /// Offline rendering: wait for every answer (see [`OFFLINE_WAIT`]) instead of passing late
    /// blocks dry.
    offline: bool,
}

/// A plug-in running in its own process, seen from Pro Wave.
pub(crate) struct Remote {
    info: PluginInfo,
    params: Vec<ParamInfo>,
    has_editor: bool,
    mirror: Arc<Mirror>,
    /// The plug-in's wording of each parameter, for the position it was asked at.
    texts: Mutex<HashMap<usize, (u32, String)>>,
    pipes: Mutex<Pipes>,
    map: Map,
    wake: Wake,
    audio: UnsafeCell<AudioSide>,
}

// SAFETY: `audio` is used only by the one `RemoteProcessor` being played (the engine plays one
// handle at a time, and its methods take `&mut self`); everything else is a `Mutex`, an atomic
// or immutable.
unsafe impl Send for Remote {}
// SAFETY: as above.
unsafe impl Sync for Remote {}

/// A number for this process's next plug-in, for the names of what it shares.
fn next_number() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Remote {
    pub(crate) fn start(
        info: &PluginInfo,
        rate: f64,
        block: usize,
        program: &Path,
    ) -> Result<Arc<Remote>, Error> {
        let fail = |why: &str| Error::Process(why.to_string());
        // macOS allows 31 characters in these names.
        let tag = format!("{}-{}", std::process::id(), next_number());
        let (map_name, wake_name) = (format!("/pwm-{tag}"), format!("/pws-{tag}"));
        let map = Map::open(&map_name, true).ok_or_else(|| fail("no shared memory"))?;
        let wake = Wake::open(&wake_name, true).ok_or_else(|| fail("no semaphore"))?;
        let mut child = Command::new(program)
            .arg(crate::HOST_ARG)
            .arg(info.format.name())
            .arg(&info.id)
            .arg(rate.to_string())
            .arg(block.to_string())
            .arg(&map_name)
            .arg(&wake_name)
            .arg(&info.name)
            .arg(&info.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| fail(&e.to_string()))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(fail("no pipes"));
        };
        let mirror = Arc::new(Mirror {
            values: Mutex::new(Vec::new()),
            latency: AtomicUsize::new(0),
            dead: AtomicBool::new(false),
            editor_open: AtomicBool::new(false),
            power_clicks: AtomicU32::new(0),
        });
        let (tx, answers) = std::sync::mpsc::channel();
        let for_reader = Arc::clone(&mirror);
        std::thread::spawn(move || read_output(output, &for_reader, &tx));

        // The process says what it loaded: `ready`, its parameters, `end`; or `fail`.
        let mut pipes = Pipes {
            child,
            input,
            answers,
        };
        let deadline = Instant::now() + START;
        let next = || {
            let left = deadline.saturating_duration_since(Instant::now());
            pipes.answers.recv_timeout(left).ok()
        };
        let stop = |pipes: &mut Pipes, error: Error| {
            let _ = pipes.child.kill();
            let _ = pipes.child.wait();
            error
        };
        let Some(first) = next() else {
            return Err(stop(&mut pipes, fail("the plug-in did not load in time")));
        };
        let mut parts = first.split('\t');
        match parts.next() {
            Some("ready") => {}
            Some("fail") => {
                let why = parts.next().unwrap_or("it did not say why").to_string();
                return Err(stop(&mut pipes, Error::Process(why)));
            }
            _ => return Err(stop(&mut pipes, fail("an answer that makes no sense"))),
        }
        let latency = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let has_editor = parts.next() == Some("1");
        let mut params = Vec::new();
        let mut values = Vec::new();
        loop {
            let Some(line) = next() else {
                return Err(stop(&mut pipes, fail("the parameters did not come")));
            };
            let mut parts = line.split('\t');
            match parts.next() {
                Some("end") => break,
                Some("param") if params.len() < crate::MAX_PARAMS => {
                    let mut number = || parts.next().and_then(|n| n.parse::<f32>().ok());
                    let (default, value) = (number().unwrap_or(0.0), number().unwrap_or(0.0));
                    params.push(ParamInfo {
                        name: parts.next().unwrap_or_default().to_string(),
                        default,
                    });
                    values.push(value);
                }
                _ => {}
            }
        }
        if let Ok(mut known) = mirror.values.lock() {
            *known = values;
        }
        mirror.latency.store(latency, Ordering::Relaxed);
        Ok(Arc::new(Remote {
            info: info.clone(),
            params,
            has_editor,
            mirror,
            texts: Mutex::default(),
            pipes: Mutex::new(pipes),
            map,
            wake,
            audio: UnsafeCell::new(AudioSide {
                behind: false,
                pending: 0,
                changes: [(0, 0.0); MAX_CHANGES],
                offline: false,
            }),
        }))
    }

    /// Sends one line; with `answer`, waits for the process's answer to it.
    fn send(&self, line: &str, answer: bool) -> Option<String> {
        if self.crashed() {
            return None;
        }
        let mut pipes = self.pipes.lock().ok()?;
        // An answer that came too late for its request is not this one's.
        while pipes.answers.try_recv().is_ok() {}
        writeln!(pipes.input, "{line}").ok()?;
        pipes.input.flush().ok()?;
        if answer {
            pipes.answers.recv_timeout(ANSWER).ok()
        } else {
            None
        }
    }

    pub(crate) fn info(&self) -> &PluginInfo {
        &self.info
    }

    pub(crate) fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    pub(crate) fn param(&self, index: usize) -> f32 {
        self.mirror
            .values
            .lock()
            .ok()
            .and_then(|v| v.get(index).copied())
            .unwrap_or(0.0)
    }

    pub(crate) fn set_param(&self, index: usize, norm: f32) {
        if index >= self.params.len() || !norm.is_finite() {
            return;
        }
        let norm = norm.clamp(0.0, 1.0);
        if let Ok(mut values) = self.mirror.values.lock() {
            values[index] = norm;
        }
        self.send(&format!("set\t{index}\t{norm}"), false);
    }

    pub(crate) fn param_text(&self, index: usize) -> String {
        if index >= self.params.len() {
            return String::new();
        }
        let value = self.param(index);
        if let Ok(texts) = self.texts.lock() {
            if let Some((at, text)) = texts.get(&index) {
                if *at == value.to_bits() {
                    return text.clone();
                }
            }
        }
        let text = self
            .send(&format!("text\t{index}\t{value}"), true)
            .and_then(|answer| answer.strip_prefix("ok\t").map(str::to_string))
            .unwrap_or_default();
        if let Ok(mut texts) = self.texts.lock() {
            texts.insert(index, (value.to_bits(), text.clone()));
        }
        text
    }

    pub(crate) fn latency_samples(&self) -> usize {
        self.mirror.latency.load(Ordering::Relaxed)
    }

    pub(crate) fn save_state(&self) -> Option<Vec<u8>> {
        let answer = self.send("save", true)?;
        unhex(answer.strip_prefix("ok\t")?)
    }

    pub(crate) fn load_state(&self, state: &[u8]) -> bool {
        // The process reports the parameters the state moved, as it does any other move.
        self.send(&format!("load\t{}", hex(state)), true).as_deref() == Some("ok")
    }

    /// Nothing to do here: the plug-in's main-thread work runs in its process.
    pub(crate) fn idle(&self) {}

    pub(crate) fn crashed(&self) -> bool {
        self.mirror.dead.load(Ordering::Relaxed)
    }

    pub(crate) fn has_editor(&self) -> bool {
        self.has_editor && !self.crashed()
    }

    pub(crate) fn open_editor(&self, title: &str, bypassed: bool) {
        let power = u8::from(!bypassed);
        self.send(&format!("editor\topen\t{power}\t{}", field(title)), false);
    }

    pub(crate) fn close_editor(&self) {
        self.send("editor\tclose", false);
    }

    pub(crate) fn editor_open(&self) -> bool {
        !self.crashed() && self.mirror.editor_open.load(Ordering::Relaxed)
    }

    pub(crate) fn take_power_click(&self) -> bool {
        self.mirror.power_clicks.swap(0, Ordering::Relaxed) % 2 == 1
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.map.get().quit.store(1, Ordering::Release);
        self.wake.post();
        if let Ok(pipes) = self.pipes.get_mut() {
            let _ = writeln!(pipes.input, "quit");
            let _ = pipes.input.flush();
            // A plug-in may take its time to close, or never do: it gets a moment, not more.
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until && matches!(pipes.child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = pipes.child.kill();
            let _ = pipes.child.wait();
        }
    }
}

/// Reads what the process writes until it ends: answers go to the request waiting for one,
/// the rest is kept in `mirror`.
fn read_output(output: std::process::ChildStdout, mirror: &Mirror, answers: &Sender<String>) {
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else {
            break;
        };
        if let Some(answer) = line.strip_prefix("r\t") {
            let _ = answers.send(answer.to_string());
            continue;
        }
        let Some(event) = line.strip_prefix("e\t") else {
            continue;
        };
        let mut parts = event.split('\t');
        let kind = parts.next();
        let mut number = || parts.next().and_then(|n| n.parse::<f32>().ok());
        match kind {
            Some("param") => {
                if let (Some(index), Some(value), Ok(mut values)) =
                    (number(), number(), mirror.values.lock())
                {
                    if let Some(slot) = values.get_mut(index as usize) {
                        *slot = value.clamp(0.0, 1.0);
                    }
                }
            }
            Some("latency") => {
                if let Some(samples) = number() {
                    mirror.latency.store(samples as usize, Ordering::Relaxed);
                }
            }
            Some("editor") => {
                let open = number() == Some(1.0);
                mirror.editor_open.store(open, Ordering::Relaxed);
            }
            Some("power") => {
                mirror.power_clicks.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
    mirror.editor_open.store(false, Ordering::Relaxed);
    mirror.dead.store(true, Ordering::Release);
}

/// The audio thread's handle on a plug-in in its own process.
#[derive(Clone)]
pub(crate) struct RemoteProcessor(Arc<Remote>);

impl RemoteProcessor {
    pub(crate) fn new(remote: &Arc<Remote>) -> Self {
        Self(Arc::clone(remote))
    }

    fn audio(&mut self) -> &mut AudioSide {
        // SAFETY: only the audio thread reaches `audio`, through the one handle being played
        // (see `Remote`), and `&mut self` keeps this handle to one caller.
        unsafe { &mut *self.0.audio.get() }
    }

    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let remote = Arc::as_ptr(&self.0);
        let audio = self.audio();
        // SAFETY: `remote` points into the `Arc` this handle holds.
        let remote = unsafe { &*remote };
        let shared = remote.map.get();
        let frames = left.len().min(right.len()).min(MAX_BLOCK);
        if frames == 0 || remote.crashed() {
            return;
        }
        let asked = shared.asked.load(Ordering::Relaxed);
        if audio.behind {
            if shared.done.load(Ordering::Acquire) != asked {
                return;
            }
            audio.behind = false;
        }
        for (slot, (index, norm)) in shared.changes.iter().zip(&audio.changes[..audio.pending]) {
            slot[0].store(*index, Ordering::Relaxed);
            slot[1].store(norm.to_bits(), Ordering::Relaxed);
        }
        shared
            .change_count
            .store(audio.pending as u32, Ordering::Relaxed);
        audio.pending = 0;
        for (i, (l, r)) in left.iter().zip(right.iter()).enumerate().take(frames) {
            shared.audio[0][i].store(l.to_bits(), Ordering::Relaxed);
            shared.audio[1][i].store(r.to_bits(), Ordering::Relaxed);
        }
        shared.frames.store(frames as u32, Ordering::Relaxed);
        let asked = asked.wrapping_add(1);
        shared.asked.store(asked, Ordering::Release);
        remote.wake.post();

        // The answer takes about a thread switch: spin, looking at the clock now and then.
        // Offline, nothing is real time: the wait yields the processor instead, and only a
        // plug-in that has died (or hung for `OFFLINE_WAIT`) is given up.
        let started = Instant::now();
        let mut spins = 0u32;
        let offline = audio.offline;
        while shared.done.load(Ordering::Acquire) != asked {
            if offline {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(256) {
                let limit = if offline { OFFLINE_WAIT } else { WAIT };
                if started.elapsed() > limit || remote.crashed() {
                    audio.behind = true;
                    return;
                }
            }
        }
        for (i, (l, r)) in left
            .iter_mut()
            .zip(right.iter_mut())
            .enumerate()
            .take(frames)
        {
            let (a, b) = (
                f32::from_bits(shared.audio[0][i].load(Ordering::Relaxed)),
                f32::from_bits(shared.audio[1][i].load(Ordering::Relaxed)),
            );
            // Whatever the other process wrote, only sound goes on.
            *l = if a.is_finite() { a } else { 0.0 };
            *r = if b.is_finite() { b } else { 0.0 };
        }
    }

    /// Offline rendering on or off: see [`AudioSide::offline`].
    pub(crate) fn set_offline(&mut self, offline: bool) {
        self.audio().offline = offline;
    }

    pub(crate) fn set_param_norm(&mut self, index: usize, norm: f32) {
        if index >= self.0.params.len() || !norm.is_finite() {
            return;
        }
        let audio = self.audio();
        if audio.pending < MAX_CHANGES {
            audio.changes[audio.pending] = (index as u32, norm.clamp(0.0, 1.0));
            audio.pending += 1;
        }
    }

    pub(crate) fn latency_samples(&self) -> usize {
        self.0.latency_samples()
    }

    pub(crate) fn reset(&mut self) {
        self.0.map.get().reset.store(1, Ordering::Release);
    }
}

// ---- The plug-in's process ------------------------------------------------------------------

/// What the process writes to Pro Wave: its real output, kept apart from anything a plug-in
/// prints.
struct Out(Mutex<std::fs::File>);

impl Out {
    /// Takes the process's standard output for the protocol and points the plug-in's prints at
    /// standard error instead.
    fn take() -> Out {
        use std::os::fd::FromRawFd;
        // SAFETY: descriptors 1 and 2 are open in a process started with pipes; the copy of 1
        // is owned by the `File` made from it.
        let file = unsafe {
            let copy = libc::dup(1);
            libc::dup2(2, 1);
            std::fs::File::from_raw_fd(if copy < 0 { 1 } else { copy })
        };
        Out(Mutex::new(file))
    }

    fn line(&self, text: &str) {
        if let Ok(mut file) = self.0.lock() {
            let _ = writeln!(file, "{text}");
            let _ = file.flush();
        }
    }
}

/// The plug-in's audio thread: a block each time Pro Wave raises the semaphore.
fn serve_audio(map: &Map, wake: &Wake, mut processor: crate::LocalProcessor, block: usize) {
    // SAFETY: a plain request to the scheduler about the calling thread.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
    let shared = map.get();
    let mut left = vec![0.0f32; block];
    let mut right = vec![0.0f32; block];
    loop {
        wake.wait();
        if shared.quit.load(Ordering::Acquire) != 0 {
            return;
        }
        let asked = shared.asked.load(Ordering::Acquire);
        if asked == shared.done.load(Ordering::Relaxed) {
            continue;
        }
        if shared.reset.swap(0, Ordering::AcqRel) != 0 {
            processor.reset();
        }
        let changes = (shared.change_count.load(Ordering::Relaxed) as usize).min(MAX_CHANGES);
        for slot in &shared.changes[..changes] {
            processor.set_param_norm(
                slot[0].load(Ordering::Relaxed) as usize,
                f32::from_bits(slot[1].load(Ordering::Relaxed)),
            );
        }
        let frames = (shared.frames.load(Ordering::Relaxed) as usize).min(block);
        for i in 0..frames {
            left[i] = f32::from_bits(shared.audio[0][i].load(Ordering::Relaxed));
            right[i] = f32::from_bits(shared.audio[1][i].load(Ordering::Relaxed));
        }
        processor.process(&mut left[..frames], &mut right[..frames]);
        for i in 0..frames {
            shared.audio[0][i].store(left[i].to_bits(), Ordering::Relaxed);
            shared.audio[1][i].store(right[i].to_bits(), Ordering::Relaxed);
        }
        shared.done.store(asked, Ordering::Release);
    }
}

/// The plug-in's main thread, called again and again: Pro Wave's requests, then what changed.
struct Server {
    instance: crate::Instance,
    out: Arc<Out>,
    requests: Receiver<String>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    name: String,
    latency: usize,
    /// Parameters Pro Wave just set: the plug-in may still report the old position for a
    /// moment, which must not travel back as a move.
    quiet: HashMap<usize, Instant>,
    last_tick: Instant,
    #[cfg(target_os = "macos")]
    window: Option<crate::mac::Window>,
}

impl Server {
    fn local(&self) -> Option<&crate::LocalInstance> {
        self.instance.local()
    }

    fn answer(&self, text: &str) {
        self.out.line(&format!("r\t{text}"));
    }

    fn request(&mut self, line: &str) {
        let mut parts = line.splitn(4, '\t');
        let (verb, a, b, c) = (parts.next(), parts.next(), parts.next(), parts.next());
        match verb {
            Some("set") => {
                let index = a.and_then(|n| n.parse::<usize>().ok());
                let norm = b.and_then(|n| n.parse::<f32>().ok());
                if let (Some(index), Some(norm)) = (index, norm) {
                    self.instance.set_param(index, norm);
                    self.quiet
                        .insert(index, Instant::now() + Duration::from_millis(250));
                }
            }
            Some("text") => {
                let index = a.and_then(|n| n.parse::<usize>().ok());
                let norm = b.and_then(|n| n.parse::<f32>().ok());
                let text = match (index, norm, self.local()) {
                    (Some(index), Some(norm), Some(local)) => {
                        field(&local.backend().main_text(index, norm))
                    }
                    _ => String::new(),
                };
                self.answer(&format!("ok\t{text}"));
            }
            Some("save") => match self.instance.save_state() {
                Some(state) => self.answer(&format!("ok\t{}", hex(&state))),
                None => self.answer("no"),
            },
            Some("load") => {
                let ok = a
                    .and_then(unhex)
                    .is_some_and(|state| self.instance.load_state(&state));
                // The state's positions are news to Pro Wave: forget what it was told.
                self.report_all();
                self.answer(if ok { "ok" } else { "no" });
            }
            Some("editor") => self.editor(a == Some("open"), b == Some("1"), c.unwrap_or("")),
            Some("quit") => std::process::exit(0),
            _ => {}
        }
    }

    fn report_all(&self) {
        for i in 0..self.instance.params().len() {
            self.out
                .line(&format!("e\tparam\t{i}\t{}", self.instance.param(i)));
        }
    }

    #[cfg(target_os = "macos")]
    fn editor(&mut self, open: bool, power: bool, title: &str) {
        let Some(local) = self.instance.local().cloned() else {
            return;
        };
        if open {
            if self.window.is_none() {
                let window = crate::mac::Window::new(&self.name);
                match local
                    .backend()
                    .main_editor_open(crate::EditorParent::Cocoa(window.parent()))
                {
                    Some((width, height)) => {
                        window.set_plugin_size(width, height);
                        self.window = Some(window);
                    }
                    None => return,
                }
            }
            if let Some(window) = &self.window {
                window.set_title(title);
                window.set_power(power);
                window.show();
                self.out.line("e\teditor\t1");
            }
        } else if let Some(window) = &self.window {
            window.hide();
            self.out.line("e\teditor\t0");
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn editor(&mut self, _open: bool, _power: bool, _title: &str) {}

    fn tick(&mut self) {
        loop {
            match self.requests.try_recv() {
                Ok(line) => self.request(&line),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Pro Wave closed the pipe: it is gone, and so is this process.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => std::process::exit(0),
            }
        }
        if self.last_tick.elapsed() < TICK {
            return;
        }
        self.last_tick = Instant::now();
        self.instance.idle();
        let now = Instant::now();
        self.quiet.retain(|_, until| *until > now);
        if let Some(local) = self.local() {
            for (index, value) in local.refresh(|index| self.quiet.contains_key(&index)) {
                self.out.line(&format!("e\tparam\t{index}\t{value}"));
            }
        }
        let latency = self.instance.latency_samples();
        if latency != self.latency {
            self.latency = latency;
            self.out.line(&format!("e\tlatency\t{latency}"));
        }
        #[cfg(target_os = "macos")]
        if let (Some(window), Some(local)) = (&self.window, self.instance.local()) {
            if let Some((width, height)) = local.backend().main_editor_resized() {
                window.set_plugin_size(width, height);
            }
            if window.take_closed() {
                self.out.line("e\teditor\t0");
            }
            if window.take_power_click() {
                self.out.line("e\tpower");
            }
        }
    }
}

/// The plug-in's process: loads the plug-in named by `args` (what [`Remote::start`] passes) and
/// serves it until Pro Wave closes the pipe. Never returns.
pub(crate) fn serve(args: &[OsString]) -> ! {
    let out = Arc::new(Out::take());
    let text = |i: usize| {
        args.get(i)
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let fail = |why: &str| -> ! {
        out.line(&format!("r\tfail\t{}", field(why)));
        std::process::exit(1)
    };
    let Some(format) = crate::Format::from_name(&text(0)) else {
        fail("an unknown plug-in format")
    };
    let rate = text(2).parse::<f64>().unwrap_or(48_000.0);
    let block = text(3).parse::<usize>().unwrap_or(64).clamp(1, MAX_BLOCK);
    let info = PluginInfo {
        format,
        id: text(1),
        name: text(6),
        vendor: String::new(),
        path: args
            .get(7)
            .map(std::path::PathBuf::from)
            .unwrap_or_default(),
    };
    let (Some(map), Some(wake)) = (Map::open(&text(4), false), Wake::open(&text(5), false)) else {
        fail("the shared memory is not there")
    };
    let instance = match crate::load(&info, rate, block) {
        Ok(instance) => instance,
        Err(error) => fail(&error.to_string()),
    };
    let Some(local) = instance.local().cloned() else {
        fail("the plug-in is not in this process")
    };
    let has_editor = cfg!(target_os = "macos") && local.backend().main_has_editor();
    let latency = instance.latency_samples();
    out.line(&format!("r\tready\t{latency}\t{}", u8::from(has_editor)));
    for (i, param) in instance.params().iter().enumerate() {
        out.line(&format!(
            "r\tparam\t{}\t{}\t{}",
            param.default,
            instance.param(i),
            field(&param.name)
        ));
    }
    out.line("r\tend");

    let processor = local.processor();
    std::thread::spawn(move || serve_audio(&map, &wake, processor, block));
    let (tx, requests) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if tx.send(line).is_err() {
                break;
            }
        }
        // Dropping `tx` tells the main thread the pipe closed.
    });
    let mut server = Server {
        instance,
        out,
        requests,
        name: info.name.clone(),
        latency,
        quiet: HashMap::new(),
        last_tick: Instant::now(),
        #[cfg(target_os = "macos")]
        window: None,
    };
    #[cfg(target_os = "macos")]
    crate::mac::run(Box::new(move || server.tick()));
    #[cfg(not(target_os = "macos"))]
    loop {
        server.tick();
        std::thread::sleep(Duration::from_millis(4));
    }
}
