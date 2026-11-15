//! The Drum Machine: sixteen pads, each a synthesized drum or a sample, played from the keys 36
//! to 51 (the General MIDI drum layout), as a track instrument. Logic Pro equivalent: Drum
//! Machine Designer, with its pads, synthesized drums, one-shot samples and choke groups.
//!
//! Every synthesized drum is made from oscillators and noise, with no samples, in the manner of
//! the classic analogue machines:
//!
//! - **Kick:** a sine whose pitch falls fast from a punch to its base, with a click.
//! - **Snare:** two tuned bodies and a burst of high-passed noise (*Tone* mixes the noise in).
//! - **Clap:** three quick noise bursts and a tail through a band pass.
//! - **Closed and open hi-hat:** six square oscillators at metallic ratios, high-passed, with
//!   noise; they differ in length.
//! - **Tom:** a sine with a smaller pitch drop.
//! - **Cymbal:** the same metallic mix with a long decay.
//! - **Cowbell** and **Rim shot:** two square tones, and a very short click tone.
//!
//! A *Sample* pad plays a loaded sample once. Each pad has Tune (semitones), Decay (a time
//! multiplier; for a sample, the part of it that plays), Tone, Level, Pan and a Choke group:
//! starting a pad cuts the sounding drums of the same group (a closed hi-hat cutting an open
//! one). Drums ignore the key coming up. 24 voices; the oldest is stolen.
//!
//! Loading samples reads files, so a kit with sample pads is built off the audio thread (the app
//! does it in the background); changing a pad's settings is allocation free and works while it
//! plays.

use std::f32::consts::{FRAC_PI_4, TAU};
use std::sync::Arc;

pub use crate::sample_file::SampleData;

/// Pads, and the key of the first one.
pub const PADS: usize = 16;
pub const FIRST_KEY: u8 = 36;
/// Drums that can sound at once.
pub const VOICES: usize = 24;
/// The highest choke group number.
pub const MAX_CHOKE: u8 = 8;

/// What a pad plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadKind {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    Tom,
    Cymbal,
    Cowbell,
    Rim,
    Sample,
}

impl PadKind {
    pub const ALL: [PadKind; 10] = [
        PadKind::Kick,
        PadKind::Snare,
        PadKind::Clap,
        PadKind::ClosedHat,
        PadKind::OpenHat,
        PadKind::Tom,
        PadKind::Cymbal,
        PadKind::Cowbell,
        PadKind::Rim,
        PadKind::Sample,
    ];

    /// The name shown on a pad.
    pub fn name(self) -> &'static str {
        match self {
            PadKind::Kick => "Kick",
            PadKind::Snare => "Snare",
            PadKind::Clap => "Clap",
            PadKind::ClosedHat => "Closed Hi-Hat",
            PadKind::OpenHat => "Open Hi-Hat",
            PadKind::Tom => "Tom",
            PadKind::Cymbal => "Cymbal",
            PadKind::Cowbell => "Cowbell",
            PadKind::Rim => "Rim Shot",
            PadKind::Sample => "Sample",
        }
    }

    /// The kind with this name (as saved in a project).
    pub fn from_name(name: &str) -> Option<PadKind> {
        PadKind::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(name.trim()))
    }

    /// A new pad of this kind with its natural settings.
    pub fn settings(self) -> PadSettings {
        let (tone, choke) = match self {
            PadKind::Kick => (0.5, 0),
            PadKind::Snare => (0.6, 0),
            PadKind::Clap => (0.5, 0),
            PadKind::ClosedHat => (0.6, 1),
            PadKind::OpenHat => (0.6, 1),
            PadKind::Tom => (0.4, 0),
            PadKind::Cymbal => (0.6, 0),
            PadKind::Cowbell => (0.5, 0),
            PadKind::Rim => (0.5, 0),
            PadKind::Sample => (1.0, 0),
        };
        PadSettings {
            kind: self,
            tune: 0.0,
            decay: 1.0,
            tone,
            level_db: 0.0,
            pan: 0.0,
            choke,
        }
    }
}

/// One pad's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PadSettings {
    pub kind: PadKind,
    /// Pitch in semitones, -12 to 12.
    pub tune: f32,
    /// How long it rings, 0.2 to 4 times the natural time. For a sample, the part of it that
    /// plays (1 and up is all of it).
    pub decay: f32,
    /// Brightness, 0 to 1 (the meaning depends on the drum; for a sample, its low pass).
    pub tone: f32,
    /// Level in dB, -24 to 6.
    pub level_db: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    /// Choke group, 0 (none) to [`MAX_CHOKE`].
    pub choke: u8,
}

impl PadSettings {
    /// The settings put into range (a value that is not finite becomes its natural value).
    pub fn clamped(self) -> Self {
        let natural = self.kind.settings();
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        Self {
            kind: self.kind,
            tune: f(self.tune, -12.0, 12.0, 0.0),
            decay: f(self.decay, 0.2, 4.0, 1.0),
            tone: f(self.tone, 0.0, 1.0, natural.tone),
            level_db: f(self.level_db, -24.0, 6.0, 0.0),
            pan: f(self.pan, -1.0, 1.0, 0.0),
            choke: self.choke.min(MAX_CHOKE),
        }
    }
}

/// The key a pad plays.
pub fn key_of(pad: usize) -> u8 {
    FIRST_KEY + pad.min(PADS - 1) as u8
}

/// The pad a key plays, if it is one of the sixteen.
pub fn pad_of(key: u8) -> Option<usize> {
    (FIRST_KEY..FIRST_KEY + PADS as u8)
        .contains(&key)
        .then(|| usize::from(key - FIRST_KEY))
}

/// The starting kit: the General MIDI drum layout from 36 to 51 (kick, rim, snare, clap, a tight
/// snare, three toms, closed, pedal and open hats, more toms, crash and ride).
pub fn default_kit() -> [PadSettings; PADS] {
    let pad = |kind: PadKind, f: &dyn Fn(&mut PadSettings)| {
        let mut s = kind.settings();
        f(&mut s);
        s
    };
    [
        pad(PadKind::Kick, &|_| {}),
        pad(PadKind::Rim, &|_| {}),
        pad(PadKind::Snare, &|_| {}),
        pad(PadKind::Clap, &|_| {}),
        pad(PadKind::Snare, &|s| {
            s.tune = 3.0;
            s.tone = 0.85;
            s.decay = 0.7;
        }),
        pad(PadKind::Tom, &|s| s.tune = -8.0),
        pad(PadKind::ClosedHat, &|_| {}),
        pad(PadKind::Tom, &|s| s.tune = -6.0),
        pad(PadKind::ClosedHat, &|s| s.decay = 1.6),
        pad(PadKind::Tom, &|s| s.tune = -3.0),
        pad(PadKind::OpenHat, &|_| {}),
        pad(PadKind::Tom, &|s| s.tune = 0.0),
        pad(PadKind::Tom, &|s| s.tune = 2.0),
        pad(PadKind::Cymbal, &|s| s.decay = 1.5),
        pad(PadKind::Tom, &|s| s.tune = 5.0),
        pad(PadKind::Cymbal, &|s| {
            s.decay = 0.6;
            s.tone = 0.8;
        }),
    ]
}

/// A one-pole filter's memory.
#[derive(Clone, Copy, Default)]
struct OnePole(f32);

impl OnePole {
    #[inline]
    fn low(&mut self, x: f32, k: f32) -> f32 {
        self.0 += (x - self.0) * k;
        self.0
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

/// The frequencies of the six oscillators of the metallic sound, in Hz (the classic ratios).
const METAL: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    pad: u8,
    kind: PadKind,
    choke: u8,
    /// Frames since the hit.
    n: u32,
    vel: f32,
    tune_ratio: f32,
    decay: f32,
    tone: f32,
    gain_l: f32,
    gain_r: f32,
    ph: [f32; 6],
    f1: OnePole,
    f2: OnePole,
    /// A sample pad's position, in frames of the sample.
    pos: f64,
    /// 1 normally; falling when the voice is being cut by a choke.
    fade: f32,
    fade_mul: f32,
}

impl Voice {
    const IDLE: Voice = Voice {
        active: false,
        pad: 0,
        kind: PadKind::Kick,
        choke: 0,
        n: 0,
        vel: 0.0,
        tune_ratio: 1.0,
        decay: 1.0,
        tone: 0.5,
        gain_l: 0.0,
        gain_r: 0.0,
        ph: [0.0; 6],
        f1: OnePole(0.0),
        f2: OnePole(0.0),
        pos: 0.0,
        fade: 1.0,
        fade_mul: 1.0,
    };
}

/// The drum machine: the pads and their voices.
pub struct DrumMachine {
    sr: f32,
    pads: [PadSettings; PADS],
    samples: [Option<Arc<SampleData>>; PADS],
    voices: [Voice; VOICES],
    noise: u32,
}

impl DrumMachine {
    /// A machine at `sample_rate` Hz with `pads` and the samples of its sample pads. Allocation
    /// free; build it off the audio thread when it has samples (the app does).
    pub fn new(
        pads: [PadSettings; PADS],
        samples: [Option<Arc<SampleData>>; PADS],
        sample_rate: f32,
    ) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(8_000.0, 384_000.0)
        } else {
            48_000.0
        };
        Self {
            sr,
            pads: pads.map(PadSettings::clamped),
            samples,
            voices: [Voice::IDLE; VOICES],
            noise: 0x1234_5678,
        }
    }

    /// New settings for pad `index` (put into range); allocation free, so a pad can be tuned
    /// while the kit plays. A pad that is a different kind keeps its sample, if it has one.
    pub fn set_pad(&mut self, index: usize, settings: PadSettings) {
        if let Some(p) = self.pads.get_mut(index) {
            *p = settings.clamped();
        }
    }

    /// The pads' settings.
    pub fn pads(&self) -> &[PadSettings; PADS] {
        &self.pads
    }

    /// How many drums are sounding.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Hits the pad `key` plays (36 to 51; any other key does nothing) at `velocity`.
    pub fn note_on(&mut self, key: u8, velocity: u8) {
        let Some(pad) = pad_of(key) else {
            return;
        };
        let s = self.pads[pad];
        if s.kind == PadKind::Sample
            && self.samples[pad]
                .as_ref()
                .is_none_or(|d| d.frames.is_empty())
        {
            return;
        }
        // Choke: the sounding drums of this pad's group are cut short.
        if s.choke > 0 {
            let cut = (-6.9078 / (0.012 * self.sr)).exp();
            for v in self
                .voices
                .iter_mut()
                .filter(|v| v.active && v.choke == s.choke)
            {
                v.fade_mul = cut;
            }
        }
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, v)| v.n)
                    .map(|(i, _)| i)
            })
            .unwrap_or(0);
        let vel = f32::from(velocity.clamp(1, 127)) / 127.0;
        let gain = vel.powf(1.3) * 10f32.powf(s.level_db / 20.0);
        let angle = (s.pan + 1.0) * FRAC_PI_4;
        self.voices[slot] = Voice {
            active: true,
            pad: pad as u8,
            kind: s.kind,
            choke: s.choke,
            vel,
            tune_ratio: 2f32.powf(s.tune / 12.0),
            decay: s.decay,
            tone: s.tone,
            gain_l: gain * angle.cos(),
            gain_r: gain * angle.sin(),
            ..Voice::IDLE
        };
    }

    /// Drums ignore the key coming up.
    pub fn note_off(&mut self, _key: u8) {}

    /// Cuts every sounding drum (a quick fade).
    pub fn all_notes_off(&mut self) {
        let cut = (-6.9078 / (0.02 * self.sr)).exp();
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.fade_mul = cut;
        }
    }

    #[inline]
    fn white(&mut self) -> f32 {
        // xorshift32, scaled to -1..1.
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// One frame of the machine, `(left, right)`. Does not allocate.
    pub fn render(&mut self) -> (f32, f32) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        let sr = self.sr;
        for i in 0..VOICES {
            if !self.voices[i].active {
                continue;
            }
            let noise = self.white();
            let mut v = self.voices[i];
            let s = self.voice_sample(&mut v, noise);
            v.fade *= v.fade_mul;
            let out = s * v.fade;
            l += out * v.gain_l;
            r += out * v.gain_r;
            v.n = v.n.saturating_add(1);
            if v.fade < 1e-3 || !out.is_finite() {
                v.active = false;
            }
            self.voices[i] = v;
            let _ = sr;
        }
        (
            if l.is_finite() {
                l.clamp(-4.0, 4.0)
            } else {
                0.0
            },
            if r.is_finite() {
                r.clamp(-4.0, 4.0)
            } else {
                0.0
            },
        )
    }

    /// The next sample of voice `v`; sets `v.active` false when the drum is over.
    fn voice_sample(&self, v: &mut Voice, noise: f32) -> f32 {
        let sr = self.sr;
        let t = v.n as f32 / sr;
        let d = v.decay;
        let end = |v: &mut Voice, tau: f32| {
            // 60 dB down: the drum is over.
            if t > 6.9 * tau {
                v.active = false;
            }
        };
        let step = |ph: &mut f32, hz: f32| {
            *ph += TAU * hz / sr;
            if *ph > TAU {
                *ph -= TAU;
            }
            *ph
        };
        match v.kind {
            PadKind::Kick => {
                let tau = 0.16 * d;
                let base = 46.0 * v.tune_ratio;
                let punch = 1.0 + (2.5 + 3.0 * v.vel) * (-t / 0.035).exp();
                let ph = step(&mut v.ph[0], base * punch);
                let body = ph.sin() * (-t / tau).exp();
                let click = noise * (-t / 0.0025).exp() * 0.35 * v.tone;
                end(v, tau);
                ((body * 1.4 + click).tanh()) * 0.85
            }
            PadKind::Snare => {
                let tau_b = 0.07 * d;
                let tau_n = 0.16 * d;
                let f = 185.0 * v.tune_ratio;
                let drop = 1.0 + 0.4 * (-t / 0.02).exp();
                let a = step(&mut v.ph[0], f * drop).sin();
                let b = step(&mut v.ph[1], f * 1.78 * drop).sin();
                let body = (a * 0.7 + b * 0.4) * (-t / tau_b).exp();
                let hp = v.f1.high(noise, corner(1_800.0, sr));
                let hiss = v.f2.low(hp, corner(9_000.0, sr)) * (-t / tau_n).exp();
                end(v, tau_n);
                (body * (1.0 - 0.4 * v.tone) + hiss * (0.35 + 0.9 * v.tone)) * 0.9
            }
            PadKind::Clap => {
                let tail = 0.18 * d;
                let burst = [0.0, 0.011, 0.022]
                    .iter()
                    .map(|t0| {
                        if t >= *t0 {
                            (-(t - *t0) / 0.004).exp()
                        } else {
                            0.0
                        }
                    })
                    .sum::<f32>();
                let env = if t < 0.033 {
                    burst * 0.6
                } else {
                    0.6 * (-(t - 0.033) / tail).exp()
                };
                let band = v.f2.low(
                    v.f1.high(noise, corner(700.0, sr)),
                    corner(2_800.0 + 2_000.0 * v.tone, sr),
                );
                end(v, tail + 0.033);
                band * env * 1.8
            }
            PadKind::ClosedHat | PadKind::OpenHat => {
                let tau = if v.kind == PadKind::OpenHat {
                    0.32
                } else {
                    0.035
                } * d;
                let metal = self.metal(v) + noise * 0.3;
                let out =
                    v.f2.high(v.f1.high(metal, corner(6_500.0, sr)), corner(6_500.0, sr));
                // A little attack so the hit does not click.
                let attack = (t / 0.0008).min(1.0);
                end(v, tau);
                out * (-t / tau).exp() * attack * (0.35 + 0.5 * v.tone) * 7.0
            }
            PadKind::Cymbal => {
                let tau = 0.6 * d;
                let metal = self.metal(v) * 0.7 + noise * 0.5;
                let out = v.f2.high(
                    v.f1.high(metal, corner(3_500.0 + 3_500.0 * v.tone, sr)),
                    corner(3_500.0, sr),
                );
                let attack = (t / 0.002).min(1.0);
                end(v, tau);
                out * (-t / tau).exp() * attack * 0.45 * 5.0
            }
            PadKind::Tom => {
                let tau = 0.26 * d;
                let base = 105.0 * v.tune_ratio;
                let drop = 1.0 + 0.9 * (-t / 0.045).exp();
                let body = step(&mut v.ph[0], base * drop).sin() * (-t / tau).exp();
                let click = noise * (-t / 0.003).exp() * 0.2 * v.tone;
                end(v, tau);
                (body + click) * 0.9
            }
            PadKind::Cowbell => {
                let tau = 0.3 * d;
                let sq = |p: f32| if p.sin() >= 0.0 { 1.0 } else { -1.0 };
                let a = sq(step(&mut v.ph[0], 540.0 * v.tune_ratio));
                let b = sq(step(&mut v.ph[1], 800.0 * v.tune_ratio));
                let band = v.f2.low(
                    v.f1.high(a + b, corner(500.0, sr)),
                    corner(2_500.0 + 3_000.0 * v.tone, sr),
                );
                let env = 0.6 * (-t / 0.012).exp() + 0.4 * (-t / tau).exp();
                end(v, tau);
                band * env * 0.5
            }
            PadKind::Rim => {
                let tau = 0.012 * d;
                let body = step(&mut v.ph[0], 1_750.0 * v.tune_ratio).sin() * (-t / tau).exp();
                let click = noise * (-t / 0.0015).exp() * 0.3;
                end(v, tau);
                (body * 0.9 + click) * (0.6 + 0.4 * v.tone)
            }
            PadKind::Sample => {
                let Some(sample) = self.samples[usize::from(v.pad)].as_ref() else {
                    v.active = false;
                    return 0.0;
                };
                let frames = &sample.frames;
                let rate = f64::from(sample.sample_rate.max(1)) / f64::from(sr);
                let step = (f64::from(v.tune_ratio) * rate).clamp(1e-4, 64.0);
                // The part of the sample that plays, and a short fade at its end.
                let limit = ((frames.len() as f64) * f64::from(v.decay.min(1.0))).max(2.0);
                let fade_len = (0.005 * f64::from(sample.sample_rate)).max(1.0);
                if v.pos >= limit.min(frames.len() as f64) {
                    v.active = false;
                    return 0.0;
                }
                let i = v.pos.floor() as i64;
                let f = (v.pos - i as f64) as f32;
                let at = |j: i64| -> f32 {
                    if j >= 0 && (j as usize) < frames.len() {
                        let fr = frames[j as usize];
                        0.5 * (fr[0] + fr[1])
                    } else {
                        0.0
                    }
                };
                let (a, b, c, dd) = (at(i - 1), at(i), at(i + 1), at(i + 2));
                let s = b + 0.5
                    * f
                    * (c - a
                        + f * (2.0 * a - 5.0 * b + 4.0 * c - dd + f * (3.0 * (b - c) + dd - a)));
                let tail = (((limit - v.pos) / fade_len) as f32).clamp(0.0, 1.0);
                v.pos += step;
                // Tone is a low pass for a sample: fully open at 1.
                let s = if v.tone < 0.999 {
                    v.f1.low(s, corner(200.0 * 100f32.powf(v.tone), sr))
                } else {
                    s
                };
                s * tail
            }
        }
    }

    /// The metallic mix: six square oscillators at the classic ratios.
    fn metal(&self, v: &mut Voice) -> f32 {
        let sr = self.sr;
        let mut sum = 0.0;
        for (k, hz) in METAL.iter().enumerate() {
            v.ph[k] += TAU * hz * v.tune_ratio * 1.6 / sr;
            if v.ph[k] > TAU {
                v.ph[k] -= TAU;
            }
            sum += if v.ph[k] < std::f32::consts::PI {
                1.0
            } else {
                -1.0
            };
        }
        sum / 6.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn kit() -> DrumMachine {
        DrumMachine::new(default_kit(), std::array::from_fn(|_| None), SR)
    }

    fn render(m: &mut DrumMachine, secs: f32) -> Vec<f32> {
        (0..(secs * SR) as usize).map(|_| m.render().0).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// How long until the output stays under 1e-3, in seconds (0 for silence from the start).
    fn length(x: &[f32]) -> f32 {
        x.iter()
            .rposition(|v| v.abs() > 1e-3)
            .map_or(0.0, |i| i as f32 / SR)
    }

    /// The level of `x` at `hz`.
    fn bin(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let w = f64::from(TAU * hz * i as f32 / SR);
            re += f64::from(*v) * w.cos();
            im += f64::from(*v) * w.sin();
        }
        ((re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    /// The first difference weighs the highs: its level over the plain level.
    fn brightness(x: &[f32]) -> f32 {
        let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(x).max(1e-9)
    }

    fn hit(kind: PadKind, f: impl Fn(&mut PadSettings)) -> Vec<f32> {
        let mut pads = default_kit();
        pads[0] = kind.settings();
        f(&mut pads[0]);
        let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), SR);
        m.note_on(36, 127);
        render(&mut m, 6.0)
    }

    #[test]
    fn it_is_silent_until_a_pad_is_hit_and_only_the_sixteen_keys_are_pads() {
        let mut m = kit();
        assert!(render(&mut m, 0.1).iter().all(|x| *x == 0.0));
        for key in [0u8, 35, 52, 127] {
            m.note_on(key, 127);
            assert_eq!(m.active_voices(), 0, "key {key}");
        }
        for key in 36..=51u8 {
            m.note_on(key, 127);
        }
        assert_eq!(m.active_voices(), 16);
        assert_eq!(
            (pad_of(36), pad_of(51), pad_of(52), key_of(0), key_of(15)),
            (Some(0), Some(15), None, 36, 51)
        );
        // Drums ignore the key coming up.
        m.note_off(36);
        assert_eq!(m.active_voices(), 16);
    }

    #[test]
    fn every_synthesized_drum_sounds_decays_to_silence_and_has_head_room() {
        for kind in PadKind::ALL.into_iter().filter(|k| *k != PadKind::Sample) {
            let x = hit(kind, |_| {});
            let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.15 && peak < 1.5, "{kind:?} peaks at {peak}");
            assert!(x.iter().all(|v| v.is_finite()), "{kind:?}");
            let len = length(&x);
            assert!(len > 0.01 && len < 5.0, "{kind:?} lasts {len} s");
            assert!(rms(&x[(5.5 * SR) as usize..]) < 1e-3, "{kind:?} has ended");
            // No DC left behind.
            let mean = x.iter().sum::<f32>() / x.len() as f32;
            assert!(mean.abs() < 0.01, "{kind:?} DC {mean}");
        }
    }

    #[test]
    fn the_kick_is_low_falls_in_pitch_and_tune_moves_it_an_octave() {
        let x = hit(PadKind::Kick, |_| {});
        let low = [40.0, 50.0, 60.0, 80.0]
            .iter()
            .map(|f| bin(&x[2_400..24_000], *f))
            .fold(0.0, f32::max);
        let high = bin(&x[2_400..24_000], 2_000.0);
        assert!(low > high * 20.0, "mostly low: {low} vs {high}");
        let up = hit(PadKind::Kick, |s| s.tune = 12.0);
        let at = |x: &[f32], lo: f32, hi: f32| {
            (lo as i32..hi as i32)
                .step_by(2)
                .map(|f| (bin(&x[4_800..24_000], f as f32), f))
                .fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a })
                .1
        };
        let (base, octave) = (at(&x, 30.0, 90.0), at(&up, 60.0, 180.0));
        assert!(octave as f32 > base as f32 * 1.7, "{base} -> {octave}");
    }

    #[test]
    fn hats_and_snares_are_bright_and_a_kick_is_not_and_open_rings_longer_than_closed() {
        let kick = brightness(&hit(PadKind::Kick, |_| {}));
        let snare = brightness(&hit(PadKind::Snare, |_| {}));
        let hat = brightness(&hit(PadKind::ClosedHat, |_| {}));
        assert!(hat > snare && snare > kick * 3.0, "{kick} {snare} {hat}");
        let closed = length(&hit(PadKind::ClosedHat, |_| {}));
        let open = length(&hit(PadKind::OpenHat, |_| {}));
        assert!(open > closed * 3.0, "{open} vs {closed}");
        // The snare's Tone mixes in more noise: brighter.
        let dull = brightness(&hit(PadKind::Snare, |s| s.tone = 0.0));
        let snappy = brightness(&hit(PadKind::Snare, |s| s.tone = 1.0));
        assert!(snappy > dull * 1.3, "{dull} vs {snappy}");
    }

    #[test]
    fn decay_stretches_a_drum_velocity_and_level_scale_it_and_pan_places_it() {
        let short = length(&hit(PadKind::Tom, |s| s.decay = 0.5));
        let long = length(&hit(PadKind::Tom, |s| s.decay = 2.0));
        assert!(long > short * 2.5, "{long} vs {short}");
        let level = |vel: u8, db: f32| {
            let mut pads = default_kit();
            pads[0].level_db = db;
            let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), SR);
            m.note_on(36, vel);
            rms(&render(&mut m, 0.3))
        };
        assert!(level(127, 0.0) > level(40, 0.0) * 3.0);
        assert!((level(127, 0.0) / level(127, -6.0) - 2.0).abs() < 0.1);
        let mut pads = default_kit();
        pads[2].pan = 1.0;
        let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), SR);
        m.note_on(38, 127);
        let (mut l, mut r) = (0f32, 0f32);
        for _ in 0..9_600 {
            let (a, b) = m.render();
            l += a.abs();
            r += b.abs();
        }
        assert!(l < r * 0.01, "panned right: {l} vs {r}");
    }

    #[test]
    fn a_closed_hat_chokes_an_open_one_and_other_groups_are_left_alone() {
        let mut m = kit(); // 42 closed and 46 open are both in choke group 1
        m.note_on(46, 127);
        render(&mut m, 0.1);
        assert!(m.active_voices() >= 1);
        m.note_on(42, 127);
        render(&mut m, 0.3);
        // The open hat was cut within a few milliseconds; the closed one rings only 35 ms.
        assert_eq!(m.active_voices(), 0);
        // A kick (no group) is not cut by a hat.
        let mut m = kit();
        m.note_on(36, 127);
        render(&mut m, 0.05);
        m.note_on(42, 127);
        render(&mut m, 0.1);
        assert!(m.active_voices() >= 1, "the kick still rings");
        // Without a group the open hat rings on.
        let mut pads = default_kit();
        pads[6].choke = 0;
        pads[10].choke = 0;
        let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), SR);
        m.note_on(46, 127);
        render(&mut m, 0.05);
        m.note_on(42, 127);
        render(&mut m, 0.15);
        assert!(m.active_voices() >= 1);
    }

    fn click_sample(frames: usize) -> Arc<SampleData> {
        let v: Vec<[f32; 2]> = (0..frames)
            .map(|i| if i < 200 { [0.6, 0.6] } else { [0.3, 0.3] })
            .collect();
        Arc::new(SampleData::new(v, 48_000))
    }

    #[test]
    fn a_sample_pad_plays_its_sample_once_tune_decay_and_tone_shape_it_and_no_sample_is_silent() {
        let mut pads = default_kit();
        pads[1] = PadKind::Sample.settings();
        let mut samples: [Option<Arc<SampleData>>; PADS] = std::array::from_fn(|_| None);
        samples[1] = Some(click_sample(24_000));
        let mut m = DrumMachine::new(pads, samples.clone(), SR);
        m.note_on(37, 127);
        let x = render(&mut m, 1.0);
        assert!(rms(&x[..4_800]) > 0.1);
        let len = length(&x);
        assert!((len - 0.5).abs() < 0.02, "plays its 0.5 s once ({len})");
        assert_eq!(m.active_voices(), 0);
        // An octave up plays twice as fast: half as long.
        pads[1].tune = 12.0;
        let mut m = DrumMachine::new(pads, samples.clone(), SR);
        m.note_on(37, 127);
        assert!((length(&render(&mut m, 1.0)) - 0.25).abs() < 0.02);
        // Decay below 1 plays only that part of it.
        pads[1].tune = 0.0;
        pads[1].decay = 0.4;
        let mut m = DrumMachine::new(pads, samples.clone(), SR);
        m.note_on(37, 127);
        assert!((length(&render(&mut m, 1.0)) - 0.2).abs() < 0.02);
        // A dark Tone takes the highs out of it.
        pads[1].decay = 1.0;
        let bright = {
            let mut m = DrumMachine::new(pads, samples.clone(), SR);
            m.note_on(37, 127);
            brightness(&render(&mut m, 0.5))
        };
        pads[1].tone = 0.1;
        let dark = {
            let mut m = DrumMachine::new(pads, samples.clone(), SR);
            m.note_on(37, 127);
            brightness(&render(&mut m, 0.5))
        };
        assert!(dark < bright, "{dark} vs {bright}");
        // A sample pad with no sample plays nothing.
        let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), SR);
        m.note_on(37, 127);
        assert_eq!(m.active_voices(), 0);
    }

    #[test]
    fn more_hits_than_voices_steal_the_oldest_and_stay_bounded() {
        let mut m = kit();
        for i in 0..60u32 {
            m.note_on(36 + (i % 16) as u8, 127);
            for _ in 0..50 {
                let (l, r) = m.render();
                assert!(l.is_finite() && r.is_finite() && l.abs() <= 4.0);
            }
        }
        assert!(m.active_voices() <= VOICES);
        // All notes off fades everything out.
        m.all_notes_off();
        let x = render(&mut m, 0.3);
        assert!(rms(&x[(0.25 * SR) as usize..]) < 1e-3);
        assert_eq!(m.active_voices(), 0);
    }

    #[test]
    fn settings_are_clamped_and_hostile_ones_never_make_nan() {
        let wild = PadSettings {
            kind: PadKind::Snare,
            tune: f32::NAN,
            decay: f32::INFINITY,
            tone: -9.0,
            level_db: 1e30,
            pan: f32::NAN,
            choke: 200,
        }
        .clamped();
        assert_eq!(
            (
                wild.tune,
                wild.decay,
                wild.tone,
                wild.level_db,
                wild.pan,
                wild.choke
            ),
            (0.0, 1.0, 0.0, 6.0, 0.0, MAX_CHOKE)
        );
        for kind in PadKind::ALL {
            let mut pads = [kind.settings(); PADS];
            for p in pads.iter_mut() {
                p.tune = f32::NAN;
                p.decay = -5.0;
                p.level_db = f32::NEG_INFINITY;
            }
            let mut m = DrumMachine::new(pads, std::array::from_fn(|_| None), f32::NAN);
            for key in 30..60u8 {
                m.note_on(key, if key % 2 == 0 { 0 } else { 255 });
            }
            for _ in 0..20_000 {
                let (l, r) = m.render();
                assert!(l.is_finite() && r.is_finite());
            }
        }
        // set_pad takes new settings (clamped) and ignores a pad that is not there.
        let mut m = kit();
        let mut s = PadKind::Cowbell.settings();
        s.tune = 99.0;
        m.set_pad(3, s);
        m.set_pad(99, s);
        assert_eq!(
            (m.pads()[3].kind, m.pads()[3].tune),
            (PadKind::Cowbell, 12.0)
        );
    }

    #[test]
    fn the_default_kit_is_the_general_midi_layout_and_kinds_have_names() {
        let kit = default_kit();
        assert_eq!(kit[0].kind, PadKind::Kick);
        assert_eq!(kit[2].kind, PadKind::Snare);
        assert_eq!(kit[3].kind, PadKind::Clap);
        assert_eq!(
            (kit[6].kind, kit[10].kind),
            (PadKind::ClosedHat, PadKind::OpenHat)
        );
        assert_eq!(
            (kit[13].kind, kit[15].kind),
            (PadKind::Cymbal, PadKind::Cymbal)
        );
        assert_eq!(kit[6].choke, kit[10].choke);
        assert!(kit[6].choke > 0);
        for k in PadKind::ALL {
            assert_eq!(PadKind::from_name(k.name()), Some(k));
        }
        assert_eq!(PadKind::from_name("  kick "), Some(PadKind::Kick));
        assert_eq!(PadKind::from_name("tuba"), None);
    }

    #[test]
    fn rendering_hitting_and_tuning_pads_do_not_allocate() {
        let mut pads = default_kit();
        pads[1] = PadKind::Sample.settings();
        let mut samples: [Option<Arc<SampleData>>; PADS] = std::array::from_fn(|_| None);
        samples[1] = Some(click_sample(4_800));
        let mut m = DrumMachine::new(pads, samples, SR);
        assert_no_alloc::assert_no_alloc(|| {
            for key in 36..52u8 {
                m.note_on(key, 100);
            }
            for _ in 0..4_800 {
                m.render();
            }
            m.set_pad(2, PadKind::Clap.settings());
            m.note_off(36);
            m.all_notes_off();
        });
    }
}
