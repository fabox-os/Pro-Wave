//! The biquad filters (`biquad.c`): first- and second-order low, high, band, notch, all-pass,
//! peaking and shelving filters, the Linkwitz transform, CD de-emphasis and a raw
//! `b0 b1 b2 a0 a1 a2` biquad. All run in transposed direct form II on `f64`.

use std::f64::consts::{LN_2, PI};

use crate::args::{check_freq, get_freq, get_number, strtod, strtol, Getopt};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Merge, Stream};
use crate::error::{Error, Result};

pub const LOWPASS_1: i32 = 1;
pub const HIGHPASS_1: i32 = 2;
pub const ALLPASS_1: i32 = 3;
pub const LOWSHELF_1: i32 = 4;
pub const HIGHSHELF_1: i32 = 5;
pub const LOWPASS_1P: i32 = 6;
pub const LOWPASS: i32 = 7;
pub const HIGHPASS: i32 = 8;
pub const BANDPASS_SKIRT: i32 = 9;
pub const BANDPASS_PEAK: i32 = 10;
pub const NOTCH: i32 = 11;
pub const ALLPASS: i32 = 12;
pub const PEAK: i32 = 13;
pub const LOWSHELF: i32 = 14;
pub const HIGHSHELF: i32 = 15;
pub const LOWPASS_TRANSFORM: i32 = 16;
pub const HIGHPASS_TRANSFORM: i32 = 17;
/// Only for the effect table.
pub const DEEMPH: i32 = 18;
pub const BIQUAD: i32 = 19;

/// How the width argument of a filter is meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Q,
    Slope,
    SlopeDb,
    BwOct,
    BwHz,
}

macro_rules! info {
    ($name:expr, $usage:expr, $n:expr) => {
        EffectInfo {
            name: $name,
            usage: $usage,
            init,
            number: $n,
        }
    };
}

pub(crate) static INFO: [EffectInfo; 20] = [
    info!("lowpass_1", "[-r[thresh]] f0[k]", LOWPASS_1),
    info!("highpass_1", "[-r[thresh]] f0[k]", HIGHPASS_1),
    info!("allpass_1", "[-r[thresh]] f0[k]", ALLPASS_1),
    info!("lowshelf_1", "[-r[thresh]] f0[k] gain", LOWSHELF_1),
    info!("highshelf_1", "[-r[thresh]] f0[k] gain", HIGHSHELF_1),
    info!("lowpass_1p", "[-r[thresh]] f0[k]", LOWPASS_1P),
    info!("lowpass", "[-r[thresh]] f0[k] width[q|o|h|k]", LOWPASS),
    info!("highpass", "[-r[thresh]] f0[k] width[q|o|h|k]", HIGHPASS),
    info!(
        "bandpass_skirt",
        "[-r[thresh]] f0[k] width[q|o|h|k]", BANDPASS_SKIRT
    ),
    info!(
        "bandpass_peak",
        "[-r[thresh]] f0[k] width[q|o|h|k]", BANDPASS_PEAK
    ),
    info!("notch", "[-r[thresh]] f0[k] width[q|o|h|k]", NOTCH),
    info!("allpass", "[-r[thresh]] f0[k] width[q|o|h|k]", ALLPASS),
    info!("eq", "[-r[thresh]] f0[k] width[q|o|h|k] gain", PEAK),
    info!(
        "lowshelf",
        "[-r[thresh]] f0[k] width[q|s|d|o|h|k] gain", LOWSHELF
    ),
    info!(
        "highshelf",
        "[-r[thresh]] f0[k] width[q|s|d|o|h|k] gain", HIGHSHELF
    ),
    info!(
        "lowpass_transform",
        "[-r[thresh]] fz[k] width_z[q] fp[k] width_p[q]", LOWPASS_TRANSFORM
    ),
    info!(
        "highpass_transform",
        "[-r[thresh]] fz[k] width_z[q] fp[k] width_p[q]", HIGHPASS_TRANSFORM
    ),
    info!(
        "linkwitz_transform",
        "[-r[thresh]] fz[k] width_z[q] fp[k] width_p[q]", HIGHPASS_TRANSFORM
    ),
    info!("deemph", "[-r[thresh]]", DEEMPH),
    info!("biquad", "[-r[thresh]] b0 b1 b2 a0 a1 a2", BIQUAD),
];

/// One biquad: its normalised coefficients and its memory (transposed direct form II).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Biquad {
    pub c0: f64,
    pub c1: f64,
    pub c2: f64,
    pub c3: f64,
    pub c4: f64,
    m0: f64,
    m1: f64,
}

impl Biquad {
    /// `biquad_init`: coefficients from `b0 b1 b2 a0 a1 a2`, memory cleared.
    pub fn new(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            c0: b0 / a0,
            c1: b1 / a0,
            c2: b2 / a0,
            c3: a1 / a0,
            c4: a2 / a0,
            m0: 0.0,
            m1: 0.0,
        }
    }

    /// Clears the memory.
    pub fn reset(&mut self) {
        self.m0 = 0.0;
        self.m1 = 0.0;
    }

    /// One sample through the filter.
    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let r = self.c0 * s + self.m0;
        self.m0 = self.m1 + self.c1 * s - self.c3 * r;
        self.m1 = self.c2 * s - self.c4 * r;
        r
    }

    /// The complex response `(re, im)` at angular frequency `w` (radians per sample).
    pub fn response(&self, w: f64) -> (f64, f64) {
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.c0 + self.c1 * c1 + self.c2 * c2;
        let ni = -(self.c1 * s1 + self.c2 * s2);
        let dr = 1.0 + self.c3 * c1 + self.c4 * c2;
        let di = -(self.c3 * s1 + self.c4 * s2);
        let d = dr * dr + di * di;
        ((nr * dr + ni * di) / d, (ni * dr - nr * di) / d)
    }

    /// `biquad_init_using_type`: the filter of `kind` at sample rate `fs`. `arg0` is the
    /// frequency (`fz` for the transforms), `arg1` the width (`qz`), `arg2` the gain (`fp`) and
    /// `arg3` `qp`.
    pub fn from_type(
        kind: i32,
        fs: f64,
        arg0: f64,
        arg1: f64,
        arg2: f64,
        arg3: f64,
        mut width_type: Width,
    ) -> Self {
        let (mut b0, mut b1, mut b2, mut a0, mut a1, mut a2) = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        if kind == LOWPASS_TRANSFORM || kind == HIGHPASS_TRANSFORM {
            let (fz, qz, fp, qp) = (arg0, arg1, arg2, arg3);
            let w0z = 2.0 * PI * fz / fs;
            let w0p = 2.0 * PI * fp / fs;
            let (cos_w0z, cos_w0p) = (w0z.cos(), w0p.cos());
            let alpha_z = w0z.sin() / (2.0 * qz);
            let alpha_p = w0p.sin() / (2.0 * qp);
            let (kz, kp) = if kind == LOWPASS_TRANSFORM {
                (2.0 / (1.0 - cos_w0z), 2.0 / (1.0 - cos_w0p))
            } else {
                (2.0 / (1.0 + cos_w0z), 2.0 / (1.0 + cos_w0p))
            };
            b0 = (1.0 + alpha_z) * kz;
            b1 = (-2.0 * cos_w0z) * kz;
            b2 = (1.0 - alpha_z) * kz;
            a0 = (1.0 + alpha_p) * kp;
            a1 = (-2.0 * cos_w0p) * kp;
            a2 = (1.0 - alpha_p) * kp;
        } else {
            let mut f0 = arg0;
            let mut width = arg1;
            let gain = arg2;
            if width_type == Width::SlopeDb {
                width_type = Width::Slope;
                width /= 12.0;
                if kind == LOWSHELF {
                    f0 *= 10f64.powf(gain.abs() / 80.0 / width);
                } else if kind == HIGHSHELF {
                    f0 /= 10f64.powf(gain.abs() / 80.0 / width);
                }
            }
            let a = 10f64.powf(gain / 40.0);
            let w0 = 2.0 * PI * f0 / fs;
            let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
            let alpha = match width_type {
                Width::Slope => sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / width - 1.0) + 2.0).sqrt(),
                Width::BwOct => sin_w0 * (LN_2 / 2.0 * width * w0 / sin_w0).sinh(),
                Width::BwHz => sin_w0 / (2.0 * f0 / width),
                Width::Q | Width::SlopeDb => sin_w0 / (2.0 * width),
            };
            match kind {
                LOWPASS_1 => {
                    let c = 1.0 + cos_w0;
                    b0 = sin_w0;
                    b1 = sin_w0;
                    b2 = 0.0;
                    a0 = sin_w0 + c;
                    a1 = sin_w0 - c;
                    a2 = 0.0;
                }
                HIGHPASS_1 => {
                    let c = 1.0 + cos_w0;
                    b0 = c;
                    b1 = -c;
                    b2 = 0.0;
                    a0 = sin_w0 + c;
                    a1 = sin_w0 - c;
                    a2 = 0.0;
                }
                ALLPASS_1 => {
                    let c = 1.0 + cos_w0;
                    b0 = sin_w0 - c;
                    b1 = sin_w0 + c;
                    b2 = 0.0;
                    a0 = b1;
                    a1 = b0;
                    a2 = 0.0;
                }
                LOWSHELF_1 => {
                    let c = 1.0 + cos_w0;
                    b0 = a * sin_w0 + c;
                    b1 = a * sin_w0 - c;
                    b2 = 0.0;
                    a0 = sin_w0 / a + c;
                    a1 = sin_w0 / a - c;
                    a2 = 0.0;
                }
                HIGHSHELF_1 => {
                    let c = 1.0 + cos_w0;
                    b0 = sin_w0 + c * a;
                    b1 = sin_w0 - c * a;
                    b2 = 0.0;
                    a0 = sin_w0 + c / a;
                    a1 = sin_w0 - c / a;
                    a2 = 0.0;
                }
                LOWPASS_1P => {
                    let c = 1.0 - cos_w0;
                    b0 = -c + (c * c + 2.0 * c).sqrt();
                    b1 = 0.0;
                    b2 = 0.0;
                    a0 = 1.0;
                    a1 = -1.0 + b0;
                    a2 = 0.0;
                }
                LOWPASS => {
                    b0 = (1.0 - cos_w0) / 2.0;
                    b1 = 1.0 - cos_w0;
                    b2 = b0;
                    a0 = 1.0 + alpha;
                    a1 = -2.0 * cos_w0;
                    a2 = 1.0 - alpha;
                }
                HIGHPASS => {
                    b0 = (1.0 + cos_w0) / 2.0;
                    b1 = -(1.0 + cos_w0);
                    b2 = b0;
                    a0 = 1.0 + alpha;
                    a1 = -2.0 * cos_w0;
                    a2 = 1.0 - alpha;
                }
                BANDPASS_SKIRT => {
                    b0 = sin_w0 / 2.0;
                    b1 = 0.0;
                    b2 = -b0;
                    a0 = 1.0 + alpha;
                    a1 = -2.0 * cos_w0;
                    a2 = 1.0 - alpha;
                }
                BANDPASS_PEAK => {
                    b0 = alpha;
                    b1 = 0.0;
                    b2 = -alpha;
                    a0 = 1.0 + alpha;
                    a1 = -2.0 * cos_w0;
                    a2 = 1.0 - alpha;
                }
                NOTCH => {
                    b0 = 1.0;
                    b1 = -2.0 * cos_w0;
                    b2 = 1.0;
                    a0 = 1.0 + alpha;
                    a1 = b1;
                    a2 = 1.0 - alpha;
                }
                ALLPASS => {
                    b0 = 1.0 - alpha;
                    b1 = -2.0 * cos_w0;
                    b2 = 1.0 + alpha;
                    a0 = b2;
                    a1 = b1;
                    a2 = b0;
                }
                PEAK => {
                    b0 = 1.0 + alpha * a;
                    b1 = -2.0 * cos_w0;
                    b2 = 1.0 - alpha * a;
                    a0 = 1.0 + alpha / a;
                    a1 = b1;
                    a2 = 1.0 - alpha / a;
                }
                LOWSHELF => {
                    let c = 2.0 * a.sqrt() * alpha;
                    b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + c);
                    b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                    b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - c);
                    a0 = (a + 1.0) + (a - 1.0) * cos_w0 + c;
                    a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                    a2 = (a + 1.0) + (a - 1.0) * cos_w0 - c;
                }
                HIGHSHELF => {
                    let c = 2.0 * a.sqrt() * alpha;
                    b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + c);
                    b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                    b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - c);
                    a0 = (a + 1.0) - (a - 1.0) * cos_w0 + c;
                    a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                    a2 = (a + 1.0) - (a - 1.0) * cos_w0 - c;
                }
                _ => {}
            }
        }
        Biquad::new(b0, b1, b2, a0, a1, a2)
    }
}

/// `parse_width`: a width argument and how it is meant (`0.7q`, `1o`, `100h`, `1k`, `0.5s`,
/// `6d`, or a Butterworth section `bw4` / `bw6.1`). A bare number is a Q. `None` when it does
/// not parse (including trailing characters).
pub fn parse_width(s: &str) -> Option<(f64, Width)> {
    let b = s.as_bytes();
    if b.len() > 2 && b[0] == b'b' && b[1] == b'w' {
        let rest = &s[2..];
        let (order, end) = strtol(rest);
        if end == 0 || !(end == rest.len() || rest.as_bytes()[end] == b'.') {
            return None;
        }
        if order < 2 {
            return None;
        }
        let n_biquads = order / 2;
        let mut p_idx = 0i64;
        if end < rest.len() {
            let idx_text = &rest[end + 1..];
            let (idx, e2) = strtol(idx_text);
            if e2 == 0 || e2 != idx_text.len() {
                return None;
            }
            if idx < 0 || idx >= n_biquads {
                return None;
            }
            p_idx = idx;
        }
        p_idx = n_biquads - p_idx;
        let w = 1.0 / (2.0 * (PI / order as f64 * (p_idx as f64 - 0.5)).sin());
        return Some((w, Width::Q));
    }
    let (mut w, end) = strtod(s);
    if end == 0 {
        // strtod found nothing: the original reports a parse failure.
        return None;
    }
    let mut kind = Width::Q;
    let tail = &s[end..];
    if !tail.is_empty() {
        let mut chars = tail.chars();
        match chars.next() {
            Some('q') => kind = Width::Q,
            Some('s') => kind = Width::Slope,
            Some('d') => kind = Width::SlopeDb,
            Some('o') => kind = Width::BwOct,
            Some('k') => {
                w *= 1000.0;
                kind = Width::BwHz;
            }
            Some('h') => kind = Width::BwHz,
            _ => return None,
        }
        if !chars.as_str().is_empty() {
            return None;
        }
    }
    Some((w, kind))
}

struct BiquadEffect {
    name: &'static str,
    stream: Stream,
    selected: Vec<bool>,
    state: Vec<Biquad>,
}

impl BiquadEffect {
    /// `biquad_channel_can_merge`: channel `k` of `src` can go to channel `ch_map[k]` of this
    /// effect if that one is free and every channel of `src` that maps there has the same
    /// filter.
    fn channel_can_merge(&self, src: &BiquadEffect, k: usize, ch_map: &[i32]) -> bool {
        if !src.selected[k] || ch_map[k] < 0 || self.selected[ch_map[k] as usize] {
            return false;
        }
        (0..src.stream.channels).all(|i| {
            ch_map[i] != ch_map[k] || (src.selected[i] && same_filter(&src.state[i], &src.state[k]))
        })
    }
}

/// `biquad_equal`: the same coefficients.
fn same_filter(a: &Biquad, b: &Biquad) -> bool {
    a.c0 == b.c0 && a.c1 == b.c1 && a.c2 == b.c2 && a.c3 == b.c3 && a.c4 == b.c4
}

impl Effect for BiquadEffect {
    fn reorderable(&self) -> bool {
        true
    }
    fn opt_selector(&self) -> Option<&[bool]> {
        Some(&self.selected)
    }
    fn allow_remap(&self) -> bool {
        true
    }
    fn merge_id(&self) -> Option<&'static str> {
        Some("biquad")
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    /// `biquad_effect_merge`: takes the channels of `src` whose place in this effect is free,
    /// following them through effects that moved them (`ch_map`).
    fn merge(&mut self, src: &mut dyn Effect, ch_map: &[i32]) -> Merge {
        let Some(src) = src
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<BiquadEffect>())
        else {
            return Merge::None;
        };
        let n_src = src.selected.iter().filter(|s| **s).count();
        let mut rem = n_src;
        for k in 0..src.stream.channels {
            if self.channel_can_merge(src, k, ch_map) {
                let m = ch_map[k];
                self.selected[m as usize] = true;
                self.state[m as usize] = src.state[k];
                for i in k..src.stream.channels {
                    if src.selected[i] && ch_map[i] == m {
                        src.selected[i] = false;
                        src.state[i] = Biquad::default();
                        rem -= 1;
                    }
                }
            }
        }
        if rem == n_src {
            Merge::None
        } else if rem > 0 {
            Merge::Partial
        } else {
            Merge::Full
        }
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for frame in ibuf[..*frames * ch].chunks_exact_mut(ch) {
            for ((s, on), b) in frame
                .iter_mut()
                .zip(&self.selected)
                .zip(self.state.iter_mut())
            {
                if *on {
                    *s = b.run(*s);
                }
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        for (on, b) in self.selected.iter().zip(self.state.iter_mut()) {
            if *on {
                b.reset();
            }
        }
    }
}

fn parse_opts(argv: &[&str], argc: usize, g: &mut Getopt) -> Result<(bool, f64)> {
    let (mut reverse, mut thresh) = (false, 80.0);
    while let Some(opt) = g.next(argc, argv, "r::") {
        if opt == 'r' {
            reverse = true;
            if let Some(arg) = g.arg.clone() {
                let (v, end) = strtol(&arg);
                if end == 0 || end != arg.len() {
                    return Err(Error::Parse {
                        effect: argv[0].into(),
                        param: "thresh".into(),
                        text: arg,
                    });
                }
                thresh = v as f64;
                if !(10.0..=200.0).contains(&thresh) {
                    return Err(Error::Range {
                        effect: argv[0].into(),
                        param: "thresh".into(),
                    });
                }
            }
        } else {
            return Err(Error::Message(format!(
                "{}: invalid option -- '{}'",
                argv[0], g.opt
            )));
        }
    }
    Ok((reverse, thresh))
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    make(info, istream, selector, argv).map(|e| vec![e])
}

fn make(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Box<dyn Effect>> {
    let name = argv[0];
    let argc = argv.len();
    let mut g = Getopt::default();
    let (mut kind, mut width_type) = (info.number, Width::Q);
    let (mut arg0, mut arg1, mut arg2, mut arg3) = (0.0, 0.0, 0.0, 0.0);
    let mut raw = [0.0f64; 6];
    let fs = istream.fs;

    // `INIT_COMMON(n_args, type)`: options come first, then exactly `n_args` arguments.
    let common = |n_args: usize, g: &mut Getopt| -> Result<(bool, f64)> {
        let opts = parse_opts(argv, argc.saturating_sub(n_args), g)?;
        if argc - g.ind != n_args {
            return Err(info.usage_error());
        }
        Ok(opts)
    };
    let width_arg = |text: &str, param: &str| -> Result<(f64, Width)> {
        let (w, t) = parse_width(text).ok_or_else(|| Error::Parse {
            effect: name.into(),
            param: param.into(),
            text: text.into(),
        })?;
        if w > 0.0 {
            Ok((w, t))
        } else {
            Err(Error::Range {
                effect: name.into(),
                param: param.into(),
            })
        }
    };
    let freq_arg = |text: &str, param: &str| -> Result<f64> {
        let f = get_freq(name, text, param)?;
        check_freq(name, f, fs, param)?;
        Ok(f)
    };
    let no_slope = |t: Width| -> Result<()> {
        if t != Width::Slope && t != Width::SlopeDb {
            Ok(())
        } else {
            Err(Error::Message(format!("{name}: error: invalid width type")))
        }
    };
    let only_q = |t: Width| -> Result<()> {
        if t == Width::Q {
            Ok(())
        } else {
            Err(Error::Message(format!("{name}: error: invalid width type")))
        }
    };

    let opts = match info.number {
        LOWPASS_1 | HIGHPASS_1 | ALLPASS_1 | LOWPASS_1P => {
            let o = common(1, &mut g)?;
            arg0 = freq_arg(argv[g.ind], "f0")?;
            o
        }
        LOWSHELF_1 | HIGHSHELF_1 => {
            let o = common(2, &mut g)?;
            arg0 = freq_arg(argv[g.ind], "f0")?;
            arg2 = get_number(name, argv[g.ind + 1], "gain")?;
            o
        }
        LOWPASS | HIGHPASS | BANDPASS_SKIRT | BANDPASS_PEAK | NOTCH | ALLPASS => {
            let o = common(2, &mut g)?;
            arg0 = freq_arg(argv[g.ind], "f0")?;
            (arg1, width_type) = width_arg(argv[g.ind + 1], "width")?;
            no_slope(width_type)?;
            o
        }
        PEAK | LOWSHELF | HIGHSHELF => {
            let o = common(3, &mut g)?;
            arg0 = freq_arg(argv[g.ind], "f0")?;
            (arg1, width_type) = width_arg(argv[g.ind + 1], "width")?;
            if info.number == PEAK {
                no_slope(width_type)?;
            }
            arg2 = get_number(name, argv[g.ind + 2], "gain")?;
            o
        }
        LOWPASS_TRANSFORM | HIGHPASS_TRANSFORM => {
            let o = common(4, &mut g)?;
            arg0 = freq_arg(argv[g.ind], "fz")?;
            (arg1, width_type) = width_arg(argv[g.ind + 1], "width_z")?;
            only_q(width_type)?;
            arg2 = freq_arg(argv[g.ind + 2], "fp")?;
            (arg3, width_type) = width_arg(argv[g.ind + 3], "width_p")?;
            only_q(width_type)?;
            o
        }
        DEEMPH => {
            let o = common(0, &mut g)?;
            kind = HIGHSHELF;
            width_type = Width::Slope;
            match fs {
                44_100 => (arg0, arg1, arg2) = (5283.0, 0.4845, -9.477),
                48_000 => (arg0, arg1, arg2) = (5356.0, 0.479, -9.62),
                _ => {
                    return Err(Error::Message(format!(
                        "{name}: error: sample rate must be 44100 or 48000"
                    )))
                }
            }
            o
        }
        BIQUAD => {
            let o = common(6, &mut g)?;
            let names = ["b0", "b1", "b2", "a0", "a1", "a2"];
            for (k, p) in names.iter().enumerate() {
                raw[k] = get_number(name, argv[g.ind + k], p)?;
            }
            o
        }
        _ => return Err(Error::Message(format!("{name}: no such effect number"))),
    };
    let b = if info.number == BIQUAD {
        Biquad::new(raw[0], raw[1], raw[2], raw[3], raw[4], raw[5])
    } else {
        Biquad::from_type(kind, f64::from(fs), arg0, arg1, arg2, arg3, width_type)
    };
    if opts.0 {
        return Ok(crate::reverse_iir::from_biquad(
            info, istream, selector, &b, opts.1,
        ));
    }
    let selected = default_selector(selector, istream.channels);
    Ok(Box::new(BiquadEffect {
        name: info.name,
        stream: *istream,
        state: vec![b; istream.channels],
        selected,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chain;

    const S1: Stream = Stream {
        fs: 48_000,
        channels: 1,
    };

    fn gain_db_at(args: &[&str], hz: f64) -> f64 {
        let mut c = Chain::from_args(S1, args).unwrap();
        let n = 48_000;
        let x: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * hz * i as f64 / 48_000.0).sin())
            .collect();
        let y = c.run(&x);
        let rms = |v: &[f64]| (v.iter().map(|a| a * a).sum::<f64>() / v.len() as f64).sqrt();
        20.0 * (rms(&y[n / 2..]) / rms(&x[n / 2..])).log10()
    }

    #[test]
    fn widths_parse_with_their_suffixes_and_butterworth_sections() {
        assert_eq!(parse_width("0.7"), Some((0.7, Width::Q)));
        assert_eq!(parse_width("0.7q"), Some((0.7, Width::Q)));
        assert_eq!(parse_width("0.5s"), Some((0.5, Width::Slope)));
        assert_eq!(parse_width("6d"), Some((6.0, Width::SlopeDb)));
        assert_eq!(parse_width("1.5o"), Some((1.5, Width::BwOct)));
        assert_eq!(parse_width("100h"), Some((100.0, Width::BwHz)));
        assert_eq!(parse_width("1k"), Some((1000.0, Width::BwHz)));
        assert_eq!(parse_width("0.7x"), None);
        assert_eq!(parse_width("0.7qq"), None);
        assert_eq!(parse_width("q"), None);
        // bw2 is one section with Q = 1/sqrt(2); bw4 has two sections with different Qs.
        let (q2, _) = parse_width("bw2").unwrap();
        assert!((q2 - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        let (q4a, _) = parse_width("bw4.0").unwrap();
        let (q4b, _) = parse_width("bw4.1").unwrap();
        assert!((q4a - 0.541_196_100_146_197).abs() < 1e-9, "{q4a}");
        assert!((q4b - 1.306_562_964_876_377).abs() < 1e-9, "{q4b}");
        assert_eq!(parse_width("bw4.2"), None);
        assert_eq!(parse_width("bw1"), None);
        assert_eq!(parse_width("bw4x"), None);
    }

    #[test]
    fn the_second_order_filters_have_their_textbook_responses() {
        // Low pass at 1 kHz with Q = 1/sqrt(2): -3 dB at the corner, 12 dB per octave above.
        assert!(
            (gain_db_at(&["lowpass", "1k", "0.7071067811865476q"], 1000.0) + 3.0103).abs() < 0.05
        );
        assert!((gain_db_at(&["lowpass", "1k", "0.7071067811865476q"], 4000.0) + 24.0).abs() < 1.2);
        assert!(
            (gain_db_at(&["highpass", "1k", "0.7071067811865476q"], 1000.0) + 3.0103).abs() < 0.05
        );
        // A peak of +6 dB is +6 dB at its centre and nothing far away; a notch is deep.
        assert!((gain_db_at(&["eq", "1k", "1q", "6"], 1000.0) - 6.0).abs() < 0.05);
        assert!(gain_db_at(&["eq", "1k", "1q", "6"], 50.0).abs() < 0.2);
        assert!(gain_db_at(&["notch", "1k", "5q"], 1000.0) < -40.0);
        // Shelves reach their gain far from the corner.
        assert!((gain_db_at(&["lowshelf", "500", "0.7q", "6"], 20.0) - 6.0).abs() < 0.1);
        assert!((gain_db_at(&["highshelf", "2k", "0.7q", "-6"], 20_000.0) + 6.0).abs() < 0.3);
        // An all-pass passes every level.
        assert!(gain_db_at(&["allpass", "1k", "0.7q"], 3000.0).abs() < 0.01);
    }

    #[test]
    fn first_order_filters_and_the_single_pole() {
        assert!((gain_db_at(&["lowpass_1", "1k"], 1000.0) + 3.0103).abs() < 0.1);
        assert!((gain_db_at(&["highpass_1", "1k"], 1000.0) + 3.0103).abs() < 0.1);
        assert!(gain_db_at(&["allpass_1", "1k"], 123.0).abs() < 0.01);
        assert!((gain_db_at(&["lowshelf_1", "1k", "6"], 20.0) - 6.0).abs() < 0.1);
        assert!((gain_db_at(&["highshelf_1", "1k", "-6"], 20_000.0) + 6.0).abs() < 0.3);
        // `lowpass_1p` is an EWMA with unit DC gain.
        assert!(gain_db_at(&["lowpass_1p", "1k"], 20.0).abs() < 0.05);
        assert!(gain_db_at(&["lowpass_1p", "1k"], 10_000.0) < -15.0);
    }

    #[test]
    fn a_raw_biquad_is_normalised_by_a0_and_a_slope_changes_the_shelf() {
        // y = 0.5 x, written with a0 = 2.
        let mut c = Chain::from_args(S1, &["biquad", "1", "0", "0", "2", "0", "0"]).unwrap();
        assert_eq!(c.run(&[1.0, 2.0]), vec![0.5, 1.0]);
        // A shelf with a steeper slope has a different corner, so a different level at 700 Hz.
        let gentle = gain_db_at(&["lowshelf", "500", "0.3s", "6"], 700.0);
        let steep = gain_db_at(&["lowshelf", "500", "1s", "6"], 700.0);
        assert!((gentle - steep).abs() > 0.3, "{gentle} {steep}");
    }

    #[test]
    fn the_linkwitz_transform_moves_a_pole_and_deemphasis_needs_cd_rates() {
        // Zero at 50 Hz, pole at 25 Hz: lifts the bottom by 40 * log10(50 / 25) = 12 dB.
        let low = gain_db_at(&["linkwitz_transform", "50", "0.7q", "25", "0.7q"], 10.0);
        let top = gain_db_at(&["linkwitz_transform", "50", "0.7q", "25", "0.7q"], 5_000.0);
        assert!((low - 12.0).abs() < 0.5, "{low}");
        assert!(top.abs() < 0.01, "{top}");
        assert!(Chain::from_args(S1, &["deemph"]).is_ok());
        assert!(Chain::from_args(
            Stream {
                fs: 44_100,
                channels: 1
            },
            &["deemph"]
        )
        .is_ok());
        assert!(Chain::from_args(
            Stream {
                fs: 96_000,
                channels: 1
            },
            &["deemph"]
        )
        .is_err());
        // De-emphasis cuts the top by about 9.5 dB.
        assert!(gain_db_at(&["deemph"], 20_000.0) < -8.0);
    }

    #[test]
    fn arguments_are_checked_like_the_original() {
        assert!(Chain::from_args(S1, &["lowpass", "1k"]).is_err());
        assert!(Chain::from_args(S1, &["lowpass", "1k", "0.7q", "x"]).is_err());
        assert!(Chain::from_args(S1, &["lowpass", "24k", "0.7q"]).is_err());
        assert!(Chain::from_args(S1, &["lowpass", "1k", "0q"]).is_err());
        assert!(Chain::from_args(S1, &["lowpass", "1k", "1s"]).is_err());
        assert!(Chain::from_args(S1, &["eq", "1k", "1s", "3"]).is_err());
        assert!(Chain::from_args(S1, &["lowshelf", "1k", "1s", "3"]).is_ok());
        assert!(Chain::from_args(S1, &["lowpass_transform", "1k", "1o", "1k", "1q"]).is_err());
        // A negative gain is an argument, not an option.
        assert!(Chain::from_args(S1, &["eq", "1k", "1q", "-3"]).is_ok());
        assert!(Chain::from_args(S1, &["lowpass", "-r", "1k", "0.7q"]).is_ok());
        assert!(Chain::from_args(S1, &["lowpass", "-r5", "1k", "0.7q"]).is_err());
        assert!(Chain::from_args(S1, &["lowpass", "-z", "1k", "0.7q"]).is_err());
    }

    #[test]
    fn selected_channels_only_and_reset_clears_the_memory() {
        let s2 = Stream {
            fs: 48_000,
            channels: 2,
        };
        let mut c = Chain::from_args(s2, &[":1", "lowpass_1p", "100"]).unwrap();
        let y = c.run(&[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(y[0], 1.0);
        assert_eq!(y[2], 1.0);
        assert!(y[1] < 0.1 && y[3] > y[1]);
        c.reset();
        let z = c.run(&[1.0, 1.0]);
        assert_eq!(z[1], y[1]);
    }

    #[test]
    fn the_response_helper_matches_the_measured_gain() {
        let b = Biquad::from_type(PEAK, 48_000.0, 1000.0, 1.0, 6.0, 0.0, Width::Q);
        let (re, im) = b.response(2.0 * PI * 1000.0 / 48_000.0);
        let db = 10.0 * (re * re + im * im).log10();
        assert!((db - 6.0).abs() < 1e-9, "{db}");
    }
}
