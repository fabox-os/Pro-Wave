//! Electric Piano: a physically modelled reed piano in the manner of the Wurlitzer 200A, as a
//! track instrument (Logic Pro equivalent: the Vintage Electric Piano, Wurlitzer model).
//!
//! The model follows the instrument's signal chain, each stage from its physics, with no
//! samples. It is written from scratch for Pro Wave; the free OpenWurli plug-in (GPL-3.0,
//! issue #117) was the reference for which stages matter, and none of its code is used.
//!
//! 1. **Reed:** a struck steel reed is a cantilever beam, so it rings in a few modes whose
//!    frequencies are not harmonic (Euler-Bernoulli: 1, 6.27, 17.55, 34.4, 56.8, 84.9, 118.6
//!    times the fundamental). Each mode is a damped resonator; the upper modes die fast, which
//!    is the "bark" of the attack. A soft hammer blow excites the high modes less (the felt
//!    dwell), so soft notes are mellow and hard notes bright.
//! 2. **Pickup:** the reed swings in an electrostatic pickup whose output is not proportional
//!    to the displacement (capacitance goes as 1 over the gap), so it makes harmonics that grow
//!    with the reed's amplitude. Each reed has its own pickup.
//! 3. **Preamp:** a shared, asymmetric, saturating stage after a high pass; the tremolo
//!    changes its gain, so it changes the timbre and not only the volume.
//! 4. **Speaker:** a small cabinet: high pass, a tone low pass and a mild cubic nonlinearity.
//!
//! Everything runs in the audio thread without allocating: the voices are a fixed array of
//! damped complex resonators (one complex multiply per mode and sample).

use std::f32::consts::{LN_2, TAU};

/// Number of notes that can sound at once; a new note steals the oldest.
pub const VOICES: usize = 32;
/// Reed modes per note.
pub const MODES: usize = 7;
/// Number of dials.
pub const PARAMS: usize = 6;

/// Dial numbers.
pub mod dial {
    /// Tremolo depth, 0 to 1 (0: off).
    pub const TREMOLO_DEPTH: usize = 0;
    /// Tremolo speed in Hz.
    pub const TREMOLO_RATE: usize = 1;
    /// How hard the pickup and preamp are driven, 0 to 1: the bark.
    pub const BARK: usize = 2;
    /// The speaker's tone, 0 (dark) to 1 (bright).
    pub const TONE: usize = 3;
    /// How long notes ring, as a multiple of the normal time (0.5 to 2).
    pub const DECAY: usize = 4;
    /// Output level in dB.
    pub const LEVEL: usize = 5;
}

/// One dial: name, least, most, start value, unit.
#[derive(Clone, Copy, Debug)]
pub struct DialInfo {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
}

/// The dials, in the order of [`dial`].
pub const DIALS: [DialInfo; PARAMS] = [
    DialInfo {
        name: "Tremolo",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        unit: "",
    },
    DialInfo {
        name: "Tremolo Rate",
        min: 1.0,
        max: 10.0,
        default: 5.5,
        unit: "Hz",
    },
    DialInfo {
        name: "Bark",
        min: 0.0,
        max: 1.0,
        default: 0.5,
        unit: "",
    },
    DialInfo {
        name: "Tone",
        min: 0.0,
        max: 1.0,
        default: 0.6,
        unit: "",
    },
    DialInfo {
        name: "Decay",
        min: 0.5,
        max: 2.0,
        default: 1.0,
        unit: "x",
    },
    DialInfo {
        name: "Level",
        min: -12.0,
        max: 6.0,
        default: 0.0,
        unit: "dB",
    },
];

/// The start values of all dials.
pub fn defaults() -> [f32; PARAMS] {
    let mut v = [0.0; PARAMS];
    for (slot, d) in v.iter_mut().zip(DIALS) {
        *slot = d.default;
    }
    v
}

/// `values` with each dial put into its range (not finite: its start value).
pub fn clamp(values: [f32; PARAMS]) -> [f32; PARAMS] {
    let mut v = [0.0; PARAMS];
    for ((slot, d), x) in v.iter_mut().zip(DIALS).zip(values) {
        *slot = if x.is_finite() {
            x.clamp(d.min, d.max)
        } else {
            d.default
        };
    }
    v
}

/// Frequency ratios of a steel cantilever's bending modes (Euler-Bernoulli beam theory).
const RATIOS: [f32; MODES] = [1.0, 6.267, 17.55, 34.39, 56.84, 84.9, 118.6];
/// How much of each mode a firm blow starts with.
const WEIGHTS: [f32; MODES] = [1.0, 0.55, 0.30, 0.18, 0.10, 0.06, 0.03];

/// One damped resonator: `z` turns by `c` each sample; the output is its imaginary part.
#[derive(Clone, Copy, Default)]
struct Mode {
    re: f32,
    im: f32,
    c_re: f32,
    c_im: f32,
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    released: bool,
    key: u8,
    /// Samples since the note began, for stealing the oldest.
    age: u32,
    modes: [Mode; MODES],
    /// How many modes are below the Nyquist frequency.
    count: usize,
    /// The damper's gain: 1 while held, falling after the key is released.
    damper: f32,
    /// A cheap estimate of how loud the voice still is, to know when it is over.
    level: f32,
    level_decay: f32,
}

impl Voice {
    const IDLE: Voice = Voice {
        active: false,
        released: false,
        key: 0,
        age: 0,
        modes: [Mode {
            re: 0.0,
            im: 0.0,
            c_re: 1.0,
            c_im: 0.0,
        }; MODES],
        count: 0,
        damper: 1.0,
        level: 0.0,
        level_decay: 1.0,
    };
}

/// A one-pole filter's memory.
#[derive(Clone, Copy, Default)]
struct OnePole {
    lp: f32,
}

impl OnePole {
    #[inline]
    fn low(&mut self, x: f32, k: f32) -> f32 {
        self.lp += (x - self.lp) * k;
        self.lp
    }
    #[inline]
    fn high(&mut self, x: f32, k: f32) -> f32 {
        x - self.low(x, k)
    }
}

/// The one-pole coefficient for a corner of `hz` at `sr`.
fn corner(hz: f32, sr: f32) -> f32 {
    1.0 - (-TAU * hz.min(sr * 0.45) / sr).exp()
}

/// ln(cosh(u)), the antiderivative of tanh, without overflow.
#[inline]
fn ln_cosh(u: f32) -> f32 {
    let a = u.abs();
    a + (1.0 + (-2.0 * a).exp()).ln() - LN_2
}

/// The electric piano: 32 reeds and the chain after them. Make it off the audio thread with
/// [`EPiano::new`]; the audio thread calls only the other methods, which never allocate.
pub struct EPiano {
    sr: f32,
    params: [f32; PARAMS],
    voices: [Voice; VOICES],
    lfo: f32,
    /// Pickup high pass, speaker high pass and the two tone low passes.
    pickup_hp: OnePole,
    speaker_hp: OnePole,
    tone: [OnePole; 2],
    k_pickup: f32,
    k_speaker: f32,
    k_tone: f32,
    /// The preamp's saturation memory (antiderivative anti-aliasing).
    u1: f32,
    f1: f32,
    /// DC blocker after the preamp.
    dc_in: f32,
    dc_out: f32,
    level_gain: f32,
}

/// The asymmetry of the preamp.
const BIAS: f32 = 0.12;

impl EPiano {
    /// A piano at `sample_rate` Hz with the dials at `params`. Allocation free, but call it off
    /// the audio thread all the same (it is made with the other instruments).
    pub fn new(sample_rate: f32, params: [f32; PARAMS]) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(8_000.0, 384_000.0)
        } else {
            48_000.0
        };
        let mut p = Self {
            sr,
            params: defaults(),
            voices: [Voice::IDLE; VOICES],
            lfo: 0.0,
            pickup_hp: OnePole::default(),
            speaker_hp: OnePole::default(),
            tone: [OnePole::default(); 2],
            k_pickup: corner(60.0, sr),
            k_speaker: corner(90.0, sr),
            k_tone: corner(5_000.0, sr),
            u1: BIAS,
            f1: ln_cosh(BIAS),
            dc_in: 0.0,
            dc_out: 0.0,
            level_gain: 1.0,
        };
        p.set_params(params);
        p
    }

    /// New dial values (put into range). Allocation free: dials can move while notes ring.
    pub fn set_params(&mut self, params: [f32; PARAMS]) {
        self.params = clamp(params);
        let tone = self.params[dial::TONE];
        self.k_tone = corner(1_200.0 + 8_000.0 * tone, self.sr);
        self.level_gain = 10f32.powf(self.params[dial::LEVEL] / 20.0);
    }

    /// The dial values now.
    pub fn params(&self) -> [f32; PARAMS] {
        self.params
    }

    /// Starts `key` (MIDI note number) at `velocity` (1 to 127; 0 counts as 1).
    pub fn note_on(&mut self, key: u8, velocity: u8) {
        let key = key.min(127);
        let v = f32::from(velocity.clamp(1, 127)) / 127.0;
        // The same key again restarts its reed; else a free voice; else the oldest.
        let slot = self
            .voices
            .iter()
            .position(|x| x.active && x.key == key)
            .or_else(|| self.voices.iter().position(|x| !x.active))
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, x)| (x.released, x.age))
                    .map(|(i, _)| i)
            })
            .unwrap_or(0);
        // Each reed is a little different: tuning within +-1.5 cents, level within +-0.7 dB.
        let h = (u32::from(key).wrapping_mul(2_654_435_761) >> 8) & 0xffff;
        let unit = h as f32 / 65_535.0 * 2.0 - 1.0;
        let cents = unit * 1.5;
        let wobble = 10f32.powf(unit * 0.7 / 20.0);
        let f0 = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0) * 2f32.powf(cents / 1200.0);
        // Low reeds ring for seconds, high ones for about one.
        let t60 =
            ((6.0 - 0.045 * (f32::from(key) - 33.0)).clamp(1.2, 6.0)) * self.params[dial::DECAY];
        let pitch_gain = 1.0 / (1.0 + 0.012 * (f32::from(key) - 60.0).max(0.0));
        let mut voice = Voice::IDLE;
        voice.active = true;
        voice.key = key;
        for n in 0..MODES {
            let f = f0 * RATIOS[n];
            if f >= self.sr * 0.45 {
                break;
            }
            // The upper modes die much faster than the fundamental.
            let t = t60 / (1.0 + 0.9 * (RATIOS[n].powf(0.7) - 1.0));
            let r = (-6.9078 / (t * self.sr)).exp();
            let w = TAU * f / self.sr;
            // A soft blow excites the upper modes less (the felt's dwell).
            let a = WEIGHTS[n] * v.powf(1.0 + 0.55 * n as f32) * pitch_gain * wobble * 0.16;
            voice.modes[n] = Mode {
                re: 0.0,
                im: a,
                c_re: r * w.cos(),
                c_im: r * w.sin(),
            };
            voice.count = n + 1;
        }
        voice.level = 0.16 * v * pitch_gain;
        voice.level_decay = (-6.9078 / (t60 * self.sr)).exp();
        self.voices[slot] = voice;
    }

    /// Releases `key`: the damper falls on the reed.
    pub fn note_off(&mut self, key: u8) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.key == key.min(127))
        {
            v.released = true;
        }
    }

    /// Releases every note.
    pub fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.released = true;
        }
    }

    /// How many notes are sounding.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// One frame of the piano, `(left, right)` (the speaker is mono). Does not allocate.
    pub fn render(&mut self) -> (f32, f32) {
        let bark = self.params[dial::BARK];
        // Pickup: output = d / (1 - c d), with c below 1 for a displacement d in [-1, 1].
        let c = 0.75 * bark;
        let release = (-6.9078 / (0.12 * self.sr)).exp();
        let mut sum = 0.0f32;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let mut d = 0.0;
            for m in v.modes.iter_mut().take(v.count) {
                let re = m.re * m.c_re - m.im * m.c_im;
                let im = m.re * m.c_im + m.im * m.c_re;
                m.re = re;
                m.im = im;
                d += im;
            }
            d *= v.damper;
            // The pickup's response; a stray large value is held where it stays finite.
            let d = (d * 4.0).clamp(-1.0, 1.0);
            sum += d / (1.0 - c * d) * 0.25;
            v.age = v.age.saturating_add(1);
            v.level *= v.level_decay;
            if v.released {
                v.damper *= release;
                v.level *= release;
            }
            if v.level < 1e-5 {
                v.active = false;
            }
        }
        // Tremolo: a slow oscillator that moves the preamp's gain (and a little of the level).
        self.lfo += self.params[dial::TREMOLO_RATE] / self.sr;
        if self.lfo >= 1.0 {
            self.lfo -= 1.0;
        }
        let lfo = (self.lfo * TAU).sin();
        let depth = self.params[dial::TREMOLO_DEPTH];
        let x = self.pickup_hp.high(sum, self.k_pickup);
        let gain = (1.0 + 5.0 * bark) * (1.0 + 0.6 * depth * lfo).max(0.05);
        // Preamp: asymmetric tanh with first-order antiderivative anti-aliasing.
        let u = gain * x + BIAS;
        let f = ln_cosh(u);
        let du = u - self.u1;
        let y = if du.abs() > 1e-4 {
            (f - self.f1) / du
        } else {
            (0.5 * (u + self.u1)).tanh()
        };
        self.u1 = u;
        self.f1 = f;
        let y = (y - BIAS.tanh()) / (1.0 + 2.0 * bark).sqrt();
        let out = y - self.dc_in + 0.9993 * self.dc_out;
        self.dc_in = y;
        self.dc_out = out;
        let y = out * (1.0 + 0.25 * depth * lfo);
        // Speaker: high pass, tone low pass (two poles), a touch of cubic compression.
        let y = self.speaker_hp.high(y, self.k_speaker);
        let y = self.tone[0].low(y, self.k_tone);
        let y = self.tone[1].low(y, self.k_tone);
        let y = (y - 0.08 * y * y * y) * self.level_gain * 1.2;
        let y = if y.is_finite() {
            y.clamp(-4.0, 4.0)
        } else {
            0.0
        };
        (y, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn render(p: &mut EPiano, seconds: f32) -> Vec<f32> {
        (0..(seconds * SR) as usize).map(|_| p.render().0).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// The level of `x` at `hz` (a Goertzel-style single-bin DFT, normalised).
    fn bin(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let w = f64::from(TAU * hz * i as f32 / SR);
            re += f64::from(*v) * w.cos();
            im += f64::from(*v) * w.sin();
        }
        ((re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    /// The frequency in `lo..hi` where `x` is strongest.
    fn peak_hz(x: &[f32], lo: f32, hi: f32) -> f32 {
        let mut best = (0.0, lo);
        let mut f = lo;
        while f <= hi {
            let b = bin(x, f);
            if b > best.0 {
                best = (b, f);
            }
            f += 0.5;
        }
        best.1
    }

    fn piano(set: &[(usize, f32)]) -> EPiano {
        let mut v = defaults();
        for (i, x) in set {
            v[*i] = *x;
        }
        EPiano::new(SR, v)
    }

    #[test]
    fn it_is_silent_until_a_note_is_played() {
        let mut p = piano(&[]);
        assert!(render(&mut p, 0.2).iter().all(|x| *x == 0.0));
        assert_eq!(p.active_voices(), 0);
    }

    #[test]
    fn a_note_sounds_at_its_pitch_and_an_octave_up_doubles_it() {
        let mut p = piano(&[]);
        p.note_on(57, 100); // A3, 220 Hz
        let x = render(&mut p, 1.0);
        let hz = peak_hz(&x[4_800..], 200.0, 240.0);
        assert!((hz - 220.0).abs() < 1.5, "A3 sounded at {hz} Hz");
        let mut p = piano(&[]);
        p.note_on(69, 100);
        let x = render(&mut p, 1.0);
        let hz = peak_hz(&x[4_800..], 420.0, 460.0);
        assert!((hz - 440.0).abs() < 3.0, "A4 sounded at {hz} Hz");
    }

    #[test]
    fn the_reed_rings_down_and_the_damper_ends_it_when_the_key_comes_up() {
        let mut p = piano(&[]);
        p.note_on(57, 100);
        let x = render(&mut p, 4.0);
        let early = rms(&x[(0.2 * SR) as usize..(0.4 * SR) as usize]);
        let late = rms(&x[(3.0 * SR) as usize..(3.2 * SR) as usize]);
        assert!(early > 0.01, "the note is audible ({early})");
        assert!(late < early * 0.7, "it decays ({late} vs {early})");
        assert!(late > early * 0.005, "but it rings for seconds ({late})");
        p.note_off(57);
        let after = render(&mut p, 0.6);
        assert!(
            rms(&after[(0.4 * SR) as usize..]) < late * 0.05,
            "the damper stops it"
        );
        assert_eq!(p.active_voices(), 0, "the voice is free again");
    }

    #[test]
    fn a_harder_blow_is_louder_and_brighter() {
        let bright = |vel: u8| {
            let mut p = piano(&[]);
            p.note_on(57, vel);
            let x = render(&mut p, 0.5);
            let x = &x[2_400..];
            let hi: f32 = [1_000.0, 1_320.0, 1_760.0, 2_200.0]
                .iter()
                .map(|f| bin(x, *f))
                .sum();
            (rms(x), hi / bin(x, 220.0).max(1e-9))
        };
        let (soft_level, soft_bright) = bright(25);
        let (hard_level, hard_bright) = bright(127);
        assert!(
            hard_level > soft_level * 3.0,
            "{hard_level} vs {soft_level}"
        );
        assert!(
            hard_bright > soft_bright * 1.5,
            "{hard_bright} vs {soft_bright}"
        );
    }

    #[test]
    fn bark_adds_harmonics_and_the_tone_dial_darkens_the_sound() {
        let upper = |set: &[(usize, f32)]| {
            let mut p = piano(set);
            p.note_on(52, 110);
            let x = render(&mut p, 0.6);
            let x = &x[4_800..];
            let f0 = 164.81;
            let harmonics: f32 = (2..8).map(|k| bin(x, f0 * k as f32)).sum();
            harmonics / bin(x, f0).max(1e-9)
        };
        assert!(upper(&[(dial::BARK, 1.0)]) > upper(&[(dial::BARK, 0.0)]) * 1.5);
        let treble = |tone: f32| {
            let mut p = piano(&[(dial::TONE, tone), (dial::BARK, 1.0)]);
            p.note_on(52, 120);
            let x = render(&mut p, 0.4);
            let x = &x[4_800..];
            // The sound's first difference weighs the highs: its level next to the plain level.
            let diff: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&diff) / rms(x)
        };
        assert!(
            treble(1.0) > treble(0.0) * 1.3,
            "{} vs {}",
            treble(1.0),
            treble(0.0)
        );
    }

    #[test]
    fn the_tremolo_moves_the_level_and_off_it_does_not() {
        let swing = |depth: f32| {
            let mut p = piano(&[(dial::TREMOLO_DEPTH, depth), (dial::DECAY, 2.0)]);
            p.note_on(45, 100);
            let x = render(&mut p, 2.0);
            // The level in 20 ms windows over one second, once the attack is over.
            let win = (0.02 * SR) as usize;
            let levels: Vec<f32> = (0..50)
                .map(|i| rms(&x[SR as usize + i * win..][..win]))
                .collect();
            let hi = levels.iter().cloned().fold(f32::MIN, f32::max);
            let lo = levels.iter().cloned().fold(f32::MAX, f32::min);
            (hi - lo) / hi
        };
        let (still, moving) = (swing(0.0), swing(1.0));
        assert!(moving > still + 0.1, "tremolo {moving} vs none {still}");
    }

    #[test]
    fn the_tremolo_changes_the_timbre_and_not_only_the_volume() {
        // At the bottom and the top of the oscillator's swing the preamp's gain differs, so the
        // balance of harmonics differs too.
        let mut p = piano(&[
            (dial::TREMOLO_DEPTH, 1.0),
            (dial::TREMOLO_RATE, 2.0),
            (dial::BARK, 0.9),
            (dial::DECAY, 2.0),
        ]);
        p.note_on(45, 120);
        let x = render(&mut p, 2.0);
        let win = (0.1 * SR) as usize;
        let ratio = |start: f32| {
            let w = &x[(start * SR) as usize..][..win];
            let f0 = 110.0;
            let harm: f32 = (3..8).map(|k| bin(w, f0 * k as f32)).sum();
            harm / bin(w, f0).max(1e-9)
        };
        // At 2 Hz the swing's peaks are a quarter period (0.125 s) and three quarters apart.
        let (a, b) = (ratio(0.6), ratio(0.85));
        assert!((a - b).abs() / a.max(b) > 0.05, "{a} vs {b}");
    }

    #[test]
    fn the_longest_ringing_low_notes_and_the_shortest_high_ones_differ() {
        let ring = |key: u8| {
            let mut p = piano(&[]);
            p.note_on(key, 100);
            let x = render(&mut p, 3.0);
            rms(&x[(2.0 * SR) as usize..]) / rms(&x[(0.1 * SR) as usize..(0.5 * SR) as usize])
        };
        assert!(ring(36) > ring(90) * 2.0);
    }

    #[test]
    fn many_notes_steal_the_oldest_voice_and_stay_bounded() {
        let mut p = piano(&[(dial::BARK, 1.0), (dial::LEVEL, 6.0)]);
        for k in 0..60u8 {
            p.note_on(36 + k, 127);
            for _ in 0..200 {
                let (l, _) = p.render();
                assert!(l.is_finite() && l.abs() <= 4.0);
            }
        }
        assert_eq!(p.active_voices(), VOICES);
        let x = render(&mut p, 0.3);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() <= 4.0));
        // A chord of six hard notes keeps its head room at the normal level.
        let mut q = piano(&[]);
        for k in [48u8, 52, 55, 59, 62, 67] {
            q.note_on(k, 127);
        }
        let x = render(&mut q, 1.0);
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1 && peak < 1.5, "chord peak {peak}");
    }

    #[test]
    fn all_notes_off_and_stray_note_offs_are_harmless() {
        let mut p = piano(&[]);
        p.note_off(60);
        p.note_on(60, 100);
        p.note_on(64, 100);
        p.all_notes_off();
        let x = render(&mut p, 1.0);
        assert!(rms(&x[(0.8 * SR) as usize..]) < 1e-3);
        assert_eq!(p.active_voices(), 0);
    }

    #[test]
    fn hostile_dials_and_extreme_notes_never_make_nan() {
        for bad in [f32::NAN, f32::INFINITY, -1e30, 1e30] {
            let mut p = EPiano::new(SR, [bad; PARAMS]);
            for k in [0u8, 12, 60, 127, 200] {
                p.note_on(k, if k % 2 == 0 { 0 } else { 255 });
            }
            for _ in 0..20_000 {
                let (l, r) = p.render();
                assert!(l.is_finite() && r.is_finite() && l.abs() <= 4.0);
            }
        }
        // Silly sample rates are put into range.
        for sr in [f32::NAN, 1.0, 1e9] {
            let mut p = EPiano::new(sr, defaults());
            p.note_on(60, 100);
            assert!((0..5_000).all(|_| p.render().0.is_finite()));
        }
    }

    #[test]
    fn rendering_does_not_allocate() {
        let mut p = piano(&[(dial::TREMOLO_DEPTH, 0.5)]);
        assert_no_alloc::assert_no_alloc(|| {
            for k in 40..80u8 {
                p.note_on(k, 100);
            }
            for _ in 0..4_800 {
                p.render();
            }
            p.note_off(50);
            p.all_notes_off();
            p.set_params(defaults());
        });
    }

    #[test]
    fn the_dials_are_clamped_and_named() {
        assert_eq!(clamp([f32::NAN; PARAMS]), defaults());
        let c = clamp([9.0, 99.0, 9.0, 9.0, 9.0, 99.0]);
        assert_eq!(c, [1.0, 10.0, 1.0, 1.0, 2.0, 6.0]);
        assert_eq!(DIALS[dial::BARK].name, "Bark");
    }
}
