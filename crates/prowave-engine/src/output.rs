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
            _output: Output::Wasapi(stream),
            disk: DiskThread::spawn(),
        })
    }

    fn start_on_host(host: &cpal::Host, buffer_frames: Option<u32>) -> Result<Self, EngineError> {
        let mut last = EngineError::NoOutputDevice;
        if let Some(device) = host.default_output_device() {
            match Self::start_on(&device, buffer_frames) {
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
                match Self::start_on(&device, buffer_frames) {
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
    fn start_on(device: &cpal::Device, buffer_frames: Option<u32>) -> Result<Self, EngineError> {
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
        if let Some(n) = buffer_frames {
            let n = range.map_or(n, |(lo, hi)| n.clamp(lo, hi));
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

    /// [`Engine::stream_clip_in`] for a trimmed region: plays `length_secs` of the file (to its
    /// end when `None`) from `offset_secs` into it.
    #[allow(clippy::too_many_arguments)]
    pub fn stream_clip_range(
        &self,
        file: &AudioFile,
        track: u16,
        start_beat: f64,
        edit: ClipEdit,
        clips_in_song: usize,
        offset_secs: f64,
        length_secs: Option<f64>,
    ) -> AudioClip {
        self.disk.stream_range(
            file,
            self.sample_rate,
            track,
            start_beat,
            edit,
            crate::disk::ring_blocks_for(clips_in_song),
            offset_secs,
            length_secs,
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
