//! Impulse-response files for Space Designer: reading a WAV, AIFF, FLAC, MP3, CAF or OGG file
//! into stereo frames, and resampling them to the project's rate.
//!
//! Logic Pro equivalent: Space Designer's "Load Impulse Response…". A file with one channel
//! (mono) is heard on both sides; two channels (stereo) are used as they are; four channels
//! ("true stereo": left-to-left, left-to-right, right-to-left, right-to-right) use the
//! left-to-left and right-to-right responses, since the convolver runs each side through its own
//! response; with any other number of channels the first two are used. Only the first
//! [`MAX_IR_SECONDS`] seconds are read, and files over [`MAX_FILE_BYTES`] are refused, so a
//! wrong file cannot use unbounded memory.
//!
//! Reading and resampling allocate and take a moment: call them off the audio thread (the app
//! does it on a background thread).

use std::f32::consts::PI;
use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::convolver::MAX_IR_SECONDS;

/// The largest file accepted, in bytes.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// The sample rates accepted, in Hz.
const RATES: std::ops::RangeInclusive<u32> = 8_000..=384_000;
/// The most channels a file may have.
const MAX_CHANNELS: usize = 32;

/// An impulse response read from a file, as stereo frames at the file's own rate.
#[derive(Clone, Debug, PartialEq)]
pub struct IrFile {
    pub frames: Vec<[f32; 2]>,
    pub sample_rate: u32,
    /// The number of channels the file had.
    pub channels: usize,
}

impl IrFile {
    /// The length of the response in seconds.
    pub fn seconds(&self) -> f32 {
        self.frames.len() as f32 / self.sample_rate.max(1) as f32
    }

    /// The response at `sample_rate` Hz.
    pub fn at_rate(&self, sample_rate: u32) -> Vec<[f32; 2]> {
        resample(&self.frames, self.sample_rate, sample_rate)
    }
}

/// Reads the impulse response in the file at `path`. Errors (no such file, not audio, too large,
/// a rate or channel count out of range, no audio in it) are returned as text.
pub fn read_ir_file(path: &Path) -> Result<IrFile, String> {
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
    let max_frames = (MAX_IR_SECONDS * sample_rate as f32) as usize;
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
            // A damaged packet is skipped; the rest of the response is still usable.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let spec = *decoded.spec();
        let n = spec.channels.count().max(1);
        let buf = sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        buf.copy_interleaved_ref(decoded);
        for f in buf.samples().chunks_exact(n) {
            let s = |i: usize| f.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
            frames.push(match n {
                1 => [s(0), s(0)],
                // True stereo: left to left, left to right, right to left, right to right.
                4 => [s(0), s(3)],
                _ => [s(0), s(1)],
            });
            if frames.len() >= max_frames {
                break;
            }
        }
    }
    if frames.is_empty() || frames.iter().all(|f| f[0] == 0.0 && f[1] == 0.0) {
        return Err(format!("{}: the file has no sound", path.display()));
    }
    Ok(IrFile {
        frames,
        sample_rate,
        channels,
    })
}

/// `frames` (at `from` Hz) at `to` Hz, with a windowed-sinc filter (a low pass at the lower of
/// the two Nyquist frequencies, so going down does not alias).
pub fn resample(frames: &[[f32; 2]], from: u32, to: u32) -> Vec<[f32; 2]> {
    let (from, to) = (from.max(1), to.max(1));
    if from == to || frames.is_empty() {
        return frames.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let out_len = ((frames.len() as f64) * ratio).round().max(1.0) as usize;
    // The filter's cut-off, as a fraction of the input's Nyquist frequency, and its half width
    // in input samples (wider when it cuts lower).
    let cutoff = ratio.min(1.0) as f32 * 0.97;
    let half = (24.0 / cutoff).ceil() as i64;
    let mut out = Vec::with_capacity(out_len);
    let last = frames.len() as i64 - 1;
    for j in 0..out_len {
        let t = j as f64 / ratio;
        let center = t.floor() as i64;
        let (mut acc, mut sum) = ([0f32; 2], 0f32);
        for i in (center - half + 1)..=(center + half) {
            let x = (i as f64 - t) as f32;
            let w = {
                let u = x / half as f32;
                if u.abs() >= 1.0 {
                    continue;
                }
                let hann = 0.5 * (1.0 + (PI * u).cos());
                let px = PI * cutoff * x;
                let sinc = if px.abs() < 1e-6 { 1.0 } else { px.sin() / px };
                cutoff * sinc * hann
            };
            let f = frames[i.clamp(0, last) as usize];
            acc[0] += f[0] * w;
            acc[1] += f[1] * w;
            sum += w;
        }
        let norm = if sum.abs() > 1e-9 { 1.0 / sum } else { 1.0 };
        out.push([acc[0] * norm, acc[1] * norm]);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A WAV with `channels` channels of 32-bit float samples: `frames[n]` is frame n.
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

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).expect("write");
        p
    }

    #[test]
    fn a_mono_file_is_heard_on_both_sides_and_a_stereo_file_keeps_its_sides() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mono = wav(48_000, 1, &[vec![1.0], vec![0.5], vec![0.25]]);
        let ir = read_ir_file(&file(dir.path(), "mono.wav", &mono)).expect("mono");
        assert_eq!(ir.channels, 1);
        assert_eq!(ir.sample_rate, 48_000);
        assert_eq!(ir.frames, vec![[1.0, 1.0], [0.5, 0.5], [0.25, 0.25]]);
        let stereo = wav(44_100, 2, &[vec![1.0, -1.0], vec![0.5, 0.25]]);
        let ir = read_ir_file(&file(dir.path(), "stereo.wav", &stereo)).expect("stereo");
        assert_eq!(ir.frames, vec![[1.0, -1.0], [0.5, 0.25]]);
        assert!((ir.seconds() - 2.0 / 44_100.0).abs() < 1e-9);
    }

    #[test]
    fn a_true_stereo_file_uses_left_to_left_and_right_to_right() {
        let dir = tempfile::tempdir().expect("temp dir");
        // LL, LR, RL, RR.
        let four = wav(
            48_000,
            4,
            &[vec![1.0, 0.1, 0.2, -1.0], vec![0.5, 0.0, 0.0, -0.5]],
        );
        let ir = read_ir_file(&file(dir.path(), "true.wav", &four)).expect("true stereo");
        assert_eq!(ir.channels, 4);
        assert_eq!(ir.frames, vec![[1.0, -1.0], [0.5, -0.5]]);
        // Three or six channels: the first two.
        let six = wav(48_000, 6, &[vec![0.3, 0.6, 9.0, 9.0, 9.0, 9.0]]);
        let ir = read_ir_file(&file(dir.path(), "six.wav", &six)).expect("six");
        assert_eq!(ir.frames, vec![[0.3, 0.6]]);
    }

    #[test]
    fn only_the_first_twelve_seconds_are_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let frames: Vec<Vec<f32>> = (0..8_000 * 14)
            .map(|i| vec![((i % 7) as f32) * 0.1 + 0.1])
            .collect();
        let ir =
            read_ir_file(&file(dir.path(), "long.wav", &wav(8_000, 1, &frames))).expect("long");
        assert_eq!(ir.frames.len(), (MAX_IR_SECONDS * 8_000.0) as usize);
    }

    #[test]
    fn bad_files_are_errors_not_panics() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(read_ir_file(&dir.path().join("missing.wav")).is_err());
        assert!(read_ir_file(&file(dir.path(), "junk.wav", b"not audio at all")).is_err());
        assert!(read_ir_file(&file(dir.path(), "empty.wav", b"")).is_err());
        // Silence is not an impulse response.
        let silent = wav(48_000, 1, &vec![vec![0.0]; 100]);
        assert!(read_ir_file(&file(dir.path(), "silent.wav", &silent)).is_err());
        // A header that lies about its length: what is there is read, or it is an error.
        let mut cut = wav(48_000, 2, &vec![vec![0.5, 0.5]; 1000]);
        cut.truncate(300);
        let _ = read_ir_file(&file(dir.path(), "cut.wav", &cut));
        // A rate outside the range.
        let odd = wav(1_000, 1, &vec![vec![1.0]; 10]);
        assert!(read_ir_file(&file(dir.path(), "slow.wav", &odd)).is_err());
        // A folder is not a file.
        assert!(read_ir_file(dir.path()).is_err());
    }

    #[test]
    fn resampling_keeps_a_tone_s_pitch_and_level_and_removes_what_would_alias() {
        let tone = |hz: f32, rate: u32, n: usize| -> Vec<[f32; 2]> {
            (0..n)
                .map(|i| {
                    let v = (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin();
                    [v, v]
                })
                .collect()
        };
        let rms =
            |x: &[[f32; 2]]| (x.iter().map(|f| f[0] * f[0]).sum::<f32>() / x.len() as f32).sqrt();
        // 1 kHz, 48 kHz to 44.1 kHz: same pitch (same number of cycles per second) and level.
        let down = resample(&tone(1000.0, 48_000, 48_000), 48_000, 44_100);
        assert_eq!(down.len(), 44_100);
        let crossings = down
            .windows(2)
            .filter(|w| w[0][0] <= 0.0 && w[1][0] > 0.0)
            .count();
        assert!((crossings as i32 - 1000).abs() <= 2, "{crossings}");
        assert!((rms(&down[2000..42_000]) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);
        // 30 kHz at 96 kHz down to 48 kHz would alias to 18 kHz: it is filtered out.
        let high = resample(&tone(30_000.0, 96_000, 96_000), 96_000, 48_000);
        assert!(
            rms(&high[2000..46_000]) < 0.05,
            "{}",
            rms(&high[2000..46_000])
        );
        // Up: twice the frames and the same level.
        let up = resample(&tone(500.0, 22_050, 22_050), 22_050, 44_100);
        assert_eq!(up.len(), 44_100);
        assert!((rms(&up[2000..42_000]) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);
        // The same rate is the same frames; nothing is nothing.
        assert_eq!(resample(&[[1.0, 0.5]], 48_000, 48_000), vec![[1.0, 0.5]]);
        assert!(resample(&[], 48_000, 44_100).is_empty());
    }

    #[test]
    fn an_impulse_stays_an_impulse_when_resampled() {
        let mut ir = vec![[0.0f32; 2]; 4_800];
        ir[100] = [1.0, 1.0];
        let out = resample(&ir, 48_000, 96_000);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1[0].abs().total_cmp(&b.1[0].abs()))
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(peak, 200);
    }
}
