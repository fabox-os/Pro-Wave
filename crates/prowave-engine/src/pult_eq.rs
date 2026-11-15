//! Pult EQ: a passive-style tube equaliser in the manner of the classic Pultec program EQ, as
//! an Audio FX slot effect.
//!
//! Logic Pro equivalent: the Vintage EQ family (Vintage Console EQ, Vintage Graphic EQ…); the
//! controls follow the Pultec layout that Pult EQ (https://github.com/consint/Pult-EQ) made
//! popular on free plug-ins. The code is written from scratch for Pro Wave.
//!
//! - **Low band:** a selectable corner (20, 30, 60 or 100 Hz) with a *Boost* shelf and an *Atten*
//!   shelf. Turning both up is the "Pultec trick": the boost shelf rings a little and the atten
//!   shelf sits higher, so the lows swell and the area above them is tightened.
//! - **High band:** a *Boost* bell at 3 to 16 kHz whose width is set by *Bandwidth*, and an
//!   *Atten* shelf starting at 5, 10 or 20 kHz.
//! - **Drive:** warm tube-style saturation (asymmetric, with a little even-order colour) with
//!   first-order antiderivative anti-aliasing, so it does not fold harmonics back as harshness
//!   and needs no oversampling latency, and automatic level compensation.
//! - **Mode:** the EQ can be off, work on left and right, or work on the mid or the side signal
//!   only.
//!
//! The dials run 0 to 10 like the hardware's; the maximum is 16 to 18 dB of boost or cut.
//! Everything is allocation free and safe for any parameter value.

use crate::fx::{Biquad, Shape};

/// Parameter numbers.
pub mod idx {
    pub const MODE: usize = 0;
    pub const INPUT: usize = 1;
    pub const LOW_FREQ: usize = 2;
    pub const LOW_BOOST: usize = 3;
    pub const LOW_ATTEN: usize = 4;
    pub const HIGH_BOOST: usize = 5;
    pub const HIGH_FREQ: usize = 6;
    pub const BANDWIDTH: usize = 7;
    pub const HIGH_ATTEN: usize = 8;
    pub const ATTEN_FREQ: usize = 9;
    pub const DRIVE: usize = 10;
    pub const OUTPUT: usize = 11;
    pub const COUNT: usize = 12;
}

/// The values of the Mode selector.
pub const MODES: [&str; 4] = ["Off", "Stereo", "Mid", "Side"];
/// The corner frequencies of the Low Freq selector, in Hz.
pub const LOW_FREQS: [f32; 4] = [20.0, 30.0, 60.0, 100.0];
/// The centre frequencies of the High Freq selector, in Hz.
pub const HIGH_FREQS: [f32; 7] = [
    3_000.0, 4_000.0, 5_000.0, 8_000.0, 10_000.0, 12_000.0, 16_000.0,
];
/// The start frequencies of the Atten Freq selector, in Hz.
pub const ATTEN_FREQS: [f32; 3] = [5_000.0, 10_000.0, 20_000.0];

/// The most boost of the low band and the most cut of either attenuator, in dB.
const MAX_LOW_DB: f32 = 16.0;
/// The most boost of the high band, in dB.
const MAX_HIGH_DB: f32 = 18.0;
/// Most the attenuators cut, in dB.
const MAX_CUT_DB: f32 = 16.0;

fn pick(list: &[f32], v: f32) -> f32 {
    let i = if v.is_finite() {
        v.round().max(0.0) as usize
    } else {
        0
    };
    list[i.min(list.len() - 1)]
}

/// The corner of the low band for selector value `v`, in Hz.
pub fn low_freq_hz(v: f32) -> f32 {
    pick(&LOW_FREQS, v)
}

/// The centre of the high boost for selector value `v`, in Hz.
pub fn high_freq_hz(v: f32) -> f32 {
    pick(&HIGH_FREQS, v)
}

/// The start of the high attenuator for selector value `v`, in Hz.
pub fn atten_freq_hz(v: f32) -> f32 {
    pick(&ATTEN_FREQS, v)
}

/// The name of mode `v`.
pub fn mode_name(v: f32) -> &'static str {
    let i = if v.is_finite() {
        v.round().max(0.0) as usize
    } else {
        0
    };
    MODES[i.min(MODES.len() - 1)]
}

/// The four filters for `pr` at `sr`: low boost shelf, low atten shelf, high boost bell, high
/// atten shelf. Dial values are 0 to 10.
pub(crate) fn filters(pr: &[f32; crate::fx::MAX_FX_PARAMS], sr: f32) -> [Biquad; 4] {
    let dial = |v: f32| v.clamp(0.0, 10.0) * 0.1;
    let low = low_freq_hz(pr[idx::LOW_FREQ]);
    let boost = dial(pr[idx::LOW_BOOST]) * MAX_LOW_DB;
    let atten = dial(pr[idx::LOW_ATTEN]) * MAX_CUT_DB;
    let high = high_freq_hz(pr[idx::HIGH_FREQ]);
    // Bandwidth 0 is a narrow bell, 10 a broad one.
    let q = 2.2 - 1.9 * dial(pr[idx::BANDWIDTH]);
    let hboost = dial(pr[idx::HIGH_BOOST]) * MAX_HIGH_DB;
    let hatten = dial(pr[idx::HIGH_ATTEN]) * MAX_CUT_DB;
    let corner = atten_freq_hz(pr[idx::ATTEN_FREQ]) * 0.5;
    [
        // The boost shelf is smooth, the attenuator sits higher and is gentle: both up makes the
        // Pultec trick's bump below and dip above.
        Biquad::make(Shape::LowShelf, low, QB, boost, sr),
        Biquad::make(Shape::LowShelf, low * FA, QA, -atten, sr),
        Biquad::make(Shape::Peak, high, q, hboost, sr),
        Biquad::make(Shape::HighShelf, corner, 0.55, -hatten, sr),
    ]
}

const QB: f32 = 1.0;
const QA: f32 = 0.55;
const FA: f32 = 0.55;

/// ln(cosh(u)), the antiderivative of tanh, without overflow.
#[inline]
fn ln_cosh(u: f32) -> f32 {
    let a = u.abs();
    a + (1.0 + (-2.0 * a).exp()).ln() - std::f32::consts::LN_2
}

/// What the tube stage does to a channel's memory.
#[derive(Clone, Copy, Debug, Default)]
struct Tube {
    /// The previous pre-clip value and its antiderivative.
    u1: f32,
    f1: f32,
    dc_in: f32,
    dc_out: f32,
}

/// The state of one Pult EQ: filters for the two channels and the tube stage.
#[derive(Clone, Debug)]
pub(crate) struct PultState {
    pub(crate) f: [[Biquad; 4]; 2],
    tube: [Tube; 2],
    /// The tube's pre-gain and level compensation for the current Drive (0: tube off).
    gain: f32,
    comp: f32,
}

/// The asymmetry of the tube (even harmonics), in units of the pre-gain signal.
const BIAS: f32 = 0.18;

impl Tube {
    /// A tube at rest: its memory is what it holds with silence going in, so starting does not
    /// make a click.
    fn at_rest() -> Self {
        Self {
            u1: BIAS,
            f1: ln_cosh(BIAS),
            dc_in: 0.0,
            dc_out: 0.0,
        }
    }
}

impl PultState {
    pub(crate) fn new() -> Self {
        Self {
            f: [[Biquad::unity(); 4]; 2],
            tube: [Tube::at_rest(); 2],
            gain: 0.0,
            comp: 1.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        for b in self.f.iter_mut().flatten() {
            *b = b.cleared();
        }
        self.tube = [Tube::at_rest(); 2];
    }

    /// The pre-gain and level compensation for `drive` (0 to 10): the compensation makes a
    /// -12 dBFS sine come out at the same RMS level, so Drive changes the colour, not the
    /// loudness.
    fn set_drive(&mut self, drive: f32) {
        let drive = if drive.is_finite() {
            drive.clamp(0.0, 10.0)
        } else {
            0.0
        };
        if drive <= 0.0 {
            self.gain = 0.0;
            self.comp = 1.0;
            return;
        }
        let g = 1.0 + drive * 1.3;
        let amp = 0.25;
        let n = 64;
        let mut sum = 0.0f32;
        for i in 0..n {
            let x = amp * (std::f32::consts::TAU * i as f32 / n as f32).sin();
            let y = (g * x + BIAS).tanh() - BIAS.tanh();
            sum += y * y;
        }
        let rms = (sum / n as f32).sqrt().max(1e-4);
        self.gain = g;
        self.comp = amp * std::f32::consts::FRAC_1_SQRT_2 / rms;
    }

    /// Moves the filters to `pr`, keeping their memory so a dial that turns does not click.
    pub(crate) fn retune(&mut self, pr: &[f32; crate::fx::MAX_FX_PARAMS], sr: f32) {
        let new = filters(pr, sr);
        for ch in self.f.iter_mut() {
            for (old, n) in ch.iter_mut().zip(new) {
                *old = old.retuned(n);
            }
        }
        self.set_drive(pr[idx::DRIVE]);
    }

    /// The tube stage for one sample: pre-gain `g`, level compensation `comp`.
    #[inline]
    fn tube(&mut self, ch: usize, x: f32, g: f32, comp: f32) -> f32 {
        let t = &mut self.tube[ch];
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
        // Remove what the bias adds at rest, then the DC the asymmetry makes (a 1-pole high
        // pass at about 10 Hz).
        let y = (y - BIAS.tanh()) * comp;
        let out = y - t.dc_in + 0.9993 * t.dc_out;
        t.dc_in = y;
        t.dc_out = out;
        out
    }

    /// One stereo sample through the whole effect.
    #[inline]
    pub(crate) fn process(
        &mut self,
        pr: &[f32; crate::fx::MAX_FX_PARAMS],
        l: f32,
        r: f32,
    ) -> (f32, f32) {
        let input = 10f32.powf(pr[idx::INPUT].clamp(-12.0, 12.0) / 20.0);
        let (mut l, mut r) = (l * input, r * input);
        let mode = if pr[idx::MODE].is_finite() {
            pr[idx::MODE].round() as i32
        } else {
            1
        };
        match mode {
            0 => {}
            2 => {
                // Mid only.
                let (m, s) = (0.5 * (l + r), 0.5 * (l - r));
                let m = self.f[0].iter_mut().fold(m, |x, f| f.tick(x));
                (l, r) = (m + s, m - s);
            }
            3 => {
                // Side only.
                let (m, s) = (0.5 * (l + r), 0.5 * (l - r));
                let s = self.f[0].iter_mut().fold(s, |x, f| f.tick(x));
                (l, r) = (m + s, m - s);
            }
            _ => {
                l = self.f[0].iter_mut().fold(l, |x, f| f.tick(x));
                r = self.f[1].iter_mut().fold(r, |x, f| f.tick(x));
            }
        }
        if self.gain > 0.0 {
            let (g, comp) = (self.gain, self.comp);
            l = self.tube(0, l, g, comp);
            r = self.tube(1, r, g, comp);
        }
        let out = 10f32.powf(pr[idx::OUTPUT].clamp(-12.0, 12.0) / 20.0);
        (l * out, r * out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxKind, MAX_FX_PARAMS};

    const SR: f32 = 48_000.0;

    fn params(set: &[(usize, f32)]) -> [f32; MAX_FX_PARAMS] {
        let mut p = FxKind::PultEq.defaults();
        for (i, v) in set {
            p[*i] = *v;
        }
        p
    }

    /// The gain of the EQ's filters at `hz`, in dB.
    fn gain_db(set: &[(usize, f32)], hz: f32) -> f32 {
        filters(&params(set), SR)
            .iter()
            .map(|b| b.response_db(hz, SR))
            .sum()
    }

    /// RMS gain in dB of the whole effect for a sine at `hz` and `amp`, after settling.
    fn tone_gain_db(set: &[(usize, f32)], hz: f32, amp: f32) -> f32 {
        let mut fx = Fx::new(FxKind::PultEq, params(set), false, SR);
        let n = SR as usize;
        let (mut e_in, mut e_out) = (0f64, 0f64);
        for i in 0..n {
            let x = amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin();
            let (y, _) = fx.process(x, x);
            if i > n / 2 {
                e_in += f64::from(x * x);
                e_out += f64::from(y * y);
            }
        }
        (10.0 * (e_out / e_in).log10()) as f32
    }

    #[test]
    fn the_defaults_are_flat_and_clean() {
        let p = FxKind::PultEq.defaults();
        assert_eq!(p[idx::MODE], 1.0);
        assert_eq!(p[idx::LOW_BOOST], 0.0);
        assert_eq!(p[idx::DRIVE], 0.0);
        for hz in [30.0, 100.0, 1000.0, 5000.0, 12000.0] {
            assert!(gain_db(&[], hz).abs() < 0.01, "flat at {hz} Hz");
        }
        assert!(tone_gain_db(&[], 1000.0, 0.3).abs() < 0.01);
    }

    #[test]
    fn the_low_boost_lifts_the_lows_by_up_to_its_maximum_and_leaves_the_highs() {
        let at = |v| gain_db(&[(idx::LOW_FREQ, 2.0), (idx::LOW_BOOST, v)], 20.0);
        assert!(at(5.0) > 5.0 && at(5.0) < at(10.0));
        assert!(at(10.0) > 14.0 && at(10.0) < 20.0, "{}", at(10.0));
        assert!(gain_db(&[(idx::LOW_BOOST, 10.0)], 5_000.0).abs() < 0.5);
    }

    #[test]
    fn the_low_selector_moves_the_corner() {
        // Where the boost is half way up (the -6 dB point below its plateau) rises with the selector.
        let half_way = |sel: f32| {
            let set = [(idx::LOW_FREQ, sel), (idx::LOW_BOOST, 8.0)];
            let top = (10..=400)
                .map(|h| gain_db(&set, h as f32))
                .fold(f32::MIN, f32::max);
            (10..=400)
                .map(|h| h as f32)
                .find(|h| gain_db(&set, *h) < top / 2.0)
                .unwrap_or(0.0)
        };
        let corners: Vec<f32> = (0..4).map(|i| half_way(i as f32)).collect();
        assert!(corners.windows(2).all(|w| w[0] < w[1]), "{corners:?}");
    }

    #[test]
    fn the_pultec_trick_boosts_and_cuts_the_lows_at_once_for_a_bump_with_a_dip_above() {
        let set = [
            (idx::LOW_FREQ, 2.0),
            (idx::LOW_BOOST, 7.0),
            (idx::LOW_ATTEN, 7.0),
        ];
        let bump = (10..=80)
            .map(|h| gain_db(&set, h as f32))
            .fold(f32::MIN, f32::max);
        let dip = (80..=400)
            .map(|h| gain_db(&set, h as f32))
            .fold(f32::MAX, f32::min);
        assert!(bump > 4.0, "the lows swell ({bump} dB)");
        assert!(dip < -0.8, "the area above them is tightened ({dip} dB)");
        // At the very bottom the two shelves cancel, as on the hardware.
        assert!(gain_db(&set, 10.0).abs() < 1.5);
        // The boost alone lifts the whole bottom and does not tighten what is above it as much.
        let only_boost = [(idx::LOW_FREQ, 2.0), (idx::LOW_BOOST, 7.0)];
        let boost_dip = (80..=400)
            .map(|h| gain_db(&only_boost, h as f32))
            .fold(f32::MAX, f32::min);
        assert!(boost_dip > dip + 0.3, "{boost_dip} vs {dip}");
        assert!(gain_db(&only_boost, 10.0) > 8.0);
    }

    #[test]
    fn the_high_boost_peaks_at_its_frequency_and_bandwidth_sets_the_width() {
        let width = |bw| {
            let set = [
                (idx::HIGH_BOOST, 8.0),
                (idx::HIGH_FREQ, 3.0),
                (idx::BANDWIDTH, bw),
            ];
            let peak = gain_db(&set, 8_000.0);
            // How far above the peak the bell stays within 3 dB of it.
            let hz_within = (2_000..16_000)
                .step_by(100)
                .filter(|h| gain_db(&set, *h as f32) > peak - 3.0)
                .count();
            (peak, hz_within)
        };
        let (narrow_peak, narrow) = width(0.0);
        let (broad_peak, broad) = width(10.0);
        assert!(
            narrow_peak > 10.0 && broad_peak > 8.0,
            "{narrow_peak} {broad_peak}"
        );
        assert!(
            broad > narrow,
            "a broad bell is wider ({broad} vs {narrow})"
        );
        let at_4k = gain_db(&[(idx::HIGH_BOOST, 8.0), (idx::HIGH_FREQ, 3.0)], 500.0);
        assert!(at_4k.abs() < 1.0, "the lows are left alone");
    }

    #[test]
    fn the_high_atten_cuts_the_top_and_its_selector_moves_where_it_starts() {
        let cut_at_10k = |sel| gain_db(&[(idx::HIGH_ATTEN, 8.0), (idx::ATTEN_FREQ, sel)], 10_000.0);
        assert!(cut_at_10k(0.0) < cut_at_10k(1.0) && cut_at_10k(1.0) < cut_at_10k(2.0));
        assert!(cut_at_10k(0.0) < -6.0);
        assert!(gain_db(&[(idx::HIGH_ATTEN, 8.0)], 200.0).abs() < 0.5);
    }

    #[test]
    fn mid_mode_changes_only_the_mid_and_side_mode_only_the_side() {
        let eq = [(idx::LOW_BOOST, 10.0), (idx::LOW_FREQ, 3.0)];
        let run = |mode: f32, l: f32, r: f32| {
            let mut set = eq.to_vec();
            set.push((idx::MODE, mode));
            let mut fx = Fx::new(FxKind::PultEq, params(&set), false, SR);
            let mut last = (0.0, 0.0);
            for i in 0..SR as usize {
                let t = std::f32::consts::TAU * 40.0 * i as f32 / SR;
                last = fx.process(l * t.sin(), r * t.sin());
            }
            // Peak of the last sample is not a level: measure over the last cycle instead.
            let mut peak = 0f32;
            for i in 0..2000 {
                let t = std::f32::consts::TAU * 40.0 * (SR + i as f32) / SR;
                let (a, b) = fx.process(l * t.sin(), r * t.sin());
                peak = peak.max(a.abs()).max(b.abs());
            }
            let _ = last;
            peak
        };
        // A tone only in the side signal (l = -r) is untouched by Mid mode and boosted by Side.
        let side_in_mid = run(2.0, 0.5, -0.5);
        let side_in_side = run(3.0, 0.5, -0.5);
        assert!(
            (side_in_mid - 0.5).abs() < 0.02,
            "Mid leaves the side alone ({side_in_mid})"
        );
        assert!(side_in_side > 1.0, "Side boosts it ({side_in_side})");
        // A tone only in the mid signal (l = r) is the other way round.
        let mid_in_mid = run(2.0, 0.5, 0.5);
        let mid_in_side = run(3.0, 0.5, 0.5);
        assert!(mid_in_mid > 1.0 && (mid_in_side - 0.5).abs() < 0.02);
        // Off passes everything unchanged.
        assert!((run(0.0, 0.5, 0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn drive_adds_harmonics_but_keeps_the_level_and_zero_drive_is_exactly_clean() {
        let clean = tone_gain_db(&[], 1000.0, 0.25);
        assert!(clean.abs() < 0.01);
        for d in [2.0, 5.0, 10.0] {
            let g = tone_gain_db(&[(idx::DRIVE, d)], 1000.0, 0.25);
            assert!(g.abs() < 2.0, "drive {d} changes the level by {g} dB");
        }
        // Harmonic content: the third harmonic is present with drive and absent without.
        let third = |d: f32| {
            let mut fx = Fx::new(FxKind::PultEq, params(&[(idx::DRIVE, d)]), false, SR);
            let n = 48_000usize;
            let (mut re, mut im, mut base) = (0f64, 0f64, 0f64);
            for i in 0..n {
                let t = i as f32 / SR;
                let (y, _) = fx.process(0.5 * (std::f32::consts::TAU * 500.0 * t).sin(), 0.0);
                if i >= 8_000 {
                    let w = f64::from(std::f32::consts::TAU * 1500.0 * t);
                    re += f64::from(y) * w.cos();
                    im += f64::from(y) * w.sin();
                    let w1 = f64::from(std::f32::consts::TAU * 500.0 * t);
                    base += f64::from(y) * w1.sin();
                }
            }
            ((re * re + im * im).sqrt() / base.abs().max(1e-9)) as f32
        };
        assert!(third(0.0) < 0.001);
        assert!(third(8.0) > 0.02, "{}", third(8.0));
    }

    #[test]
    fn the_tube_makes_even_harmonics_too_and_leaves_no_dc() {
        let mut fx = Fx::new(FxKind::PultEq, params(&[(idx::DRIVE, 8.0)]), false, SR);
        let (mut sum, mut second_re, mut second_im) = (0f64, 0f64, 0f64);
        let n = 96_000usize;
        for i in 0..n {
            let t = i as f32 / SR;
            let (y, _) = fx.process(0.4 * (std::f32::consts::TAU * 500.0 * t).sin(), 0.0);
            if i >= 48_000 {
                sum += f64::from(y);
                let w = f64::from(std::f32::consts::TAU * 1000.0 * t);
                second_re += f64::from(y) * w.cos();
                second_im += f64::from(y) * w.sin();
            }
        }
        let count = (n - 48_000) as f64;
        assert!((sum / count).abs() < 0.002, "DC {}", sum / count);
        assert!((second_re * second_re + second_im * second_im).sqrt() / count > 0.002);
    }

    #[test]
    fn extreme_and_hostile_parameters_never_make_nan_or_blow_up() {
        let wild: [[f32; 12]; 4] = [
            [f32::NAN; 12],
            [1e30; 12],
            [-1e30; 12],
            [
                3.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0,
            ],
        ];
        for w in wild {
            let mut p = [0.0; MAX_FX_PARAMS];
            p[..12].copy_from_slice(&w);
            let mut fx = Fx::new(FxKind::PultEq, p, false, SR);
            for i in 0..20_000 {
                let x = if i % 97 == 0 { 1.0 } else { -0.3 };
                let (l, r) = fx.process(x, -x);
                assert!(l.is_finite() && r.is_finite() && l.abs() < 100.0, "{l}");
            }
        }
        // Loud input at full drive stays finite and bounded.
        let mut fx = Fx::new(
            FxKind::PultEq,
            params(&[(idx::DRIVE, 10.0), (idx::LOW_BOOST, 10.0)]),
            false,
            SR,
        );
        for i in 0..20_000 {
            let (l, _) = fx.process(8.0 * (i as f32 * 0.05).sin(), 0.0);
            assert!(l.is_finite() && l.abs() < 20.0);
        }
    }

    #[test]
    fn a_bypassed_pult_eq_passes_the_sound_and_has_no_latency() {
        let mut fx = Fx::new(
            FxKind::PultEq,
            params(&[(idx::LOW_BOOST, 10.0), (idx::DRIVE, 9.0)]),
            true,
            SR,
        );
        assert_eq!(fx.process(0.3, -0.2), (0.3, -0.2));
        assert_eq!(fx.latency_samples(), 0);
    }

    #[test]
    fn turning_a_dial_while_playing_does_not_click() {
        let mut fx = Fx::new(FxKind::PultEq, params(&[]), false, SR);
        let mut prev = 0.0f32;
        let mut worst = 0f32;
        for i in 0..48_000usize {
            if i == 24_000 {
                fx.set(
                    params(&[
                        (idx::LOW_BOOST, 10.0),
                        (idx::HIGH_BOOST, 10.0),
                        (idx::LOW_FREQ, 3.0),
                    ]),
                    false,
                );
            }
            let x = 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin();
            let (y, _) = fx.process(x, x);
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // A 220 Hz tone of this size steps at most ~0.01 per sample, even with +16 dB.
        assert!(worst < 0.2, "{worst}");
    }

    #[test]
    fn a_second_of_silence_stays_silent() {
        let mut fx = Fx::new(
            FxKind::PultEq,
            params(&[(idx::DRIVE, 10.0), (idx::LOW_BOOST, 10.0)]),
            false,
            SR,
        );
        for _ in 0..48_000 {
            let (l, r) = fx.process(0.0, 0.0);
            assert!(l.abs() < 1e-4 && r.abs() < 1e-4);
        }
    }

    #[test]
    fn selector_helpers_clamp_and_name() {
        assert_eq!(low_freq_hz(0.4), 20.0);
        assert_eq!(low_freq_hz(99.0), 100.0);
        assert_eq!(low_freq_hz(f32::NAN), 20.0);
        assert_eq!(high_freq_hz(3.0), 8_000.0);
        assert_eq!(atten_freq_hz(-5.0), 5_000.0);
        assert_eq!(mode_name(2.2), "Mid");
        assert_eq!(mode_name(f32::NAN), "Off");
    }
}
