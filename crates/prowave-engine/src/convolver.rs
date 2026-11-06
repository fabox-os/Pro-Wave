//! Space Designer: the convolution reverb behind the "Space D" effect (Logic Pro: Space
//! Designer). Its sound is an impulse response (IR) the input is convolved with.
//!
//! The IR is made off the audio thread ([`ir_samples`]) from one of the built-in responses
//! ([`IR_NAMES`]) and the plug-in's settings: IR Offset, Size and Length (the response is
//! stretched), Reverse, the Volume Envelope and Filter Envelope, the cross-over with the
//! Lo and Hi Spread of the stereo width, and Quality. [`Convolver::new`] then cuts it into
//! partitions and keeps their spectra. The audio thread only calls [`Convolver::tick`], which
//! runs a uniformly partitioned FFT convolution one block at a time and never allocates; the
//! sound comes out [`BLOCK`] samples late (the effect reports that delay for compensation).

use std::f32::consts::TAU;

/// Convolution block: the delay the effect adds, in samples.
pub const BLOCK: usize = 512;
const NFFT: usize = BLOCK * 2;
const BINS: usize = BLOCK + 1;

/// The longest IR, in seconds (Length times Size is cut to this).
pub const MAX_IR_SECONDS: f32 = 12.0;

/// Positions of the Space Designer settings in an effect's parameter list.
pub mod sd {
    pub const PREDELAY: usize = 0;
    pub const LENGTH: usize = 1;
    pub const SIZE: usize = 2;
    pub const XOVER: usize = 3;
    pub const LO_SPREAD: usize = 4;
    pub const HI_SPREAD: usize = 5;
    pub const DRY: usize = 6;
    pub const WET: usize = 7;
    pub const INPUT: usize = 8;
    pub const INIT_LEVEL: usize = 9;
    pub const ATTACK: usize = 10;
    pub const DECAY: usize = 11;
    /// 0 = linear decay, 1 = exponential.
    pub const CURVE: usize = 12;
    pub const END_LEVEL: usize = 13;
    pub const IR_OFFSET: usize = 14;
    pub const REVERSE: usize = 15;
    /// 0 = Low, 1 = High.
    pub const QUALITY: usize = 16;
    pub const OUTPUT_EQ: usize = 17;
    pub const FILTER_ENV: usize = 18;
    pub const VOLUME_ENV: usize = 19;
    /// 0 = Sampled IR, 1 = Synthesized IR.
    pub const IR_TYPE: usize = 20;
    /// Which built-in response the Sampled IR tab plays.
    pub const IR_INDEX: usize = 21;
    /// How bright the Synthesized IR is, 0 to 1.
    pub const BRIGHTNESS: usize = 22;
}

/// The built-in impulse responses of the Sampled IR tab.
pub const IR_NAMES: [&str; 6] = [
    "03.0s Long Reverb",
    "01.5s Medium Hall",
    "00.8s Small Room",
    "06.0s Cathedral",
    "02.0s Bright Plate",
    "00.4s Tight Booth",
];

/// Length of each built-in IR in seconds, and how dark it gets (0 bright, 1 dark).
const IR_SHAPE: [(f32, f32); 6] = [
    (3.0, 0.45),
    (1.5, 0.4),
    (0.8, 0.3),
    (6.0, 0.7),
    (2.0, 0.1),
    (0.4, 0.5),
];

/// A radix-2 FFT of a fixed size.
#[derive(Clone, Debug)]
pub struct Fft {
    n: usize,
    rev: Vec<u32>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        let n = n.next_power_of_two().max(2);
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let cos = (0..n / 2)
            .map(|k| (TAU * k as f32 / n as f32).cos())
            .collect();
        let sin = (0..n / 2)
            .map(|k| (TAU * k as f32 / n as f32).sin())
            .collect();
        Self { n, rev, cos, sin }
    }

    /// In-place transform of `re` + i `im` (both `n` long). The inverse is scaled by 1/n.
    pub fn run(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        if re.len() < n || im.len() < n {
            return;
        }
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.cos[k * step];
                    let wi = if inverse {
                        self.sin[k * step]
                    } else {
                        -self.sin[k * step]
                    };
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += len;
            }
            len <<= 1;
        }
        if inverse {
            let s = 1.0 / n as f32;
            re[..n].iter_mut().for_each(|x| *x *= s);
            im[..n].iter_mut().for_each(|x| *x *= s);
        }
    }
}

/// Deterministic noise (xorshift), so a response sounds the same every time.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// A built-in response: noise with an exponential decay that gets darker, a few early
/// reflections, different on each side.
fn base_ir(seconds: f32, dark: f32, sr: f32) -> Vec<[f32; 2]> {
    let n = ((seconds * sr) as usize).max(64);
    let mut out = vec![[0.0f32; 2]; n];
    for ch in 0..2 {
        let mut rng = Noise(0x9e37_79b9 ^ (ch as u32 + 1).wrapping_mul(0x85eb_ca6b));
        let mut lp = 0.0f32;
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f32 / sr;
            // -60 dB at `seconds`.
            let env = (-6.9 * t / seconds).exp();
            let attack = (t / 0.003).min(1.0);
            // The tail loses its highs as it decays.
            let cutoff = 16_000.0 * (1.0 - dark).max(0.05) * (-2.5 * t / seconds).exp() + 400.0;
            let k = 1.0 - (-TAU * cutoff / sr).exp();
            lp += (rng.next() - lp) * k;
            o[ch] = lp * env * attack;
        }
        // Early reflections in the first 90 ms.
        for _ in 0..14 {
            let at = ((rng.next().abs() * 0.09 + 0.004) * sr) as usize;
            let amp = rng.next() * 0.6;
            if let Some(o) = out.get_mut(at) {
                o[ch] += amp;
            }
        }
    }
    out
}

/// The Volume Envelope at `t` seconds into the response: it starts at the Init Level, rises to
/// full level over the Attack, then falls over the Decay to the End Level, along a straight or
/// an exponential curve, and stays there. 1.0 is full level.
pub fn volume_env(params: &[f32], t: f32) -> f32 {
    let init = params[sd::INIT_LEVEL] / 100.0;
    let end = params[sd::END_LEVEL] / 100.0;
    let attack = (params[sd::ATTACK] / 1000.0).max(0.0);
    let decay = params[sd::DECAY].max(1e-3);
    if t < attack {
        return init + (1.0 - init) * (t / attack.max(1e-6));
    }
    let x = ((t - attack) / decay).clamp(0.0, 1.0);
    if params[sd::CURVE] >= 0.5 {
        // Exponential: the same drop in dB per second.
        let floor = end.max(1e-3);
        floor.powf(x)
    } else {
        1.0 + (end - 1.0) * x
    }
}

/// The response the settings in `params` make, at `sr`: stereo frames, not yet normalised for
/// the convolver. Longer than [`MAX_IR_SECONDS`] is cut.
#[allow(clippy::manual_memcpy)]
pub fn ir_samples(params: &[f32], sr: f32) -> Vec<[f32; 2]> {
    // Settings that are not finite count as zero, so a damaged project cannot make a NaN IR.
    let clean: Vec<f32> = params
        .iter()
        .map(|x| if x.is_finite() { *x } else { 0.0 })
        .collect();
    let params = &clean[..];
    if params.len() <= sd::BRIGHTNESS {
        return vec![[0.0; 2]; 64];
    }
    let sr = sr.clamp(8_000.0, 384_000.0);
    let synth = params[sd::IR_TYPE] >= 0.5;
    let (secs, dark) = if synth {
        (
            params[sd::LENGTH].clamp(0.1, 10.0),
            1.0 - params[sd::BRIGHTNESS],
        )
    } else {
        let i = (params[sd::IR_INDEX].round().max(0.0) as usize).min(IR_NAMES.len() - 1);
        IR_SHAPE[i]
    };
    let mut ir = base_ir(secs, dark, sr);

    // IR Offset: the response starts later into itself.
    let skip = ((params[sd::IR_OFFSET].max(0.0) * sr) as usize).min(ir.len().saturating_sub(64));
    ir.drain(..skip);

    // Length and Size stretch the response in time.
    let size = (params[sd::SIZE] / 100.0).clamp(0.1, 10.0);
    let target = if synth {
        1.0
    } else {
        params[sd::LENGTH] / secs.max(0.1)
    };
    let factor = (target * size).max(0.01);
    let out_len = (((ir.len() as f32) * factor) as usize).clamp(64, (MAX_IR_SECONDS * sr) as usize);
    if out_len != ir.len() {
        let mut stretched = Vec::with_capacity(out_len);
        let last = ir.len() - 1;
        for i in 0..out_len {
            let x = i as f32 * (ir.len() as f32 / out_len as f32);
            let a = (x as usize).min(last);
            let b = (a + 1).min(last);
            let f = x - a as f32;
            stretched.push([
                ir[a][0] + (ir[b][0] - ir[a][0]) * f,
                ir[a][1] + (ir[b][1] - ir[a][1]) * f,
            ]);
        }
        ir = stretched;
    }

    if params[sd::REVERSE] >= 0.5 {
        ir.reverse();
    }

    if params[sd::VOLUME_ENV] >= 0.5 {
        for (i, f) in ir.iter_mut().enumerate() {
            let g = volume_env(params, i as f32 / sr);
            f[0] *= g;
            f[1] *= g;
        }
    }

    // Filter Envelope: the highs fade faster, from 18 kHz down to 1.5 kHz over the response.
    if params[sd::FILTER_ENV] >= 0.5 {
        let n = ir.len().max(1) as f32;
        let mut lp = [0.0f32; 2];
        for (i, f) in ir.iter_mut().enumerate() {
            let cutoff = 18_000.0 * (1500.0f32 / 18_000.0).powf(i as f32 / n);
            let k = 1.0 - (-TAU * cutoff / sr).exp();
            for ch in 0..2 {
                lp[ch] += (f[ch] - lp[ch]) * k;
                f[ch] = lp[ch];
            }
        }
    }

    // X-Over, Lo Spread and Hi Spread: the stereo width of the lows and of the highs.
    let (lo, hi) = (params[sd::LO_SPREAD] / 100.0, params[sd::HI_SPREAD] / 100.0);
    if (lo - 1.0).abs() > 1e-3 || (hi - 1.0).abs() > 1e-3 {
        let k = 1.0 - (-TAU * params[sd::XOVER].clamp(20.0, sr * 0.4) / sr).exp();
        let mut s1 = [0.0f32; 2];
        let mut s2 = [0.0f32; 2];
        for f in ir.iter_mut() {
            let mut low = [0.0f32; 2];
            for ch in 0..2 {
                s1[ch] += (f[ch] - s1[ch]) * k;
                s2[ch] += (s1[ch] - s2[ch]) * k;
                low[ch] = s2[ch];
            }
            let high = [f[0] - low[0], f[1] - low[1]];
            let width = |b: [f32; 2], w: f32| {
                let (m, s) = ((b[0] + b[1]) * 0.5, (b[0] - b[1]) * 0.5 * w);
                [m + s, m - s]
            };
            let (l, h) = (width(low, lo), width(high, hi));
            *f = [l[0] + h[0], l[1] + h[1]];
        }
    }

    // Quality Low: the highs go and the silent end is cut, which is also less to compute.
    if params[sd::QUALITY] < 0.5 {
        let k = 1.0 - (-TAU * 8_000.0 / sr).exp();
        let mut lp = [[0.0f32; 2]; 2];
        for f in ir.iter_mut() {
            for ch in 0..2 {
                lp[0][ch] += (f[ch] - lp[0][ch]) * k;
                lp[1][ch] += (lp[0][ch] - lp[1][ch]) * k;
                f[ch] = lp[1][ch];
            }
        }
        let peak = ir
            .iter()
            .fold(0.0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
        let floor = peak * 1e-3;
        if let Some(last) = ir.iter().rposition(|f| f[0].abs().max(f[1].abs()) > floor) {
            ir.truncate(last + 1);
        }
    }

    // A short fade at the end, so cutting the response does not click.
    let fade = ((0.01 * sr) as usize).min(ir.len() / 4).max(1);
    let n = ir.len();
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        let f = &mut ir[n - 1 - i];
        f[0] *= g;
        f[1] *= g;
    }
    // No DC.
    for ch in 0..2 {
        let mean = ir.iter().map(|f| f[ch]).sum::<f32>() / ir.len() as f32;
        ir.iter_mut().for_each(|f| f[ch] -= mean);
    }
    // The same loudness whatever the shape: the power gain of the response is 1.
    let power = ir
        .iter()
        .map(|f| f64::from(f[0] * f[0] + f[1] * f[1]) * 0.5)
        .sum::<f64>()
        .max(1e-12);
    let g = (1.0 / power.sqrt()) as f32;
    ir.iter_mut().for_each(|f| {
        f[0] *= g;
        f[1] *= g;
    });
    ir
}

/// Min and max of the response in `bins` stretches, over its length in seconds, for drawing.
pub fn ir_preview(params: &[f32], sr: f32, bins: usize) -> (Vec<(f32, f32)>, f32) {
    let ir = ir_samples(params, sr);
    let bins = bins.max(1);
    let secs = ir.len() as f32 / sr.max(1.0);
    let peak = ir
        .iter()
        .fold(1e-9f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
    let out = (0..bins)
        .map(|b| {
            let a = b * ir.len() / bins;
            let z = ((b + 1) * ir.len() / bins).max(a + 1).min(ir.len());
            ir[a..z].iter().fold((0.0f32, 0.0f32), |(lo, hi), f| {
                let m = (f[0] + f[1]) * 0.5 / peak;
                (lo.min(m), hi.max(m))
            })
        })
        .collect();
    (out, secs)
}

/// A response cut into partitions and ready to run, with the memory it needs.
#[derive(Clone, Debug)]
pub struct Convolver {
    fft: Fft,
    parts: usize,
    /// Spectra of the partitions: `parts * BINS` bins for each of left and right.
    h_l: [Vec<f32>; 2],
    h_r: [Vec<f32>; 2],
    /// Spectra of the last `parts` input blocks (a ring), same layout.
    x_l: [Vec<f32>; 2],
    x_r: [Vec<f32>; 2],
    slot: usize,
    /// The last two input blocks of each side.
    win_l: Vec<f32>,
    win_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    at: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    acc_l: [Vec<f32>; 2],
    acc_r: [Vec<f32>; 2],
    /// Samples left of the fade-in of a new response.
    fade_left: u32,
}

/// Splits the spectrum of left + i right into the spectra of left and of right, bins 0 to
/// [`BLOCK`].
#[inline]
fn split(re: &[f32], im: &[f32], k: usize) -> ((f32, f32), (f32, f32)) {
    let m = (NFFT - k) % NFFT;
    let (zr, zi, cr, ci) = (re[k], im[k], re[m], -im[m]);
    (
        ((zr + cr) * 0.5, (zi + ci) * 0.5),
        ((zi - ci) * 0.5, -(zr - cr) * 0.5),
    )
}

impl Convolver {
    /// Prepares `ir` (stereo frames) for convolution. Allocates; run it off the audio thread.
    pub fn new(ir: &[[f32; 2]]) -> Self {
        let parts = ir.len().div_ceil(BLOCK).max(1);
        let fft = Fft::new(NFFT);
        let mut h_l = [vec![0.0; parts * BINS], vec![0.0; parts * BINS]];
        let mut h_r = [vec![0.0; parts * BINS], vec![0.0; parts * BINS]];
        let mut re = vec![0.0f32; NFFT];
        let mut im = vec![0.0f32; NFFT];
        for p in 0..parts {
            re.fill(0.0);
            im.fill(0.0);
            for i in 0..BLOCK {
                if let Some(f) = ir.get(p * BLOCK + i) {
                    re[i] = f[0];
                    im[i] = f[1];
                }
            }
            fft.run(&mut re, &mut im, false);
            for k in 0..BINS {
                let (l, r) = split(&re, &im, k);
                h_l[0][p * BINS + k] = l.0;
                h_l[1][p * BINS + k] = l.1;
                h_r[0][p * BINS + k] = r.0;
                h_r[1][p * BINS + k] = r.1;
            }
        }
        Self {
            fft,
            parts,
            h_l,
            h_r,
            x_l: [vec![0.0; parts * BINS], vec![0.0; parts * BINS]],
            x_r: [vec![0.0; parts * BINS], vec![0.0; parts * BINS]],
            slot: 0,
            win_l: vec![0.0; NFFT],
            win_r: vec![0.0; NFFT],
            out_l: vec![0.0; BLOCK],
            out_r: vec![0.0; BLOCK],
            at: 0,
            re,
            im,
            acc_l: [vec![0.0; BINS], vec![0.0; BINS]],
            acc_r: [vec![0.0; BINS], vec![0.0; BINS]],
            fade_left: 0,
        }
    }

    /// Makes the convolver for the settings in `params`.
    pub fn for_params(params: &[f32], sr: f32) -> Self {
        let mut c = Self::new(&ir_samples(params, sr));
        c.fade_left = 256;
        c
    }

    /// Partitions in the response (its length in blocks).
    pub fn partitions(&self) -> usize {
        self.parts
    }

    /// Clears the sound inside, so the tail stops.
    pub fn clear(&mut self) {
        for v in self.x_l.iter_mut().chain(self.x_r.iter_mut()) {
            v.fill(0.0);
        }
        self.win_l.fill(0.0);
        self.win_r.fill(0.0);
        self.out_l.fill(0.0);
        self.out_r.fill(0.0);
        self.at = 0;
    }

    /// One frame in, one frame out, [`BLOCK`] samples late. Does not allocate.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.win_l[BLOCK + self.at] = l;
        self.win_r[BLOCK + self.at] = r;
        let mut out = (self.out_l[self.at], self.out_r[self.at]);
        self.at += 1;
        if self.at == BLOCK {
            self.at = 0;
            self.run_block();
        }
        if self.fade_left > 0 {
            let g = 1.0 - self.fade_left as f32 / 256.0;
            out = (out.0 * g, out.1 * g);
            self.fade_left -= 1;
        }
        out
    }

    fn run_block(&mut self) {
        // The window: the block before, then this one.
        self.re.copy_from_slice(&self.win_l);
        self.im.copy_from_slice(&self.win_r);
        self.fft.run(&mut self.re, &mut self.im, false);
        let slot = self.slot;
        for k in 0..BINS {
            let (l, r) = split(&self.re, &self.im, k);
            let i = slot * BINS + k;
            self.x_l[0][i] = l.0;
            self.x_l[1][i] = l.1;
            self.x_r[0][i] = r.0;
            self.x_r[1][i] = r.1;
        }
        for a in self.acc_l.iter_mut().chain(self.acc_r.iter_mut()) {
            a.fill(0.0);
        }
        for p in 0..self.parts {
            let s = (slot + self.parts - p) % self.parts;
            let (xo, ho) = (s * BINS, p * BINS);
            // Left, then right: acc += x * h.
            for k in 0..BINS {
                let (xr, xi) = (self.x_l[0][xo + k], self.x_l[1][xo + k]);
                let (hr, hi) = (self.h_l[0][ho + k], self.h_l[1][ho + k]);
                self.acc_l[0][k] += xr * hr - xi * hi;
                self.acc_l[1][k] += xr * hi + xi * hr;
            }
            for k in 0..BINS {
                let (xr, xi) = (self.x_r[0][xo + k], self.x_r[1][xo + k]);
                let (hr, hi) = (self.h_r[0][ho + k], self.h_r[1][ho + k]);
                self.acc_r[0][k] += xr * hr - xi * hi;
                self.acc_r[1][k] += xr * hi + xi * hr;
            }
        }
        // Back to one spectrum of left + i right.
        for k in 0..BINS {
            let (lr, li) = (self.acc_l[0][k], self.acc_l[1][k]);
            let (rr, ri) = (self.acc_r[0][k], self.acc_r[1][k]);
            self.re[k] = lr - ri;
            self.im[k] = li + rr;
            if k > 0 && k < BLOCK {
                self.re[NFFT - k] = lr + ri;
                self.im[NFFT - k] = -li + rr;
            }
        }
        self.fft.run(&mut self.re, &mut self.im, true);
        self.out_l.copy_from_slice(&self.re[BLOCK..]);
        self.out_r.copy_from_slice(&self.im[BLOCK..]);
        // The newest block becomes the older one.
        self.win_l.copy_within(BLOCK.., 0);
        self.win_r.copy_within(BLOCK.., 0);
        self.slot = (slot + 1) % self.parts;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn defaults() -> [f32; 24] {
        let mut p = [0.0; 24];
        p[sd::PREDELAY] = 15.0;
        p[sd::LENGTH] = 3.0;
        p[sd::SIZE] = 100.0;
        p[sd::XOVER] = 710.0;
        p[sd::LO_SPREAD] = 100.0;
        p[sd::HI_SPREAD] = 100.0;
        p[sd::INIT_LEVEL] = 100.0;
        p[sd::ATTACK] = 122.0;
        p[sd::DECAY] = 4.52;
        p[sd::END_LEVEL] = 18.0;
        p[sd::QUALITY] = 1.0;
        p[sd::VOLUME_ENV] = 1.0;
        p[sd::BRIGHTNESS] = 0.6;
        p
    }

    #[test]
    fn the_fft_inverts_and_finds_a_tone() {
        let fft = Fft::new(64);
        let mut re: Vec<f32> = (0..64)
            .map(|i| (TAU * 4.0 * i as f32 / 64.0).cos())
            .collect();
        let mut im = vec![0.0; 64];
        let orig = re.clone();
        fft.run(&mut re, &mut im, false);
        assert!((re[4] - 32.0).abs() < 1e-3 && re[5].abs() < 1e-3);
        fft.run(&mut re, &mut im, true);
        assert!(re.iter().zip(&orig).all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn convolving_matches_the_direct_sum_one_block_late() {
        // A response over three partitions, with different left and right.
        let ir: Vec<[f32; 2]> = (0..BLOCK * 2 + 100)
            .map(|i| {
                [
                    ((i * 7 % 13) as f32 - 6.0) * 0.01,
                    ((i * 5 % 11) as f32 - 5.0) * 0.02,
                ]
            })
            .collect();
        let mut c = Convolver::new(&ir);
        let n = BLOCK * 6;
        let input: Vec<[f32; 2]> = (0..n)
            .map(|i| {
                [
                    ((i * 3 % 17) as f32 - 8.0) * 0.05,
                    ((i * 11 % 7) as f32 - 3.0) * 0.1,
                ]
            })
            .collect();
        let out: Vec<(f32, f32)> = input.iter().map(|f| c.tick(f[0], f[1])).collect();
        for i in (BLOCK..n).step_by(37) {
            let t = i - BLOCK;
            let (mut l, mut r) = (0.0f64, 0.0f64);
            for (j, h) in ir.iter().enumerate().take(t + 1) {
                l += f64::from(input[t - j][0] * h[0]);
                r += f64::from(input[t - j][1] * h[1]);
            }
            assert!(
                (out[i].0 as f64 - l).abs() < 2e-3,
                "L {i}: {} vs {l}",
                out[i].0
            );
            assert!(
                (out[i].1 as f64 - r).abs() < 2e-3,
                "R {i}: {} vs {r}",
                out[i].1
            );
        }
    }

    #[test]
    fn the_envelope_follows_init_attack_decay_and_end_level() {
        let p = defaults();
        assert!((volume_env(&p, 0.0) - 1.0).abs() < 1e-6);
        let mut q = p;
        q[sd::INIT_LEVEL] = 20.0;
        assert!((volume_env(&q, 0.0) - 0.2).abs() < 1e-6);
        assert!(
            (volume_env(&q, 0.122) - 1.0).abs() < 1e-3,
            "full at the end of the attack"
        );
        assert!((volume_env(&q, 0.122 + 4.52) - 0.18).abs() < 1e-3);
        assert!(
            (volume_env(&q, 99.0) - 0.18).abs() < 1e-3,
            "it stays at the end level"
        );
        q[sd::CURVE] = 1.0;
        let mid = volume_env(&q, 0.122 + 2.26);
        assert!(
            mid < 0.5 && mid > 0.18,
            "exponential falls faster at first: {mid}"
        );
    }

    #[test]
    fn the_settings_shape_the_response() {
        let sr = 48_000.0;
        let base = defaults();
        let long = ir_samples(&base, sr).len() as f32 / sr;
        assert!((long - 3.0).abs() < 0.05, "{long}");
        let mut stretched = base;
        stretched[sd::LENGTH] = 6.0;
        let l2 = ir_samples(&stretched, sr).len() as f32 / sr;
        assert!((l2 - 6.0).abs() < 0.05, "{l2}");
        let mut big = base;
        big[sd::SIZE] = 200.0;
        let l3 = ir_samples(&big, sr).len() as f32 / sr;
        assert!((l3 - 6.0).abs() < 0.05, "{l3}");
        // Reverse puts the loud part at the end.
        let mut rev = base;
        rev[sd::REVERSE] = 1.0;
        rev[sd::VOLUME_ENV] = 0.0;
        let mut fwd = rev;
        fwd[sd::REVERSE] = 0.0;
        let (a, b) = (ir_samples(&fwd, sr), ir_samples(&rev, sr));
        let early = |v: &[[f32; 2]]| v[..v.len() / 4].iter().map(|f| f[0] * f[0]).sum::<f32>();
        let late = |v: &[[f32; 2]]| {
            v[v.len() * 3 / 4..]
                .iter()
                .map(|f| f[0] * f[0])
                .sum::<f32>()
        };
        assert!(early(&a) > late(&a) * 4.0);
        assert!(late(&b) > early(&b) * 4.0);
        // The volume envelope makes the end quieter.
        let mut flat = base;
        flat[sd::VOLUME_ENV] = 0.0;
        let mut steep = base;
        steep[sd::END_LEVEL] = 0.0;
        steep[sd::DECAY] = 0.5;
        let (f, s) = (ir_samples(&flat, sr), ir_samples(&steep, sr));
        let tail = |v: &[[f32; 2]]| v[v.len() / 2..].iter().map(|x| x[0] * x[0]).sum::<f32>();
        assert!(tail(&s) < tail(&f) * 0.2);
        // Offset starts later; Lo Spread 0 makes the lows mono.
        let mut off = base;
        off[sd::IR_OFFSET] = 1.0;
        assert!(((ir_samples(&off, sr).len() as f32 / sr) - 2.0).abs() < 0.05);
        // Every response has the same loudness.
        for ir in [&a, &b, &f, &s] {
            let pw: f32 = ir.iter().map(|x| (x[0] * x[0] + x[1] * x[1]) * 0.5).sum();
            assert!((pw - 1.0).abs() < 0.05, "{pw}");
        }
    }

    #[test]
    fn no_setting_can_make_a_non_finite_or_huge_response() {
        let sr = 48_000.0;
        for v in [0.0f32, 1.0, 5000.0] {
            let mut p = [v; 24];
            p[sd::SIZE] = 1000.0;
            p[sd::LENGTH] = 10.0;
            p[sd::IR_INDEX] = 3.0;
            let ir = ir_samples(&p, sr);
            assert!(ir.len() as f32 <= MAX_IR_SECONDS * sr + 1.0);
            assert!(ir.iter().all(|f| f[0].is_finite() && f[1].is_finite()));
        }
    }
}
