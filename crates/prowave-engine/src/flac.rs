//! FLAC writing: a lossless, 24-bit stereo FLAC encoder in plain Rust (Logic Pro: Bounce with
//! the FLAC file type).
//!
//! Each channel of a 4096-frame block is stored as the cheapest of three forms: a constant
//! (silence), a fixed polynomial predictor of order 0 to 4 with Rice-coded residuals, or the
//! samples as they are. The file is written next to its final name first and renamed, so a
//! reader never sees half a file. Not for the audio thread: it allocates and does file I/O.

use std::io::Write;
use std::path::Path;

/// Frames per block.
const BLOCK: usize = 4096;
/// The largest Rice parameter of a 4-bit partition.
const MAX_RICE: u32 = 14;
const BITS: u32 = 24;

/// Collects bits, most significant first.
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    /// Writes the low `bits` bits (at most 32) of `value`.
    fn put(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (value & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.bytes.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }

    /// `zeros` zero bits and then a one.
    fn unary(&mut self, mut zeros: u64) {
        while zeros >= 32 {
            self.put(0, 32);
            zeros -= 32;
        }
        self.put(1, zeros as u32 + 1);
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The "UTF-8" form FLAC uses for a frame number.
fn utf8_number(n: u64) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let mut extra = 1;
    while extra < 6 && n >= 1u64 << (5 * extra + 6) {
        extra += 1;
    }
    let lead = !(0xFFu8 >> (extra + 1));
    let mut out = vec![lead | (n >> (6 * extra)) as u8];
    for i in (0..extra).rev() {
        out.push(0x80 | ((n >> (6 * i)) & 0x3F) as u8);
    }
    out
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// The residual of the fixed predictor of `order` (0 to 4) over `x`, for the samples after the
/// first `order`.
fn residual(x: &[i32], order: usize) -> Vec<i64> {
    (order..x.len())
        .map(|i| {
            let s = |k: usize| i64::from(x[i - k]);
            match order {
                0 => s(0),
                1 => s(0) - s(1),
                2 => s(0) - 2 * s(1) + s(2),
                3 => s(0) - 3 * s(1) + 3 * s(2) - s(3),
                _ => s(0) - 4 * s(1) + 6 * s(2) - 4 * s(3) + s(4),
            }
        })
        .collect()
}

/// The Rice parameter with the fewest bits for `res`, and that many bits.
fn best_rice(res: &[i64]) -> (u32, u64) {
    let mut best = (0, u64::MAX);
    for k in 0..=MAX_RICE {
        let bits: u64 = res
            .iter()
            .map(|&r| (zigzag(r) >> k) + 1 + u64::from(k))
            .sum();
        if bits < best.1 {
            best = (k, bits);
        }
    }
    best
}

/// Writes one channel's subframe for `x` (24-bit samples).
fn subframe(w: &mut BitWriter, x: &[i32]) {
    let first = x.first().copied().unwrap_or(0);
    if x.iter().all(|&v| v == first) {
        w.put(0, 8); // zero pad, CONSTANT, no wasted bits
        w.put(first as u32 as u64, BITS);
        return;
    }
    let verbatim_bits = x.len() as u64 * u64::from(BITS);
    let mut best: Option<(usize, u32, u64, Vec<i64>)> = None;
    for order in 0..=4usize.min(x.len().saturating_sub(1)) {
        let res = residual(x, order);
        let (k, bits) = best_rice(&res);
        let total = bits + order as u64 * u64::from(BITS);
        if best.as_ref().is_none_or(|b| total < b.2) {
            best = Some((order, k, total, res));
        }
    }
    match best {
        Some((order, k, total, res)) if total + 10 < verbatim_bits => {
            w.put(0, 1);
            w.put(0b001000 | order as u64, 6);
            w.put(0, 1);
            for &v in &x[..order] {
                w.put(v as u32 as u64, BITS);
            }
            w.put(0, 2); // 4-bit Rice parameters
            w.put(0, 4); // one partition
            w.put(u64::from(k), 4);
            for r in res {
                let u = zigzag(r);
                w.unary(u >> k);
                w.put(u & ((1u64 << k) - 1), k);
            }
        }
        _ => {
            w.put(0b0000_0010, 8); // zero pad, VERBATIM, no wasted bits
            for &v in x {
                w.put(v as u32 as u64, BITS);
            }
        }
    }
}

/// One frame holding the channels `left` and `right` as block number `number`.
fn frame(number: u64, left: &[i32], right: &[i32]) -> Vec<u8> {
    let n = left.len();
    let mut head = vec![0xFF, 0xF8];
    let size_code = if n == BLOCK { 12 } else { 7 };
    head.push(size_code << 4); // sample rate: from STREAMINFO
    head.push(0b0001_1100); // two independent channels, 24 bits
    head.extend(utf8_number(number));
    if size_code == 7 {
        head.extend(((n - 1) as u16).to_be_bytes());
    }
    head.push(crc8(&head));
    let mut w = BitWriter::new();
    subframe(&mut w, left);
    subframe(&mut w, right);
    w.align();
    let mut out = head;
    out.extend(w.bytes);
    let crc = crc16(&out);
    out.extend(crc.to_be_bytes());
    out
}

/// The bytes of a 24-bit stereo FLAC file for the interleaved `stereo` samples.
pub fn encode_flac(stereo: &[f32], sample_rate: u32) -> Vec<u8> {
    let frames = stereo.len() / 2;
    let to_int = |s: f32| -> i32 {
        let v = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        (v * 8_388_607.0).round() as i32
    };
    let left: Vec<i32> = stereo.iter().step_by(2).map(|&s| to_int(s)).collect();
    let right: Vec<i32> = stereo
        .iter()
        .skip(1)
        .step_by(2)
        .map(|&s| to_int(s))
        .collect();
    let mut frames_out = Vec::new();
    let (mut min_size, mut max_size) = (u32::MAX, 0u32);
    for (i, start) in (0..frames).step_by(BLOCK).enumerate() {
        let end = (start + BLOCK).min(frames);
        let f = frame(i as u64, &left[start..end], &right[start..end]);
        min_size = min_size.min(f.len() as u32);
        max_size = max_size.max(f.len() as u32);
        frames_out.extend(f);
    }
    if max_size == 0 {
        min_size = 0;
    }
    // STREAMINFO (the only metadata block); the MD5 is left at zero, which means "not computed".
    let mut info = BitWriter::new();
    info.put(BLOCK as u64, 16);
    info.put(BLOCK as u64, 16);
    info.put(u64::from(min_size), 24);
    info.put(u64::from(max_size), 24);
    info.put(u64::from(sample_rate), 20);
    info.put(1, 3); // two channels
    info.put(u64::from(BITS - 1), 5);
    info.put(frames as u64 >> 32, 4);
    info.put(frames as u64 & 0xFFFF_FFFF, 32);
    let mut out = b"fLaC".to_vec();
    out.extend([0x80, 0, 0, 34]);
    out.extend(info.bytes);
    out.extend([0u8; 16]);
    out.extend(frames_out);
    out
}

/// Writes interleaved stereo as a 24-bit FLAC file (the counterpart of `write_wav`).
pub fn write_flac(path: &Path, stereo: &[f32], sample_rate: u32) -> std::io::Result<()> {
    if !(1..=655_350).contains(&sample_rate) {
        return Err(std::io::Error::other("sample rate out of range for FLAC"));
    }
    let bytes = encode_flac(stereo, sample_rate);
    let tmp = path.with_extension("flac.part");
    std::fs::File::create(&tmp)?.write_all(&bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    /// Decodes a FLAC file back into interleaved f32 and its sample rate.
    fn decode(bytes: Vec<u8>) -> (Vec<f32>, u32) {
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                Hint::new().with_extension("flac"),
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .unwrap();
        let mut format = probed.format;
        let track = format.default_track().unwrap().clone();
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .unwrap();
        let rate = track.codec_params.sample_rate.unwrap();
        let mut out = Vec::new();
        while let Ok(packet) = format.next_packet() {
            let decoded = decoder.decode(&packet).unwrap();
            let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
            buf.copy_interleaved_ref(decoded);
            out.extend_from_slice(buf.samples());
        }
        (out, rate)
    }

    fn tone(frames: usize) -> Vec<f32> {
        let mut v = Vec::new();
        let mut seed = 12345u32;
        for i in 0..frames {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 16) as f32 / 65_536.0 - 0.5;
            let s = (i as f32 * 0.05).sin() * 0.6 + noise * 0.01;
            v.push(s);
            v.push(-s * 0.5);
        }
        v
    }

    #[test]
    fn what_is_written_decodes_back_to_the_same_24_bit_samples() {
        // A short last block and more than one block.
        let stereo = tone(BLOCK * 2 + 777);
        let (back, rate) = decode(encode_flac(&stereo, 44_100));
        assert_eq!(rate, 44_100);
        assert_eq!(back.len(), stereo.len());
        for (a, b) in stereo.iter().zip(&back) {
            assert!((a - b).abs() < 2e-7, "{a} vs {b}");
        }
    }

    #[test]
    fn silence_is_tiny_and_full_scale_and_loud_noise_survive() {
        let silence = vec![0.0f32; BLOCK * 4 * 2];
        let bytes = encode_flac(&silence, 48_000);
        assert!(bytes.len() < 200, "{} bytes", bytes.len());
        assert!(decode(bytes).0.iter().all(|&s| s == 0.0));
        // Loud random samples force the verbatim form; the extremes and bad numbers are clamped.
        let mut seed = 7u32;
        let mut loud: Vec<f32> = (0..BLOCK * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect();
        loud[0] = 3.0;
        loud[1] = f32::NAN;
        let (back, _) = decode(encode_flac(&loud, 96_000));
        assert_eq!(back.len(), loud.len());
        assert!((back[0] - 1.0).abs() < 1e-6);
        assert_eq!(back[1], 0.0);
        for (a, b) in loud.iter().zip(&back).skip(2) {
            assert!((a - b).abs() < 2e-7);
        }
    }

    #[test]
    fn an_empty_song_is_a_valid_file_and_a_tone_is_smaller_than_the_pcm() {
        // No frames: just the marker and the stream info, which says there are no samples.
        let empty = encode_flac(&[], 44_100);
        assert_eq!(empty.len(), 4 + 4 + 34);
        assert_eq!(&empty[..4], b"fLaC");
        assert_eq!(&empty[18..22], &[0x0A, 0xC4, 0x43, 0x70]); // 44100 Hz, 2 channels, 24 bits
        assert!(empty[22..26].iter().all(|&b| b == 0), "no samples");
        let stereo = tone(BLOCK * 8);
        assert!(encode_flac(&stereo, 44_100).len() < stereo.len() * 3 * 8 / 10);
    }

    #[test]
    fn frame_numbers_use_the_utf8_form() {
        assert_eq!(utf8_number(5), vec![5]);
        assert_eq!(utf8_number(0x80), vec![0xC2, 0x80]);
        assert_eq!(utf8_number(0x800), vec![0xE0, 0xA0, 0x80]);
    }

    #[test]
    fn the_file_is_written_whole_and_a_bad_rate_is_refused() {
        let dir = std::env::temp_dir().join(format!("prowave-flac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.flac");
        write_flac(&path, &tone(100), 44_100).unwrap();
        assert!(path.exists() && !dir.join("x.flac.part").exists());
        assert!(write_flac(&path, &[], 0).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
