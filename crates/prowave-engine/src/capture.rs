//! Audio input and recording: the "Input" of an audio track in Logic.
//!
//! The device's callback does the least possible: it measures the peaks and copies the samples
//! into a lock-free ring (no allocation, no lock, no file access). A writer thread takes them
//! from the ring and writes one 32-bit float WAV file per recording, so a slow disk never
//! reaches the audio thread. Recordings are cut from the same stream, so several tracks can
//! record from different inputs of one interface at the same time.
//!
//! The file is valid while it grows (its header is refreshed every second), so a crash keeps
//! what was recorded.
use crate::monitor::InputFeed;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Error, ErrorKind, FromSample, InputCallbackInfo, SampleFormat, SizedSample, Stream,
    StreamConfig,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Most input channels that are metered and can be recorded.
pub const MAX_INPUT_CHANNELS: usize = 32;
/// How much audio the ring holds when the writer thread is held up.
const RING_SECONDS: usize = 4;
/// How far back a recording can reach when the writer thread was a moment late to start it.
const HISTORY_SECONDS: usize = 1;
/// How often the header of a growing file is brought up to date.
const HEADER_REFRESH: Duration = Duration::from_secs(1);
/// A WAV file cannot hold more audio than its 32-bit sizes can count.
const MAX_DATA_BYTES: u64 = u32::MAX as u64 - 36;

/// Which input channels a track records (Logic: the Input slot of an audio channel strip).
/// Channels count from 0: `Mono(0)` is "Input 1", `Stereo(0)` is "Input 1 + 2".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    Mono(u16),
    Stereo(u16),
}

impl InputSource {
    /// Channels in the recorded file.
    pub fn file_channels(self) -> u16 {
        match self {
            Self::Mono(_) => 1,
            Self::Stereo(_) => 2,
        }
    }

    /// How many channels the device needs for this source.
    pub fn needs(self) -> u16 {
        match self {
            Self::Mono(c) => c.saturating_add(1),
            Self::Stereo(c) => c.saturating_add(2),
        }
    }
}

/// An audio input device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputInfo {
    /// Identifies the device to [`Capture::open`] (the system's own id, with the audio system).
    pub id: String,
    /// The name to show.
    pub name: String,
    /// Channels in its default format.
    pub channels: u16,
    /// Whether it is the system's default input.
    pub is_default: bool,
}

#[derive(Debug)]
pub enum CaptureError {
    NoInputDevice,
    /// The system does not let this program use the microphone.
    PermissionDenied,
    Backend(String),
    /// The device has fewer channels than the track's input needs.
    MissingChannels {
        needs: u16,
        has: u16,
    },
    Io(std::io::Error),
    /// The writer thread is gone, or does not know the recording.
    Closed,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoInputDevice => write!(f, "no audio input device"),
            Self::PermissionDenied => write!(f, "the system does not allow access to the input"),
            Self::Backend(e) => write!(f, "audio input error: {e}"),
            Self::MissingChannels { needs, has } => {
                write!(f, "the input needs {needs} channels, the device has {has}")
            }
            Self::Io(e) => write!(f, "cannot write the recording: {e}"),
            Self::Closed => write!(f, "the recording is not running"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn backend(e: Error) -> CaptureError {
    if e.kind() == ErrorKind::PermissionDenied {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Backend(e.to_string())
    }
}

/// A finished recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Audio was lost: the ring overflowed, the file filled up, or the disk failed. The file
    /// holds what was written before.
    pub lost: bool,
}

/// State shared by the device callback, the writer thread and the UI.
struct Shared {
    /// Frames the callback has put in the ring since the stream started.
    frames_pushed: AtomicU64,
    /// Callbacks whose audio did not fit in the ring.
    overruns: AtomicU64,
    /// Glitches the device reported. The stream goes on, so a recording does too.
    xruns: AtomicU64,
    /// Delay from the converter to the callback, as the system reports it.
    latency_us: AtomicU32,
    /// Peak of each channel since the last [`Capture::take_peaks`] (an `f32` as bits; for
    /// non-negative floats the order of the bits is the order of the values).
    peaks: [AtomicU32; MAX_INPUT_CHANNELS],
}

impl Shared {
    fn new() -> Self {
        Self {
            frames_pushed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            latency_us: AtomicU32::new(0),
            peaks: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

/// The device callback's work: peaks, then the samples into the ring. Never allocates or
/// waits.
fn push<T: SizedSample>(data: &[T], channels: usize, producer: &mut Producer<f32>, shared: &Shared)
where
    f32: FromSample<T>,
{
    let data = &data[..data.len() / channels * channels];
    let mut peak = [0f32; MAX_INPUT_CHANNELS];
    for frame in data.chunks_exact(channels) {
        for (c, s) in frame.iter().take(MAX_INPUT_CHANNELS).enumerate() {
            let v = <f32 as FromSample<T>>::from_sample_(*s).abs();
            if v > peak[c] {
                peak[c] = v;
            }
        }
    }
    for (slot, p) in shared.peaks.iter().zip(peak).take(channels) {
        slot.fetch_max(p.to_bits(), Ordering::Relaxed);
    }
    match producer.write_chunk_uninit(data.len()) {
        Ok(chunk) => {
            chunk.fill_from_iter(
                data.iter()
                    .map(|s| <f32 as FromSample<T>>::from_sample_(*s)),
            );
            shared
                .frames_pushed
                .fetch_add((data.len() / channels) as u64, Ordering::Release);
        }
        Err(_) => {
            shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The same audio for monitoring: nothing is counted as lost when no one takes it.
fn push_monitor<T: SizedSample>(data: &[T], monitor: &mut Producer<f32>)
where
    f32: FromSample<T>,
{
    if let Ok(chunk) = monitor.write_chunk_uninit(data.len()) {
        chunk.fill_from_iter(
            data.iter()
                .map(|s| <f32 as FromSample<T>>::from_sample_(*s)),
        );
    }
}

/// A 32-bit float WAV file that grows.
struct WavFile {
    out: BufWriter<File>,
    channels: u16,
    sample_rate: u32,
    data_bytes: u64,
}

impl WavFile {
    fn create(path: &Path, channels: u16, sample_rate: u32) -> std::io::Result<Self> {
        let mut file = Self {
            out: BufWriter::with_capacity(1 << 16, File::create(path)?),
            channels,
            sample_rate,
            data_bytes: 0,
        };
        let header = file.header();
        file.out.write_all(&header)?;
        Ok(file)
    }

    fn header(&self) -> [u8; 44] {
        let block = u32::from(self.channels) * 4;
        let data = self.data_bytes.min(MAX_DATA_BYTES) as u32;
        let mut h = [0u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
        h[8..16].copy_from_slice(b"WAVEfmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
        h[22..24].copy_from_slice(&self.channels.to_le_bytes());
        h[24..28].copy_from_slice(&self.sample_rate.to_le_bytes());
        h[28..32].copy_from_slice(&(self.sample_rate * block).to_le_bytes());
        h[32..34].copy_from_slice(&(block as u16).to_le_bytes());
        h[34..36].copy_from_slice(&32u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data.to_le_bytes());
        h
    }

    /// Whether one more frame still fits.
    fn has_room(&self) -> bool {
        self.data_bytes + u64::from(self.channels) * 4 <= MAX_DATA_BYTES
    }

    fn write_frame(&mut self, frame: &[f32]) -> std::io::Result<()> {
        for s in frame {
            let v = if s.is_finite() { *s } else { 0.0 };
            self.out.write_all(&v.to_le_bytes())?;
        }
        self.data_bytes += frame.len() as u64 * 4;
        Ok(())
    }

    /// Brings the header up to date, so what is written so far can be read.
    fn refresh(&mut self) -> std::io::Result<()> {
        let header = self.header();
        self.out.flush()?;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&header)?;
        self.out.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.refresh()?;
        self.out.flush()
    }
}

/// One recording on the writer thread.
struct Rec {
    path: PathBuf,
    source: InputSource,
    file: WavFile,
    /// The first frame of the stream that belongs to the recording.
    from_frame: u64,
    frames: u64,
    lost: bool,
    /// The file can take no more (full, or the disk failed).
    closed: bool,
}

impl Rec {
    /// Writes the part of `samples` (whole frames starting at stream frame `base`) that
    /// belongs to the recording.
    fn feed(&mut self, base: u64, samples: &[f32], channels: usize) {
        let frames = (samples.len() / channels) as u64;
        if self.closed || base + frames <= self.from_frame {
            return;
        }
        let skip = self.from_frame.saturating_sub(base) as usize;
        for frame in samples.chunks_exact(channels).skip(skip) {
            let pair = match self.source {
                InputSource::Mono(c) => [frame[usize::from(c)], 0.0],
                InputSource::Stereo(c) => [frame[usize::from(c)], frame[usize::from(c) + 1]],
            };
            let out = &pair[..usize::from(self.source.file_channels())];
            if !self.file.has_room() || self.file.write_frame(out).is_err() {
                self.lost = true;
                self.closed = true;
                return;
            }
            self.frames += 1;
        }
    }

    fn finish(self) -> Result<Recorded, CaptureError> {
        let (path, frames, lost) = (self.path, self.frames, self.lost);
        let (channels, sample_rate) = (self.file.channels, self.file.sample_rate);
        self.file.finish()?;
        Ok(Recorded {
            path,
            frames,
            sample_rate,
            channels,
            lost,
        })
    }
}

enum Ctl {
    Start {
        id: u32,
        path: PathBuf,
        source: InputSource,
        from_frame: u64,
        reply: Sender<Result<(), CaptureError>>,
    },
    Stop {
        id: u32,
        reply: Sender<Result<Recorded, CaptureError>>,
    },
}

/// The writer thread: takes the audio from the ring and feeds each running recording.
fn writer_loop(
    mut ring: Consumer<f32>,
    ctl: Receiver<Ctl>,
    channels: usize,
    sample_rate: u32,
    shared: Arc<Shared>,
) {
    let mut consumed = 0u64;
    let mut recs: Vec<(u32, Rec)> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    // The latest audio, for a recording that starts a moment before the thread hears of it.
    let mut history: VecDeque<f32> = VecDeque::new();
    let history_samples = sample_rate as usize * channels * HISTORY_SECONDS;
    let mut overruns_seen = 0u64;
    let mut refreshed = Instant::now();
    loop {
        let n = ring.slots() / channels * channels;
        if n > 0 {
            scratch.clear();
            if let Ok(chunk) = ring.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                scratch.extend_from_slice(a);
                scratch.extend_from_slice(b);
                chunk.commit_all();
            }
            for (_, rec) in &mut recs {
                rec.feed(consumed, &scratch, channels);
            }
            consumed += (scratch.len() / channels) as u64;
            history.extend(scratch.iter().copied());
            let extra = history.len().saturating_sub(history_samples);
            history.drain(..extra);
        }
        let overruns = shared.overruns.load(Ordering::Relaxed);
        if overruns != overruns_seen {
            // Audio is missing from the ring: the running files are shorter than the take.
            overruns_seen = overruns;
            for (_, rec) in &mut recs {
                rec.lost = true;
            }
        }
        let msg = if n == 0 {
            match ctl.recv_timeout(Duration::from_millis(2)) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match ctl.try_recv() {
                Ok(m) => Some(m),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        };
        match msg {
            Some(Ctl::Start {
                id,
                path,
                source,
                from_frame,
                reply,
            }) => {
                let result =
                    start_rec(&path, source, from_frame, channels, sample_rate).map(|mut rec| {
                        // The part that went by before this message was handled.
                        let have = (history.len() / channels) as u64;
                        history.make_contiguous();
                        rec.feed(consumed - have, history.as_slices().0, channels);
                        recs.push((id, rec));
                    });
                let _ = reply.send(result);
            }
            Some(Ctl::Stop { id, reply }) => {
                let result = match recs.iter().position(|(i, _)| *i == id) {
                    Some(at) => recs.remove(at).1.finish(),
                    None => Err(CaptureError::Closed),
                };
                let _ = reply.send(result);
            }
            None => {}
        }
        if refreshed.elapsed() >= HEADER_REFRESH {
            refreshed = Instant::now();
            for (_, rec) in &mut recs {
                if rec.file.refresh().is_err() {
                    rec.lost = true;
                }
            }
        }
    }
    for (_, rec) in recs {
        let _ = rec.finish();
    }
}

fn start_rec(
    path: &Path,
    source: InputSource,
    from_frame: u64,
    channels: usize,
    sample_rate: u32,
) -> Result<Rec, CaptureError> {
    if usize::from(source.needs()) > channels {
        return Err(CaptureError::MissingChannels {
            needs: source.needs(),
            has: channels as u16,
        });
    }
    Ok(Rec {
        path: path.to_path_buf(),
        source,
        file: WavFile::create(path, source.file_channels(), sample_rate)?,
        from_frame,
        frames: 0,
        lost: false,
        closed: false,
    })
}

/// The open input: a running stream and its writer thread. Keep it on the UI thread (cpal
/// streams are not `Send` everywhere).
pub struct Capture {
    _stream: Option<Stream>,
    ctl: Option<Sender<Ctl>>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    sample_rate: u32,
    channels: u16,
    device_id: String,
    device_name: String,
    next_id: u32,
    monitor: Option<Box<InputFeed>>,
}

/// The audio systems to look at: the platform's default first.
fn hosts() -> Vec<cpal::Host> {
    let default_id = cpal::default_host().id();
    let mut ids = vec![default_id];
    ids.extend(
        cpal::available_hosts()
            .into_iter()
            .filter(|id| *id != default_id),
    );
    ids.into_iter()
        .filter_map(|id| cpal::host_from_id(id).ok())
        .collect()
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|d| d.name().to_string())
        .or_else(|_| d.id().map(|i| i.to_string()))
        .unwrap_or_else(|_| "Input".into())
}

/// The audio inputs of this computer, the default input first. Looks at the platform's own
/// audio system, and at the others only when it has no input.
pub fn input_devices() -> Vec<InputInfo> {
    for host in hosts() {
        let default_id = host
            .default_input_device()
            .and_then(|d| d.id().ok())
            .map(|i| i.to_string());
        let Ok(devices) = host.input_devices() else {
            continue;
        };
        let mut list: Vec<InputInfo> = devices
            .filter_map(|d| {
                let id = d.id().ok()?.to_string();
                let channels = d.default_input_config().ok()?.channels();
                Some(InputInfo {
                    is_default: default_id.as_deref() == Some(id.as_str()),
                    id,
                    name: device_name(&d),
                    channels,
                })
            })
            .collect();
        if !list.is_empty() {
            list.sort_by_key(|d| !d.is_default);
            return list;
        }
    }
    Vec::new()
}

/// `id`'s device, else the default input, with the audio system it belongs to.
fn find_device(id: Option<&str>) -> Option<cpal::Device> {
    let hosts = hosts();
    if let Some(wanted) = id {
        for host in &hosts {
            let found = host
                .input_devices()
                .ok()?
                .find(|d| d.id().is_ok_and(|i| i.to_string() == wanted));
            if found.is_some() {
                return found;
            }
        }
        tracing::warn!("audio input {wanted} is not there; using the default input");
    }
    hosts.iter().find_map(|h| h.default_input_device())
}

impl Capture {
    /// Opens the input `device` (an [`InputInfo::id`]; `None` or an unknown id is the default
    /// input) and starts listening.
    pub fn open(device: Option<&str>) -> Result<Self, CaptureError> {
        let device = find_device(device).ok_or(CaptureError::NoInputDevice)?;
        let supported = device.default_input_config().map_err(backend)?;
        let format = supported.sample_format();
        let mut config: StreamConfig = supported.into();
        config.buffer_size = BufferSize::Default;
        let (channels, sample_rate) = (config.channels, config.sample_rate);
        let (producer, consumer) = RingBuffer::<f32>::new(
            (sample_rate as usize * usize::from(channels) * RING_SECONDS).max(4096),
        );
        let shared = Arc::new(Shared::new());
        let (monitor_tx, monitor_rx) =
            RingBuffer::<f32>::new((sample_rate as usize * usize::from(channels) / 4).max(4096));
        let tx = (producer, monitor_tx);
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, tx, &shared),
            SampleFormat::I16 => build::<i16>(&device, config, tx, &shared),
            SampleFormat::U16 => build::<u16>(&device, config, tx, &shared),
            SampleFormat::I32 => build::<i32>(&device, config, tx, &shared),
            SampleFormat::U32 => build::<u32>(&device, config, tx, &shared),
            SampleFormat::I8 => build::<i8>(&device, config, tx, &shared),
            SampleFormat::U8 => build::<u8>(&device, config, tx, &shared),
            SampleFormat::F64 => build::<f64>(&device, config, tx, &shared),
            other => {
                return Err(CaptureError::Backend(format!(
                    "unsupported sample format {other}"
                )))
            }
        }?;
        let mut capture = Self::with_ring(consumer, shared, channels, sample_rate);
        capture.device_id = device.id().map(|i| i.to_string()).unwrap_or_default();
        capture.device_name = device_name(&device);
        capture.monitor = Some(Box::new(InputFeed::new(
            monitor_rx,
            usize::from(channels.max(1)),
            sample_rate,
        )));
        stream.play().map_err(backend)?;
        capture._stream = Some(stream);
        Ok(capture)
    }

    /// A capture fed by `consumer` instead of a device.
    fn with_ring(
        consumer: Consumer<f32>,
        shared: Arc<Shared>,
        channels: u16,
        sample_rate: u32,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("prowave-capture".into())
                .spawn(move || {
                    writer_loop(
                        consumer,
                        rx,
                        usize::from(channels.max(1)),
                        sample_rate,
                        shared,
                    );
                })
                .ok()
        };
        Self {
            _stream: None,
            ctl: Some(tx),
            thread,
            shared,
            sample_rate,
            channels: channels.max(1),
            device_id: String::new(),
            device_name: String::new(),
            next_id: 0,
            monitor: None,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The live input for [`crate::Command::SetInput`], once.
    pub fn take_monitor_feed(&mut self) -> Option<Box<InputFeed>> {
        self.monitor.take()
    }

    /// Channels the device delivers (the inputs a track can pick from).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// The [`InputInfo::id`] of the device that was opened.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Callbacks whose audio was lost because the writer thread could not keep up.
    pub fn overruns(&self) -> u64 {
        self.shared.overruns.load(Ordering::Relaxed)
    }

    /// Glitches the device has reported since it was opened.
    pub fn xruns(&self) -> u64 {
        self.shared.xruns.load(Ordering::Relaxed)
    }

    /// The loudest sample of each channel since the last call (0..1 and above), for the level
    /// meters. `out` is filled with one value per channel.
    pub fn take_peaks(&self, out: &mut Vec<f32>) {
        out.clear();
        out.extend(
            self.shared
                .peaks
                .iter()
                .take(usize::from(self.channels))
                .map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed))),
        );
    }

    /// Starts recording `source` into the WAV file `path`. `latency_us` is the delay of what
    /// the track is playing from the program to the ear (the output latency): a performer
    /// plays along with what they hear, so the recording is cut that much later, plus the
    /// input's own delay, which lines it up with the song. Returns the recording's id.
    pub fn start(
        &mut self,
        path: &Path,
        source: InputSource,
        latency_us: u32,
    ) -> Result<u32, CaptureError> {
        let total_us =
            u64::from(latency_us) + u64::from(self.shared.latency_us.load(Ordering::Relaxed));
        let skip = total_us * u64::from(self.sample_rate) / 1_000_000;
        let from_frame = self.shared.frames_pushed.load(Ordering::Acquire) + skip;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (reply, answer) = mpsc::channel();
        self.ctl
            .as_ref()
            .ok_or(CaptureError::Closed)?
            .send(Ctl::Start {
                id,
                path: path.to_path_buf(),
                source,
                from_frame,
                reply,
            })
            .map_err(|_| CaptureError::Closed)?;
        answer.recv().map_err(|_| CaptureError::Closed)??;
        Ok(id)
    }

    /// Ends recording `id` and closes its file. Waits for the audio already captured to be
    /// written.
    pub fn stop(&mut self, id: u32) -> Result<Recorded, CaptureError> {
        // What the callback has delivered up to now reaches the file before it closes: the
        // writer thread empties the ring before it looks at this message.
        let (reply, answer) = mpsc::channel();
        self.ctl
            .as_ref()
            .ok_or(CaptureError::Closed)?
            .send(Ctl::Stop { id, reply })
            .map_err(|_| CaptureError::Closed)?;
        answer.recv().map_err(|_| CaptureError::Closed)?
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // The writer thread closes any file still open when its control channel closes.
        self.ctl.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    (mut producer, mut monitor): (Producer<f32>, Producer<f32>),
    shared: &Arc<Shared>,
) -> Result<Stream, CaptureError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let (cb_shared, err_shared) = (shared.clone(), shared.clone());
    device
        .build_input_stream(
            config,
            move |data: &[T], info: &InputCallbackInfo| {
                let ts = info.timestamp();
                let lag = ts.callback.duration_since(ts.capture).as_micros();
                cb_shared
                    .latency_us
                    .store(lag.min(u128::from(u32::MAX)) as u32, Ordering::Relaxed);
                assert_no_alloc::assert_no_alloc(|| {
                    push(data, channels, &mut producer, &cb_shared);
                    push_monitor(data, &mut monitor);
                });
            },
            move |err: Error| match err.kind() {
                ErrorKind::Xrun => {
                    err_shared.xruns.fetch_add(1, Ordering::Relaxed);
                }
                _ => tracing::error!("audio input error: {err}"),
            },
            None,
        )
        .map_err(backend)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::disk::AudioFile;

    /// A capture with no device, and the producer a device callback would use.
    fn fake(channels: u16, rate: u32) -> (Capture, Producer<f32>, Arc<Shared>) {
        let (producer, consumer) =
            RingBuffer::<f32>::new(rate as usize * usize::from(channels) * RING_SECONDS);
        let shared = Arc::new(Shared::new());
        (
            Capture::with_ring(consumer, shared.clone(), channels, rate),
            producer,
            shared,
        )
    }

    /// `frames` frames of `channels` channels whose sample is `frame * 10 + channel`, counting
    /// from `first`.
    fn ramp(first: usize, frames: usize, channels: usize) -> Vec<f32> {
        (first..first + frames)
            .flat_map(|f| (0..channels).map(move |c| (f * 10 + c) as f32))
            .collect()
    }

    fn read_back(path: &Path) -> Vec<f32> {
        let file = AudioFile::open(path).unwrap();
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(
            file.channels,
            usize::from(u16::from_le_bytes([bytes[22], bytes[23]]))
        );
        bytes[44..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    }

    fn settle() {
        std::thread::sleep(Duration::from_millis(60));
    }

    #[test]
    fn a_recording_opens_as_an_audio_file_of_the_right_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let (mut cap, mut tx, shared) = fake(1, 48_000);
        let id = cap.start(&path, InputSource::Mono(0), 0).unwrap();
        let wave: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        for block in wave.chunks(512) {
            push(block, 1, &mut tx, &shared);
        }
        settle();
        let done = cap.stop(id).unwrap();
        assert_eq!(
            (done.frames, done.sample_rate, done.channels, done.lost),
            (48_000, 48_000, 1, false)
        );
        let file = AudioFile::open(&path).unwrap();
        assert_eq!(
            (file.frames, file.sample_rate, file.channels),
            (48_000, 48_000, 1)
        );
        assert_eq!(read_back(&path), wave);
    }

    #[test]
    fn each_recording_takes_its_own_channels() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
        let (mut cap, mut tx, shared) = fake(4, 44_100);
        let ida = cap.start(&a, InputSource::Mono(2), 0).unwrap();
        let idb = cap.start(&b, InputSource::Stereo(0), 0).unwrap();
        push(&ramp(0, 100, 4), 4, &mut tx, &shared);
        settle();
        assert_eq!(cap.stop(ida).unwrap().frames, 100);
        assert_eq!(cap.stop(idb).unwrap().frames, 100);
        let mono = read_back(&a);
        assert_eq!(&mono[..3], &[2.0, 12.0, 22.0], "Input 3 only");
        let stereo = read_back(&b);
        assert_eq!(&stereo[..4], &[0.0, 1.0, 10.0, 11.0], "Input 1 + 2");
    }

    #[test]
    fn the_latency_is_cut_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.wav");
        let (mut cap, mut tx, shared) = fake(1, 48_000);
        // 10 ms of output latency is 480 frames.
        let id = cap.start(&path, InputSource::Mono(0), 10_000).unwrap();
        push(&ramp(0, 1000, 1), 1, &mut tx, &shared);
        settle();
        let done = cap.stop(id).unwrap();
        assert_eq!(done.frames, 520);
        assert_eq!(
            read_back(&path)[0],
            4800.0,
            "frame 480 is the first one kept"
        );
    }

    #[test]
    fn a_recording_starts_at_the_frame_it_was_asked_for_even_when_the_thread_was_late() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("early.wav");
        let (mut cap, mut tx, shared) = fake(1, 48_000);
        push(&ramp(0, 200, 1), 1, &mut tx, &shared);
        settle();
        // The 200 frames have gone by when the recording is asked to start at frame 150.
        let (reply, answer) = mpsc::channel();
        cap.ctl
            .as_ref()
            .unwrap()
            .send(Ctl::Start {
                id: 9,
                path: path.clone(),
                source: InputSource::Mono(0),
                from_frame: 150,
                reply,
            })
            .unwrap();
        answer.recv().unwrap().unwrap();
        push(&ramp(200, 50, 1), 1, &mut tx, &shared);
        settle();
        let done = cap.stop(9).unwrap();
        assert_eq!(done.frames, 100);
        assert_eq!(read_back(&path)[0], 1500.0);
    }

    #[test]
    fn a_recording_goes_on_after_audio_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gap.wav");
        let (mut cap, mut tx, shared) = fake(1, 48_000);
        let id = cap.start(&path, InputSource::Mono(0), 0).unwrap();
        push(&ramp(0, 100, 1), 1, &mut tx, &shared);
        settle();
        shared.overruns.fetch_add(1, Ordering::Relaxed);
        settle();
        push(&ramp(100, 100, 1), 1, &mut tx, &shared);
        settle();
        let done = cap.stop(id).unwrap();
        assert_eq!(done.frames, 200, "the take keeps going");
        assert!(done.lost, "and says that it has a gap");
    }

    #[test]
    fn a_device_glitch_does_not_stop_or_mark_a_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("glitch.wav");
        let (mut cap, mut tx, shared) = fake(1, 48_000);
        let id = cap.start(&path, InputSource::Mono(0), 0).unwrap();
        shared.xruns.fetch_add(1, Ordering::Relaxed);
        settle();
        push(&ramp(0, 100, 1), 1, &mut tx, &shared);
        settle();
        let done = cap.stop(id).unwrap();
        assert_eq!((done.frames, done.lost), (100, false));
        assert_eq!(cap.xruns(), 1);
    }

    #[test]
    fn a_device_with_too_few_channels_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut cap, _tx, _shared) = fake(2, 48_000);
        let err = cap
            .start(&dir.path().join("x.wav"), InputSource::Stereo(1), 0)
            .unwrap_err();
        assert!(matches!(
            err,
            CaptureError::MissingChannels { needs: 3, has: 2 }
        ));
        assert!(matches!(cap.stop(77), Err(CaptureError::Closed)));
    }

    #[test]
    fn peaks_are_the_loudest_sample_since_the_last_look() {
        let (cap, mut tx, shared) = fake(2, 48_000);
        push(&[0.25f32, -0.5, -0.75, 0.125], 2, &mut tx, &shared);
        let mut peaks = Vec::new();
        cap.take_peaks(&mut peaks);
        assert_eq!(peaks, vec![0.75, 0.5]);
        cap.take_peaks(&mut peaks);
        assert_eq!(peaks, vec![0.0, 0.0], "read once");
    }

    #[test]
    fn integer_samples_are_converted_and_a_full_ring_drops_the_block_whole() {
        let (mut tx, mut rx) = RingBuffer::<f32>::new(8);
        let shared = Shared::new();
        push(&[i16::MAX, i16::MIN, 0, 0], 2, &mut tx, &shared);
        let chunk = rx.read_chunk(4).unwrap();
        let (got, _) = chunk.as_slices();
        assert!((got[0] - 1.0).abs() < 1e-3 && (got[1] + 1.0).abs() < 1e-3);
        chunk.commit_all();
        push(&[0i16; 10], 2, &mut tx, &shared);
        assert_eq!(shared.overruns.load(Ordering::Relaxed), 1);
        assert_eq!(
            shared.frames_pushed.load(Ordering::Relaxed),
            2,
            "only the first block"
        );
    }

    #[test]
    fn pushing_samples_does_not_allocate() {
        let (mut tx, _rx) = RingBuffer::<f32>::new(4096);
        let shared = Shared::new();
        let block = ramp(0, 128, 2);
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..8 {
                push(&block, 2, &mut tx, &shared);
            }
        });
    }

    #[test]
    fn a_file_still_open_is_readable_after_the_header_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.wav");
        let mut wav = WavFile::create(&path, 2, 48_000).unwrap();
        for i in 0..300 {
            wav.write_frame(&[i as f32, -(i as f32)]).unwrap();
        }
        wav.refresh().unwrap();
        // As after a crash: the file is read without being closed.
        let file = AudioFile::open(&path).unwrap();
        assert_eq!((file.frames, file.channels), (300, 2));
        drop(wav);
    }

    #[test]
    fn input_source_channels() {
        assert_eq!(InputSource::Mono(0).needs(), 1);
        assert_eq!(InputSource::Stereo(2).needs(), 4);
        assert_eq!(InputSource::Stereo(0).file_channels(), 2);
    }
}
