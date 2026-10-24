//! Disk streaming for audio regions (Logic Pro equivalent: audio file playback from disk).
//!
//! One background thread decodes with `symphonia`, resamples to the device rate (linear) and
//! pushes fixed-size [`Block`]s into a lock-free ring per clip. The audio thread only pops
//! blocks; it never touches the file system. Seeks are requested through an atomic
//! `(generation, frame)` word; blocks carry the generation they were produced for so stale
//! data is discarded without any locking.
use crate::core::{AudioClip, ClipEdit};
use rtrb::{Consumer, Producer, RingBuffer};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const BLOCK_FRAMES: usize = 1024;
/// Fewest prefetch blocks per clip: ~170 ms at 48 kHz (~64 KiB of stereo samples).
pub const RING_BLOCKS: usize = 8;
/// Most prefetch blocks per clip: ~0.68 s (~256 KiB), for projects with few regions.
pub const RING_BLOCKS_MAX: usize = 32;
/// Prefetch memory shared by all clips of a song: 1024 blocks, 8 MiB.
const PREFETCH_BUDGET_BLOCKS: usize = 1024;

/// Prefetch blocks for each clip of a song with `clips` streamed regions. A disk thread that
/// is held up (an antivirus scan, a busy drive, other programs using every core) must not
/// outlast what is buffered: the fewer clips the project has, the more each one buffers, up
/// to 0.68 s (up to 32 clips). All clips together stay within 8 MiB until there are more than
/// 128 clips, from which each keeps the 170 ms minimum.
pub fn ring_blocks_for(clips: usize) -> usize {
    (PREFETCH_BUDGET_BLOCKS / clips.max(1)).clamp(RING_BLOCKS, RING_BLOCKS_MAX)
}
const BLOCKS_PER_SERVICE: usize = 8;
/// How long a clip with nothing to read lets a memory-mapped file go for.
const RELEASE_AFTER: Duration = Duration::from_secs(1);
// Imports must not drain an unbounded queue before active clips get a refill.
const MESSAGES_PER_SERVICE: usize = 8;
// Native-rate frames retained ahead of the next reverse block (~64 KiB stereo).
const REVERSE_CACHE_FRAMES: u64 = 8192;
const MAX_SAMPLE_RATE: u32 = 192_000;
const MAX_CHANNELS: usize = 64;

fn validate_format(sample_rate: u32, channels: usize) -> Result<(), AudioError> {
    if !(1..=MAX_SAMPLE_RATE).contains(&sample_rate) || !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(AudioError::Decode(format!(
            "unsupported audio format: {sample_rate} Hz, {channels} channels (limits: 1-{MAX_SAMPLE_RATE} Hz, 1-{MAX_CHANNELS} channels)"
        )));
    }
    Ok(())
}

fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample
    } else {
        0.0
    }
}

/// A chunk of interleaved stereo audio in the device sample rate.
pub struct Block {
    pub gen: u16,
    /// Index of the first frame, relative to the clip start (device-rate frames).
    pub start: u64,
    pub frames: u32,
    pub data: [f32; BLOCK_FRAMES * 2],
}

/// Audio-thread -> disk-thread seek request, packed as `gen << 48 | frame`.
#[derive(Default)]
pub struct ClipShared {
    request: AtomicU64,
}

impl ClipShared {
    pub fn request(&self, gen: u16, frame: u64) {
        self.request.store(
            ((gen as u64) << 48) | (frame & 0x0000_FFFF_FFFF_FFFF),
            Ordering::Release,
        );
    }
    fn load(&self) -> (u16, u64) {
        let v = self.request.load(Ordering::Acquire);
        ((v >> 48) as u16, v & 0x0000_FFFF_FFFF_FFFF)
    }
}

#[derive(Debug)]
pub enum AudioError {
    Io(std::io::Error),
    Decode(String),
    /// The container does not report a length, so the clip length is unknown.
    UnknownLength,
    NoTrack,
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
            Self::UnknownLength => write!(f, "file length unknown"),
            Self::NoTrack => write!(f, "no audio track in file"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<SymError> for AudioError {
    fn from(e: SymError) -> Self {
        Self::Decode(e.to_string())
    }
}

/// Metadata of an audio file on disk (WAV, AIFF, FLAC).
#[derive(Clone, Debug)]
pub struct AudioFile {
    pub path: PathBuf,
    pub sample_rate: u32,
    /// Length in frames at the file's own sample rate.
    pub frames: u64,
    pub channels: usize,
}

impl AudioFile {
    /// Opens an audio region's source (Logic: Import Audio). Rejects zero/unsupported
    /// sample rates and channel counts before creating the decoder; see SECURITY.md limits.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AudioError> {
        let path = path.as_ref();
        let src = Source::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            sample_rate: src.sample_rate,
            frames: src.frames,
            channels: src.channels,
        })
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }

    /// Decodes the file off the audio thread and returns its maximum absolute channel sample.
    /// Non-finite samples count as silence, matching streamed playback.
    pub fn peak(&self) -> Result<f32, AudioError> {
        let mut src = Source::open(&self.path)?;
        let mut buf = Vec::new();
        let mut peak = 0.0f32;
        while src.next_stereo(&mut buf)? {
            for sample in &buf {
                peak = peak.max(sample.abs());
            }
            buf.clear();
        }
        Ok(peak)
    }

    /// Onsets and tempo of the file (Smart Tempo): decodes up to ten minutes of it. Run off
    /// the UI thread.
    pub fn analyze(&self) -> Result<crate::analysis::Analysis, AudioError> {
        let mut src = Source::open(&self.path)?;
        let mut analyzer = crate::analysis::Analyzer::new(self.sample_rate);
        let mut buf = Vec::new();
        while !analyzer.is_full() && src.next_stereo(&mut buf)? {
            for f in buf.as_chunks::<2>().0 {
                analyzer.push((f[0] + f[1]) * 0.5);
            }
            buf.clear();
        }
        Ok(analyzer.finish())
    }

    /// Min/max overview for waveform display. Decodes the whole file: run off the UI thread.
    /// Non-finite source samples are replaced with silence, as in streamed playback.
    pub fn peaks(&self, bins: usize) -> Result<Vec<(f32, f32)>, AudioError> {
        let bins = bins.max(1);
        let mut out = vec![(0f32, 0f32); bins];
        let mut src = Source::open(&self.path)?;
        let mut buf = Vec::new();
        let mut frame = 0u64;
        let total = self.frames.max(1);
        while src.next_stereo(&mut buf)? {
            for f in buf.as_chunks::<2>().0 {
                let m = (f[0] + f[1]) * 0.5;
                let b = ((frame * bins as u64) / total).min(bins as u64 - 1) as usize;
                let e = &mut out[b];
                e.0 = e.0.min(m);
                e.1 = e.1.max(m);
                frame += 1;
            }
            buf.clear();
        }
        Ok(out)
    }
}

/// Where a [`Source`] gets its samples.
enum Kind {
    Decoded(Decoded),
    /// A PCM file read straight from a memory map (see `mapped.rs`).
    #[cfg(not(target_arch = "wasm32"))]
    Mapped(crate::mapped::Mapped),
    /// A mapped file whose map was let go while nothing was read from it (see
    /// [`Source::release`]); the next read maps it again at `pos`.
    #[cfg(not(target_arch = "wasm32"))]
    Released {
        path: PathBuf,
        pos: u64,
    },
}

/// Sequential reader returning interleaved stereo f32 at the file's sample rate: a PCM file
/// through a memory map when it can be, the decoder otherwise.
struct Source {
    kind: Kind,
    sample_rate: u32,
    frames: u64,
    channels: usize,
}

/// A `symphonia` decoder.
struct Decoded {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sbuf: Option<SampleBuffer<f32>>,
}

impl Source {
    fn open(path: &Path) -> Result<Self, AudioError> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(src) = Self::map(path) {
            return Ok(src);
        }
        let mut src = Self::probe(path)?;
        if src.frames == 0 {
            // MP3 files without a Xing/LAME header do not say how long they are.
            src.frames = Self::count_frames(path)?;
        }
        Ok(src)
    }

    /// Opens the decoder. `frames` is 0 when the file does not state its length.
    fn probe(path: &Path) -> Result<Self, AudioError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;
        let format = probed.format;
        let track = format.default_track().ok_or(AudioError::NoTrack)?;
        let params = track.codec_params.clone();
        let sample_rate = params.sample_rate.ok_or(AudioError::NoTrack)?;
        let channels = params
            .channels
            .map(|c| c.count())
            .ok_or(AudioError::NoTrack)?;
        validate_format(sample_rate, channels)?;
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
        Ok(Self {
            kind: Kind::Decoded(Decoded {
                track_id: track.id,
                format,
                decoder,
                sbuf: None,
            }),
            sample_rate,
            frames: params.n_frames.unwrap_or(0),
            channels,
        })
    }

    /// The file through a memory map, if it is plain PCM in a layout `mapped.rs` reads.
    #[cfg(not(target_arch = "wasm32"))]
    fn map(path: &Path) -> Option<Self> {
        let m = crate::mapped::Mapped::open(path)?;
        validate_format(m.sample_rate, m.channels).ok()?;
        Some(Self {
            sample_rate: m.sample_rate,
            frames: m.frames,
            channels: m.channels,
            kind: Kind::Mapped(m),
        })
    }

    /// Whether a mapped file's map has been let go of.
    #[cfg(test)]
    fn is_released(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(self.kind, Kind::Released { .. }) {
            return true;
        }
        false
    }

    /// Whether the samples come from a memory map.
    #[cfg(test)]
    fn is_mapped(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(self.kind, Kind::Mapped(_)) {
            return true;
        }
        false
    }

    /// Appends the next packet as stereo frames. `Ok(false)` at end of stream.
    fn next_stereo(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        self.reacquire();
        match &mut self.kind {
            Kind::Decoded(d) => d.next_stereo(out),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Mapped(m) => Ok(m.next_stereo(out)),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Released { .. } => Ok(false),
        }
    }

    /// Seeks to `ts` (source frames); returns the frame the stream actually starts at.
    fn seek(&mut self, ts: u64) -> Result<u64, AudioError> {
        self.reacquire();
        match &mut self.kind {
            Kind::Decoded(d) => d.seek(ts),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Mapped(m) => Ok(m.seek(ts)),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Released { .. } => Ok(ts.min(self.frames)),
        }
    }

    /// Lets go of the map while nothing reads from it. A mapped file cannot be changed or
    /// deleted by other programs on Windows, so a project does not hold its audio files that
    /// way while it sits still.
    fn release(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Kind::Mapped(m) = &self.kind {
            self.kind = Kind::Released {
                path: m.path().to_path_buf(),
                pos: m.pos(),
            };
        }
    }

    /// Maps a released file again where it was left (or reads it with the decoder if it can
    /// no longer be mapped).
    fn reacquire(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Kind::Released { path, pos } = &self.kind {
            let (path, pos) = (path.clone(), *pos);
            let again = match crate::mapped::Mapped::open(&path) {
                Some(mut m) if m.frames == self.frames => {
                    m.seek(pos);
                    Some(Kind::Mapped(m))
                }
                _ => Self::probe(&path).ok().and_then(|mut s| {
                    s.seek(pos).ok()?;
                    Some(s.kind)
                }),
            };
            match again {
                Some(kind) => self.kind = kind,
                None => tracing::error!("cannot open {} again", path.display()),
            }
        }
    }

    /// A mapped file that has got shorter is not read any more: the decoder takes over where
    /// the map left off (and if that cannot be, the clip falls silent).
    fn revalidate(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Kind::Mapped(m) = &mut self.kind {
            if m.still_whole() {
                return;
            }
            let (path, pos) = (m.path().to_path_buf(), m.pos());
            tracing::warn!(
                "{} got shorter; reading it with the decoder",
                path.display()
            );
            let decoded = Self::probe(&path).ok().and_then(|mut s| {
                s.seek(pos).ok()?;
                Some(s)
            });
            match decoded {
                Some(mut decoded) => {
                    decoded.frames = self.frames;
                    *self = decoded;
                }
                None => tracing::error!("cannot go on reading {}", path.display()),
            }
        }
    }

    /// Length of a file that does not state it, found by decoding all of it once.
    fn count_frames(path: &Path) -> Result<u64, AudioError> {
        let mut counter = Self::probe(path)?;
        let (mut buf, mut frames) = (Vec::new(), 0u64);
        while counter.next_stereo(&mut buf)? {
            frames += buf.len() as u64 / 2;
            buf.clear();
        }
        if frames == 0 {
            return Err(AudioError::UnknownLength);
        }
        Ok(frames)
    }
}

impl Decoded {
    /// Appends the next packet as stereo frames. `Ok(false)` at end of stream.
    fn next_stereo(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false)
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            let spec = *decoded.spec();
            validate_format(spec.rate, spec.channels.count())?;
            let cap = decoded.capacity() as u64;
            let sbuf = match &mut self.sbuf {
                Some(b) if b.capacity() as u64 >= cap * spec.channels.count() as u64 => b,
                slot => slot.insert(SampleBuffer::<f32>::new(cap, spec)),
            };
            sbuf.copy_interleaved_ref(decoded);
            let nch = spec.channels.count().max(1);
            for f in sbuf.samples().chunks_exact(nch) {
                // Float WAV/AIFF samples can contain NaN/Inf. Keep them out of the
                // resampler and mixer; ordinary finite values above unity remain valid.
                let l = finite_sample(f[0]);
                out.push(l);
                out.push(if nch > 1 { finite_sample(f[1]) } else { l });
            }
            return Ok(true);
        }
    }

    /// Seeks to `ts` (source frames); returns the frame the stream actually starts at.
    fn seek(&mut self, ts: u64) -> Result<u64, AudioError> {
        let to = self.format.seek(
            SeekMode::Accurate,
            SeekTo::TimeStamp {
                ts,
                track_id: self.track_id,
            },
        )?;
        self.decoder.reset();
        Ok(to.actual_ts)
    }
}

enum Msg {
    Register {
        file: AudioFile,
        producer: Producer<Block>,
        shared: Arc<ClipShared>,
        out_rate: u32,
        out_len: u64,
        edit: ClipEdit,
    },
    Quit,
}

enum Progress {
    Idle,
    Worked,
    Gone,
}

struct Stream {
    src: Source,
    /// Since when there has been nothing to do for this clip (its ring is full, or the clip
    /// has played to its end).
    idle_since: Option<Instant>,
    producer: Producer<Block>,
    shared: Arc<ClipShared>,
    ratio: f64,
    out_len: u64,
    gen: Option<u16>,
    out_frame: u64,
    buf: Vec<f32>,
    buf_base: u64,
    eof: bool,
    reverse: bool,
}

impl Stream {
    fn seek_out(&mut self, frame: u64) {
        self.out_frame = frame.min(self.out_len);
        let s = (self.out_frame as f64 * self.ratio) as u64;
        self.buf.clear();
        self.eof = false;
        match self.src.seek(s) {
            Ok(actual) => self.buf_base = actual,
            Err(e) => {
                tracing::error!("disk seek failed: {e}");
                self.out_frame = self.out_len; // stop producing
            }
        }
    }

    fn ensure(&mut self, upto: u64) {
        while !self.eof && self.buf_base + (self.buf.len() / 2) as u64 <= upto {
            match self.src.next_stereo(&mut self.buf) {
                Ok(true) => {}
                Ok(false) => self.eof = true,
                Err(e) => {
                    tracing::error!("disk decode failed: {e}");
                    self.eof = true;
                }
            }
        }
    }

    fn frame(&self, i: u64) -> (f32, f32) {
        let n = self.buf.len() / 2;
        if n == 0 {
            return (0.0, 0.0);
        }
        let idx = (i.saturating_sub(self.buf_base) as usize).min(n - 1);
        (self.buf[idx * 2], self.buf[idx * 2 + 1])
    }

    fn ensure_reverse_window(&mut self, lo: u64, hi: u64) -> Result<(), AudioError> {
        // The final interpolation frame may be just beyond EOF; frame() repeats the
        // last source frame there, so it need not trigger another decoder seek.
        let needed_hi = hi.min(self.src.frames.saturating_sub(1));
        let buffered_end = self.buf_base + (self.buf.len() / 2) as u64;
        if lo >= self.buf_base && needed_hi < buffered_end {
            return Ok(());
        }
        let actual = self.src.seek(lo.saturating_sub(REVERSE_CACHE_FRAMES))?;
        self.buf.clear();
        self.buf_base = actual;
        self.eof = false;
        self.ensure(hi);
        Ok(())
    }

    fn fill_block(&mut self, gen: u16) -> Block {
        let n = (BLOCK_FRAMES as u64).min(self.out_len - self.out_frame) as usize;
        let mut block = Block {
            gen,
            start: self.out_frame,
            frames: n as u32,
            data: [0.0; BLOCK_FRAMES * 2],
        };
        if self.reverse && n > 0 {
            let first = self.out_len - self.out_frame - 1;
            let last = self.out_len - self.out_frame - n as u64;
            let lo = (last as f64 * self.ratio).floor() as u64;
            let hi = (first as f64 * self.ratio).floor() as u64 + 1;
            match self.ensure_reverse_window(lo, hi) {
                Ok(()) => {
                    for i in 0..n {
                        let pos =
                            (self.out_len - (self.out_frame + i as u64) - 1) as f64 * self.ratio;
                        let i0 = pos.floor() as u64;
                        let frac = (pos - i0 as f64) as f32;
                        let (l0, r0) = self.frame(i0);
                        let (l1, r1) = self.frame(i0 + 1);
                        block.data[i * 2] = l0 + (l1 - l0) * frac;
                        block.data[i * 2 + 1] = r0 + (r1 - r0) * frac;
                    }
                }
                Err(e) => tracing::error!("reverse disk seek failed: {e}"),
            }
            self.out_frame += n as u64;
            return block;
        }
        for i in 0..n {
            let pos = (self.out_frame + i as u64) as f64 * self.ratio;
            let i0 = pos.floor() as u64;
            let frac = (pos - i0 as f64) as f32;
            self.ensure(i0 + 1);
            let (l0, r0) = self.frame(i0);
            let (l1, r1) = self.frame(i0 + 1);
            block.data[i * 2] = l0 + (l1 - l0) * frac;
            block.data[i * 2 + 1] = r0 + (r1 - r0) * frac;
        }
        self.out_frame += n as u64;
        // Drop source frames we will never read again.
        let keep_from = ((self.out_frame as f64 * self.ratio).floor() as u64).saturating_sub(1);
        if keep_from > self.buf_base {
            let drop_frames = ((keep_from - self.buf_base) as usize).min(self.buf.len() / 2);
            self.buf.drain(..drop_frames * 2);
            self.buf_base += drop_frames as u64;
        }
        block
    }

    fn service(&mut self) -> Progress {
        if self.producer.is_abandoned() {
            return Progress::Gone;
        }
        self.src.revalidate();
        let (gen, frame) = self.shared.load();
        if self.gen != Some(gen) {
            self.gen = Some(gen);
            self.seek_out(frame);
        }
        let mut worked = false;
        for _ in 0..BLOCKS_PER_SERVICE {
            if self.producer.slots() == 0 || self.out_frame >= self.out_len {
                break;
            }
            let block = self.fill_block(gen);
            if self.producer.push(block).is_err() {
                break;
            }
            worked = true;
        }
        if worked {
            self.idle_since = None;
            Progress::Worked
        } else {
            let since = *self.idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= RELEASE_AFTER {
                self.src.release();
            }
            Progress::Idle
        }
    }
}

/// Owns the streaming thread. Dropping it stops the thread.
pub struct DiskThread {
    tx: mpsc::Sender<Msg>,
    handle: Option<JoinHandle<()>>,
}

impl DiskThread {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        let handle = std::thread::Builder::new()
            .name("prowave-disk".into())
            .spawn(move || run(rx))
            .map_err(|e| tracing::error!("cannot spawn disk thread: {e}"))
            .ok();
        Self { tx, handle }
    }

    /// Creates a streamed clip placed at `start_beat` on `track`. The file is opened on the
    /// disk thread; if that fails the clip simply stays silent.
    pub fn stream(
        &self,
        file: &AudioFile,
        out_rate: u32,
        track: u16,
        start_beat: f64,
    ) -> AudioClip {
        self.stream_with(file, out_rate, track, start_beat, ClipEdit::default())
    }

    /// Creates a streamed clip with non-destructive region edits and the largest read-ahead.
    /// For a song with many clips use [`DiskThread::stream_with_ring`].
    pub fn stream_with(
        &self,
        file: &AudioFile,
        out_rate: u32,
        track: u16,
        start_beat: f64,
        edit: ClipEdit,
    ) -> AudioClip {
        self.stream_with_ring(file, out_rate, track, start_beat, edit, RING_BLOCKS_MAX)
    }

    /// [`DiskThread::stream_with`] buffering `ring_blocks` blocks ahead (see
    /// [`ring_blocks_for`]; kept within [`RING_BLOCKS`]..=[`RING_BLOCKS_MAX`]).
    pub fn stream_with_ring(
        &self,
        file: &AudioFile,
        out_rate: u32,
        track: u16,
        start_beat: f64,
        edit: ClipEdit,
        ring_blocks: usize,
    ) -> AudioClip {
        let out_len =
            (file.frames as f64 * out_rate as f64 / file.sample_rate.max(1) as f64) as u64;
        let ring_blocks = ring_blocks.clamp(RING_BLOCKS, RING_BLOCKS_MAX);
        let (producer, consumer): (Producer<Block>, Consumer<Block>) = RingBuffer::new(ring_blocks);
        let shared = Arc::new(ClipShared::default());
        shared.request(0, 0);
        let msg = Msg::Register {
            file: file.clone(),
            producer,
            shared: shared.clone(),
            out_rate,
            out_len,
            edit,
        };
        if self.tx.send(msg).is_err() {
            tracing::error!("disk thread is not running");
        }
        AudioClip::new(track, start_beat, out_len, edit, consumer, shared)
    }
}

impl Drop for DiskThread {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Applies one message; `false` means quit.
fn handle(msg: Msg, streams: &mut Vec<Stream>) -> bool {
    match msg {
        Msg::Register {
            file,
            producer,
            shared,
            out_rate,
            out_len,
            edit,
        } => {
            if producer.is_abandoned() {
                return true;
            }
            match Source::open(&file.path) {
                Ok(src) => streams.push(Stream {
                    ratio: src.sample_rate as f64 / out_rate.max(1) as f64,
                    src,
                    idle_since: None,
                    producer,
                    shared,
                    out_len,
                    gen: None,
                    out_frame: 0,
                    buf: Vec::new(),
                    buf_base: 0,
                    eof: false,
                    reverse: edit.reverse,
                }),
                Err(e) => tracing::error!("cannot open {}: {e}", file.path.display()),
            }
        }
        Msg::Quit => return false,
    }
    true
}

/// Bound registration work so every worker pass can refill active streams.
fn service_messages(rx: &mpsc::Receiver<Msg>, streams: &mut Vec<Stream>) -> bool {
    for _ in 0..MESSAGES_PER_SERVICE {
        match rx.try_recv() {
            Ok(msg) => {
                if !handle(msg, streams) {
                    return false;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => return false,
            Err(mpsc::TryRecvError::Empty) => break,
        }
    }
    true
}

fn run(rx: mpsc::Receiver<Msg>) {
    let mut streams: Vec<Stream> = Vec::new();
    loop {
        // Nothing to stream (no audio regions): block until a clip arrives instead of
        // waking up every 2 ms.
        if streams.is_empty() {
            let Ok(msg) = rx.recv() else { return };
            if !handle(msg, &mut streams) {
                return;
            }
        }
        if !service_messages(&rx, &mut streams) {
            return;
        }
        let mut any = false;
        streams.retain_mut(|s| match s.service() {
            Progress::Gone => false,
            Progress::Worked => {
                any = true;
                true
            }
            Progress::Idle => true,
        });
        if !any {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::core::{ClipEdit, Command, EngineCore, FadeCurve, Shared, Song, TrackMix};
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    const SRC_RATE: u32 = 44_100;
    const OUT_RATE: u32 = 48_000;

    #[test]
    fn format_limits_reject_invalid_metadata() {
        for rate in [1, 44_100, 48_000, MAX_SAMPLE_RATE] {
            for channels in [1, 2, MAX_CHANNELS] {
                assert!(validate_format(rate, channels).is_ok());
            }
        }
        for (rate, channels) in [(0, 2), (MAX_SAMPLE_RATE + 1, 2), (48_000, 0), (48_000, 65)] {
            assert!(validate_format(rate, channels).is_err());
        }
    }

    #[test]
    fn prefetch_memory_is_bounded_and_grows_as_the_song_shrinks() {
        let block = std::mem::size_of::<Block>();
        // One 1024-frame stereo f32 block is 8 KiB: 8 blocks ~64 KiB, 32 blocks ~256 KiB.
        assert!(block * RING_BLOCKS <= 66_000);
        assert!(block * RING_BLOCKS_MAX <= 270_000);
        assert_eq!(ring_blocks_for(0), RING_BLOCKS_MAX);
        assert_eq!(ring_blocks_for(1), RING_BLOCKS_MAX);
        assert_eq!(
            ring_blocks_for(32),
            RING_BLOCKS_MAX,
            "up to 32 clips: 0.68 s each"
        );
        assert!(ring_blocks_for(33) < RING_BLOCKS_MAX);
        assert_eq!(ring_blocks_for(1000), RING_BLOCKS, "never below 170 ms");
        let mut last = RING_BLOCKS_MAX;
        for clips in 1..=2000 {
            let n = ring_blocks_for(clips);
            assert!(n <= last, "never grows with more clips ({clips})");
            last = n;
            if clips <= 128 {
                // 8 MiB for every clip together (the budget; a few blocks of rounding).
                assert!(block * n * clips <= 8 * 1024 * 1024 + block * clips);
            }
        }
    }

    #[test]
    fn registration_bursts_leave_time_to_refill_active_streams() {
        let dir = tempfile::tempdir().unwrap();
        let file = AudioFile::open(write_sine_wav(dir.path())).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut consumers = Vec::new();
        for _ in 0..MESSAGES_PER_SERVICE * 2 {
            let (producer, consumer) = RingBuffer::new(RING_BLOCKS);
            consumers.push(consumer);
            tx.send(Msg::Register {
                file: file.clone(),
                producer,
                shared: Arc::new(ClipShared::default()),
                out_rate: SRC_RATE,
                out_len: file.frames,
                edit: ClipEdit::default(),
            })
            .unwrap();
        }
        let mut streams = Vec::new();
        for count in [MESSAGES_PER_SERVICE, MESSAGES_PER_SERVICE * 2] {
            assert!(service_messages(&rx, &mut streams));
            assert_eq!(streams.len(), count);
            for stream in &mut streams {
                assert!(matches!(stream.service(), Progress::Worked));
            }
            for consumer in &mut consumers[..count] {
                let block = consumer.pop().unwrap();
                assert!(block.data.iter().any(|sample| *sample != 0.0));
            }
        }
        tx.send(Msg::Quit).unwrap();
        assert!(!service_messages(&rx, &mut streams));
    }

    #[test]
    fn canceled_registrations_do_not_open_streams() {
        let dir = tempfile::tempdir().unwrap();
        let file = AudioFile::open(write_sine_wav(dir.path())).unwrap();
        let (producer, consumer) = RingBuffer::new(RING_BLOCKS);
        drop(consumer);
        let mut streams = Vec::new();
        assert!(handle(
            Msg::Register {
                out_len: file.frames,
                file,
                producer,
                shared: Arc::new(ClipShared::default()),
                out_rate: SRC_RATE,
                edit: ClipEdit::default(),
            },
            &mut streams,
        ));
        assert!(streams.is_empty());
    }

    #[test]
    fn non_finite_samples_become_silence_without_clipping_finite_audio() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(finite_sample(value), 0.0);
        }
        for value in [-2.0, -0.25, 0.0, 0.25, 2.0] {
            assert_eq!(finite_sample(value), value);
        }
    }

    /// 1 s, mono, 16-bit PCM, 440 Hz sine at amplitude 0.5.
    fn write_sine_wav(dir: &Path) -> PathBuf {
        let path = dir.join("sine.wav");
        let n = SRC_RATE as usize;
        let mut bytes = Vec::with_capacity(44 + n * 2);
        let data_len = (n * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&SRC_RATE.to_le_bytes());
        bytes.extend_from_slice(&(SRC_RATE * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let s = (i as f32 / SRC_RATE as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            bytes.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
        let mut f = File::create(&path).unwrap();
        f.write_all(&bytes).unwrap();
        path
    }

    struct Rig {
        core: EngineCore,
        tx: Producer<Command>,
        shared: Arc<Shared>,
        _garbage: Consumer<crate::core::Garbage>,
        _disk: DiskThread,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        rig_with(ClipEdit::default())
    }

    fn rig_with(edit: ClipEdit) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let file = AudioFile::open(write_sine_wav(dir.path())).unwrap();
        let disk = DiskThread::spawn();
        let clip = disk.stream_with(&file, OUT_RATE, 0, 0.0, edit);
        let (mut tx, rx) = RingBuffer::new(64);
        let (gtx, grx) = RingBuffer::new(8);
        let shared = Arc::new(Shared::new());
        let core = EngineCore::new(OUT_RATE, rx, gtx, shared.clone());
        let song = Song::new(vec![], vec![TrackMix::default()]).with_clips(vec![clip]);
        assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
        Rig {
            core,
            tx,
            shared,
            _garbage: grx,
            _disk: disk,
            _dir: dir,
        }
    }

    /// Runs `secs` of audio at playback speed and returns the left channel.
    /// Offline rendering can outrun the real-time disk worker's bounded prefetch.
    fn run(r: &mut Rig, secs: f32) -> Vec<f32> {
        let mut buf = [0f32; 1024];
        let mut left = Vec::new();
        let began = Instant::now();
        for _ in 0..((secs * OUT_RATE as f32) as usize / 512) {
            assert_no_alloc::assert_no_alloc(|| r.core.process(&mut buf));
            left.extend(buf.iter().step_by(2));
            let deadline = began + Duration::from_secs_f64(left.len() as f64 / OUT_RATE as f64);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        left
    }

    fn prefetch(r: &mut Rig) {
        run(r, 0.02); // applies queued commands (seek requests)
        std::thread::sleep(Duration::from_millis(300));
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn reads_wav_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let f = AudioFile::open(write_sine_wav(dir.path())).unwrap();
        assert_eq!(
            (f.sample_rate, f.frames, f.channels),
            (SRC_RATE, SRC_RATE as u64, 1)
        );
        let peaks = f.peaks(50).unwrap();
        let max = peaks.iter().map(|p| p.1).fold(0.0, f32::max);
        assert!((max - 0.5).abs() < 0.02, "{max}");
        let peak = crate::peak_of(&f).unwrap();
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }

    #[test]
    fn streams_resampled_region_without_underruns() {
        let mut r = rig();
        prefetch(&mut r);
        assert!(r.tx.push(Command::Play).is_ok());
        let out = run(&mut r, 0.5);
        // 0.5 amplitude * 0.8 master, sine RMS = A/sqrt(2)
        let expected = 0.5 * 0.8 / std::f32::consts::SQRT_2;
        assert!(
            (rms(&out) - expected).abs() < expected * 0.05,
            "rms {}",
            rms(&out)
        );
        let c = crossings(&out) as f32 / (out.len() as f32 / OUT_RATE as f32);
        assert!((c / 2.0 - 440.0).abs() < 5.0, "freq {}", c / 2.0);
        assert!(out.iter().all(|x| x.is_finite()));
        assert_eq!(r.shared.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn reverse_starts_at_the_source_end() {
        let mut r = rig_with(ClipEdit {
            reverse: true,
            ..ClipEdit::default()
        });
        prefetch(&mut r);
        assert!(r.tx.push(Command::Play).is_ok());
        let out = run(&mut r, 0.02);
        assert!(out[0] < -0.015, "first reversed sample: {}", out[0]);
    }

    #[test]
    fn reverse_matches_forward_reference_across_cache_boundaries_and_seeks() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sine_wav(dir.path());
        let mut reference_source = Source::open(&path).unwrap();
        let mut decoded = Vec::new();
        while reference_source.next_stereo(&mut decoded).unwrap() {}
        let source_frames = decoded.len() / 2;
        for rate in [SRC_RATE, OUT_RATE, SRC_RATE / 2] {
            let ratio = SRC_RATE as f64 / rate as f64;
            let out_len = (source_frames as f64 / ratio) as u64;
            let mut reference: Vec<[f32; 2]> = (0..out_len)
                .map(|frame| {
                    let position = frame as f64 * ratio;
                    let base = position.floor() as usize;
                    let next = (base + 1).min(source_frames - 1);
                    let fraction = (position - base as f64) as f32;
                    std::array::from_fn(|channel| {
                        let a = decoded[base * 2 + channel];
                        let b = decoded[next * 2 + channel];
                        a + (b - a) * fraction
                    })
                })
                .collect();
            reference.reverse();
            let (producer, _consumer) = RingBuffer::new(RING_BLOCKS);
            let mut stream = Stream {
                src: Source::open(&path).unwrap(),
                idle_since: None,
                producer,
                shared: Arc::new(ClipShared::default()),
                ratio,
                out_len,
                gen: None,
                out_frame: 0,
                buf: Vec::new(),
                buf_base: 0,
                eof: false,
                reverse: true,
            };
            // Full playback crosses several cache windows. Later seeks invalidate
            // those windows, including a seek back after reaching the file's start.
            for offset in [0, out_len / 3 + 17, out_len - 1, 0] {
                stream.seek_out(offset);
                while stream.out_frame < out_len {
                    let block = stream.fill_block(0);
                    let start = block.start as usize;
                    for (i, stereo) in block.data[..block.frames as usize * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .enumerate()
                    {
                        assert_eq!(
                            *stereo,
                            reference[start + i],
                            "rate={rate}, frame={}",
                            start + i
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn zero_db_edit_is_bit_identical_to_default_playback() {
        let mut plain = rig();
        let mut edited = rig_with(ClipEdit {
            gain_db: 0.0,
            ..ClipEdit::default()
        });
        prefetch(&mut plain);
        prefetch(&mut edited);
        assert!(plain.tx.push(Command::Play).is_ok());
        assert!(edited.tx.push(Command::Play).is_ok());
        assert_eq!(run(&mut plain, 0.02), run(&mut edited, 0.02));
    }

    #[test]
    fn beat_fades_start_and_end_at_silence() {
        let edit = ClipEdit {
            fade_in_beats: 1.0,
            fade_out_beats: 1.0,
            fade_curve: FadeCurve::Linear,
            ..ClipEdit::default()
        };
        let mut r = rig_with(edit);
        prefetch(&mut r);
        assert!(r.tx.push(Command::Play).is_ok());
        let mut out = run(&mut r, 1.0);
        let mut tail = [0.0; 768]; // Complete the final 384 frames after the 512-frame callbacks.
        assert_no_alloc::assert_no_alloc(|| r.core.process(&mut tail));
        out.extend(tail.iter().step_by(2).copied());
        assert_eq!(out[0], 0.0);
        let middle = &out[(0.20 * OUT_RATE as f32) as usize..(0.30 * OUT_RATE as f32) as usize];
        assert!(middle.iter().any(|sample| sample.abs() > 0.1));
        assert!(out[out.len() - 1].abs() < 0.001);
    }

    #[test]
    fn seek_into_region_then_silence_after_end() {
        let mut r = rig();
        // 120 BPM: beat 1.5 = 0.75 s into the 1 s file.
        assert!(r.tx.push(Command::SetPosition { beat: 1.5 }).is_ok());
        prefetch(&mut r);
        assert!(r.tx.push(Command::Play).is_ok());
        let out = run(&mut r, 0.6);
        let head = &out[..(0.2 * OUT_RATE as f32) as usize];
        let tail = &out[(0.35 * OUT_RATE as f32) as usize..];
        assert!(rms(head) > 0.25, "head rms {}", rms(head));
        assert!(rms(tail) == 0.0, "tail rms {}", rms(tail));
        assert_eq!(r.shared.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn muted_track_silences_region() {
        let mut r = rig();
        let mix = TrackMix {
            gain_db: 0.0,
            mute: true,
            solo: false,
            pan: 0.0,
        };
        assert!(r.tx.push(Command::SetTrackMix { track: 0, mix }).is_ok());
        prefetch(&mut r);
        assert!(r.tx.push(Command::Play).is_ok());
        assert_eq!(rms(&run(&mut r, 0.3)), 0.0);
    }

    /// Samples of the test files: a different wave on each channel, and the extremes.
    fn test_sample(frame: usize, channel: usize) -> f64 {
        match frame {
            0 => -1.0,
            1 => 0.999,
            _ => (frame as f64 * 0.013 * (channel + 1) as f64).sin() * 0.8,
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Container {
        Wav,
        /// WAVE_FORMAT_EXTENSIBLE, with a `JUNK` chunk before the data.
        WavExtensible,
        Aiff,
        /// AIFF-C with the `sowt` (little-endian) compression type.
        AiffLittleEndian,
        AiffFloat,
    }

    /// The sample rate as the 80-bit extended float an AIFF file stores it as.
    fn extended_rate(rate: u32) -> [u8; 10] {
        let shift = rate.leading_zeros() + 32;
        let mantissa = (rate as u64) << shift;
        let exponent = 16383 + 63 - shift as u16;
        let mut out = [0u8; 10];
        out[..2].copy_from_slice(&exponent.to_be_bytes());
        out[2..].copy_from_slice(&mantissa.to_be_bytes());
        out
    }

    /// Writes `frames` frames of a PCM file and returns its path.
    #[allow(clippy::too_many_arguments)]
    fn write_pcm(
        dir: &Path,
        name: &str,
        container: Container,
        bits: u16,
        float: bool,
        channels: usize,
        rate: u32,
        frames: usize,
    ) -> PathBuf {
        let big = matches!(container, Container::Aiff | Container::AiffFloat);
        let mut data = Vec::new();
        for f in 0..frames {
            for c in 0..channels {
                let v = test_sample(f, c);
                match (bits, float) {
                    (16, false) => {
                        let s = (v * 32767.0).round().max(-32768.0) as i16;
                        data.extend_from_slice(&if big {
                            s.to_be_bytes()
                        } else {
                            s.to_le_bytes()
                        });
                    }
                    (24, false) => {
                        let s = (v * 8_388_607.0).round().max(-8_388_608.0) as i32;
                        let b = if big {
                            s.to_be_bytes()
                        } else {
                            s.to_le_bytes()
                        };
                        data.extend_from_slice(if big { &b[1..] } else { &b[..3] });
                    }
                    (32, false) => {
                        let s = (v * 2_147_483_647.0).round().max(-2_147_483_648.0) as i32;
                        data.extend_from_slice(&if big {
                            s.to_be_bytes()
                        } else {
                            s.to_le_bytes()
                        });
                    }
                    (32, true) => {
                        let s = v as f32;
                        data.extend_from_slice(&if big {
                            s.to_be_bytes()
                        } else {
                            s.to_le_bytes()
                        });
                    }
                    (64, true) => {
                        data.extend_from_slice(&if big {
                            v.to_be_bytes()
                        } else {
                            v.to_le_bytes()
                        });
                    }
                    _ => unreachable!("not a test format"),
                }
            }
        }
        let align = channels * bits as usize / 8;
        let mut out = Vec::new();
        match container {
            Container::Wav | Container::WavExtensible => {
                let ext = matches!(container, Container::WavExtensible);
                let tag: u16 = if float { 3 } else { 1 };
                let mut fmt = Vec::new();
                fmt.extend_from_slice(&(if ext { 0xfffeu16 } else { tag }).to_le_bytes());
                fmt.extend_from_slice(&(channels as u16).to_le_bytes());
                fmt.extend_from_slice(&rate.to_le_bytes());
                fmt.extend_from_slice(&(rate * align as u32).to_le_bytes());
                fmt.extend_from_slice(&(align as u16).to_le_bytes());
                fmt.extend_from_slice(&bits.to_le_bytes());
                if ext {
                    fmt.extend_from_slice(&22u16.to_le_bytes());
                    fmt.extend_from_slice(&bits.to_le_bytes());
                    fmt.extend_from_slice(&0u32.to_le_bytes());
                    fmt.extend_from_slice(&tag.to_le_bytes());
                    fmt.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00]);
                    fmt.extend_from_slice(&[0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
                }
                let mut body = b"WAVE".to_vec();
                body.extend_from_slice(b"fmt ");
                body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
                body.extend_from_slice(&fmt);
                if ext {
                    // An odd-sized chunk (padded to even) between the format and the data.
                    body.extend_from_slice(b"JUNK");
                    body.extend_from_slice(&5u32.to_le_bytes());
                    body.extend_from_slice(b"hello\0");
                }
                body.extend_from_slice(b"data");
                body.extend_from_slice(&(data.len() as u32).to_le_bytes());
                body.extend_from_slice(&data);
                out.extend_from_slice(b"RIFF");
                out.extend_from_slice(&(body.len() as u32).to_le_bytes());
                out.extend_from_slice(&body);
            }
            Container::Aiff | Container::AiffLittleEndian | Container::AiffFloat => {
                let aifc = !matches!(container, Container::Aiff);
                let mut comm = Vec::new();
                comm.extend_from_slice(&(channels as u16).to_be_bytes());
                comm.extend_from_slice(&(frames as u32).to_be_bytes());
                comm.extend_from_slice(&bits.to_be_bytes());
                comm.extend_from_slice(&extended_rate(rate));
                if aifc {
                    comm.extend_from_slice(match container {
                        Container::AiffLittleEndian => b"sowt",
                        _ if bits == 64 => b"fl64",
                        _ => b"fl32",
                    });
                    comm.extend_from_slice(&[0, 0]); // an empty name
                }
                let mut body = if aifc {
                    b"AIFC".to_vec()
                } else {
                    b"AIFF".to_vec()
                };
                body.extend_from_slice(b"COMM");
                body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
                body.extend_from_slice(&comm);
                body.extend_from_slice(b"SSND");
                body.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
                body.extend_from_slice(&[0; 8]);
                body.extend_from_slice(&data);
                out.extend_from_slice(b"FORM");
                out.extend_from_slice(&(body.len() as u32).to_be_bytes());
                out.extend_from_slice(&body);
            }
        }
        let path = dir.join(name);
        std::fs::write(&path, out).unwrap();
        path
    }

    fn read_all(src: &mut Source) -> Vec<f32> {
        let mut out = Vec::new();
        while src.next_stereo(&mut out).unwrap() {}
        out
    }

    #[test]
    fn mapped_pcm_gives_exactly_the_samples_the_decoder_does() {
        let dir = tempfile::tempdir().unwrap();
        // 10 000 frames: more than two chunks of the map, and a short last one.
        let frames = 10_000;
        let cases = [
            (Container::Wav, 16, false, 1, 44_100),
            (Container::Wav, 16, false, 2, 48_000),
            (Container::Wav, 24, false, 2, 96_000),
            (Container::WavExtensible, 24, false, 2, 44_100),
            (Container::Wav, 32, false, 2, 44_100),
            (Container::Wav, 32, true, 2, 48_000),
            (Container::Wav, 64, true, 1, 48_000),
            (Container::Wav, 16, false, 6, 48_000),
            (Container::Aiff, 16, false, 2, 44_100),
            (Container::Aiff, 24, false, 1, 48_000),
            (Container::Aiff, 32, false, 2, 22_050),
            (Container::AiffLittleEndian, 16, false, 2, 44_100),
            (Container::AiffFloat, 32, true, 2, 44_100),
            (Container::AiffFloat, 64, true, 2, 88_200),
        ];
        for (n, (container, bits, float, channels, rate)) in cases.into_iter().enumerate() {
            let what = format!("{container:?} {bits}-bit float={float} {channels}ch {rate} Hz");
            let ext = if matches!(container, Container::Wav | Container::WavExtensible) {
                "wav"
            } else {
                "aif"
            };
            let name = format!("t{n}.{ext}");
            let path = write_pcm(
                dir.path(),
                &name,
                container,
                bits,
                float,
                channels,
                rate,
                frames,
            );
            let mut mapped = Source::open(&path).unwrap();
            assert!(mapped.is_mapped(), "{what} is read from the map");
            let mut decoded = Source::probe(&path).unwrap();
            assert!(!decoded.is_mapped());
            assert_eq!(
                (mapped.sample_rate, mapped.channels),
                (decoded.sample_rate, decoded.channels),
                "{what}: the header"
            );
            assert_eq!(mapped.frames, frames as u64, "{what}");
            // The decoder counts an AIFF's frames from the size of its sound chunk, which
            // includes the chunk's own 8-byte header: two stereo frames too many.
            assert!(decoded.frames >= mapped.frames, "{what}");
            let (a, b) = (read_all(&mut mapped), read_all(&mut decoded));
            assert_eq!(a.len(), frames * 2, "{what}: length");
            assert!(b.len() >= a.len(), "{what}: the decoder's length");
            if let Some(i) = (0..a.len()).find(|&i| a[i].to_bits() != b[i].to_bits()) {
                panic!("{what}: sample {i} is {} mapped and {} decoded", a[i], b[i]);
            }
            // A seek lands on the frame asked for (the decoder starts at the packet before it,
            // which the stream handles), with the same samples after it.
            assert_eq!(mapped.seek(7_777).unwrap(), 7_777);
            let from = decoded.seek(7_777).unwrap();
            let (mut x, mut y) = (Vec::new(), Vec::new());
            mapped.next_stereo(&mut x).unwrap();
            decoded.next_stereo(&mut y).unwrap();
            let skip = (7_777 - from) as usize * 2;
            let n = x.len().min(y.len() - skip).min(2_000);
            assert_eq!(x[..n], y[skip..skip + n], "{what}: after a seek");
        }
    }

    /// `cargo test --release -p prowave-engine --lib mapped_reading_is_faster -- --ignored
    /// --nocapture`: how long ten minutes of 24-bit stereo takes to read both ways.
    #[test]
    #[ignore = "a timing, not a check"]
    fn mapped_reading_is_faster_than_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_pcm(
            dir.path(),
            "long.wav",
            Container::Wav,
            24,
            false,
            2,
            48_000,
            48_000 * 600,
        );
        for (name, mapped) in [("decoded", false), ("mapped", true)] {
            let mut best = Duration::MAX;
            for _ in 0..3 {
                let mut src = if mapped {
                    Source::open(&path).unwrap()
                } else {
                    Source::probe(&path).unwrap()
                };
                let began = Instant::now();
                let (mut buf, mut sum) = (Vec::new(), 0.0f64);
                while src.next_stereo(&mut buf).unwrap() {
                    sum += buf.iter().map(|x| *x as f64).sum::<f64>();
                    buf.clear();
                }
                best = best.min(began.elapsed());
                assert!(sum.is_finite());
            }
            println!("{name}: 10 minutes of 24-bit stereo in {best:?}");
            // And 2000 jumps about the file, reading a block after each (what a seek, a cycle
            // wrap or a reversed region costs).
            let mut src = if mapped {
                Source::open(&path).unwrap()
            } else {
                Source::probe(&path).unwrap()
            };
            let began = Instant::now();
            let mut buf = Vec::new();
            for i in 0..2000u64 {
                src.seek((i * 7_919_117) % (48_000 * 600)).unwrap();
                src.next_stereo(&mut buf).unwrap();
                buf.clear();
            }
            println!("{name}: 2000 seeks in {:?}", began.elapsed());
        }
    }

    #[test]
    fn a_clip_with_nothing_to_read_lets_go_of_its_file_and_takes_it_again_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_pcm(
            dir.path(),
            "idle.wav",
            Container::Wav,
            16,
            false,
            2,
            44_100,
            30_000,
        );
        let (producer, mut consumer) = RingBuffer::new(RING_BLOCKS);
        let src = Source::open(&path).unwrap();
        assert!(src.is_mapped());
        let mut stream = Stream {
            ratio: 1.0,
            out_len: 30_000,
            src,
            idle_since: None,
            producer,
            shared: Arc::new(ClipShared::default()),
            gen: None,
            out_frame: 0,
            buf: Vec::new(),
            buf_base: 0,
            eof: false,
            reverse: false,
        };
        // Fills its ring (8 blocks), then has nothing to do.
        assert!(matches!(stream.service(), Progress::Worked));
        assert!(matches!(stream.service(), Progress::Idle));
        assert!(stream.src.is_mapped(), "not for the first moments");
        // On Windows a mapped file cannot be overwritten (saving over it from an audio editor
        // fails): that is why it is let go of.
        #[cfg(windows)]
        assert!(File::create(&path).is_err());
        stream.idle_since = Some(Instant::now() - RELEASE_AFTER);
        assert!(matches!(stream.service(), Progress::Idle));
        assert!(stream.src.is_released());
        // Now another program can have the file: here it is replaced by a different one.
        drop(File::create(&path).unwrap());
        let other = write_pcm(
            dir.path(),
            "idle.wav",
            Container::Wav,
            16,
            false,
            2,
            44_100,
            30_000,
        );
        assert_eq!(other, path);
        // The player takes blocks, and the stream goes on where it was, now from the new file.
        let first = consumer.pop().unwrap();
        assert_eq!(first.start, 0);
        while consumer.pop().is_ok() {}
        assert!(matches!(stream.service(), Progress::Worked));
        assert!(stream.src.is_mapped(), "mapped again");
        let next = consumer.pop().unwrap();
        assert_eq!(next.start, 8 * BLOCK_FRAMES as u64);
        assert_eq!(stream.idle_since, None);
        // A clip that has played to its end also lets go.
        stream.seek_out(30_000);
        assert!(matches!(stream.service(), Progress::Idle));
        stream.idle_since = Some(Instant::now() - RELEASE_AFTER);
        stream.service();
        assert!(stream.src.is_released());
    }

    #[test]
    fn files_the_map_cannot_read_go_to_the_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for name in ["tone.mp3", "tone.caf"] {
            let src = Source::open(&fixtures.join(name)).unwrap();
            assert!(!src.is_mapped(), "{name} is compressed or not PCM it reads");
        }
        // A WAV whose data length was never filled in (an unfinished recording), a WAV with
        // 24-bit samples in a 32-bit container, and files that are not audio at all.
        let good = write_pcm(
            dir.path(),
            "a.wav",
            Container::Wav,
            16,
            false,
            2,
            44_100,
            2_000,
        );
        let mut bytes = std::fs::read(&good).unwrap();
        let data_at = bytes.windows(4).position(|w| w == b"data").unwrap();
        let mut unfinished = bytes.clone();
        unfinished[data_at + 4..data_at + 8].copy_from_slice(&0u32.to_le_bytes());
        let unfinished_path = dir.path().join("unfinished.wav");
        std::fs::write(&unfinished_path, &unfinished).unwrap();
        assert!(!Source::open(&unfinished_path).is_ok_and(|s| s.is_mapped()));
        // Padded container: block align 8 for two 24-bit channels.
        let fmt_at = bytes.windows(4).position(|w| w == b"fmt ").unwrap() + 8;
        bytes[fmt_at + 12..fmt_at + 14].copy_from_slice(&8u16.to_le_bytes());
        bytes[fmt_at + 14..fmt_at + 16].copy_from_slice(&24u16.to_le_bytes());
        let padded_path = dir.path().join("padded.wav");
        std::fs::write(&padded_path, &bytes).unwrap();
        assert!(!Source::open(&padded_path).is_ok_and(|s| s.is_mapped()));
        let junk = dir.path().join("junk.wav");
        std::fs::write(&junk, vec![0x5au8; 4096]).unwrap();
        assert!(Source::open(&junk).is_err());
        let short = dir.path().join("short.wav");
        std::fs::write(&short, b"RIFF").unwrap();
        assert!(Source::open(&short).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_mapped_file_that_gets_shorter_is_read_by_the_decoder_instead() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_pcm(
            dir.path(),
            "s.wav",
            Container::Wav,
            16,
            false,
            2,
            44_100,
            20_000,
        );
        let mut src = Source::open(&path).unwrap();
        assert!(src.is_mapped());
        let mut head = Vec::new();
        src.next_stereo(&mut head).unwrap();
        // Someone cuts the file to half. The map is not read again.
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len / 2)
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        src.revalidate();
        assert!(!src.is_mapped(), "now the decoder");
        // It carries on from where the map stopped, with the samples the file still has.
        let mut rest = Vec::new();
        src.next_stereo(&mut rest).unwrap();
        let reference = write_pcm(
            dir.path(),
            "r.wav",
            Container::Wav,
            16,
            false,
            2,
            44_100,
            20_000,
        );
        let mut whole = Source::open(&reference).unwrap();
        let all = read_all(&mut whole);
        assert_eq!(rest[..], all[head.len()..head.len() + rest.len()]);
    }

    #[test]
    fn opens_and_streams_mp3_and_caf() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        // 0.5 s of 44.1 kHz mono MP3 (the encoder pads a little) and 0.2 s of 22.05 kHz CAF.
        let mp3 = AudioFile::open(dir.join("tone.mp3")).unwrap();
        assert_eq!((mp3.sample_rate, mp3.channels), (44_100, 1));
        assert!(
            (mp3.duration_secs() - 0.5).abs() < 0.08,
            "{}",
            mp3.duration_secs()
        );
        let caf = AudioFile::open(dir.join("tone.caf")).unwrap();
        assert_eq!(
            (caf.sample_rate, caf.channels, caf.frames),
            (22_050, 1, 4410)
        );
        for f in [&mp3, &caf] {
            let peak = f.peak().unwrap();
            assert!(
                peak > 0.1 && peak <= 1.0,
                "{} peak {peak}",
                f.path.display()
            );
            let bins = f.peaks(8).unwrap();
            assert!(bins.iter().any(|b| b.1 > 0.08), "waveform has a signal");
        }
    }
}
