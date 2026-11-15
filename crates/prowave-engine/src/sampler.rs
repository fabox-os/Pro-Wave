//! The Sampler: plays recorded samples across the keyboard, the way Logic Pro's Sampler (the
//! EXS24 successor) does, as a track instrument.
//!
//! An instrument is a list of [`Zone`]s. Each zone is a sample with the keys and velocities it
//! answers to, its root key and tuning, level and pan, a loop, an amplitude envelope (attack,
//! hold, decay, sustain, release), and optionally a round-robin turn and a choke group:
//!
//! - **Pitch:** a note plays the sample faster or slower by resampling (4-point cubic
//!   interpolation), `2^((key - root) / 12)` times, plus the zone's tuning.
//! - **Velocity layers:** zones whose velocity ranges differ are chosen by the note's velocity;
//!   zones that overlap play together (a layered sound). The note's velocity also sets its level.
//! - **Loops:** `NoLoop` plays to the end, `OneShot` plays to the end even after the key comes
//!   up (drums), `Continuous` loops for as long as the note sounds, `Sustain` loops while the
//!   key is held and plays on past the loop when it is let go.
//! - **Round robin:** zones with `seq_length` above 1 take turns, one for each time the same key
//!   is played, so a repeated drum hit is not the same sample twice.
//! - **Choke groups:** starting a zone of `group` N cuts the sounding notes of zones that are
//!   `off_by` N (an open hi-hat closed by the closed one).
//! - **Dials:** Level, and an Attack and Release that are added to every zone's own.
//!
//! 32 voices; a new note beyond them steals the oldest (released ones first). Loading samples
//! allocates and reads files, so an instrument is built off the audio thread (see the app's
//! `sampler_load`); the audio thread calls only the methods that do not allocate.

use std::f32::consts::FRAC_PI_4;
use std::sync::Arc;

pub use crate::sample_file::SampleData;

/// Notes that can sound at once.
pub const VOICES: usize = 32;
/// The most zones an instrument may have (more are ignored).
pub const MAX_ZONES: usize = 4096;
/// Number of dials.
pub const PARAMS: usize = 3;

/// Dial numbers.
pub mod dial {
    pub const LEVEL: usize = 0;
    pub const ATTACK: usize = 1;
    pub const RELEASE: usize = 2;
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
        name: "Level",
        min: -24.0,
        max: 12.0,
        default: 0.0,
        unit: "dB",
    },
    DialInfo {
        name: "Attack",
        min: 0.0,
        max: 2000.0,
        default: 0.0,
        unit: "ms",
    },
    DialInfo {
        name: "Release",
        min: 0.0,
        max: 5000.0,
        default: 60.0,
        unit: "ms",
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

/// How a zone's sample loops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopMode {
    /// Play to the end; the key's release fades it.
    #[default]
    NoLoop,
    /// Play to the end whatever the key does.
    OneShot,
    /// Loop for as long as the note sounds.
    Continuous,
    /// Loop while the key is held; after that play on to the end.
    Sustain,
}

/// One playable zone: a sample and what selects and shapes it.
#[derive(Clone, Debug)]
pub struct Zone {
    pub sample: Arc<SampleData>,
    pub key_low: u8,
    pub key_high: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    /// The key the sample was recorded at.
    pub root_key: u8,
    /// Tuning in cents, transposition included.
    pub tune_cents: f32,
    /// Whether the note's key changes the pitch (a drum zone that plays one pitch on every key
    /// of its range does not).
    pub pitch_track: bool,
    pub volume_db: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    pub loop_mode: LoopMode,
    /// Loop bounds in frames (`loop_end` is exclusive); an empty or bad loop is no loop.
    pub loop_start: usize,
    pub loop_end: usize,
    /// The part of the sample that plays, in frames (`end` is exclusive).
    pub start: usize,
    pub end: usize,
    /// Envelope times in seconds, and the sustain level 0 to 1.
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// Round robin: plays on turn `seq_position` (1 to `seq_length`).
    pub seq_length: u8,
    pub seq_position: u8,
    /// Choke groups: starting this zone cuts the zones that are `off_by` its `group`.
    pub group: u32,
    pub off_by: u32,
}

impl Zone {
    /// A zone that plays all of `sample` on every key and velocity, rooted at middle C, with no
    /// loop and a plain envelope.
    pub fn new(sample: Arc<SampleData>) -> Self {
        let len = sample.frames.len();
        Self {
            sample,
            key_low: 0,
            key_high: 127,
            vel_low: 0,
            vel_high: 127,
            root_key: 60,
            tune_cents: 0.0,
            pitch_track: true,
            volume_db: 0.0,
            pan: 0.0,
            loop_mode: LoopMode::NoLoop,
            loop_start: 0,
            loop_end: len,
            start: 0,
            end: len,
            attack: 0.0,
            hold: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            seq_length: 1,
            seq_position: 1,
            group: 0,
            off_by: 0,
        }
    }

    /// Whether the zone answers a note at `key` and `vel`.
    pub fn plays(&self, key: u8, vel: u8) -> bool {
        (self.key_low..=self.key_high).contains(&key)
            && (self.vel_low..=self.vel_high).contains(&vel)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    zone: usize,
    key: u8,
    pos: f64,
    step: f64,
    gain_l: f32,
    gain_r: f32,
    stage: Stage,
    env: f32,
    attack_step: f32,
    hold_left: u32,
    decay_mul: f32,
    sustain: f32,
    release_mul: f32,
    looping: bool,
    age: u32,
}

impl Voice {
    const IDLE: Voice = Voice {
        active: false,
        zone: 0,
        key: 0,
        pos: 0.0,
        step: 0.0,
        gain_l: 0.0,
        gain_r: 0.0,
        stage: Stage::Release,
        env: 0.0,
        attack_step: 0.0,
        hold_left: 0,
        decay_mul: 1.0,
        sustain: 1.0,
        release_mul: 0.0,
        looping: false,
        age: 0,
    };
}

/// The per-sample multiplier that takes a level down by 60 dB over `seconds`.
fn fall(seconds: f32, sr: f32) -> f32 {
    (-6.9078 / (seconds.max(0.001) * sr)).exp()
}

/// The sampler: zones, voices and the dials.
pub struct Sampler {
    sr: f32,
    zones: Vec<Zone>,
    voices: [Voice; VOICES],
    params: [f32; PARAMS],
    level: f32,
    /// How many times each key has been played, for round robin.
    plays: [u32; 128],
}

impl Sampler {
    /// A sampler at `sample_rate` Hz for `zones` (the first [`MAX_ZONES`]) with the dials at
    /// `params`. Allocation free apart from the zone list it keeps; build it off the audio thread.
    pub fn new(mut zones: Vec<Zone>, sample_rate: f32, params: [f32; PARAMS]) -> Self {
        zones.truncate(MAX_ZONES);
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(8_000.0, 384_000.0)
        } else {
            48_000.0
        };
        let mut s = Self {
            sr,
            zones,
            voices: [Voice::IDLE; VOICES],
            params: defaults(),
            level: 1.0,
            plays: [0; 128],
        };
        s.set_params(params);
        s
    }

    /// New dial values (put into range). Allocation free: dials can move while notes ring.
    pub fn set_params(&mut self, params: [f32; PARAMS]) {
        self.params = clamp(params);
        self.level = 10f32.powf(self.params[dial::LEVEL] / 20.0);
    }

    /// The zones.
    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// How many notes are sounding.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Starts `key` (0 to 127) at `velocity` (1 to 127; 0 counts as 1).
    pub fn note_on(&mut self, key: u8, velocity: u8) {
        let key = key.min(127);
        let vel = velocity.clamp(1, 127);
        let turn = self.plays[usize::from(key)];
        self.plays[usize::from(key)] = turn.wrapping_add(1);
        for z in 0..self.zones.len() {
            let zone = &self.zones[z];
            if !zone.plays(key, vel) || zone.end <= zone.start || zone.sample.frames.is_empty() {
                continue;
            }
            // Round robin: only the zone whose turn it is.
            let seq = u32::from(zone.seq_length.max(1));
            if seq > 1 && turn % seq != u32::from(zone.seq_position.clamp(1, 255)) - 1 {
                continue;
            }
            self.start_voice(z, key, vel);
        }
    }

    fn start_voice(&mut self, z: usize, key: u8, vel: u8) {
        let zone = &self.zones[z];
        let (group, sr) = (zone.group, self.sr);
        // Choke: the zones that are `off_by` this zone's group are cut short.
        if group > 0 {
            let fast = fall(0.01, sr);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                if self.zones[v.zone].off_by == group {
                    v.stage = Stage::Release;
                    v.release_mul = fast;
                    v.looping = false;
                }
            }
        }
        let zone = &self.zones[z];
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, v)| (v.stage == Stage::Release, v.age))
                    .map(|(i, _)| i)
            })
            .unwrap_or(0);
        let keys = if zone.pitch_track {
            f64::from(key) - f64::from(zone.root_key)
        } else {
            0.0
        };
        let cents = if zone.tune_cents.is_finite() {
            f64::from(zone.tune_cents)
        } else {
            0.0
        };
        let tune = keys + cents / 100.0;
        let rate = f64::from(zone.sample.sample_rate.max(1)) / f64::from(sr);
        let step = (2f64.powf(tune / 12.0) * rate).clamp(1e-4, 64.0);
        let v = (f32::from(vel) / 127.0).powf(1.6);
        let db = if zone.volume_db.is_finite() {
            zone.volume_db.clamp(-144.0, 24.0)
        } else {
            0.0
        };
        let gain = v * 10f32.powf(db / 20.0);
        // Equal-power pan.
        let pan = if zone.pan.is_finite() {
            zone.pan.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let angle = (pan + 1.0) * FRAC_PI_4;
        let secs = |x: f32| {
            if x.is_finite() {
                x.clamp(0.0, 60.0)
            } else {
                0.0
            }
        };
        let attack = secs(zone.attack) + self.params[dial::ATTACK] / 1000.0;
        let release = (secs(zone.release) + self.params[dial::RELEASE] / 1000.0).max(0.005);
        let sustain = if zone.sustain.is_finite() {
            zone.sustain.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let loops = matches!(zone.loop_mode, LoopMode::Continuous | LoopMode::Sustain)
            && zone.loop_end > zone.loop_start + 1
            && zone.loop_end <= zone.end;
        self.voices[slot] = Voice {
            active: true,
            zone: z,
            key,
            pos: zone.start as f64,
            step,
            gain_l: gain * angle.cos(),
            gain_r: gain * angle.sin(),
            stage: if attack > 0.0 {
                Stage::Attack
            } else {
                Stage::Hold
            },
            env: if attack > 0.0 { 0.0 } else { 1.0 },
            attack_step: if attack > 0.0 {
                1.0 / (attack * sr)
            } else {
                1.0
            },
            hold_left: (secs(zone.hold) * sr) as u32,
            decay_mul: if zone.decay > 0.0 {
                fall(secs(zone.decay), sr)
            } else {
                0.0
            },
            sustain,
            release_mul: fall(release, sr),
            looping: loops,
            age: 0,
        };
    }

    /// Releases `key`: its notes fade out with their release (a one-shot plays on).
    pub fn note_off(&mut self, key: u8) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.key == key.min(127) && v.stage != Stage::Release)
        {
            match self.zones[v.zone].loop_mode {
                LoopMode::OneShot => {}
                LoopMode::Sustain => {
                    v.looping = false;
                    v.stage = Stage::Release;
                }
                _ => v.stage = Stage::Release,
            }
        }
    }

    /// Releases every note.
    pub fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.stage = Stage::Release;
            v.looping = false;
        }
    }

    /// One frame of the sampler, `(left, right)`. Does not allocate.
    pub fn render(&mut self) -> (f32, f32) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let zone = &self.zones[v.zone];
            let frames = &zone.sample.frames;
            // The envelope.
            match v.stage {
                Stage::Attack => {
                    v.env += v.attack_step;
                    if v.env >= 1.0 {
                        v.env = 1.0;
                        v.stage = Stage::Hold;
                    }
                }
                Stage::Hold => {
                    if v.hold_left > 0 {
                        v.hold_left -= 1;
                    } else {
                        v.stage = if v.decay_mul > 0.0 && v.sustain < 1.0 {
                            Stage::Decay
                        } else {
                            Stage::Sustain
                        };
                    }
                }
                Stage::Decay => {
                    v.env = v.sustain + (v.env - v.sustain) * v.decay_mul;
                    if v.env - v.sustain < 1e-4 {
                        v.env = v.sustain;
                        v.stage = Stage::Sustain;
                    }
                }
                Stage::Sustain => v.env = v.sustain,
                Stage::Release => {
                    v.env *= v.release_mul;
                    if v.env < 1e-4 {
                        v.active = false;
                        continue;
                    }
                }
            }
            // The sample, with a 4-point cubic between its frames.
            let i = v.pos.floor() as i64;
            let f = (v.pos - i as f64) as f32;
            let mut p = [[0.0f32; 2]; 4];
            for (k, slot) in p.iter_mut().enumerate() {
                let mut j = i + k as i64 - 1;
                if v.looping && j >= zone.loop_end as i64 {
                    let len = (zone.loop_end - zone.loop_start) as i64;
                    j = zone.loop_start as i64 + (j - zone.loop_end as i64) % len;
                }
                if j >= zone.start as i64 && (j as usize) < zone.end.min(frames.len()) {
                    *slot = frames[j as usize];
                }
            }
            let cubic = |a: f32, b: f32, c: f32, d: f32| {
                b + 0.5
                    * f
                    * (c - a + f * (2.0 * a - 5.0 * b + 4.0 * c - d + f * (3.0 * (b - c) + d - a)))
            };
            let s = [
                cubic(p[0][0], p[1][0], p[2][0], p[3][0]),
                cubic(p[0][1], p[1][1], p[2][1], p[3][1]),
            ];
            l += s[0] * v.gain_l * v.env;
            r += s[1] * v.gain_r * v.env;
            // On to the next frame; the loop, or the end.
            v.pos += v.step;
            if v.looping && v.pos >= zone.loop_end as f64 {
                let len = (zone.loop_end - zone.loop_start) as f64;
                v.pos = zone.loop_start as f64 + (v.pos - zone.loop_end as f64) % len;
            } else if v.pos >= zone.end.min(frames.len()) as f64 {
                v.active = false;
            }
            v.age = v.age.saturating_add(1);
        }
        let (l, r) = (l * self.level, r * self.level);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// A sine of `hz` at 48 kHz, `secs` long, as a sample.
    fn sine(hz: f32, secs: f32) -> Arc<SampleData> {
        let n = (secs * SR) as usize;
        Arc::new(SampleData::new(
            (0..n)
                .map(|i| {
                    let v = (std::f32::consts::TAU * hz * i as f32 / SR).sin() * 0.8;
                    [v, v]
                })
                .collect(),
            SR as u32,
        ))
    }

    /// A sample that is a constant `level`.
    fn flat(level: f32, secs: f32) -> Arc<SampleData> {
        Arc::new(SampleData::new(
            vec![[level, level]; (secs * SR) as usize],
            SR as u32,
        ))
    }

    fn render(s: &mut Sampler, secs: f32) -> Vec<f32> {
        (0..(secs * SR) as usize).map(|_| s.render().0).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// The frequency (in 1 Hz steps in `lo..hi`) where `x` is strongest.
    fn peak_hz(x: &[f32], lo: f32, hi: f32) -> f32 {
        let mut best = (0.0, lo);
        let mut f = lo;
        while f <= hi {
            let (mut re, mut im) = (0f64, 0f64);
            for (i, v) in x.iter().enumerate() {
                let w = f64::from(std::f32::consts::TAU * f * i as f32 / SR);
                re += f64::from(*v) * w.cos();
                im += f64::from(*v) * w.sin();
            }
            let m = (re * re + im * im).sqrt();
            if m > best.0 {
                best = (m, f);
            }
            f += 1.0;
        }
        best.1
    }

    fn one(zone: Zone) -> Sampler {
        Sampler::new(vec![zone], SR, defaults())
    }

    #[test]
    fn a_note_plays_the_sample_at_its_root_and_other_keys_transpose_it() {
        let mut zone = Zone::new(sine(440.0, 2.0));
        zone.root_key = 69;
        let mut s = one(zone);
        s.note_on(69, 100);
        let x = render(&mut s, 0.5);
        assert!((peak_hz(&x[2_000..], 420.0, 460.0) - 440.0).abs() < 2.0);
        // An octave up plays twice as fast; a fifth down is two thirds.
        let mut s2 = one({
            let mut z = Zone::new(sine(440.0, 2.0));
            z.root_key = 69;
            z
        });
        s2.note_on(81, 100);
        let x = render(&mut s2, 0.5);
        assert!((peak_hz(&x[2_000..], 860.0, 900.0) - 880.0).abs() < 3.0);
        let mut s3 = one({
            let mut z = Zone::new(sine(440.0, 2.0));
            z.root_key = 69;
            z
        });
        s3.note_on(62, 100);
        let x = render(&mut s3, 0.5);
        let want = 440.0 * 2f32.powf(-7.0 / 12.0);
        assert!(
            (peak_hz(&x[2_000..], 270.0, 320.0) - want).abs() < 2.0,
            "want {want}"
        );
    }

    #[test]
    fn a_zone_that_does_not_track_the_keyboard_plays_one_pitch_on_every_key() {
        let mut z = Zone::new(sine(440.0, 2.0));
        z.root_key = 69;
        z.pitch_track = false;
        let mut s = one(z);
        s.note_on(93, 100);
        let x = render(&mut s, 0.5);
        assert!((peak_hz(&x[2_000..], 420.0, 460.0) - 440.0).abs() < 2.0);
    }

    #[test]
    fn tuning_a_zone_in_cents_shifts_the_pitch_and_a_sample_at_another_rate_still_plays_in_tune() {
        let mut zone = Zone::new(sine(440.0, 2.0));
        zone.root_key = 69;
        zone.tune_cents = 100.0; // one semitone up
        let mut s = one(zone);
        s.note_on(69, 100);
        let x = render(&mut s, 0.5);
        let want = 440.0 * 2f32.powf(1.0 / 12.0);
        assert!((peak_hz(&x[2_000..], 440.0, 490.0) - want).abs() < 2.0);
        // A 22.05 kHz recording of 440 Hz plays at 440 Hz on a 48 kHz project.
        let half: Vec<[f32; 2]> = (0..44_100)
            .map(|i| {
                let v = (std::f32::consts::TAU * 440.0 * i as f32 / 22_050.0).sin() * 0.8;
                [v, v]
            })
            .collect();
        let mut z = Zone::new(Arc::new(SampleData::new(half, 22_050)));
        z.root_key = 69;
        let mut s = one(z);
        s.note_on(69, 100);
        let x = render(&mut s, 0.5);
        assert!((peak_hz(&x[2_000..], 420.0, 460.0) - 440.0).abs() < 2.0);
    }

    #[test]
    fn key_and_velocity_ranges_pick_the_zones_and_overlapping_zones_layer() {
        let mut low = Zone::new(flat(0.2, 1.0));
        low.key_low = 0;
        low.key_high = 59;
        let mut high = Zone::new(flat(0.2, 1.0));
        high.key_low = 60;
        high.key_high = 127;
        high.vel_low = 100;
        let mut s = Sampler::new(vec![low, high], SR, defaults());
        let level = |s: &mut Sampler, key, vel| {
            s.all_notes_off();
            render(s, 0.2);
            s.note_on(key, vel);
            rms(&render(s, 0.1))
        };
        assert!(
            level(&mut s, 40, 100) > 0.01,
            "the low zone answers a low key"
        );
        assert!(
            level(&mut s, 70, 120) > 0.01,
            "the high zone answers a hard high key"
        );
        assert!(
            level(&mut s, 70, 60) < 1e-6,
            "no zone answers a soft high key"
        );
        // Two zones on the same key and velocity play together: twice the level.
        let a = Zone::new(flat(0.1, 1.0));
        let mut one_zone = Sampler::new(vec![a.clone()], SR, defaults());
        let mut two_zones = Sampler::new(vec![a.clone(), a], SR, defaults());
        one_zone.note_on(60, 127);
        two_zones.note_on(60, 127);
        let (o, t) = (
            rms(&render(&mut one_zone, 0.1)),
            rms(&render(&mut two_zones, 0.1)),
        );
        assert!((t / o - 2.0).abs() < 0.05, "{t} vs {o}");
    }

    #[test]
    fn velocity_sets_the_level_and_a_zones_volume_and_pan_place_it() {
        let mut s = one(Zone::new(flat(0.5, 1.0)));
        s.note_on(60, 127);
        let hard = rms(&render(&mut s, 0.1));
        let mut s = one(Zone::new(flat(0.5, 1.0)));
        s.note_on(60, 30);
        let soft = rms(&render(&mut s, 0.1));
        assert!(hard > soft * 4.0, "{hard} vs {soft}");
        let mut z = Zone::new(flat(0.5, 1.0));
        z.pan = -1.0;
        z.volume_db = -6.0;
        let mut s = one(z);
        s.note_on(60, 127);
        let (mut l, mut r) = (0f32, 0f32);
        for _ in 0..4_800 {
            let (a, b) = s.render();
            l += a.abs();
            r += b.abs();
        }
        assert!(r < l * 0.01, "panned left: {l} vs {r}");
        let mut full = one(Zone::new(flat(0.5, 1.0)));
        full.note_on(60, 127);
        let (fl, fr) = full.render();
        assert!((fl - fr).abs() < 1e-6, "centre is equal on both sides");
    }

    #[test]
    fn a_plain_sample_ends_and_frees_its_voice_and_a_continuous_loop_goes_on() {
        let mut s = one(Zone::new(sine(440.0, 0.2)));
        s.note_on(60, 100);
        let x = render(&mut s, 0.5);
        assert!(rms(&x[..4_800]) > 0.05);
        assert!(rms(&x[(0.3 * SR) as usize..]) < 1e-6, "it ended");
        assert_eq!(s.active_voices(), 0);
        let mut z = Zone::new(sine(440.0, 0.2));
        z.loop_mode = LoopMode::Continuous;
        let mut s = one(z);
        s.note_on(60, 100);
        let x = render(&mut s, 2.0);
        assert!(
            rms(&x[(1.5 * SR) as usize..]) > 0.05,
            "the loop keeps it going"
        );
        assert_eq!(s.active_voices(), 1);
        // The loop points are honoured: a loop of the second half only.
        let mut z = Zone::new(flat(0.5, 1.0));
        let mut frames = vec![[0.0f32; 2]; 48_000];
        for f in frames.iter_mut().skip(24_000) {
            *f = [0.5, 0.5];
        }
        z.sample = Arc::new(SampleData::new(frames, SR as u32));
        z.loop_mode = LoopMode::Continuous;
        z.loop_start = 24_000;
        z.loop_end = 48_000;
        let mut s = one(z);
        s.note_on(60, 127);
        let x = render(&mut s, 3.0);
        assert!(
            rms(&x[(2.0 * SR) as usize..]) > 0.05,
            "it loops the loud half"
        );
    }

    #[test]
    fn note_off_fades_a_note_but_a_one_shot_plays_on_and_a_sustain_loop_plays_the_tail() {
        let mut z = Zone::new(sine(440.0, 1.0));
        z.release = 0.05;
        z.loop_mode = LoopMode::Continuous;
        let mut s = one(z);
        s.note_on(60, 100);
        render(&mut s, 0.1);
        s.note_off(60);
        let x = render(&mut s, 0.5);
        assert!(
            rms(&x[(0.3 * SR) as usize..]) < 1e-4,
            "released: silent after the release time"
        );
        assert_eq!(s.active_voices(), 0);
        let mut z = Zone::new(sine(440.0, 0.5));
        z.loop_mode = LoopMode::OneShot;
        let mut s = one(z);
        s.note_on(60, 100);
        render(&mut s, 0.05);
        s.note_off(60);
        let x = render(&mut s, 0.2);
        assert!(rms(&x) > 0.05, "a one-shot ignores the key coming up");
        // Sustain: loops while held (a short loop of the first 0.1 s), then plays on to the end.
        let mut z = Zone::new(sine(440.0, 1.0));
        z.loop_mode = LoopMode::Sustain;
        z.loop_start = 0;
        z.loop_end = 4_800;
        z.release = 2.0;
        let mut s = one(z);
        s.note_on(60, 100);
        let held = render(&mut s, 1.5);
        assert!(
            rms(&held[(1.2 * SR) as usize..]) > 0.05,
            "held past the sample's length by the loop"
        );
        s.note_off(60);
        let tail = render(&mut s, 1.0);
        assert!(rms(&tail[..2_400]) > 0.02);
        assert_eq!(
            s.active_voices(),
            0,
            "it played the rest of the sample and ended"
        );
    }

    #[test]
    fn the_envelope_attacks_decays_to_a_sustain_and_the_dials_add_to_it() {
        let mut z = Zone::new(flat(1.0, 3.0));
        z.attack = 0.1;
        z.decay = 0.1;
        z.sustain = 0.5;
        z.loop_mode = LoopMode::Continuous;
        let mut s = Sampler::new(vec![z.clone()], SR, [0.0, 0.0, 60.0]);
        s.note_on(60, 127);
        let x = render(&mut s, 1.0);
        assert!(x[10].abs() < 0.05, "starts at zero ({})", x[10]);
        assert!(
            x[2_400] > 0.3 && x[2_400] < 0.7,
            "half way through the attack ({})",
            x[2_400]
        );
        let held = x[(0.9 * SR) as usize];
        let full = x.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            (held / full - 0.5).abs() < 0.05,
            "sustains at half ({held} of {full})"
        );
        // The Attack dial adds to the zone's own attack: 100 + 400 ms.
        let mut s = Sampler::new(vec![z], SR, [0.0, 400.0, 60.0]);
        s.note_on(60, 127);
        let x = render(&mut s, 0.6);
        assert!(
            x[(0.2 * SR) as usize] < x[(0.45 * SR) as usize],
            "a longer rise"
        );
        // The Level dial scales the output.
        let mut loud = one(Zone::new(flat(0.1, 1.0)));
        let mut quiet = Sampler::new(vec![Zone::new(flat(0.1, 1.0))], SR, [-12.0, 0.0, 60.0]);
        loud.note_on(60, 127);
        quiet.note_on(60, 127);
        let (a, b) = (rms(&render(&mut loud, 0.1)), rms(&render(&mut quiet, 0.1)));
        assert!((a / b - 3.98).abs() < 0.1, "{a} vs {b}");
    }

    #[test]
    fn round_robin_zones_take_turns_for_each_key() {
        let mk = |level: f32, pos: u8| {
            let mut z = Zone::new(flat(level, 0.5));
            z.seq_length = 3;
            z.seq_position = pos;
            z
        };
        let mut s = Sampler::new(vec![mk(0.1, 1), mk(0.2, 2), mk(0.4, 3)], SR, defaults());
        let mut levels = Vec::new();
        for _ in 0..7 {
            s.note_on(60, 127);
            let x = render(&mut s, 0.01);
            levels.push((rms(&x) * 100.0).round() / 100.0);
            s.all_notes_off();
            render(&mut s, 0.3);
        }
        // 0.1, 0.2, 0.4, then round again (the levels are scaled by velocity and the cubic).
        assert!(levels[0] < levels[1] && levels[1] < levels[2], "{levels:?}");
        assert_eq!(levels[0], levels[3]);
        assert_eq!(levels[1], levels[4]);
        assert_eq!(levels[2], levels[5]);
        // Each key has its own count.
        let mut s = Sampler::new(vec![mk(0.1, 1), mk(0.2, 2), mk(0.4, 3)], SR, defaults());
        s.note_on(60, 127);
        let first_60 = rms(&render(&mut s, 0.01));
        s.all_notes_off();
        render(&mut s, 0.3);
        s.note_on(62, 127);
        let first_62 = rms(&render(&mut s, 0.01));
        assert!(
            (first_60 - first_62).abs() < 1e-3,
            "a new key starts at turn one"
        );
    }

    #[test]
    fn a_choke_group_cuts_the_zones_that_are_off_by_it() {
        let mut open = Zone::new(flat(0.3, 3.0));
        open.key_low = 46;
        open.key_high = 46;
        open.off_by = 7;
        open.loop_mode = LoopMode::Continuous;
        let mut closed = Zone::new(flat(0.1, 0.1));
        closed.key_low = 42;
        closed.key_high = 42;
        closed.group = 7;
        let mut s = Sampler::new(vec![open, closed], SR, defaults());
        s.note_on(46, 127);
        assert!(rms(&render(&mut s, 0.2)) > 0.05);
        s.note_on(42, 127);
        render(&mut s, 0.3);
        // The closed hit has ended (0.1 s) and the open one was cut by it.
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn more_notes_than_voices_steal_the_oldest_and_stay_bounded() {
        let mut z = Zone::new(sine(220.0, 5.0));
        z.loop_mode = LoopMode::Continuous;
        let mut s = one(z);
        for k in 30..90u8 {
            s.note_on(k, 127);
            for _ in 0..100 {
                let (l, r) = s.render();
                assert!(l.is_finite() && r.is_finite() && l.abs() <= 4.0);
            }
        }
        assert_eq!(s.active_voices(), VOICES);
    }

    #[test]
    fn broken_zones_and_dials_never_make_nan_or_panic() {
        let mut bad = Zone::new(Arc::new(SampleData::new(Vec::new(), 48_000)));
        bad.key_low = 0;
        let mut loopless = Zone::new(sine(440.0, 0.1));
        loopless.loop_mode = LoopMode::Continuous;
        loopless.loop_start = 100;
        loopless.loop_end = 50; // backwards
        let mut weird = Zone::new(sine(440.0, 0.1));
        weird.tune_cents = f32::NAN;
        weird.volume_db = f32::INFINITY;
        weird.pan = f32::NAN;
        weird.attack = f32::NAN;
        weird.sustain = f32::INFINITY;
        weird.start = 99_999_999;
        weird.end = 5;
        let mut outside = Zone::new(sine(440.0, 0.1));
        outside.end = 99_999_999;
        outside.loop_mode = LoopMode::Sustain;
        outside.loop_end = 99_999_999;
        for bad_dials in [[f32::NAN; PARAMS], [1e30; PARAMS], [-1e30; PARAMS]] {
            let mut s = Sampler::new(
                vec![
                    bad.clone(),
                    loopless.clone(),
                    weird.clone(),
                    outside.clone(),
                ],
                f32::NAN,
                bad_dials,
            );
            for k in [0u8, 60, 127, 200] {
                s.note_on(k, if k % 2 == 0 { 0 } else { 255 });
            }
            for _ in 0..20_000 {
                let (l, r) = s.render();
                assert!(l.is_finite() && r.is_finite());
            }
            s.note_off(60);
            s.all_notes_off();
        }
        // No zones: silence.
        let mut empty = Sampler::new(Vec::new(), SR, defaults());
        empty.note_on(60, 100);
        assert_eq!(empty.render(), (0.0, 0.0));
    }

    #[test]
    fn rendering_and_playing_notes_do_not_allocate() {
        let mut z = Zone::new(sine(440.0, 1.0));
        z.loop_mode = LoopMode::Continuous;
        z.seq_length = 2;
        let mut s = one(z);
        assert_no_alloc::assert_no_alloc(|| {
            for k in 40..80u8 {
                s.note_on(k, 100);
            }
            for _ in 0..4_800 {
                s.render();
            }
            s.note_off(50);
            s.all_notes_off();
            s.set_params(defaults());
        });
    }

    #[test]
    fn dials_are_clamped_and_named() {
        assert_eq!(clamp([f32::NAN; PARAMS]), defaults());
        assert_eq!(clamp([99.0, 99_999.0, -5.0]), [12.0, 2000.0, 0.0]);
        assert_eq!(DIALS[dial::RELEASE].name, "Release");
    }
}
