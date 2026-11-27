//! Kestrel EQ: a console-style channel equaliser with an input transformer, as an Audio FX slot
//! effect.
//!
//! Logic Pro equivalent: Vintage Console EQ (Logic's Vintage EQ family). The layout follows the
//! classic British console channel strip that issue #192 asked for: a switched high-pass filter,
//! a switched low shelf, a switched mid bell, a fixed high shelf, and an input stage that warms
//! the sound before the EQ. The code is written from scratch for Pro Wave.
//!
//! - **HPF:** Off, 50, 80, 160 or 300 Hz (12 dB per octave).
//! - **Low:** Off, 35, 60, 110 or 220 Hz, a shelf with a little overshoot, 16 dB of boost or cut.
//! - **Mid:** Off, 0.36, 0.7, 1.6, 3.2, 4.8 or 7.2 kHz, a broad bell, 18 dB of boost or cut.
//! - **High:** a shelf at 12 kHz, 16 dB of boost or cut.
//! - **Drive:** the input transformer: soft saturation with a little even-order colour, first-order
//!   antiderivative anti-aliasing (no oversampling delay) and automatic level compensation, so it
//!   changes the colour and not the loudness. It sits before the EQ, like the hardware's input
//!   stage.
//! - **Input / Output / Phase:** trim before the stage, trim after the EQ, and a polarity switch.
//! - **Mode:** the EQ can be out of the circuit (Off: the trim, phase and drive still work), work
//!   on left and right, or work on the mid or the side signal only.
//!
//! Everything is allocation free and safe for any parameter value.

use crate::fx::{Biquad, Shape};
use crate::pult_eq::ln_cosh;

pub use crate::pult_eq::{mode_name, MODES};

/// Parameter numbers.
pub mod idx {
    pub const MODE: usize = 0;
    pub const INPUT: usize = 1;
    pub const DRIVE: usize = 2;
    pub const PHASE: usize = 3;
    pub const HPF: usize = 4;
    pub const LOW_FREQ: usize = 5;
    pub const LOW_GAIN: usize = 6;
    pub const MID_FREQ: usize = 7;
    pub const MID_GAIN: usize = 8;
    pub const HIGH_GAIN: usize = 9;
    pub const OUTPUT: usize = 10;
    pub const COUNT: usize = 11;
}

/// The corners of the HPF selector (positions 1 to 4; position 0 is Off), in Hz.
pub const HPF_FREQS: [f32; 4] = [50.0, 80.0, 160.0, 300.0];
/// The corners of the Low selector (positions 1 to 4), in Hz.
pub const LOW_FREQS: [f32; 4] = [35.0, 60.0, 110.0, 220.0];
/// The centres of the Mid selector (positions 1 to 6), in Hz.
pub const MID_FREQS: [f32; 6] = [360.0, 700.0, 1_600.0, 3_200.0, 4_800.0, 7_200.0];
/// Where the high shelf starts, in Hz.
pub const HIGH_SHELF_HZ: f32 = 12_000.0;

/// The most the low and high bands boost or cut, in dB.
pub const MAX_SHELF_DB: f32 = 16.0;
/// The most the mid band boosts or cuts, in dB.
pub const MAX_MID_DB: f32 = 18.0;

/// The frequency a selector position stands for: `None` for Off (position 0, or anything that
/// is not a number).
fn pick(list: &[f32], v: f32) -> Option<f32> {
    let i = if v.is_finite() {
        v.round().max(0.0) as usize
    } else {
        0
    };
    if i == 0 {
        None
    } else {
        list.get(i - 1).or(list.last()).copied()
    }
}

/// The corner of the HPF for selector value `v` in Hz (`None`: Off).
pub fn hpf_hz(v: f32) -> Option<f32> {
    pick(&HPF_FREQS, v)
}

/// The corner of the low shelf for selector value `v` in Hz (`None`: Off).
pub fn low_freq_hz(v: f32) -> Option<f32> {
    pick(&LOW_FREQS, v)
}

/// The centre of the mid bell for selector value `v` in Hz (`None`: Off).
pub fn mid_freq_hz(v: f32) -> Option<f32> {
    pick(&MID_FREQS, v)
}

/// The four filters for `pr` at `sr`: high-pass, low shelf, mid bell, high shelf. A band that
/// is Off is a plain wire.
pub(crate) fn filters(pr: &[f32; crate::fx::MAX_FX_PARAMS], sr: f32) -> [Biquad; 4] {
    let db = |v: f32, max: f32| {
        if v.is_finite() {
            v.clamp(-max, max)
        } else {
            0.0
        }
    };
    let hpf = match hpf_hz(pr[idx::HPF]) {
        Some(f) => Biquad::make(Shape::High, f, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr),
        None => Biquad::unity(),
    };
    let low = match low_freq_hz(pr[idx::LOW_FREQ]) {
        // A little overshoot, as the hardware's inductor-based shelf has.
        Some(f) => Biquad::make(
            Shape::LowShelf,
            f,
            0.9,
            db(pr[idx::LOW_GAIN], MAX_SHELF_DB),
            sr,
        ),
        None => Biquad::unity(),
    };
    let mid = match mid_freq_hz(pr[idx::MID_FREQ]) {
        Some(f) => Biquad::make(Shape::Peak, f, 0.8, db(pr[idx::MID_GAIN], MAX_MID_DB), sr),
        None => Biquad::unity(),
    };
    let high = Biquad::make(
        Shape::HighShelf,
        HIGH_SHELF_HZ,
        0.7,
        db(pr[idx::HIGH_GAIN], MAX_SHELF_DB),
        sr,
    );
    [hpf, low, mid, high]
}

/// The asymmetry of the transformer stage (even harmonics), in units of the pre-gain signal.
const BIAS: f32 = 0.06;

/// The input stage's memory for one channel.
#[derive(Clone, Copy, Debug)]
struct Stage {
    /// The previous pre-clip value and its antiderivative.
    u1: f32,
    f1: f32,
    dc_in: f32,
    dc_out: f32,
}

impl Stage {
    /// A stage at rest: its memory is what it holds with silence going in, so starting does not
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

/// The state of one Kestrel EQ: filters and input stage for the two channels, and the gains the
/// dials make.
#[derive(Clone, Debug)]
pub(crate) struct KestrelState {
    pub(crate) f: [[Biquad; 4]; 2],
    stage: [Stage; 2],
    /// The stage's pre-gain and level compensation for the current Drive (0: stage off).
    gain: f32,
    comp: f32,
    /// The Input and Output dials as factors, and the polarity (1 or -1).
    input: f32,
    output: f32,
    polarity: f32,
}

fn db_to_gain(db: f32, max: f32) -> f32 {
    let db = if db.is_finite() {
        db.clamp(-max, max)
    } else {
        0.0
    };
    10f32.powf(db / 20.0)
}

impl KestrelState {
    pub(crate) fn new() -> Self {
        Self {
            f: [[Biquad::unity(); 4]; 2],
            stage: [Stage::at_rest(); 2],
            gain: 0.0,
            comp: 1.0,
            input: 1.0,
            output: 1.0,
            polarity: 1.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        for b in self.f.iter_mut().flatten() {
            *b = b.cleared();
        }
        self.stage = [Stage::at_rest(); 2];
    }

    /// The pre-gain and level compensation for `drive` (0 to 100 %): the compensation makes a
    /// -12 dBFS sine come out at the same RMS level, so Drive changes the colour, not the
    /// loudness.
    fn set_drive(&mut self, drive: f32) {
        let drive = if drive.is_finite() {
            drive.clamp(0.0, 100.0)
        } else {
            0.0
        };
        if drive <= 0.0 {
            self.gain = 0.0;
            self.comp = 1.0;
            return;
        }
        let g = 1.0 + drive * 0.11;
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

    /// Moves the filters and gains to `pr`, keeping the filters' memory so a dial that turns
    /// does not click.
    pub(crate) fn retune(&mut self, pr: &[f32; crate::fx::MAX_FX_PARAMS], sr: f32) {
        let new = filters(pr, sr);
        for ch in self.f.iter_mut() {
            for (old, n) in ch.iter_mut().zip(new) {
                *old = old.retuned(n);
            }
        }
        self.set_drive(pr[idx::DRIVE]);
        self.input = db_to_gain(pr[idx::INPUT], 24.0);
        self.output = db_to_gain(pr[idx::OUTPUT], 24.0);
        self.polarity = if pr[idx::PHASE].is_finite() && pr[idx::PHASE] >= 0.5 {
            -1.0
        } else {
            1.0
        };
    }

    /// The input stage for one sample: pre-gain `g`, level compensation `comp`.
    #[inline]
    fn transformer(&mut self, ch: usize, x: f32, g: f32, comp: f32) -> f32 {
        let t = &mut self.stage[ch];
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
        let k = self.input * self.polarity;
        let (mut l, mut r) = (l * k, r * k);
        if self.gain > 0.0 {
            let (g, comp) = (self.gain, self.comp);
            l = self.transformer(0, l, g, comp);
            r = self.transformer(1, r, g, comp);
        }
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
        (l * self.output, r * self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxKind, MAX_FX_PARAMS};

    const SR: f32 = 48_000.0;

    fn params(set: &[(usize, f32)]) -> [f32; MAX_FX_PARAMS] {
        let mut p = FxKind::KestrelEq.defaults();
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

    /// The frequency where the EQ's gain is greatest between `from` and `to` Hz, and that gain.
    fn peak(set: &[(usize, f32)], from: u32, to: u32) -> (f32, f32) {
        (from..=to)
            .step_by(5)
            .map(|h| (h as f32, gain_db(set, h as f32)))
            .fold((0.0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a })
    }

    /// RMS gain in dB of the whole effect for a sine at `hz` and `amp`, after settling.
    fn tone_gain_db(set: &[(usize, f32)], hz: f32, amp: f32) -> f32 {
        let mut fx = Fx::new(FxKind::KestrelEq, params(set), false, SR);
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
        let p = FxKind::KestrelEq.defaults();
        assert_eq!(p[idx::MODE], 1.0);
        assert_eq!(p[idx::DRIVE], 0.0);
        assert_eq!(p[idx::HPF], 0.0);
        assert_eq!(p[idx::PHASE], 0.0);
        for hz in [20.0, 60.0, 360.0, 1000.0, 7200.0, 12000.0, 20000.0] {
            assert!(gain_db(&[], hz).abs() < 0.01, "flat at {hz} Hz");
        }
        assert!(tone_gain_db(&[], 1000.0, 0.3).abs() < 0.01);
    }

    #[test]
    fn the_hpf_cuts_below_its_corner_at_twelve_db_per_octave_and_off_is_a_wire() {
        for (sel, corner) in [(1.0, 50.0), (2.0, 80.0), (3.0, 160.0), (4.0, 300.0)] {
            let at = |hz: f32| gain_db(&[(idx::HPF, sel)], hz);
            assert!(
                (at(corner) + 3.0).abs() < 0.6,
                "-3 dB at {corner} Hz: {}",
                at(corner)
            );
            assert!(at(corner * 10.0).abs() < 0.2, "passes the music");
            // An octave and two octaves below: about 12 dB per octave.
            let slope = at(corner / 2.0) - at(corner / 4.0);
            assert!(
                (slope - 12.0).abs() < 3.0,
                "{slope} dB for the second octave"
            );
        }
        assert!(gain_db(&[(idx::HPF, 0.0)], 20.0).abs() < 0.001);
    }

    #[test]
    fn the_low_shelf_boosts_and_cuts_by_up_to_sixteen_db_from_its_corner_down() {
        for sel in 1..=4 {
            let corner = LOW_FREQS[sel - 1];
            let set = |g| [(idx::LOW_FREQ, sel as f32), (idx::LOW_GAIN, g)];
            let floor = gain_db(&set(16.0), 10.0);
            assert!(floor > 15.0 && floor < 19.5, "boost at the bottom: {floor}");
            let cut = gain_db(&set(-16.0), 10.0);
            assert!(cut < -15.0 && cut > -19.5, "cut at the bottom: {cut}");
            // Half the gain is reached around the corner; the highs are left alone.
            let half = gain_db(&set(16.0), corner);
            assert!(half > 5.0 && half < 12.0, "{half} at {corner} Hz");
            assert!(gain_db(&set(16.0), corner * 40.0).abs() < 0.8);
        }
    }

    #[test]
    fn the_low_selector_moves_the_corner_up() {
        let half_way = |sel: f32| {
            let set = [(idx::LOW_FREQ, sel), (idx::LOW_GAIN, 12.0)];
            (10..=1_000)
                .map(|h| h as f32)
                .find(|h| gain_db(&set, *h) < 6.0)
                .unwrap_or(0.0)
        };
        let corners: Vec<f32> = (1..=4).map(|i| half_way(i as f32)).collect();
        assert!(corners.windows(2).all(|w| w[0] < w[1]), "{corners:?}");
    }

    #[test]
    fn the_mid_bell_peaks_at_each_of_its_six_frequencies_by_up_to_eighteen_db() {
        for sel in 1..=6 {
            let centre = MID_FREQS[sel - 1];
            let set = |g| [(idx::MID_FREQ, sel as f32), (idx::MID_GAIN, g)];
            let (at, top) = peak(&set(18.0), 100, 15_000);
            assert!(
                (at / centre - 1.0).abs() < 0.06,
                "peak at {at} Hz, not {centre} Hz"
            );
            assert!((top - 18.0).abs() < 0.3, "{top} dB");
            let (at, bottom) = (centre, gain_db(&set(-18.0), centre));
            assert!((bottom + 18.0).abs() < 0.3, "{bottom} dB at {at} Hz");
            // A broad bell: still well up an octave away, gone three decades away.
            assert!(gain_db(&set(12.0), centre * 2.0) > 3.0);
            assert!(gain_db(&set(12.0), (centre / 40.0).max(20.0)).abs() < 1.0);
        }
    }

    #[test]
    fn the_high_shelf_lifts_the_top_by_up_to_sixteen_db_and_leaves_the_rest() {
        let set = |g| [(idx::HIGH_GAIN, g)];
        let top = gain_db(&set(16.0), 22_000.0);
        assert!(top > 14.0 && top < 19.0, "{top}");
        assert!((gain_db(&set(-16.0), 22_000.0) + top).abs() < 1.5);
        assert!(gain_db(&set(16.0), 500.0).abs() < 0.3);
        let at_corner = gain_db(&set(16.0), HIGH_SHELF_HZ);
        assert!(at_corner > 5.0 && at_corner < 11.0, "{at_corner}");
    }

    #[test]
    fn a_band_that_is_off_does_nothing_whatever_its_gain_dial_says() {
        let set = [
            (idx::LOW_FREQ, 0.0),
            (idx::LOW_GAIN, 16.0),
            (idx::MID_FREQ, 0.0),
            (idx::MID_GAIN, 18.0),
        ];
        for hz in [30.0, 300.0, 3000.0] {
            assert!(gain_db(&set, hz).abs() < 0.001);
        }
    }

    #[test]
    fn a_cut_is_the_mirror_of_the_boost_so_both_together_cancel() {
        let both = |hz| {
            let up = gain_db(
                &[
                    (idx::LOW_FREQ, 2.0),
                    (idx::LOW_GAIN, 9.0),
                    (idx::MID_FREQ, 3.0),
                    (idx::MID_GAIN, 9.0),
                    (idx::HIGH_GAIN, 9.0),
                ],
                hz,
            );
            let down = gain_db(
                &[
                    (idx::LOW_FREQ, 2.0),
                    (idx::LOW_GAIN, -9.0),
                    (idx::MID_FREQ, 3.0),
                    (idx::MID_GAIN, -9.0),
                    (idx::HIGH_GAIN, -9.0),
                ],
                hz,
            );
            up + down
        };
        for hz in [25.0, 90.0, 800.0, 1600.0, 9000.0, 16000.0] {
            // A shelf with a fixed Q is not an exact inverse of its mirror image (a few
            // hundredths of a dB at the bottom); the bell is exact.
            assert!(both(hz).abs() < 0.15, "{} at {hz}", both(hz));
        }
    }

    #[test]
    fn mid_mode_changes_only_the_mid_and_side_mode_only_the_side() {
        let eq = [(idx::LOW_GAIN, 16.0), (idx::LOW_FREQ, 3.0)];
        let run = |mode: f32, l: f32, r: f32| {
            let mut set = eq.to_vec();
            set.push((idx::MODE, mode));
            let mut fx = Fx::new(FxKind::KestrelEq, params(&set), false, SR);
            for i in 0..SR as usize {
                let t = std::f32::consts::TAU * 40.0 * i as f32 / SR;
                fx.process(l * t.sin(), r * t.sin());
            }
            let mut peak = 0f32;
            for i in 0..2000 {
                let t = std::f32::consts::TAU * 40.0 * (SR + i as f32) / SR;
                let (a, b) = fx.process(l * t.sin(), r * t.sin());
                peak = peak.max(a.abs()).max(b.abs());
            }
            peak
        };
        // A tone only in the side signal (l = -r) is untouched by Mid mode and boosted by Side.
        let side_in_mid = run(2.0, 0.5, -0.5);
        let side_in_side = run(3.0, 0.5, -0.5);
        assert!((side_in_mid - 0.5).abs() < 0.02, "{side_in_mid}");
        assert!(side_in_side > 1.0, "{side_in_side}");
        // A tone only in the mid signal (l = r) is the other way round.
        let mid_in_mid = run(2.0, 0.5, 0.5);
        let mid_in_side = run(3.0, 0.5, 0.5);
        assert!(mid_in_mid > 1.0 && (mid_in_side - 0.5).abs() < 0.02);
        // Off takes the EQ out of the circuit.
        assert!((run(0.0, 0.5, 0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn input_and_output_trim_move_the_level_and_phase_flips_the_polarity() {
        assert!((tone_gain_db(&[(idx::INPUT, 6.0)], 1000.0, 0.1) - 6.0).abs() < 0.05);
        assert!((tone_gain_db(&[(idx::OUTPUT, -9.0)], 1000.0, 0.1) + 9.0).abs() < 0.05);
        assert!((tone_gain_db(&[(idx::INPUT, 30.0)], 1000.0, 0.01) - 24.0).abs() < 0.05);
        // Polarity: exactly the input, upside down, with the EQ out and no drive.
        let mut fx = Fx::new(
            FxKind::KestrelEq,
            params(&[(idx::PHASE, 1.0), (idx::MODE, 0.0)]),
            false,
            SR,
        );
        assert_eq!(fx.process(0.3, -0.2), (-0.3, 0.2));
        // Phase still works with the EQ out, and so does the trim.
        let mut fx = Fx::new(
            FxKind::KestrelEq,
            params(&[(idx::INPUT, -6.0), (idx::MODE, 0.0)]),
            false,
            SR,
        );
        let (l, _) = fx.process(1.0, 1.0);
        assert!((l - 0.501).abs() < 0.002, "{l}");
    }

    #[test]
    fn drive_adds_harmonics_but_keeps_the_level_and_zero_drive_is_exactly_clean() {
        let clean = tone_gain_db(&[], 1000.0, 0.25);
        assert!(clean.abs() < 0.01);
        for d in [20.0, 50.0, 100.0] {
            let g = tone_gain_db(&[(idx::DRIVE, d)], 1000.0, 0.25);
            assert!(g.abs() < 2.0, "drive {d} % changes the level by {g} dB");
        }
        // Harmonic content: the third harmonic is present with drive and absent without.
        let third = |d: f32| {
            let mut fx = Fx::new(FxKind::KestrelEq, params(&[(idx::DRIVE, d)]), false, SR);
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
        assert!(third(80.0) > 0.02, "{}", third(80.0));
        assert!(third(80.0) > third(20.0), "more drive, more harmonics");
    }

    #[test]
    fn the_input_stage_makes_even_harmonics_too_and_leaves_no_dc() {
        let mut fx = Fx::new(FxKind::KestrelEq, params(&[(idx::DRIVE, 80.0)]), false, SR);
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
        assert!((second_re * second_re + second_im * second_im).sqrt() / count > 0.0005);
    }

    #[test]
    fn extreme_and_hostile_parameters_never_make_nan_or_blow_up() {
        let wild: [[f32; idx::COUNT]; 4] = [
            [f32::NAN; idx::COUNT],
            [1e30; idx::COUNT],
            [-1e30; idx::COUNT],
            [
                3.0, 99.0, 999.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0, 99.0,
            ],
        ];
        for w in wild {
            let mut p = [0.0; MAX_FX_PARAMS];
            p[..idx::COUNT].copy_from_slice(&w);
            // Set through `Fx::set` as well, which clamps like the app does.
            let mut fx = Fx::new(FxKind::KestrelEq, p, false, SR);
            for i in 0..20_000 {
                let x = if i % 97 == 0 { 1.0 } else { -0.3 };
                let (l, r) = fx.process(x, -x);
                assert!(l.is_finite() && r.is_finite() && l.abs() < 1000.0, "{l}");
            }
        }
        // Loud input at full drive and full boost stays finite and bounded.
        let mut fx = Fx::new(
            FxKind::KestrelEq,
            params(&[
                (idx::DRIVE, 100.0),
                (idx::LOW_FREQ, 4.0),
                (idx::LOW_GAIN, 16.0),
                (idx::MID_FREQ, 3.0),
                (idx::MID_GAIN, 18.0),
                (idx::HIGH_GAIN, 16.0),
            ]),
            false,
            SR,
        );
        for i in 0..20_000 {
            let (l, _) = fx.process(8.0 * (i as f32 * 0.05).sin(), 0.0);
            assert!(l.is_finite() && l.abs() < 100.0, "{l}");
        }
    }

    #[test]
    fn a_bypassed_kestrel_eq_passes_the_sound_and_has_no_latency() {
        let mut fx = Fx::new(
            FxKind::KestrelEq,
            params(&[
                (idx::LOW_FREQ, 2.0),
                (idx::LOW_GAIN, 16.0),
                (idx::DRIVE, 90.0),
            ]),
            true,
            SR,
        );
        assert_eq!(fx.process(0.3, -0.2), (0.3, -0.2));
        assert_eq!(fx.latency_samples(), 0);
    }

    #[test]
    fn turning_a_dial_while_playing_does_not_click() {
        let mut fx = Fx::new(FxKind::KestrelEq, params(&[]), false, SR);
        let mut prev = 0.0f32;
        let mut worst = 0f32;
        for i in 0..48_000usize {
            if i == 24_000 {
                fx.set(
                    params(&[
                        (idx::LOW_FREQ, 3.0),
                        (idx::LOW_GAIN, 16.0),
                        (idx::MID_FREQ, 2.0),
                        (idx::MID_GAIN, 18.0),
                        (idx::HIGH_GAIN, 16.0),
                        (idx::HPF, 2.0),
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
        assert!(worst < 0.25, "{worst}");
    }

    #[test]
    fn a_second_of_silence_stays_silent() {
        let mut fx = Fx::new(
            FxKind::KestrelEq,
            params(&[
                (idx::DRIVE, 100.0),
                (idx::LOW_FREQ, 2.0),
                (idx::LOW_GAIN, 16.0),
            ]),
            false,
            SR,
        );
        for _ in 0..48_000 {
            let (l, r) = fx.process(0.0, 0.0);
            assert!(l.abs() < 1e-4 && r.abs() < 1e-4);
        }
    }

    #[test]
    fn selector_helpers_name_the_frequency_and_clamp() {
        assert_eq!(hpf_hz(0.0), None);
        assert_eq!(hpf_hz(0.4), None);
        assert_eq!(hpf_hz(1.0), Some(50.0));
        assert_eq!(hpf_hz(99.0), Some(300.0));
        assert_eq!(hpf_hz(f32::NAN), None);
        assert_eq!(low_freq_hz(3.0), Some(110.0));
        assert_eq!(low_freq_hz(-5.0), None);
        assert_eq!(mid_freq_hz(6.0), Some(7_200.0));
        assert_eq!(mid_freq_hz(2.0), Some(700.0));
        assert_eq!(mode_name(2.2), "Mid");
    }

    #[test]
    fn the_curve_the_window_draws_is_the_one_the_sound_gets() {
        let p = params(&[
            (idx::MID_FREQ, 4.0),
            (idx::MID_GAIN, 10.0),
            (idx::HIGH_GAIN, -6.0),
            (idx::HPF, 2.0),
        ]);
        let freqs = [40.0, 400.0, 3_200.0, 16_000.0];
        let drawn = crate::fx::response_db(FxKind::KestrelEq, p, &freqs, SR);
        for (f, d) in freqs.iter().zip(&drawn) {
            let heard = tone_gain_db(
                &[
                    (idx::MID_FREQ, 4.0),
                    (idx::MID_GAIN, 10.0),
                    (idx::HIGH_GAIN, -6.0),
                    (idx::HPF, 2.0),
                ],
                *f,
                0.1,
            );
            assert!((heard - d).abs() < 0.3, "{f} Hz: heard {heard}, drawn {d}");
        }
    }
}
