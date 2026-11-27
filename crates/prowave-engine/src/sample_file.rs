//! Sample files for the Sampler: reading a WAV, AIFF, FLAC, MP3, CAF, OGG or M4A file into
//! stereo frames at the file's own rate (a mono file is heard on both sides; with more than two
//! channels the first two are used).
//!
//! Instruments are untrusted input, so the reader refuses files over [`MAX_FILE_BYTES`] and
//! stops after [`MAX_SAMPLE_SECONDS`]. Reading allocates and takes a moment: call it off the
//! audio thread (the app does it on a background thread).

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// The largest sample file accepted, in bytes.
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// The most seconds read from one sample.
pub const MAX_SAMPLE_SECONDS: f32 = 120.0;
/// The sample rates accepted, in Hz.
const RATES: std::ops::RangeInclusive<u32> = 8_000..=384_000;
const MAX_CHANNELS: usize = 32;

/// A sample as stereo frames at its own sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleData {
    pub frames: Vec<[f32; 2]>,
    pub sample_rate: u32,
}

impl SampleData {
    /// A sample made in code (tests, generators).
    pub fn new(frames: Vec<[f32; 2]>, sample_rate: u32) -> Self {
        Self {
            frames,
            sample_rate: sample_rate.max(1),
        }
    }

    /// The length in seconds.
    pub fn seconds(&self) -> f32 {
        self.frames.len() as f32 / self.sample_rate.max(1) as f32
    }
}

/// Reads the audio file at `path`. Errors (no such file, not audio, too large, a rate or channel
/// count out of range, no sound in it) are returned as text.
pub fn read_sample(path: &Path) -> Result<SampleData, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{}: larger than {} MB",
            path.display(),
            MAX_FILE_BYTES >> 20
        ));
    }
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| format!("{}: not an audio file", path.display()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| format!("{}: no audio in the file", path.display()))?;
    let params = track.codec_params.clone();
    let track_id = track.id;
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| format!("{}: unknown sample rate", path.display()))?;
    let channels = params.channels.map_or(0, |c| c.count());
    if !RATES.contains(&sample_rate) || !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(format!(
            "{}: unsupported format ({sample_rate} Hz, {channels} channels)",
            path.display()
        ));
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let max_frames = (MAX_SAMPLE_SECONDS * sample_rate as f32) as usize;
    let mut frames: Vec<[f32; 2]> = Vec::new();
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    while frames.len() < max_frames {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(_)) | Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet is skipped; the rest of the sample is still usable.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let spec = *decoded.spec();
        let n = spec.channels.count().max(1);
        let buf = sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        buf.copy_interleaved_ref(decoded);
        for f in buf.samples().chunks_exact(n) {
            let s = |i: usize| f.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
            frames.push(if n == 1 { [s(0), s(0)] } else { [s(0), s(1)] });
            if frames.len() >= max_frames {
                break;
            }
        }
    }
    if frames.is_empty() {
        return Err(format!("{}: the file has no sound", path.display()));
    }
    Ok(SampleData {
        frames,
        sample_rate,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, frames: &[Vec<f32>]) -> Vec<u8> {
        let block = u32::from(channels) * 4;
        let data = frames.len() as u32 * block;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * block).to_le_bytes());
        v.extend_from_slice(&(block as u16).to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        for f in frames {
            for s in f {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        v
    }

    #[test]
    fn mono_stereo_and_many_channel_files_become_stereo_frames() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: Vec<u8>| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let mono = read_sample(&write("m.wav", wav(44_100, 1, &[vec![0.5], vec![-0.5]]))).unwrap();
        assert_eq!(mono.frames, vec![[0.5, 0.5], [-0.5, -0.5]]);
        assert_eq!(mono.sample_rate, 44_100);
        let stereo = read_sample(&write("s.wav", wav(48_000, 2, &[vec![0.1, 0.2]]))).unwrap();
        assert_eq!(stereo.frames, vec![[0.1, 0.2]]);
        let six = read_sample(&write(
            "6.wav",
            wav(48_000, 6, &[vec![0.3, 0.4, 9.0, 9.0, 9.0, 9.0]]),
        ))
        .unwrap();
        assert_eq!(six.frames, vec![[0.3, 0.4]]);
        assert!((mono.seconds() - 2.0 / 44_100.0).abs() < 1e-9);
        // Silence is a sample (a drum kit may have a silent layer).
        assert!(read_sample(&write("z.wav", wav(48_000, 1, &vec![vec![0.0]; 10]))).is_ok());
    }

    #[test]
    fn long_files_are_cut_and_bad_files_are_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let long: Vec<Vec<f32>> = (0..8_000 * 130)
            .map(|i| vec![(i % 5) as f32 * 0.1])
            .collect();
        let p = dir.path().join("long.wav");
        std::fs::write(&p, wav(8_000, 1, &long)).unwrap();
        assert_eq!(
            read_sample(&p).unwrap().frames.len(),
            (MAX_SAMPLE_SECONDS * 8_000.0) as usize
        );
        assert!(read_sample(&dir.path().join("missing.wav")).is_err());
        let junk = dir.path().join("junk.wav");
        std::fs::write(&junk, b"not audio").unwrap();
        assert!(read_sample(&junk).is_err());
        let slow = dir.path().join("slow.wav");
        std::fs::write(&slow, wav(1_000, 1, &vec![vec![1.0]; 10])).unwrap();
        assert!(read_sample(&slow).is_err());
        assert!(read_sample(dir.path()).is_err());
    }
}
