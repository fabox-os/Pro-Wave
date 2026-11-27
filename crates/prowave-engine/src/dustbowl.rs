//! Dustbowl Lo-Fi: the sound of worn tape and old vinyl, as an Audio FX slot effect.
//!
//! Logic Pro equivalent: the Lo-Fi and tape-style character effects of the Distortion and
//! Modulation groups. Issue #183 asked for a lo-fi effect among the ones Pro Wave should have
//! built in. The code is written from scratch.
//!
//! The signal goes through, in this order:
//! - **Wow** and **Flutter:** the pitch drifts slowly (wow) and shivers quickly (flutter), from a
//!   delay that is modulated by two oscillators and a slow random drift. At 0 there is no delay at
//!   all.
//! - **Drive:** soft saturation like tape, with a little even-order colour, first-order
//!   antiderivative anti-aliasing and the same level for small signals.
//! - **Hiss** and **Crackle:** tape hiss, and the clicks and pops of a dusty record, added
//!   before the filters so they are part of the worn medium.
//! - **Low Cut** and **High Cut:** the narrow band of a cheap player. Low Cut at 20 Hz and High
//!   Cut at 20 kHz take the filters out.
//! - **Mix / Output:** blend with the clean sound, and the level after it.
//!
//! The noise is the same every time the effect is started, so a bounce sounds like the
//! playback. Everything is allocation free after it is made, and is safe for any parameter value.

use crate::fx::{Biquad, Line, Shape};
use crate::pult_eq::ln_cosh;

/// Parameter numbers.
pub mod idx {
    pub const LOW_CUT: usize = 0;
    pub const HIGH_CUT: usize = 1;
    pub const WOW: usize = 2;
    pub const FLUTTER: usize = 3;
    pub const DRIVE: usize = 4;
    pub const HISS: usize = 5;
    pub const CRACKLE: usize = 6;
    pub const MIX: usize = 7;
    pub const OUTPUT: usize = 8;
    pub const COUNT: usize = 9;
}

/// The Low Cut dial's end that takes the filter out, in Hz.
pub const LOW_CUT_OFF: f32 = 20.0;
/// The High Cut dial's end that takes the filter out, in Hz.
pub const HIGH_CUT_OFF: f32 = 20_000.0;

/// The deepest wow, in seconds of delay swing (about 14 cents of pitch at its 0.6 Hz speed).
const WOW_DEPTH_S: f32 = 0.003;
/// The deepest flutter, in seconds of delay swing (about 9 cents at its 7 Hz speed).
const FLUTTER_DEPTH_S: f32 = 0.000_12;
/// How fast wow and flutter run, in Hz.
const WOW_HZ: f32 = 0.6;
const FLUTTER_HZ: f32 = 7.0;
/// The longest delay the line has to hold, in seconds (twice the depths, plus a margin).
const LINE_S: f32 = 0.012;
/// The asymmetry of the saturation (even harmonics), in units of the pre-gain signal.
const BIAS: f32 = 0.05;
/// How many clicks a second the Crackle dial makes at 100 %.
const CRACKLE_RATE: f32 = 40.0;

/// One channel's saturation memory (see `kestrel_eq.rs` for the same method).
#[derive(Clone, Copy, Debug)]
struct Sat {
    u1: f32,
    f1: f32,
    dc_in: f32,
    dc_out: f32,
}

impl Sat {
    fn at_rest() -> Self {
        Self {
            u1: BIAS,
            f1: ln_cosh(BIAS),
            dc_in: 0.0,
            dc_out: 0.0,
        }
    }
}

/// The state of one Dustbowl Lo-Fi.
#[derive(Clone, Debug)]
pub(crate) struct DustState {
    sr: f32,
    lines: [Line; 2],
    /// The wow and flutter oscillators (0 to 1), and the slow random drift (-1 to 1) with the
    /// value it is gliding to.
    wow_phase: f32,
    flutter_phase: f32,
    drift: f32,
    drift_goal: f32,
    drift_left: u32,
    /// Delay swing in samples for wow and flutter (0: off).
    wow: f32,
    flutter: f32,
    sat: [Sat; 2],
    /// The saturation's pre-gain (0: off) and its normaliser.
    gain: f32,
    norm: f32,
    hpf: [Biquad; 2],
    lpf: [Biquad; 2],
    /// Hiss level (linear, 0: off), and the hiss filter's memory.
    hiss: f32,
    hiss_lp: [f32; 2],
    /// Crackle: the chance of a click each sample (0: off), the decaying click of each channel
    /// and the factor it decays by.
    click_chance: f32,
    click: [f32; 2],
    click_decay: f32,
    mix: f32,
    output: f32,
    rng: u32,
}

impl DustState {
    pub(crate) fn new(sr: f32) -> Self {
        let max = (LINE_S * sr) as usize;
        Self {
            sr,
            lines: [Line::new(max), Line::new(max)],
            wow_phase: 0.0,
            flutter_phase: 0.25,
            drift: 0.0,
            drift_goal: 0.0,
            drift_left: 0,
            wow: 0.0,
            flutter: 0.0,
            sat: [Sat::at_rest(); 2],
            gain: 0.0,
            norm: 1.0,
            hpf: [Biquad::unity(); 2],
            lpf: [Biquad::unity(); 2],
            hiss: 0.0,
            hiss_lp: [0.0; 2],
            click_chance: 0.0,
            click: [0.0; 2],
            click_decay: 0.0,
            mix: 1.0,
            output: 1.0,
            rng: 0x2545_f491,
        }
    }

    pub(crate) fn reset(&mut self) {
        for l in self.lines.iter_mut() {
            l.buf.fill(0.0);
        }
        self.sat = [Sat::at_rest(); 2];
        for b in self.hpf.iter_mut().chain(self.lpf.iter_mut()) {
            *b = b.cleared();
        }
        self.hiss_lp = [0.0; 2];
        self.click = [0.0; 2];
        self.drift = 0.0;
        self.drift_goal = 0.0;
        self.drift_left = 0;
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
        self.sr = sr;
        let low = num(pr[idx::LOW_CUT], LOW_CUT_OFF, 2_000.0, LOW_CUT_OFF);
        let high = num(pr[idx::HIGH_CUT], 1_000.0, HIGH_CUT_OFF, HIGH_CUT_OFF);
        let hp = if low <= LOW_CUT_OFF + 0.5 {
            Biquad::unity()
        } else {
            Biquad::make(Shape::High, low, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr)
        };
        let lp = if high >= HIGH_CUT_OFF - 0.5 {
            Biquad::unity()
        } else {
            Biquad::make(Shape::Low, high, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr)
        };
        for ch in 0..2 {
            self.hpf[ch] = self.hpf[ch].retuned(hp);
            self.lpf[ch] = self.lpf[ch].retuned(lp);
        }
        self.wow = num(pr[idx::WOW], 0.0, 100.0, 0.0) * 0.01 * WOW_DEPTH_S * sr;
        self.flutter = num(pr[idx::FLUTTER], 0.0, 100.0, 0.0) * 0.01 * FLUTTER_DEPTH_S * sr;
        let drive = num(pr[idx::DRIVE], 0.0, 100.0, 0.0);
        if drive <= 0.0 {
            self.gain = 0.0;
            self.norm = 1.0;
        } else {
            self.gain = 1.0 + drive * 0.04;
            // Unit gain for small signals: the slope of tanh at the bias point.
            self.norm = 1.0 / (self.gain * (1.0 - BIAS.tanh() * BIAS.tanh()));
        }
        // Hiss from -72 dB to -36 dB (peak of the noise before it is filtered).
        let hiss = num(pr[idx::HISS], 0.0, 100.0, 0.0);
        self.hiss = if hiss <= 0.0 {
            0.0
        } else {
            10f32.powf((-72.0 + 36.0 * hiss * 0.01) / 20.0)
        };
        let crackle = num(pr[idx::CRACKLE], 0.0, 100.0, 0.0) * 0.01;
        self.click_chance = crackle * CRACKLE_RATE / sr;
        // A click dies away in about a third of a millisecond.
        self.click_decay = (-1.0 / (0.000_3 * sr)).exp();
        self.mix = num(pr[idx::MIX], 0.0, 100.0, 100.0) * 0.01;
        self.output = 10f32.powf(num(pr[idx::OUTPUT], -24.0, 12.0, 0.0) / 20.0);
    }

    /// A random number from 0 to 1.
    #[inline]
    fn uniform(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// The saturation for one sample of channel `ch`.
    #[inline]
    fn saturate(&mut self, ch: usize, x: f32) -> f32 {
        let g = self.gain;
        let t = &mut self.sat[ch];
        let u = g * x + BIAS;
        let f = ln_cosh(u);
        let du = u - t.u1;
        // First-order antiderivative anti-aliasing: the mean of tanh over the step.
        let y = if du.abs() > 1e-4 {
            (f - t.f1) / du
        } else {
            (0.5 * (u + t.u1)).tanh()
        };
        t.u1 = u;
        t.f1 = f;
        let y = (y - BIAS.tanh()) * self.norm;
        // Remove the DC the asymmetry makes (a 1-pole high pass at about 10 Hz).
        let out = y - t.dc_in + 0.9993 * t.dc_out;
        t.dc_in = y;
        t.dc_out = out;
        out
    }

    /// One stereo sample through the whole effect.
    #[inline]
    pub(crate) fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let dry = [l, r];
        let mut x = dry;

        // Wow and flutter: a delay that swings between 0 and twice its depth.
        if self.wow > 0.0 || self.flutter > 0.0 {
            self.wow_phase += WOW_HZ / self.sr;
            self.wow_phase -= self.wow_phase.floor();
            self.flutter_phase += FLUTTER_HZ / self.sr;
            self.flutter_phase -= self.flutter_phase.floor();
            // The drift glides to a new random value about every half second.
            if self.drift_left == 0 {
                self.drift_goal = self.uniform() * 2.0 - 1.0;
                self.drift_left = (0.5 * self.sr) as u32;
            }
            self.drift_left -= 1;
            self.drift += (self.drift_goal - self.drift) * (2.0 / self.sr);
            let wow = (std::f32::consts::TAU * self.wow_phase).sin() + 0.4 * self.drift;
            let flutter = (std::f32::consts::TAU * self.flutter_phase).sin();
            let delay = 1.0 + self.wow * (1.0 + wow / 1.4) + self.flutter * (1.0 + flutter);
            for ch in 0..2 {
                self.lines[ch].push(x[ch]);
                x[ch] = self.lines[ch].read(delay);
            }
        }

        if self.gain > 0.0 {
            for (ch, v) in x.iter_mut().enumerate() {
                *v = self.saturate(ch, *v);
            }
        }

        // Hiss and crackle, before the filters.
        for ch in 0..2 {
            if self.hiss > 0.0 {
                let white = self.uniform() * 2.0 - 1.0;
                // A one-pole low pass takes the harshest top off the hiss.
                self.hiss_lp[ch] += (white - self.hiss_lp[ch]) * 0.35;
                x[ch] += self.hiss_lp[ch] * self.hiss * 1.6;
            }
            if self.click_chance > 0.0 {
                if self.uniform() < self.click_chance {
                    // Many small pops and a few loud ones.
                    let u = self.uniform();
                    let amp = 0.02 + 0.3 * u * u * u;
                    self.click[ch] = if self.uniform() < 0.5 { amp } else { -amp };
                }
                x[ch] += self.click[ch];
                self.click[ch] *= self.click_decay;
            }
            x[ch] = self.lpf[ch].tick(self.hpf[ch].tick(x[ch]));
        }

        if self.mix >= 1.0 {
            (x[0] * self.output, x[1] * self.output)
        } else {
            let d = 1.0 - self.mix;
            (
                (dry[0] * d + x[0] * self.mix) * self.output,
                (dry[1] * d + x[1] * self.mix) * self.output,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxKind, MAX_FX_PARAMS};

    const SR: f32 = 48_000.0;

    /// Everything off: no filters, no motion, no drive, no noise.
    fn clean() -> Vec<(usize, f32)> {
        vec![
            (idx::LOW_CUT, 20.0),
            (idx::HIGH_CUT, 20_000.0),
            (idx::WOW, 0.0),
            (idx::FLUTTER, 0.0),
            (idx::DRIVE, 0.0),
            (idx::HISS, 0.0),
            (idx::CRACKLE, 0.0),
            (idx::MIX, 100.0),
            (idx::OUTPUT, 0.0),
        ]
    }

    fn params(set: &[(usize, f32)]) -> [f32; MAX_FX_PARAMS] {
        let mut p = FxKind::LoFi.defaults();
        for (i, v) in clean().iter().chain(set.iter()) {
            p[*i] = *v;
        }
        p
    }

    fn fx(set: &[(usize, f32)]) -> Fx {
        Fx::new(FxKind::LoFi, params(set), false, SR)
    }

    fn sine(hz: f32, amp: f32, i: usize) -> f32 {
        amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin()
    }

    /// RMS gain in dB of the effect for a sine at `hz` and `amp`, after settling.
    fn tone_gain_db(set: &[(usize, f32)], hz: f32, amp: f32) -> f32 {
        let mut e = fx(set);
        let n = SR as usize;
        let (mut e_in, mut e_out) = (0f64, 0f64);
        for i in 0..n {
            let x = sine(hz, amp, i);
            let (y, _) = e.process(x, x);
            if i > n / 2 {
                e_in += f64::from(x * x);
                e_out += f64::from(y * y);
            }
        }
        (10.0 * (e_out / e_in).log10()) as f32
    }

    /// The RMS of a second of silence going through the effect, in dBFS.
    fn silence_db(set: &[(usize, f32)]) -> f32 {
        let mut e = fx(set);
        let mut e_out = 0f64;
        let n = SR as usize;
        for _ in 0..n {
            let (y, _) = e.process(0.0, 0.0);
            e_out += f64::from(y * y);
        }
        (10.0 * (e_out / n as f64).max(1e-30).log10()) as f32
    }

    #[test]
    fn with_everything_off_the_sound_passes_through_exactly_and_without_delay() {
        let mut e = fx(&[]);
        for i in 0..5_000 {
            let x = sine(440.0, 0.8, i) + 1.5;
            assert_eq!(e.process(x, -x), (x, -x));
        }
        assert_eq!(e.latency_samples(), 0);
    }

    #[test]
    fn low_cut_and_high_cut_narrow_the_band_with_their_corners_at_minus_three_db() {
        for corner in [60.0, 200.0, 800.0] {
            let low = tone_gain_db(&[(idx::LOW_CUT, corner)], corner, 0.1);
            assert!((low + 3.0).abs() < 0.8, "low cut {corner} Hz: {low}");
            assert!(tone_gain_db(&[(idx::LOW_CUT, corner)], corner * 8.0, 0.1).abs() < 0.3);
            assert!(tone_gain_db(&[(idx::LOW_CUT, corner)], corner / 4.0, 0.1) < -9.0);
        }
        for corner in [3_000.0, 6_000.0, 12_000.0] {
            let high = tone_gain_db(&[(idx::HIGH_CUT, corner)], corner, 0.1);
            assert!((high + 3.0).abs() < 0.9, "high cut {corner} Hz: {high}");
            assert!(tone_gain_db(&[(idx::HIGH_CUT, corner)], corner / 8.0, 0.1).abs() < 0.3);
        }
        assert!(tone_gain_db(&[(idx::HIGH_CUT, 3_000.0)], 12_000.0, 0.1) < -9.0);
    }

    /// The most the pitch of a 1 kHz tone moves, in cents, measured by the change of its phase
    /// between 10 ms windows (ten whole cycles each; one cent is 0.036 radians per window).
    fn cents_swing(set: &[(usize, f32)]) -> f64 {
        let mut e = fx(set);
        let n = SR as usize * 4;
        let y: Vec<f32> = (0..n)
            .map(|i| e.process(sine(1_000.0, 0.5, i), 0.0).0)
            .collect();
        let phase_at = |from: usize| {
            let (mut re, mut im) = (0f64, 0f64);
            for (k, v) in y[from..from + 480].iter().enumerate() {
                let w = f64::from(std::f32::consts::TAU * 1_000.0 * (from + k) as f32 / SR);
                re += f64::from(*v) * w.cos();
                im += f64::from(*v) * w.sin();
            }
            im.atan2(re)
        };
        let mut worst = 0f64;
        let mut p0 = phase_at(SR as usize);
        for k in 1..300 {
            let p1 = phase_at(SR as usize + k * 480);
            let mut d = p1 - p0;
            while d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            }
            while d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            worst = worst.max(d.abs());
            p0 = p1;
        }
        worst / 0.036
    }

    #[test]
    fn wow_and_flutter_bend_the_pitch_by_cents_and_leave_the_level_alone() {
        // Nothing moves the pitch with both off.
        assert!(cents_swing(&[]) < 0.1, "{}", cents_swing(&[]));
        let wow = cents_swing(&[(idx::WOW, 100.0)]);
        let flutter = cents_swing(&[(idx::FLUTTER, 100.0)]);
        // At full depth a few cents to a few tens of cents, never semitones.
        assert!(wow > 3.0 && wow < 40.0, "wow {wow} cents");
        assert!(flutter > 2.0 && flutter < 40.0, "flutter {flutter} cents");
        // The dials scale it.
        assert!(cents_swing(&[(idx::WOW, 30.0)]) < 0.6 * wow);
        // The level stays: it is a pitch change, not a gain change.
        let level = tone_gain_db(&[(idx::WOW, 100.0), (idx::FLUTTER, 100.0)], 1_000.0, 0.3);
        assert!(level.abs() < 0.5, "{level}");
    }

    #[test]
    fn drive_saturates_but_keeps_small_signals_at_the_same_level() {
        let small = tone_gain_db(&[(idx::DRIVE, 100.0)], 1_000.0, 0.01);
        assert!(small.abs() < 0.4, "{small}");
        let loud = tone_gain_db(&[(idx::DRIVE, 100.0)], 1_000.0, 0.8);
        assert!(loud < -3.0, "a loud signal is pressed down ({loud} dB)");
        let third = |d: f32| {
            let mut e = fx(&[(idx::DRIVE, d)]);
            let n = 48_000usize;
            let (mut re, mut im, mut base) = (0f64, 0f64, 0f64);
            for i in 0..n {
                let t = i as f32 / SR;
                let (y, _) = e.process(0.5 * (std::f32::consts::TAU * 500.0 * t).sin(), 0.0);
                if i >= 8_000 {
                    let w = f64::from(std::f32::consts::TAU * 1_500.0 * t);
                    re += f64::from(y) * w.cos();
                    im += f64::from(y) * w.sin();
                    let w1 = f64::from(std::f32::consts::TAU * 500.0 * t);
                    base += f64::from(y) * w1.sin();
                }
            }
            ((re * re + im * im).sqrt() / base.abs().max(1e-9)) as f32
        };
        assert!(third(0.0) < 0.001);
        assert!(third(100.0) > 0.02, "{}", third(100.0));
        assert!(third(100.0) > third(30.0));
    }

    #[test]
    fn the_drive_leaves_no_dc() {
        let mut e = fx(&[(idx::DRIVE, 100.0)]);
        let n = 96_000usize;
        let mut sum = 0f64;
        for i in 0..n {
            let (y, _) = e.process(sine(500.0, 0.4, i), 0.0);
            if i >= 48_000 {
                sum += f64::from(y);
            }
        }
        assert!((sum / 48_000.0).abs() < 0.002, "DC {}", sum / 48_000.0);
    }

    #[test]
    fn hiss_is_silent_at_zero_and_grows_with_its_dial() {
        assert_eq!(silence_db(&[]), -300.0);
        let (a, b, c) = (
            silence_db(&[(idx::HISS, 20.0)]),
            silence_db(&[(idx::HISS, 60.0)]),
            silence_db(&[(idx::HISS, 100.0)]),
        );
        assert!(a < b && b < c, "{a} {b} {c}");
        assert!(a < -60.0 && c > -50.0 && c < -30.0, "{a} {c}");
    }

    #[test]
    fn crackle_makes_clicks_at_about_the_rate_of_the_dial_and_nothing_at_zero() {
        assert_eq!(silence_db(&[(idx::CRACKLE, 0.0)]), -300.0);
        let mut e = fx(&[(idx::CRACKLE, 100.0)]);
        let (mut clicks, mut above, mut peak) = (0, false, 0f32);
        for _ in 0..SR as usize * 10 {
            let (y, _) = e.process(0.0, 0.0);
            peak = peak.max(y.abs());
            if y.abs() > 0.015 && !above {
                clicks += 1;
            }
            above = y.abs() > 0.015;
        }
        // 40 clicks a second in each channel, left channel counted: about 400 in ten seconds.
        assert!(clicks > 200 && clicks < 650, "{clicks} clicks");
        assert!(peak > 0.1 && peak < 0.5, "loudest click {peak}");
    }

    #[test]
    fn mix_blends_with_the_clean_sound_and_output_scales() {
        let mut dry = fx(&[
            (idx::DRIVE, 100.0),
            (idx::HIGH_CUT, 2_000.0),
            (idx::MIX, 0.0),
        ]);
        assert_eq!(dry.process(0.37, -0.21), (0.37, -0.21));
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
            [0.0, 0.0, 999.0, 999.0, 999.0, 999.0, 999.0, 999.0, 999.0],
        ];
        for w in wild {
            let mut p = [0.0; MAX_FX_PARAMS];
            p[..idx::COUNT].copy_from_slice(&w);
            let mut e = Fx::new(FxKind::LoFi, p, false, SR);
            for i in 0..30_000 {
                let x = if i % 97 == 0 { 1.0 } else { -0.3 };
                let (l, r) = e.process(x, -x);
                assert!(l.is_finite() && r.is_finite() && l.abs() < 100.0, "{l}");
            }
        }
        // Loud input with everything at full stays finite and bounded.
        let mut e = fx(&[
            (idx::LOW_CUT, 400.0),
            (idx::HIGH_CUT, 3_000.0),
            (idx::WOW, 100.0),
            (idx::FLUTTER, 100.0),
            (idx::DRIVE, 100.0),
            (idx::HISS, 100.0),
            (idx::CRACKLE, 100.0),
        ]);
        for i in 0..30_000 {
            let (l, _) = e.process(8.0 * (i as f32 * 0.05).sin(), 0.0);
            assert!(l.is_finite() && l.abs() < 20.0, "{l}");
        }
    }

    #[test]
    fn works_at_other_sample_rates() {
        for sr in [8_000.0, 22_050.0, 44_100.0, 96_000.0, 192_000.0, 384_000.0] {
            let mut e = Fx::new(
                FxKind::LoFi,
                params(&[
                    (idx::WOW, 100.0),
                    (idx::FLUTTER, 100.0),
                    (idx::CRACKLE, 100.0),
                ]),
                false,
                sr,
            );
            for i in 0..(sr as usize / 2) {
                let x = 0.4 * (std::f32::consts::TAU * 440.0 * i as f32 / sr).sin();
                let (l, r) = e.process(x, x);
                assert!(l.is_finite() && r.is_finite() && l.abs() < 5.0, "{sr}: {l}");
            }
        }
    }

    #[test]
    fn a_bypassed_lo_fi_passes_the_sound() {
        let mut e = Fx::new(
            FxKind::LoFi,
            params(&[(idx::DRIVE, 100.0), (idx::HISS, 100.0), (idx::WOW, 100.0)]),
            true,
            SR,
        );
        assert_eq!(e.process(0.3, -0.2), (0.3, -0.2));
    }

    #[test]
    fn turning_a_dial_while_playing_does_not_click() {
        let mut e = fx(&[]);
        let (mut prev, mut worst) = (0.0f32, 0f32);
        for i in 0..48_000usize {
            if i == 24_000 {
                e.set(
                    params(&[
                        (idx::LOW_CUT, 300.0),
                        (idx::HIGH_CUT, 4_000.0),
                        (idx::WOW, 80.0),
                        (idx::DRIVE, 60.0),
                    ]),
                    false,
                );
            }
            let (y, _) = e.process(sine(220.0, 0.3, i), 0.0);
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // A 220 Hz tone of this size moves at most ~0.01 per sample.
        assert!(worst < 0.15, "{worst}");
    }

    #[test]
    fn two_lo_fis_make_the_same_sound() {
        let set = [
            (idx::WOW, 50.0),
            (idx::HISS, 40.0),
            (idx::CRACKLE, 40.0),
            (idx::DRIVE, 30.0),
        ];
        let (mut a, mut b) = (fx(&set), fx(&set));
        for i in 0..8_000 {
            let x = sine(500.0, 0.5, i);
            assert_eq!(a.process(x, x), b.process(x, x));
        }
    }

    #[test]
    fn stopping_and_starting_again_forgets_the_old_sound() {
        let set = [(idx::WOW, 60.0), (idx::DRIVE, 40.0)];
        let mut e = fx(&set);
        for i in 0..20_000 {
            e.process(sine(300.0, 0.9, i), 0.0);
        }
        e.reset();
        // After a reset a silent input gives a silent output (nothing left in the delay).
        for _ in 0..2_000 {
            let (l, r) = e.process(0.0, 0.0);
            assert!(l.abs() < 1e-3 && r.abs() < 1e-3);
        }
    }
}
