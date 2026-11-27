//! `dither` (`dither.c`): TPDF dither with optional noise shaping.
//!
//! The shapes are `flat` (plain TPDF), `sloped` and `sloped2` (feedback with a first-order
//! high-pass shape), `lipshitz` (the 5-tap e-weighted curve of Lipshitz, Vanderkooy and
//! Wannamaker, J. AES 1991, for 44.1 kHz), and `wan3` and `wan9` (Wannamaker's 3- and 9-tap
//! f-weighted curves, J. AES 1992, for 46 kHz). The shaped curves only apply near their sample
//! rate (5 %); anywhere else the `sloped` shape is used instead.
//!
//! Like the original, one random sequence is shared by all channels of the effect and each
//! channel dithers a whole block before the next one starts.

use crate::args::{get_number, strtol_base0};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::rng::{Tpdf, PM_RAND_MAX};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "dither",
    usage: "[shape] [[quantize_bits] bits|auto]",
    init,
    number: 0,
}];

const MAX_FIR_LEN: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Flat,
    Sloped,
    Sloped2,
    Lipshitz44,
    Wan3,
    Wan9,
}

struct ShapeInfo {
    name: &'static str,
    shape: Shape,
    fs: i64,
}

const SHAPES: [ShapeInfo; 6] = [
    ShapeInfo {
        name: "flat",
        shape: Shape::Flat,
        fs: 0,
    },
    ShapeInfo {
        name: "sloped",
        shape: Shape::Sloped,
        fs: 0,
    },
    ShapeInfo {
        name: "sloped2",
        shape: Shape::Sloped2,
        fs: 0,
    },
    ShapeInfo {
        name: "lipshitz",
        shape: Shape::Lipshitz44,
        fs: 44_100,
    },
    ShapeInfo {
        name: "wan3",
        shape: Shape::Wan3,
        fs: 46_000,
    },
    ShapeInfo {
        name: "wan9",
        shape: Shape::Wan9,
        fs: 46_000,
    },
];

const FILTER_LIPSHITZ_44: [f64; 5] = [2.033, -2.165, 1.959, -1.590, 0.6149];
const FILTER_WAN3_44: [f64; 3] = [1.623, -0.982, 0.109];
const FILTER_WAN9_44: [f64; 9] = [
    2.412, -3.370, 3.937, -4.174, 3.353, -2.205, 1.281, -0.569, 0.0847,
];

/// `get_dither_type_info`: a shape by name; with `fs` non-zero only if it suits that rate.
fn shape_info(name: &str, fs: i64) -> Option<&'static ShapeInfo> {
    SHAPES.iter().find(|s| {
        s.name == name && (fs == 0 || s.fs == 0 || ((s.fs - fs).abs() as f64) < s.fs as f64 * 0.05)
    })
}

const ENABLE: u8 = 1;
const NOISE_BITS_AUTO: u8 = 2;
const QUANTIZE_BITS_AUTO: u8 = 4;

#[derive(Clone, Debug)]
struct ChannelState {
    shape: Shape,
    n_mult: f64,
    q_mult: [f64; 2],
    z_1: f64,
    fir_buf: [f64; MAX_FIR_LEN],
    m0: i32,
    p: usize,
    flags: u8,
}

impl ChannelState {
    fn new(quantize_bits: i32, noise_bits: f64, shape: Shape, flags: u8) -> Self {
        let mut s = Self {
            shape,
            n_mult: 0.0,
            q_mult: [1.0, 1.0],
            z_1: 0.0,
            fir_buf: [0.0; MAX_FIR_LEN],
            m0: 1,
            p: 0,
            flags,
        };
        s.set_noise_bits(noise_bits);
        s.set_quantize_bits(quantize_bits);
        s
    }

    fn reset(&mut self) {
        self.z_1 = 0.0;
        self.fir_buf = [0.0; MAX_FIR_LEN];
        self.m0 = 1;
        self.p = 0;
    }

    fn set_noise_bits(&mut self, noise_bits: f64) {
        self.n_mult = 2.0 / noise_bits.exp2() / f64::from(PM_RAND_MAX);
    }

    fn set_quantize_bits(&mut self, bits: i32) {
        let bits = bits.clamp(2, 32);
        self.q_mult[0] = (1u64 << (bits - 1)) as f64;
        self.q_mult[1] = 1.0 / self.q_mult[0];
    }

    /// `FIR_DEFINE_FN`: feeds `s` into the error-feedback filter and takes its output.
    #[inline]
    fn fir(&mut self, filter: &[f64], s: f64) -> f64 {
        let n_taps = filter.len();
        let mut n = self.p;
        for c in filter {
            self.fir_buf[n] += s * c;
            n = if n + 1 < n_taps { n + 1 } else { 0 };
        }
        let r = self.fir_buf[self.p];
        self.fir_buf[self.p] = 0.0;
        self.p = if self.p + 1 < n_taps { self.p + 1 } else { 0 };
        r
    }

    /// Dithers `samples` values of `buf`, `stride` apart.
    fn run(&mut self, rng: &mut Tpdf, buf: &mut [f64], samples: usize, stride: usize) {
        let mut idx = 0;
        for _ in 0..samples {
            let noise = match self.shape {
                Shape::Sloped2 => {
                    let n1 = rng.next1() as i32;
                    let n2 = self.m0;
                    self.m0 = n1;
                    f64::from(n1 - n2) * self.n_mult
                }
                _ => rng.noise(self.n_mult),
            };
            let x = buf[idx];
            buf[idx] = match self.shape {
                Shape::Flat => self.q_mult[1] * (self.q_mult[0] * (x + noise)).round_ties_even(),
                shape => {
                    let fb = match shape {
                        Shape::Lipshitz44 => self.fir(&FILTER_LIPSHITZ_44, self.z_1),
                        Shape::Wan3 => self.fir(&FILTER_WAN3_44, self.z_1),
                        Shape::Wan9 => self.fir(&FILTER_WAN9_44, self.z_1),
                        _ => self.z_1,
                    };
                    let p0 = x - fb;
                    let p1 = self.q_mult[1] * (self.q_mult[0] * (p0 + noise)).round_ties_even();
                    self.z_1 = p1 - p0;
                    p1
                }
            };
            idx += stride;
        }
    }
}

struct Dither {
    stream: Stream,
    selected: Vec<bool>,
    state: Vec<ChannelState>,
    rng: Tpdf,
}

impl Effect for Dither {
    fn name(&self) -> &'static str {
        "dither"
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for k in 0..ch {
            if self.selected[k] && self.state[k].flags & ENABLE != 0 {
                self.state[k].run(&mut self.rng, &mut ibuf[k..], *frames, ch);
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        for (on, s) in self.selected.iter().zip(self.state.iter_mut()) {
            if *on {
                s.reset();
            }
        }
    }
    fn is_dither(&self) -> bool {
        true
    }
    fn set_dither_params(&mut self, bits: i32, enabled: bool) -> bool {
        for (on, s) in self.selected.iter().zip(self.state.iter_mut()) {
            if !*on {
                continue;
            }
            if s.flags & NOISE_BITS_AUTO != 0 {
                if !enabled || !(2..=32).contains(&bits) {
                    s.flags &= !ENABLE;
                } else {
                    s.set_noise_bits(f64::from(bits));
                    s.flags |= ENABLE;
                }
            }
            if s.flags & QUANTIZE_BITS_AUTO != 0 {
                s.set_quantize_bits(if bits < 2 { 32 } else { bits });
            }
        }
        true
    }
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
    let argc = argv.len();
    if argc > 4 {
        return Err(info.usage_error());
    }
    let (mut shape_arg, mut qb_arg, mut nb_arg) = (0, 0, 0);
    if argc == 2 {
        if shape_info(argv[1], 0).is_some() {
            shape_arg = 1;
        } else {
            nb_arg = 1;
        }
    } else {
        if argc == 3 {
            if shape_info(argv[1], 0).is_some() {
                shape_arg = 1;
            } else {
                qb_arg = 1;
            }
        } else if argc == 4 {
            shape_arg = 1;
            qb_arg = 2;
        }
        nb_arg = argc - 1;
    }
    let mut shape = Shape::Flat;
    let mut flags = ENABLE;
    let mut noise_bits = f64::INFINITY;
    let mut quantize_bits = 0i32;
    if shape_arg != 0 {
        shape =
            shape_info(argv[shape_arg], i64::from(istream.fs)).map_or(Shape::Sloped, |i| i.shape);
    }
    if qb_arg != 0 {
        if argv[qb_arg] == "auto" {
            flags |= QUANTIZE_BITS_AUTO;
        } else {
            let (v, end) = strtol_base0(argv[qb_arg]);
            if end == 0 || end != argv[qb_arg].len() {
                return Err(Error::Parse {
                    effect: argv[0].into(),
                    param: "quantize_bits".into(),
                    text: argv[qb_arg].into(),
                });
            }
            if !(2..=32).contains(&v) {
                return Err(Error::Message(format!(
                    "{}: error: quantize_bits must be within [2,32]",
                    argv[0]
                )));
            }
            quantize_bits = v as i32;
        }
    }
    if nb_arg != 0 && argv[nb_arg] != "auto" {
        noise_bits = get_number(argv[0], argv[nb_arg], "noise_bits")?;
        if !noise_bits.is_finite() {
            return Err(Error::Message(format!(
                "{}: error: bits is invalid: {noise_bits}",
                argv[0]
            )));
        }
        if qb_arg == 0 && flags & QUANTIZE_BITS_AUTO == 0 {
            quantize_bits = noise_bits.round_ties_even().clamp(2.0, 32.0) as i32;
        }
    } else {
        flags |= NOISE_BITS_AUTO;
        if qb_arg == 0 {
            flags |= QUANTIZE_BITS_AUTO;
        }
    }
    if quantize_bits == 0 {
        quantize_bits = 16;
    }
    if noise_bits == f64::INFINITY {
        noise_bits = 16.0;
    }
    let selected = default_selector(selector, istream.channels);
    let state = selected
        .iter()
        .map(|on| {
            if *on {
                ChannelState::new(quantize_bits, noise_bits, shape, flags)
            } else {
                ChannelState::new(16, 16.0, Shape::Flat, 0)
            }
        })
        .collect();
    Ok(Box::new(Dither {
        stream: *istream,
        selected,
        state,
        rng: Tpdf::default(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    fn quantized(y: &[f64], bits: i32) -> bool {
        let q = f64::from(1u32 << (bits - 1));
        y.iter().all(|v| ((v * q).round() - v * q).abs() < 1e-9)
    }

    #[test]
    fn the_output_lies_on_the_grid_of_the_quantize_bits() {
        let x: Vec<f64> = (0..4000).map(|i| 0.3 * (i as f64 * 0.05).sin()).collect();
        for words in [
            vec!["dither", "flat", "12"],
            vec!["dither", "sloped", "12"],
            vec!["dither", "sloped2", "12"],
            vec!["dither", "wan3", "12"],
            vec!["dither", "wan9", "12"],
            vec!["dither", "12"],
        ] {
            let y = Chain::from_args(S, &words).unwrap().run(&x);
            assert!(quantized(&y, 12), "{words:?}");
        }
        // A separate quantize width: 10 bit grid, noise of 16 bits.
        let y = Chain::from_args(S, &["dither", "flat", "10", "16"])
            .unwrap()
            .run(&x);
        assert!(quantized(&y, 10));
    }

    #[test]
    fn dither_keeps_a_signal_smaller_than_one_step() {
        // 0.3 of a 8-bit step, steady: the average of the dithered output stays near it.
        let step = 1.0 / 128.0;
        let x = vec![0.3 * step; 2 * 200_000];
        let y = Chain::from_args(S, &["dither", "flat", "8"])
            .unwrap()
            .run(&x);
        let mean = y.iter().step_by(2).sum::<f64>() / 200_000.0;
        assert!((mean - 0.3 * step).abs() < 0.05 * step, "{mean}");
    }

    #[test]
    fn noise_shaping_moves_the_noise_up_in_frequency() {
        // Noise power below 5 kHz against above 15 kHz: flat is even, shaped has far more on
        // top than at the bottom.
        let x = vec![0.0; 2 * 8192];
        let split = |shape: &str| {
            let y = Chain::from_args(
                Stream {
                    fs: 44_100,
                    channels: 2,
                },
                &["dither", shape, "8"],
            )
            .unwrap()
            .run(&vec![0.0013; x.len()]);
            let l: Vec<f64> = y.iter().step_by(2).copied().collect();
            let power = |lo: f64, hi: f64| {
                let mut p = 0.0;
                let mut f = lo;
                while f < hi {
                    let (mut re, mut im) = (0.0, 0.0);
                    for (i, v) in l.iter().enumerate() {
                        let w = 2.0 * std::f64::consts::PI * f * i as f64 / 44_100.0;
                        re += v * w.cos();
                        im += v * w.sin();
                    }
                    p += re * re + im * im;
                    f += 1000.0;
                }
                p
            };
            power(15_000.0, 20_000.0) / power(1_000.0, 5_000.0)
        };
        let flat = split("flat");
        let sloped = split("sloped");
        let lipshitz = split("lipshitz");
        assert!(sloped > 3.0 * flat, "{flat} {sloped}");
        assert!(lipshitz > 3.0 * flat, "{flat} {lipshitz}");
    }

    #[test]
    fn auto_follows_the_output_precision_and_can_be_switched_off() {
        let x = vec![0.1234567; 2 * 100];
        let mut c = Chain::from_args(S, &["dither"]).unwrap();
        // The original: a 24-bit output is not dithered, so the effect is switched off.
        assert!(!c.set_dither_params(24, false));
        assert_eq!(c.run(&x), x);
        let mut c = Chain::from_args(S, &["dither"]).unwrap();
        c.set_dither_params(16, true);
        assert!(quantized(&c.run(&x), 16));
        assert!(!c.needs_dither());
        // Without a dither effect the caller is told to add its own when it is wanted.
        let mut g = Chain::from_args(S, &["gain", "-3"]).unwrap();
        assert!(g.needs_dither());
        assert!(g.set_dither_params(16, true));
        assert!(!g.set_dither_params(16, false));
    }

    #[test]
    fn arguments_are_checked() {
        assert!(Chain::from_args(S, &["dither", "1", "2", "3", "4"]).is_err());
        assert!(Chain::from_args(S, &["dither", "flat", "1"]).is_ok());
        assert!(Chain::from_args(S, &["dither", "flat", "33", "16"]).is_err());
        assert!(Chain::from_args(S, &["dither", "flat", "x"]).is_err());
        assert!(Chain::from_args(S, &["dither", "flat", "16", "x"]).is_err());
        assert!(Chain::from_args(S, &["dither", "flat", "auto", "auto"]).is_ok());
        // A shape that does not suit the rate falls back to `sloped`.
        assert!(Chain::from_args(S, &["dither", "lipshitz", "16"]).is_ok());
        // Hexadecimal is accepted for the quantize width, like strtol with base 0.
        assert!(Chain::from_args(S, &["dither", "flat", "0x10", "16"]).is_ok());
    }
}
