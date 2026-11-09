//! DWOP, the adaptive predictor codec REX2 uses for its audio (`SDAT`).
//!
//! This is the decoder from the format notes (see `REX_FORMAT_SPEC.md`): five delta predictors
//! kept in a doubled representation, each with its own running "energy", and a scale factor
//! chosen from the cheapest predictor. Residuals go through a unary part and a small range
//! coder. The bit reader is MSB-first and shared by the channels; each channel keeps its own
//! predictor state (and its own range state).
//!
//! Stereo is stored as left, then right-minus-left; the decoder adds the difference back.
//!
//! The same module holds the matching encoder, which the writer uses to build files. It is the
//! exact inverse of the decoder: it picks the same predictor from the same energies and codes
//! the residual the decoder will read back. Round-trip tests are how both halves are checked.

/// The energy each predictor starts with (`0xA00`).
const INITIAL_ENERGY: u64 = 2560;
/// The range value a channel starts with.
const INITIAL_RANGE: u64 = 2;
/// Values at or above this are treated as a damaged stream (they cannot come from a real file
/// and would make the range arithmetic overflow).
const MAX_VALUE: u64 = 1 << 62;
/// The energy index to predictor order mapping.
const PREDICTOR_ORDER: [usize; 5] = [0, 1, 4, 2, 3];

/// A big-endian bit reader. Reading past the end yields zero bits and sets [`Self::overrun`],
/// which stops the decoder instead of letting a damaged file run forever.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
    overrun: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit: 0,
            overrun: false,
        }
    }

    /// True once any read went past the last byte.
    pub fn overrun(&self) -> bool {
        self.overrun
    }

    /// The next bit, most significant first.
    pub fn read_bit(&mut self) -> u64 {
        let byte = self.bit / 8;
        if byte >= self.data.len() {
            self.overrun = true;
            self.bit = self.bit.saturating_add(1);
            return 0;
        }
        let bit = (self.data[byte] >> (7 - (self.bit % 8))) & 1;
        self.bit += 1;
        u64::from(bit)
    }

    /// The next `n` bits as a number; `n <= 0` reads nothing, as in the reference decoder.
    pub fn read_bits(&mut self, n: i32) -> u64 {
        if n <= 0 {
            return 0;
        }
        let n = n.min(63);
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | self.read_bit();
        }
        v
    }
}

/// A big-endian bit writer, the inverse of [`BitReader`].
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    used: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            used: 0,
        }
    }

    pub fn write_bit(&mut self, bit: u64) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if bit & 1 == 1 {
            if let Some(last) = self.bytes.last_mut() {
                *last |= 1 << (7 - self.used);
            }
        }
        self.used = (self.used + 1) % 8;
    }

    /// Writes the low `n` bits of `value`, most significant first.
    pub fn write_bits(&mut self, value: u64, n: i32) {
        if n <= 0 {
            return;
        }
        for i in (0..n.min(63)).rev() {
            self.write_bit((value >> i) & 1);
        }
    }

    /// The bytes written; unused bits of the last byte stay zero.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// One channel's predictor and range state.
#[derive(Clone)]
pub(crate) struct State {
    s: [i32; 5],
    e: [u64; 5],
    rv: u64,
    ba: i32,
}

impl State {
    pub fn new() -> Self {
        Self {
            s: [0; 5],
            e: [INITIAL_ENERGY; 5],
            rv: INITIAL_RANGE,
            ba: 0,
        }
    }

    /// Decodes one sample and returns the doubled value (`S[0]`, before the final `>> 1`).
    ///
    /// `None` means the bitstream ended before `SINF` said it would.
    pub fn decode(&mut self, br: &mut BitReader<'_>) -> Option<i32> {
        if br.overrun() {
            return None;
        }
        let p = cheapest(&self.e);
        let step = (self.e[p].saturating_mul(3).saturating_add(36)) >> 7;
        let (acc, cs) = unary(br, step)?;
        let (nb, rv) = range_params(self.ba, self.rv, cs);
        self.ba = nb;
        self.rv = rv;
        let ext = br.read_bits(nb);
        if br.overrun() {
            return None;
        }
        let co = rv.saturating_sub(cs);
        let rem = if ext < co {
            ext
        } else {
            co.saturating_add((ext - co).saturating_mul(2))
                .saturating_add(br.read_bit())
        };
        if br.overrun() {
            return None;
        }
        let val = acc.saturating_add(rem);
        let d = zigzag_decode(val);
        self.update(p, d);
        Some(self.s[0])
    }

    /// Encodes one doubled sample (`target` = the value `S[0]` must become).
    pub fn encode(&mut self, target: i32, bw: &mut BitWriter) -> Result<(), String> {
        let p = cheapest(&self.e);
        let step = (self.e[p].saturating_mul(3).saturating_add(36)) >> 7;
        // The energy index is the prediction order: how many history values are summed.
        let mut predicted = 0i64;
        for i in 0..p {
            predicted += i64::from(self.s[i]);
        }
        let d = i64::from(target) - predicted;
        let d = i32::try_from(d).map_err(|_| "sample outside the predictor range".to_string())?;
        let val = zigzag_encode(d);

        let (acc, cs, zeros) = choose_unary(val, step, self.ba, self.rv)
            .ok_or_else(|| "no code for this residual".to_string())?;
        for _ in 0..zeros {
            bw.write_bit(0);
        }
        bw.write_bit(1);
        let (nb, rv) = range_params(self.ba, self.rv, cs);
        self.ba = nb;
        self.rv = rv;
        let ext = encode_remainder(val - acc, cs, rv, nb)
            .ok_or_else(|| "residual does not fit the range coder".to_string())?;
        bw.write_bits(ext.0, nb);
        if let Some(x) = ext.1 {
            bw.write_bit(x);
        }
        self.update(p, d);
        Ok(())
    }

    fn update(&mut self, p: usize, d: i32) {
        let old = self.s;
        // The energy index maps to a "switch case", which is what the update formulas use.
        let case = PREDICTOR_ORDER[p];
        match case {
            0 => {
                self.s[0] = d;
                self.s[1] = d.wrapping_sub(old[0]);
                self.s[2] = self.s[1].wrapping_sub(old[1]);
                self.s[3] = self.s[2].wrapping_sub(old[2]);
                self.s[4] = self.s[3].wrapping_sub(old[3]);
            }
            1 => {
                self.s[0] = old[0].wrapping_add(d);
                self.s[1] = d;
                self.s[2] = d.wrapping_sub(old[1]);
                self.s[3] = self.s[2].wrapping_sub(old[2]);
                self.s[4] = self.s[3].wrapping_sub(old[3]);
            }
            4 => {
                self.s[1] = old[1].wrapping_add(d);
                self.s[0] = old[0].wrapping_add(self.s[1]);
                self.s[2] = d;
                self.s[3] = d.wrapping_sub(old[2]);
                self.s[4] = self.s[3].wrapping_sub(old[3]);
            }
            2 => {
                self.s[2] = old[2].wrapping_add(d);
                self.s[1] = old[1].wrapping_add(self.s[2]);
                self.s[0] = old[0].wrapping_add(self.s[1]);
                self.s[3] = d;
                self.s[4] = d.wrapping_sub(old[3]);
            }
            _ => {
                // case 3
                self.s[3] = old[3].wrapping_add(d);
                self.s[2] = old[2].wrapping_add(self.s[3]);
                self.s[1] = old[1].wrapping_add(self.s[2]);
                self.s[0] = old[0].wrapping_add(self.s[1]);
                self.s[4] = d;
            }
        }
        for i in 0..5 {
            let a = self.s[i] ^ (self.s[i] >> 31);
            let a = u64::from(a.max(0) as u32);
            self.e[i] = self.e[i].saturating_add(a).saturating_sub(self.e[i] >> 5);
        }
    }
}

/// The index of the smallest energy; ties go to the lowest index.
fn cheapest(e: &[u64; 5]) -> usize {
    let mut p = 0;
    for i in 1..5 {
        if e[i] < e[p] {
            p = i;
        }
    }
    p
}

/// Reads the unary part of the code and returns `(accumulated value, scale)`.
fn unary(br: &mut BitReader<'_>, step: u64) -> Option<(u64, u64)> {
    let mut acc = 0u64;
    let mut cs = step.max(1);
    let mut qc = 7;
    loop {
        if br.overrun() {
            return None;
        }
        if br.read_bit() == 1 {
            return Some((acc, cs));
        }
        acc = acc.saturating_add(cs);
        qc -= 1;
        if qc == 0 {
            cs = cs.saturating_mul(4);
            qc = 7;
            if cs >= MAX_VALUE || acc >= MAX_VALUE {
                return None;
            }
        }
    }
}

/// The range-coder parameters the decoder derives from `cs`: `(extra bits, range)`.
fn range_params(ba: i32, rv0: u64, cs: u64) -> (i32, u64) {
    let mut nb = ba;
    let mut rv = rv0.max(INITIAL_RANGE);
    if cs >= rv {
        while cs >= rv {
            if rv >= MAX_VALUE {
                break;
            }
            rv = rv.saturating_mul(2);
            nb = nb.saturating_add(1);
        }
    } else {
        nb = nb.saturating_add(1);
        let mut t = rv;
        loop {
            rv = t;
            t /= 2;
            nb = nb.saturating_sub(1);
            if cs >= t {
                break;
            }
        }
    }
    (nb, rv)
}

fn zigzag_decode(val: u64) -> i32 {
    let v = i64::try_from(val).unwrap_or(i64::MAX);
    let d = if v & 1 == 0 { v } else { -v - 1 };
    d.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn zigzag_encode(d: i32) -> u64 {
    // The inverse of `zigzag_decode`: 0 -> 0, 2 -> 2, ..., -2 -> 1, -4 -> 3, ...
    let v = i64::from(d);
    if v >= 0 {
        v as u64
    } else {
        (-v - 1) as u64
    }
}

/// The range coder's maximum remainder for `(cs, rv, nb)`.
fn remainder_max(cs: u64, rv: u64, nb: i32) -> u64 {
    let co = rv.saturating_sub(cs);
    if nb <= 0 {
        co.saturating_sub(1)
    } else {
        let room = 1u64.checked_shl(nb as u32).unwrap_or(u64::MAX);
        if room <= co {
            co.saturating_sub(1)
        } else {
            room.saturating_mul(2).saturating_sub(co).saturating_sub(1)
        }
    }
}

/// Finds the smallest number of zero bits whose accumulated value leaves a codable remainder.
fn choose_unary(val: u64, step: u64, ba: i32, rv0: u64) -> Option<(u64, u64, usize)> {
    let mut acc = 0u64;
    let mut cs = step.max(1);
    let mut qc = 7;
    for zeros in 0..=1_000_000usize {
        if acc <= val {
            let (nb, rv) = range_params(ba, rv0, cs);
            if val - acc <= remainder_max(cs, rv, nb) {
                return Some((acc, cs, zeros));
            }
        } else {
            return None;
        }
        acc = acc.saturating_add(cs);
        qc -= 1;
        if qc == 0 {
            cs = cs.saturating_mul(4);
            qc = 7;
            if cs >= MAX_VALUE || acc >= MAX_VALUE {
                return None;
            }
        }
    }
    None
}

/// The `(bits, extra bit)` the range coder uses for `rem`, or `None` if it cannot be coded.
fn encode_remainder(rem: u64, cs: u64, rv: u64, nb: i32) -> Option<(u64, Option<u64>)> {
    let co = rv.saturating_sub(cs);
    if rem < co {
        if nb <= 0 {
            if rem == 0 {
                Some((0, None))
            } else {
                None
            }
        } else {
            let room = 1u64.checked_shl(nb as u32)?;
            if rem < room {
                Some((rem, None))
            } else {
                None
            }
        }
    } else {
        if nb <= 0 {
            return None;
        }
        let room = 1u64.checked_shl(nb as u32)?;
        let q = rem - co;
        let ext = co.checked_add(q >> 1)?;
        if ext >= room {
            return None;
        }
        Some((ext, Some(q & 1)))
    }
}

/// Decodes a whole `SDAT` stream.
///
/// Returns interleaved samples for `channels` (1 or 2) and whether the stream ran out early.
/// Never decodes more than `frames` frames per channel.
pub(crate) fn decode(
    data: &[u8],
    channels: usize,
    frames: u32,
    max_samples: usize,
) -> (Vec<i16>, bool) {
    let channels = channels.clamp(1, 2);
    let frames = (frames as usize).min(max_samples / channels) as u32;
    let mut out = Vec::with_capacity(frames as usize * channels);
    let mut br = BitReader::new(data);
    let mut left = State::new();
    let mut right = State::new();
    let mut ran_out = false;
    for _ in 0..frames {
        if br.overrun() {
            ran_out = true;
            break;
        }
        let Some(l) = left.decode(&mut br) else {
            ran_out = true;
            break;
        };
        if channels == 2 {
            let Some(r) = right.decode(&mut br) else {
                ran_out = true;
                break;
            };
            // Right is stored as right-minus-left.
            let l_out = l >> 1;
            let delta = r >> 1;
            out.push(clamp_i16(l_out));
            out.push(clamp_i16(l_out.saturating_add(delta)));
        } else {
            out.push(clamp_i16(l >> 1));
        }
    }
    if ran_out && out.len() < frames as usize * channels {
        // Keep the length the caller expects: the missing samples are silence.
        out.resize(frames as usize * channels, 0);
    }
    (out, ran_out)
}

/// Encodes interleaved `i16` samples to an `SDAT` bitstream.
pub(crate) fn encode(samples: &[i16], channels: usize) -> Result<Vec<u8>, String> {
    let channels = channels.clamp(1, 2);
    if !samples.len().is_multiple_of(channels) {
        return Err("sample count is not a whole number of frames".to_string());
    }
    let mut bw = BitWriter::new();
    let mut left = State::new();
    let mut right = State::new();
    for frame in samples.chunks_exact(channels) {
        left.encode(i32::from(frame[0]) * 2, &mut bw)?;
        if channels == 2 {
            // Right-minus-left, doubled.
            let delta = i32::from(frame[1]) - i32::from(frame[0]);
            right.encode(delta * 2, &mut bw)?;
        }
    }
    Ok(bw.finish())
}

fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MAX: usize = 1 << 20;

    #[test]
    fn round_trips_mono() {
        let samples: Vec<i16> = (0..5000)
            .map(|i| {
                let t = i as f32 * 0.05;
                ((t.sin() * 9000.0) + (t * 0.7).sin() * 3000.0) as i16
            })
            .collect();
        let bytes = encode(&samples, 1).unwrap();
        let (decoded, ran_out) = decode(&bytes, 1, samples.len() as u32, MAX);
        assert!(!ran_out, "the stream should be complete");
        assert_eq!(decoded, samples);
    }

    #[test]
    fn round_trips_stereo() {
        let mut samples = Vec::new();
        for i in 0..4000i32 {
            let a = (i as f32 * 0.03).sin() * 12000.0;
            let b = (i as f32 * 0.05).cos() * 7000.0;
            samples.push(a as i16);
            samples.push(b as i16);
        }
        let bytes = encode(&samples, 2).unwrap();
        let (decoded, ran_out) = decode(&bytes, 2, 4000, MAX);
        assert!(!ran_out);
        assert_eq!(decoded, samples);
    }

    #[test]
    fn round_trips_silence_and_full_scale() {
        for value in [0i16, 1, -1, i16::MAX, i16::MIN] {
            let samples = vec![value; 500];
            let bytes = encode(&samples, 1).unwrap();
            let (decoded, _) = decode(&bytes, 1, 500, MAX);
            assert_eq!(decoded, samples, "value {value}");
        }
    }

    #[test]
    fn a_truncated_stream_stops_instead_of_spinning() {
        let samples: Vec<i16> = (0..20000).map(|i| (i * 7919 % 3001) as i16).collect();
        let bytes = encode(&samples, 1).unwrap();
        let truncated = &bytes[..bytes.len() / 10];
        let (decoded, ran_out) = decode(truncated, 1, 20000, MAX);
        assert!(ran_out);
        assert_eq!(decoded.len(), 20000, "the caller still gets full length");
    }

    #[test]
    fn garbage_does_not_panic() {
        let bytes: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        for channels in 1..=2 {
            let (decoded, _) = decode(&bytes, channels, 1000, MAX);
            assert_eq!(decoded.len(), 1000 * channels);
        }
    }

    #[test]
    fn respects_the_sample_budget() {
        let samples = vec![0i16; 1000];
        let bytes = encode(&samples, 1).unwrap();
        let (decoded, _) = decode(&bytes, 1, 1000, 100);
        assert!(decoded.len() <= 100);
    }
}
