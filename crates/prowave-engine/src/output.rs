//! cpal output: owns the stream, the command producer and the shared state.
use crate::core::{
    dsp_load, AudioClip, ClipEdit, Command, EngineCore, EngineState, Garbage, MidiOutEvent, Shared,
    StateReader,
};
use crate::disk::{AudioError, AudioFile, DiskThread};
use crate::mtc::MtcMessage;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Error, ErrorKind, FromSample, OutputCallbackInfo, SampleFormat, SizedSample,
    Stream, StreamConfig, SupportedBufferSize,
};
use rtrb::{Producer, RingBuffer};
use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

const MAX_BLOCK_FRAMES: usize = 8192;
const GARBAGE_CAPACITY: usize = 32;

/// UI-side queue accounting: each accepted state replacement reserves one return slot.
struct CommandQueue {
    tx: Producer<Command>,
    garbage: rtrb::Consumer<Garbage>,
    credits: usize,
}

impl CommandQueue {
    fn send(&mut self, cmd: Command) -> bool {
        while self.garbage.pop().is_ok() {
            self.credits += 1;
        }
        let retires = cmd.retires_state();
        if retires && self.credits == 0 {
            return false;
        }
        if self.tx.push(cmd).is_err() {
            return false;
        }
        if retires {
            self.credits -= 1;
        }
        true
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    #[test]
    fn reserves_reclamation_capacity_and_recovers_after_collection() {
        let (tx, mut rx) = RingBuffer::new(8);
        let (mut gtx, garbage) = RingBuffer::new(1);
        let mut queue = CommandQueue {
            tx,
            garbage,
            credits: 1,
        };
        assert!(queue.send(Command::SetSong(Box::default())));
        assert!(!queue.send(Command::SetTempoMap(Box::default())));
        // Transport is still deliverable even when all reclamation credits are reserved.
        assert!(queue.send(Command::Stop));
        assert!(matches!(rx.pop(), Ok(Command::SetSong(_))));
        assert!(matches!(rx.pop(), Ok(Command::Stop)));
        assert!(gtx.push(Garbage::Song(Box::default())).is_ok());
        assert!(queue.send(Command::SetTempoMap(Box::default())));
        assert_eq!(queue.credits, 0);
        assert!(matches!(rx.pop(), Ok(Command::SetTempoMap(_))));
    }

    #[test]
    fn rejected_command_does_not_consume_a_reclamation_credit() {
        let (tx, mut rx) = RingBuffer::new(1);
        let (_gtx, garbage) = RingBuffer::new(1);
        let mut queue = CommandQueue {
            tx,
            garbage,
            credits: 1,
        };
        assert!(queue.send(Command::Play));
        assert!(!queue.send(Command::SetNotes(vec![])));
        assert_eq!(queue.credits, 1);
        assert!(rx.pop().is_ok());
        assert!(queue.send(Command::SetNotes(vec![])));
        assert_eq!(queue.credits, 0);
    }
}

#[cfg(test)]
mod buffer_tests {
    use super::*;

    #[test]
    fn choices_are_the_powers_of_two_the_device_supports() {
        assert_eq!(buffer_choices(None), vec![32, 64, 128, 256, 512, 1024]);
        assert_eq!(buffer_choices(Some((16, 512))), vec![32, 64, 128, 256, 512]);
        assert_eq!(buffer_choices(Some((2048, 4096))), Vec::<u32>::new());
        assert_eq!(buffer_choices(Some((32, 1024))), BUFFER_CHOICES.to_vec());
    }

    #[test]
    fn a_requested_buffer_is_limited_to_the_device_range() {
        assert_eq!(clamp_buffer(64, None), 64);
        assert_eq!(clamp_buffer(4, Some((16, 8192))), 16);
        assert_eq!(clamp_buffer(100_000, Some((16, 8192))), 8192);
        assert_eq!(clamp_buffer(0, None), 1, "never a zero-frame callback");
        // A broken range never panics; the upper end wins.
        assert_eq!(clamp_buffer(64, Some((512, 128))), 128);
    }

    #[test]
    fn an_unknown_output_device_is_an_error_not_a_panic() {
        assert!(Engine::start_on(Some("no such output device"), None).is_err());
    }

    #[test]
    fn listing_the_outputs_never_panics_and_ids_are_usable() {
        for device in output_devices() {
            assert!(!device.id.is_empty());
            assert!(device.id.len() <= MAX_DEVICE_ID_CHARS);
        }
    }
}

#[derive(Debug)]
pub enum EngineError {
    NoOutputDevice,
    UnsupportedSampleFormat(String),
    Backend(String),
    AudioFile(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputDevice => write!(f, "no audio output device available"),
            Self::UnsupportedSampleFormat(s) => write!(f, "unsupported sample format {s}"),
            Self::Backend(s) => write!(f, "audio backend error: {s}"),
            Self::AudioFile(s) => write!(f, "audio file error: {s}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<AudioError> for EngineError {
    fn from(error: AudioError) -> Self {
        Self::AudioFile(error.to_string())
    }
}

/// Decodes `file` off the audio thread and returns its maximum absolute sample value.
/// Use `20 * log10(peak)` to calculate the non-destructive gain for normalization.
pub fn peak_of(file: &AudioFile) -> Result<f32, EngineError> {
    file.peak().map_err(EngineError::from)
}

/// The I/O buffer sizes the app offers, in samples (Logic Pro: Settings > Audio).
pub const BUFFER_CHOICES: [u32; 6] = [32, 64, 128, 256, 512, 1024];

/// Longest device id the app accepts; a longer one is skipped rather than stored.
const MAX_DEVICE_ID_CHARS: usize = 256;

/// An audio output device of this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    /// Identifies the device to [`Engine::start_on`] (the system's own id).
    pub id: String,
    /// The name to show.
    pub name: String,
}

fn device_label(d: &cpal::Device) -> String {
    d.description()
        .map(|d| d.name().to_string())
        .or_else(|_| d.id().map(|i| i.to_string()))
        .unwrap_or_else(|_| "Output".into())
}

/// The output devices of the default audio system. Empty when there are none or the system
/// cannot be asked; never panics.
pub fn output_devices() -> Vec<OutputDevice> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            if id.is_empty() || id.len() > MAX_DEVICE_ID_CHARS {
                return None;
            }
            Some(OutputDevice {
                name: device_label(&d),
                id,
            })
        })
        .collect()
}

/// The default audio system's output device with this id, if it is still there.
fn find_output(id: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.output_devices()
        .ok()?
        .find(|d| d.id().is_ok_and(|i| i.to_string() == id))
}

/// The buffer sizes `device_id` (`None`: the default output) can take, in frames, from its
/// default configuration. `None` when the device does not say (or is not there).
pub fn buffer_range(device_id: Option<&str>) -> Option<(u32, u32)> {
    let device = match device_id {
        Some(id) => find_output(id)?,
        None => cpal::default_host().default_output_device()?,
    };
    let config = device.default_output_config().ok()?;
    match config.buffer_size() {
        SupportedBufferSize::Range { min, max } => Some((*min, *max)),
        SupportedBufferSize::Unknown => None,
    }
}

/// The [`BUFFER_CHOICES`] a device with `range` supports. Without a known range, all of them.
pub fn buffer_choices(range: Option<(u32, u32)>) -> Vec<u32> {
    BUFFER_CHOICES
        .into_iter()
        .filter(|&n| match range {
            Some((lo, hi)) => (lo..=hi).contains(&n),
            None => true,
        })
        .collect()
}

/// `n` frames, limited to what the device supports when its range is known. Never panics,
/// even for a range whose ends are in the wrong order.
pub fn clamp_buffer(n: u32, range: Option<(u32, u32)>) -> u32 {
    let n = n.max(1);
    match range {
        Some((lo, hi)) => n.max(lo).min(hi),
        None => n,
    }
}

/// The running audio output. Dropping it stops the sound.
enum Output {
    Cpal(#[allow(dead_code)] Stream),
    /// Windows: our own WASAPI stream with the smallest period the audio engine allows.
    #[cfg(windows)]
    Wasapi(#[allow(dead_code)] crate::wasapi::LowLatencyStream),
}

/// UI-thread handle. Not `Send` on some platforms (cpal `Stream`); keep it on the UI thread.
pub struct Engine {
    commands: CommandQueue,
    shared: Arc<Shared>,
    state: StateReader,
    midi_out: Option<rtrb::Consumer<MidiOutEvent>>,
    mtc_out: Option<rtrb::Consumer<MtcMessage>>,
    clock_out: Option<rtrb::Consumer<crate::clock::ClockMessage>>,
    sample_rate: u32,
    device_name: String,
    /// The id the engine was opened with, `None` for the system's default output.
    device_id: Option<String>,
    /// The buffer size asked for (clamped to the device), `None` for the device's default.
    buffer_frames: Option<u32>,
    _output: Output,
    disk: DiskThread,
}

/// Renders the engine into a device buffer of any sample format and channel count. Lives on
/// the audio thread; never allocates after it is made.
pub(crate) struct Renderer {
    core: EngineCore,
    shared: Arc<Shared>,
    scratch: Vec<f32>,
    channels: usize,
    sample_rate: u32,
}

impl Renderer {
    fn new(core: EngineCore, shared: Arc<Shared>, channels: usize, sample_rate: u32) -> Self {
        Self {
            core,
            shared,
            // Pre-allocated once, before the stream starts; the callback never allocates.
            scratch: vec![0f32; MAX_BLOCK_FRAMES * 2],
            channels: channels.max(1),
            sample_rate,
        }
    }

    /// Fills `data` (interleaved, `channels` per frame). `output_latency_us` is the delay from
    /// now until the first frame is heard, if the device says.
    pub(crate) fn fill<T: SizedSample + FromSample<f32>>(
        &mut self,
        data: &mut [T],
        output_latency_us: u32,
    ) {
        // `Instant::now` reads the OS monotonic clock (no lock, no allocation).
        let started = Instant::now();
        let channels = self.channels;
        self.core
            .set_device_info((data.len() / channels) as u32, output_latency_us);
        for chunk in data.chunks_mut(MAX_BLOCK_FRAMES * channels) {
            let frames = chunk.len() / channels;
            let buf = &mut self.scratch[..frames * 2];
            let core = &mut self.core;
            assert_no_alloc::assert_no_alloc(|| core.process(buf));
            for (i, frame) in chunk.chunks_exact_mut(channels).enumerate() {
                let (l, r) = (buf[i * 2], buf[i * 2 + 1]);
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = T::from_sample(match c {
                        0 => l,
                        1 => r,
                        _ => 0.0,
                    });
                }
            }
        }
        let load = dsp_load(started.elapsed(), data.len() / channels, self.sample_rate);
        self.shared.record_dsp_load(load);
    }
}

impl Engine {
    /// Starts on the default output device with the device's default buffer size.
    pub fn start() -> Result<Self, EngineError> {
        Self::start_with(None)
    }

    /// Starts on the output device `device_id` (an [`OutputDevice::id`]), or on the system's
    /// default output when it is `None`, which is exactly [`Engine::start_with`]. An id that is
    /// not there is an error, so the caller can fall back to the default. The Windows
    /// low-latency path is used for the default output only.
    pub fn start_on(
        device_id: Option<&str>,
        buffer_frames: Option<u32>,
    ) -> Result<Self, EngineError> {
        let Some(id) = device_id else {
            return Self::start_with(buffer_frames);
        };
        let device = find_output(id)
            .ok_or_else(|| EngineError::Backend(format!("output device {id} is not there")))?;
        Self::start_on_device(&device, buffer_frames).map(|mut engine| {
            engine.device_id = Some(id.to_string());
            engine
        })
    }

    /// The buffer size asked for, clamped to the device, in frames. `None` when the device's
    /// default was used. The size the callbacks actually get is `EngineState::block_frames`.
    pub fn buffer_frames(&self) -> Option<u32> {
        self.buffer_frames
    }

    /// The id the engine was opened on, `None` for the system's default output.
    pub fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }

    /// Like [`Engine::start`], but asks for `buffer_frames` per callback (clamped to what the
    /// device supports). Smaller buffers lower latency (Logic: I/O Buffer Size).
    pub fn start_with(buffer_frames: Option<u32>) -> Result<Self, EngineError> {
        // Windows: the shared-mode audio engine's smallest period (often 2-3 ms instead of
        // 10 ms) without taking the device away from other programs. cpal below otherwise.
        #[cfg(windows)]
        if std::env::var_os("PROWAVE_NO_LOW_LATENCY").is_none() {
            match Self::start_wasapi(buffer_frames) {
                Ok(engine) => return Ok(engine),
                Err(e) => tracing::info!("low-latency output not available ({e}); using cpal"),
            }
        }
        let mut last = EngineError::NoOutputDevice;
        // The platform's default audio system first, then any other one that is available
        // (e.g. ALSA when a sound daemon is broken).
        let default_id = cpal::default_host().id();
        let mut ids = vec![default_id];
        ids.extend(
            cpal::available_hosts()
                .into_iter()
                .filter(|id| *id != default_id),
        );
        for id in ids {
            let Ok(host) = cpal::host_from_id(id) else {
                continue;
            };
            match Self::start_on_host(&host, buffer_frames) {
                Ok(engine) => return Ok(engine),
                Err(e) => {
                    tracing::warn!("audio system {id} failed: {e}");
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Starts on the ASIO host (Windows builds with the `asio` feature only).
    /// Use this to avoid shared-mode WASAPI buffering when a native ASIO driver is available.
    #[cfg(all(windows, feature = "asio"))]
    pub fn start_asio_with(buffer_frames: Option<u32>) -> Result<Self, EngineError> {
        let host = cpal::host_from_id(cpal::HostId::Asio)
            .map_err(|error| EngineError::Backend(error.to_string()))?;
        Self::start_on_host(&host, buffer_frames)
    }

    #[cfg(windows)]
    fn start_wasapi(buffer_frames: Option<u32>) -> Result<Self, EngineError> {
        let (tx, rx) = RingBuffer::<Command>::new(1024);
        let (gtx, grx) = RingBuffer::<Garbage>::new(GARBAGE_CAPACITY);
        let shared = Arc::new(Shared::new());
        let shared_rt = shared.clone();
        let make = move |sample_rate: u32, channels: usize| {
            let mut core = EngineCore::new(sample_rate, rx, gtx, shared_rt.clone());
            let readers = core.take_readers();
            (
                Renderer::new(core, shared_rt, channels, sample_rate),
                readers,
            )
        };
        let (stream, info, readers) = crate::wasapi::LowLatencyStream::start(buffer_frames, make)
            .map_err(EngineError::Backend)?;
        let state = readers
            .state
            .ok_or_else(|| EngineError::Backend("no state reader".into()))?;
        tracing::info!(
            "audio output {} (WASAPI low latency, {} frames at {} Hz)",
            info.device_name,
            info.period_frames,
            info.sample_rate
        );
        Ok(Self {
            commands: CommandQueue {
                tx,
                garbage: grx,
                credits: GARBAGE_CAPACITY,
            },
            shared,
            state,
            midi_out: readers.midi,
            mtc_out: readers.mtc,
            clock_out: readers.clock,
            sample_rate: info.sample_rate,
            device_name: info.device_name,
            device_id: None,
            buffer_frames: Some(info.period_frames),
            _output: Output::Wasapi(stream),
            disk: DiskThread::spawn(),
        })
    }

    fn start_on_host(host: &cpal::Host, buffer_frames: Option<u32>) -> Result<Self, EngineError> {
        let mut last = EngineError::NoOutputDevice;
        if let Some(device) = host.default_output_device() {
            match Self::start_on_device(&device, buffer_frames) {
                Ok(engine) => return Ok(engine),
                Err(e) => {
                    tracing::warn!("default audio output failed: {e}");
                    last = e;
                }
            }
        }
        // The default device can be missing or busy (common with raw ALSA on Linux): try
        // every other output device before giving up.
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                let name = device.id().map(|i| i.to_string()).unwrap_or_default();
                match Self::start_on_device(&device, buffer_frames) {
                    Ok(engine) => {
                        tracing::info!("using audio output {name}");
                        return Ok(engine);
                    }
                    Err(e) => {
                        tracing::debug!("audio output {name} failed: {e}");
                        last = e;
                    }
                }
            }
        }
        Err(last)
    }

    /// Starts on `device`. If the requested buffer size is refused, retries with its default.
    fn start_on_device(
        device: &cpal::Device,
        buffer_frames: Option<u32>,
    ) -> Result<Self, EngineError> {
        match Self::start_device(device, buffer_frames) {
            Err(e) if buffer_frames.is_some() => {
                tracing::warn!("buffer size {buffer_frames:?} refused ({e}); using the default");
                Self::start_device(device, None)
            }
            other => other,
        }
    }

    fn start_device(
        device: &cpal::Device,
        buffer_frames: Option<u32>,
    ) -> Result<Self, EngineError> {
        let device_name = device
            .id()
            .map(|i| i.to_string())
            .unwrap_or_else(|_| "default".into());
        let supported = device
            .default_output_config()
            .map_err(|e| EngineError::Backend(e.to_string()))?;
        let format = supported.sample_format();
        let range = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            SupportedBufferSize::Unknown => None,
        };
        let mut config: StreamConfig = supported.into();
        let buffer_frames = buffer_frames.map(|n| clamp_buffer(n, range));
        if let Some(n) = buffer_frames {
            config.buffer_size = BufferSize::Fixed(n);
        }
        let sample_rate = config.sample_rate;

        let (tx, rx) = RingBuffer::<Command>::new(1024);
        let (gtx, grx) = RingBuffer::<Garbage>::new(GARBAGE_CAPACITY);
        let shared = Arc::new(Shared::new());
        let mut core = EngineCore::new(sample_rate, rx, gtx, shared.clone());
        let readers = core.take_readers();
        let state = readers
            .state
            .ok_or_else(|| EngineError::Backend("no state reader".into()))?;
        let renderer = Renderer::new(core, shared.clone(), config.channels as usize, sample_rate);

        let stream = match format {
            SampleFormat::F32 => build::<f32>(device, config, renderer),
            SampleFormat::I16 => build::<i16>(device, config, renderer),
            SampleFormat::U16 => build::<u16>(device, config, renderer),
            SampleFormat::I32 => build::<i32>(device, config, renderer),
            SampleFormat::U32 => build::<u32>(device, config, renderer),
            SampleFormat::I8 => build::<i8>(device, config, renderer),
            SampleFormat::U8 => build::<u8>(device, config, renderer),
            SampleFormat::F64 => build::<f64>(device, config, renderer),
            other => return Err(EngineError::UnsupportedSampleFormat(other.to_string())),
        }?;
        stream
            .play()
            .map_err(|e| EngineError::Backend(e.to_string()))?;
        Ok(Self {
            commands: CommandQueue {
                tx,
                garbage: grx,
                credits: GARBAGE_CAPACITY,
            },
            shared,
            state,
            midi_out: readers.midi,
            mtc_out: readers.mtc,
            clock_out: readers.clock,
            sample_rate,
            device_name,
            device_id: None,
            buffer_frames,
            _output: Output::Cpal(stream),
            disk: DiskThread::spawn(),
        })
    }

    /// Queue a command (Logic: transport or project edit). Returns `false` if command or
    /// state-reclamation capacity is exhausted. The rejected command is freed here on the
    /// UI thread; callers must retry their desired change before marking it synchronized.
    pub fn send(&mut self, cmd: Command) -> bool {
        let ok = self.commands.send(cmd);
        if !ok {
            tracing::error!("engine command or reclamation queue full; command dropped");
        }
        ok
    }

    /// The MIDI the engine sends to external instruments, once: the app plays it out on the
    /// tracks' ports. `None` after the first call.
    pub fn take_midi_out(&mut self) -> Option<rtrb::Consumer<MidiOutEvent>> {
        self.midi_out.take()
    }

    /// The MIDI Time Code the engine sends while [`Command::SetMtc`] has it on, once: the app
    /// plays it out on the project's MTC port. `None` after the first call.
    pub fn take_mtc_out(&mut self) -> Option<rtrb::Consumer<MtcMessage>> {
        self.mtc_out.take()
    }

    /// The MIDI Clock the engine sends while the song plays, once: the app plays it out on the
    /// project's clock port. `None` after the first call.
    pub fn take_clock_out(&mut self) -> Option<rtrb::Consumer<crate::clock::ClockMessage>> {
        self.clock_out.take()
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }
    /// The playhead, transport and meters after the newest audio block, all from that block.
    pub fn state(&mut self) -> &EngineState {
        self.state.read()
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// How busy the disk streaming thread has been lately, 0 to 1 (the app's HD meter): zero
    /// when the project has no audio regions.
    pub fn disk_load(&self) -> f32 {
        self.disk.load()
    }

    /// Streams `file` from disk as an audio region on `track` starting at `start_beat`.
    /// Put the result in [`crate::Song::with_clips`] and send it with `Command::SetSong`.
    pub fn stream_clip(&self, file: &AudioFile, track: u16, start_beat: f64) -> AudioClip {
        self.stream_clip_with(file, track, start_beat, ClipEdit::default())
    }

    /// [`Engine::stream_clip_with`] for a song with `clips_in_song` streamed regions in all:
    /// every clip buffers ahead as much as the song's 8 MiB prefetch budget allows
    /// (between 170 ms and 0.68 s), so a few regions stay safe against a held-up disk and
    /// hundreds of regions stay light. Pass the same count for every clip of the song.
    pub fn stream_clip_in(
        &self,
        file: &AudioFile,
        track: u16,
        start_beat: f64,
        edit: ClipEdit,
        clips_in_song: usize,
    ) -> AudioClip {
        self.disk.stream_with_ring(
            file,
            self.sample_rate,
            track,
            start_beat,
            edit,
            crate::disk::ring_blocks_for(clips_in_song),
        )
    }

    /// Streams an audio region with non-destructive gain, beat-based fades, and reverse.
    /// It buffers the most ahead; for a song with many regions use [`Engine::stream_clip_in`].
    pub fn stream_clip_with(
        &self,
        file: &AudioFile,
        track: u16,
        start_beat: f64,
        edit: ClipEdit,
    ) -> AudioClip {
        self.disk
            .stream_with(file, self.sample_rate, track, start_beat, edit)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut renderer: Renderer,
) -> Result<Stream, EngineError>
where
    T: SizedSample + FromSample<f32>,
{
    let shared = renderer.shared.clone();
    let err_fn = move |err: Error| match err.kind() {
        ErrorKind::Xrun => {
            shared.xruns.fetch_add(1, Ordering::Relaxed);
        }
        _ => tracing::error!("audio stream error: {err}"),
    };
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &OutputCallbackInfo| {
                let ts = info.timestamp();
                let lat = ts
                    .playback
                    .duration_since(ts.callback)
                    .as_micros()
                    .min(u32::MAX as u128) as u32;
                renderer.fill(data, lat);
            },
            err_fn,
            None,
        )
        .map_err(|e| EngineError::Backend(e.to_string()))
}
