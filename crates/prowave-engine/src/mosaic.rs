//! Mosaic Decimator: a bit-depth and sample-rate reducer, as an Audio FX slot effect.
//!
//! Logic Pro equivalent: Bitcrusher (the Distortion group). Issue #183 asked for a decimator
//! among the effects Pro Wave should have built in. The code is written from scratch.
//!
//! - **Bits:** the sound is rounded to this many bits (24 is untouched, 1 leaves only -1, 0 and
//!   1). It runs smoothly, so the knob can be turned while playing.
//! - **Rate:** the sound is held at this many samples per second (the "sample and hold" of an
//!   old sampler). The ratio does not have to be a whole number, so the aliasing glides with the
//!   knob. At or above the project's rate, nothing is held.
//! - **Dither:** noise of up to one step added before the rounding, so quiet sounds keep their
//!   shape instead of turning into gritty stairs (at the price of a hiss).
//! - **Mix / Output:** blend with the clean sound, and the level after it.
//!
//! With the defaults (24 bits, the full rate) the sound passes through exactly. Everything is
//! allocation free, adds no delay and is safe for any parameter value.

/// Parameter numbers.
pub mod idx {
    pub const BITS: usize = 0;
    pub const RATE: usize = 1;
    pub const DITHER: usize = 2;
    pub const MIX: usize = 3;
    pub const OUTPUT: usize = 4;
    pub const COUNT: usize = 5;
}

/// The most bits the Bits dial reaches; at this the rounding is switched off.
pub const MAX_BITS: f32 = 24.0;
/// The lowest and highest value of the Rate dial, in Hz. The top is where nothing is held.
pub const MIN_RATE: f32 = 500.0;
pub const MAX_RATE: f32 = 48_000.0;

/// The state of one Mosaic Decimator.
#[derive(Clone, Debug)]
pub(crate) struct MosaicState {
    /// Steps per unit of signal (2^(bits-1)); 0 means the rounding is off.
    steps: f32,
    /// How far the hold clock moves each sample (rate / sample rate); 0 means nothing is held.
    inc: f32,
    /// The hold clock, and what is held.
    phase: f32,
    held: [f32; 2],
    /// Dither in steps (0 to 1), the dry/wet balance and the output factor.
    dither: f32,
    mix: f32,
    output: f32,
    /// Random numbers for the dither (xorshift32).
    rng: u32,
}

impl MosaicState {
    pub(crate) fn new() -> Self {
        Self {
            steps: 0.0,
            inc: 0.0,
            // A clock that has run out takes the first sample at once.
            phase: 1.0,
            held: [0.0; 2],
            dither: 0.0,
            mix: 1.0,
            output: 1.0,
            rng: 0x9e37_79b9,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.phase = 1.0;
        self.held = [0.0; 2];
    }

    /// Moves the dials: `pr` are the parameters, `sr` the sample rate.
    pub(crate) fn retune(&mut self, pr: &[f32; crate::fx::MAX_FX_PARAMS], sr: f32) {
        let num = |v: f32, lo: f32, hi: f32, default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        let bits = num(pr[idx::BITS], 1.0, MAX_BITS, MAX_BITS);
        self.steps = if bits >= MAX_BITS {
            0.0
        } else {
            (bits - 1.0).exp2()
        };
        let rate = num(pr[idx::RATE], MIN_RATE, MAX_RATE, MAX_RATE);
        let ratio = rate / sr.max(1.0);
        self.inc = if ratio >= 1.0 { 0.0 } else { ratio };
        self.dither = num(pr[idx::DITHER], 0.0, 100.0, 0.0) * 0.01;
        self.mix = num(pr[idx::MIX], 0.0, 100.0, 100.0) * 0.01;
        self.output = 10f32.powf(num(pr[idx::OUTPUT], -24.0, 12.0, 0.0) / 20.0);
    }

    /// A random number from -1 to 1 (triangular: two uniform numbers added, so the noise does
    /// not change the loudness of the rounding error).
    #[inline]
    fn noise(&mut self) -> f32 {
        let mut next = || {
            let mut x = self.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.rng = x;
            (x >> 8) as f32 * (1.0 / 8_388_608.0) - 1.0
        };
        let a = next();
        let b = next();
        0.5 * (a + b)
    }

    /// Rounds `x` to the grid (nothing while the rounding is off).
    #[inline]
    fn round(&mut self, x: f32) -> f32 {
        if self.steps <= 0.0 {
            return x;
        }
        let s = self.steps;
        let dither = if self.dither > 0.0 {
            self.noise() * self.dither
        } else {
            0.0
        };
        ((x * s + dither).round()).clamp(-s, s) / s
    }

    /// One stereo sample through the whole effect.
    #[inline]
    pub(crate) fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let wet = if self.inc > 0.0 {
            self.phase += self.inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.held = [self.round(l), self.round(r)];
            }
            (self.held[0], self.held[1])
        } else {
            (self.round(l), self.round(r))
        };
        if self.mix >= 1.0 {
            (wet.0 * self.output, wet.1 * self.output)
        } else {
            let dry = 1.0 - self.mix;
            (
                (l * dry + wet.0 * self.mix) * self.output,
                (r * dry + wet.1 * self.mix) * self.output,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxKind, MAX_FX_PARAMS};

    const SR: f32 = 48_000.0;

    fn params(set: &[(usize, f32)]) -> [f32; MAX_FX_PARAMS] {
        let mut p = FxKind::Decimator.defaults();
        for (i, v) in set {
            p[*i] = *v;
        }
        p
    }

    fn fx(set: &[(usize, f32)]) -> Fx {
        Fx::new(FxKind::Decimator, params(set), false, SR)
    }

    fn sine(hz: f32, amp: f32, i: usize) -> f32 {
        amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin()
    }

    /// The amplitude of `hz` in the samples `y[from..]` (a single-bin Fourier sum).
    fn level_at(y: &[f32], hz: f32, from: usize) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in y.iter().enumerate().skip(from) {
            let w = f64::from(std::f32::consts::TAU * hz * i as f32 / SR);
            re += f64::from(*v) * w.cos();
            im += f64::from(*v) * w.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / (y.len() - from) as f64) as f32
    }

    #[test]
    fn the_defaults_pass_the_sound_through_exactly() {
        let p = FxKind::Decimator.defaults();
        assert_eq!(p[idx::BITS], 24.0);
        assert_eq!(p[idx::RATE], 48_000.0);
        assert_eq!(p[idx::DITHER], 0.0);
        assert_eq!(p[idx::MIX], 100.0);
        let mut e = fx(&[]);
        for i in 0..5_000 {
            let x = sine(440.0, 0.8, i) + 1.5;
            assert_eq!(e.process(x, -x), (x, -x));
        }
        assert_eq!(e.latency_samples(), 0);
    }

    #[test]
    fn fewer_bits_put_the_sound_on_a_coarse_grid_with_the_noise_of_that_many_bits() {
        let mut e = fx(&[(idx::BITS, 8.0)]);
        let n = 48_000;
        let (mut sig, mut err) = (0f64, 0f64);
        for i in 0..n {
            let x = sine(441.0, 0.9, i);
            let (y, _) = e.process(x, x);
            // Every output is a multiple of 1/128 (8 bits, 128 steps per unit).
            assert!(((y * 128.0).round() - y * 128.0).abs() < 1e-3, "{y}");
            sig += f64::from(x * x);
            err += f64::from((y - x) * (y - x));
        }
        let snr = 10.0 * (sig / err).log10();
        // About 6 dB per bit: 8 bits give about 50 dB for a loud sine.
        assert!(snr > 40.0 && snr < 58.0, "{snr} dB");
        // Two bits fewer, 12 dB worse.
        let mut e = fx(&[(idx::BITS, 6.0)]);
        let (mut sig, mut err) = (0f64, 0f64);
        for i in 0..n {
            let x = sine(441.0, 0.9, i);
            let (y, _) = e.process(x, x);
            sig += f64::from(x * x);
            err += f64::from((y - x) * (y - x));
        }
        let snr6 = 10.0 * (sig / err).log10();
        assert!(snr - snr6 > 8.0 && snr - snr6 < 16.0, "{snr} vs {snr6}");
    }

    #[test]
    fn one_bit_leaves_only_minus_one_zero_and_one() {
        let mut e = fx(&[(idx::BITS, 1.0)]);
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..2_000 {
            let (y, _) = e.process(sine(300.0, 0.9, i), 0.0);
            seen.insert((y * 100.0).round() as i32);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![-100, 0, 100]);
    }

    #[test]
    fn the_rate_holds_each_sample_for_the_right_number_of_samples() {
        // A whole ratio: 48 000 / 4 800 = 10 samples per hold.
        let mut e = fx(&[(idx::RATE, 4_800.0)]);
        let y: Vec<f32> = (0..2_000)
            .map(|i| e.process(sine(200.0, 0.5, i), 0.0).0)
            .collect();
        let mut runs = Vec::new();
        let mut len = 1;
        for w in y.windows(2) {
            if w[0] == w[1] {
                len += 1;
            } else {
                runs.push(len);
                len = 1;
            }
        }
        assert!(runs.iter().skip(1).all(|r| *r == 10), "{runs:?}");
        // A ratio that is not whole: 48 000 / 11 000 = 4.36, so holds of 4 or 5 samples.
        let mut e = fx(&[(idx::RATE, 11_000.0)]);
        let y: Vec<f32> = (0..48_000)
            .map(|i| e.process(sine(130.0, 0.5, i), 0.0).0)
            .collect();
        let changes = y.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(
            (changes as f32 - 11_000.0).abs() < 60.0,
            "{changes} holds in a second"
        );
    }

    #[test]
    fn a_low_rate_folds_a_high_tone_down_as_aliasing() {
        // 3 kHz held at 8 kHz leaves an image at 8 - 3 = 5 kHz.
        let mut e = fx(&[(idx::RATE, 8_000.0)]);
        let y: Vec<f32> = (0..48_000)
            .map(|i| e.process(sine(3_000.0, 0.5, i), 0.0).0)
            .collect();
        let image = level_at(&y, 5_000.0, 4_800);
        let tone = level_at(&y, 3_000.0, 4_800);
        assert!(image > 0.1 * tone && image > 0.03, "{image} vs {tone}");
        // Held at the full rate there is no image.
        let mut e = fx(&[]);
        let y: Vec<f32> = (0..48_000)
            .map(|i| e.process(sine(3_000.0, 0.5, i), 0.0).0)
            .collect();
        assert!(level_at(&y, 5_000.0, 4_800) < 0.001);
    }

    #[test]
    fn dither_keeps_a_signal_below_one_step_alive() {
        // A steady 0.3 of a step: rounding alone loses it, dither keeps its average.
        let step = 1.0 / 128.0;
        let x = 0.3 * step;
        let mean = |dither: f32| {
            let mut e = fx(&[(idx::BITS, 8.0), (idx::DITHER, dither)]);
            let n = 200_000;
            (0..n).map(|_| f64::from(e.process(x, x).0)).sum::<f64>() / f64::from(n)
        };
        assert_eq!(mean(0.0), 0.0);
        let dithered = mean(100.0);
        assert!(
            (dithered - f64::from(x)).abs() < 0.1 * f64::from(step),
            "{dithered} vs {x}"
        );
    }

    #[test]
    fn mix_and_output_blend_and_scale() {
        let mut dry = fx(&[(idx::BITS, 3.0), (idx::MIX, 0.0)]);
        assert_eq!(dry.process(0.37, -0.21), (0.37, -0.21));
        let mut half = fx(&[(idx::BITS, 2.0), (idx::MIX, 50.0)]);
        // 2 bits: steps of 1/2; 0.37 rounds to 0.5, so half way between 0.37 and 0.5.
        let (y, _) = half.process(0.37, 0.0);
        assert!((y - 0.435).abs() < 1e-5, "{y}");
        let mut quiet = fx(&[(idx::OUTPUT, -6.0)]);
        let (y, _) = quiet.process(0.5, 0.0);
        assert!((y - 0.5 * 0.501_187).abs() < 1e-4, "{y}");
    }

    #[test]
    fn extreme_and_hostile_parameters_never_make_nan_or_blow_up() {
        let wild: [[f32; idx::COUNT]; 4] = [
            [f32::NAN; idx::COUNT],
            [1e30; idx::COUNT],
            [-1e30; idx::COUNT],
            [0.0, 0.0, 999.0, 999.0, 999.0],
        ];
        for w in wild {
            let mut p = [0.0; MAX_FX_PARAMS];
            p[..idx::COUNT].copy_from_slice(&w);
            let mut e = Fx::new(FxKind::Decimator, p, false, SR);
            for i in 0..20_000 {
                let x = if i % 97 == 0 { 1.0 } else { -0.3 };
                let (l, r) = e.process(x, -x);
                assert!(l.is_finite() && r.is_finite() && l.abs() < 10.0, "{l}");
            }
        }
        // Loud input is held to full scale, not past it.
        let mut e = fx(&[(idx::BITS, 8.0)]);
        for i in 0..2_000 {
            let (l, _) = e.process(50.0 * (i as f32 * 0.1).sin(), 0.0);
            assert!(l.abs() <= 1.0);
        }
    }

    #[test]
    fn a_bypassed_decimator_passes_the_sound() {
        let mut e = Fx::new(
            FxKind::Decimator,
            params(&[(idx::BITS, 2.0), (idx::RATE, 600.0)]),
            true,
            SR,
        );
        assert_eq!(e.process(0.3, -0.2), (0.3, -0.2));
    }

    #[test]
    fn turning_a_dial_while_playing_does_not_jump_beyond_one_step() {
        let mut e = fx(&[]);
        let mut worst = 0f32;
        let mut prev = 0.0f32;
        for i in 0..48_000usize {
            if i == 24_000 {
                e.set(params(&[(idx::BITS, 10.0)]), false);
            }
            let (y, _) = e.process(sine(220.0, 0.3, i), 0.0);
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // A 220 Hz tone of this size moves at most ~0.01 per sample; 10 bits add 1/512.
        assert!(worst < 0.02, "{worst}");
    }

    #[test]
    fn silence_stays_silent_without_dither_and_within_a_step_with_it() {
        let mut e = fx(&[(idx::BITS, 4.0), (idx::RATE, 2_000.0)]);
        for _ in 0..10_000 {
            assert_eq!(e.process(0.0, 0.0), (0.0, 0.0));
        }
        let mut e = fx(&[(idx::BITS, 8.0), (idx::DITHER, 100.0)]);
        for _ in 0..10_000 {
            let (l, r) = e.process(0.0, 0.0);
            assert!(l.abs() <= 1.0 / 128.0 && r.abs() <= 1.0 / 128.0);
        }
    }

    #[test]
    fn two_decimators_make_the_same_sound() {
        let set = [(idx::BITS, 5.0), (idx::RATE, 9_000.0), (idx::DITHER, 60.0)];
        let (mut a, mut b) = (fx(&set), fx(&set));
        for i in 0..5_000 {
            let x = sine(500.0, 0.7, i);
            assert_eq!(a.process(x, x), b.process(x, x));
        }
    }
}
