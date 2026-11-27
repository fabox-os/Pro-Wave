//! `decorrelate` (`decorrelate.c`): an all-pass decorrelator after Schlecht, "Frequency-
//! Dependent Schroeder Allpass Filters" (Applied Sciences, 2020). Each channel gets a chain of
//! all-pass filters with random delays of 0.83 to 3.1 ms whose decay depends on frequency, so
//! the channels become uncorrelated without changing their levels. With the defaults the
//! average group delay at high frequencies is about 9.5 ms.
//!
//! The delays are drawn from one random sequence that lives as long as the program, as in the
//! original; `-s seed` restarts it. Two decorrelators made one after the other therefore get
//! different delays, and a program that makes the same one first always gets the same.

use std::f64::consts::PI;
use std::sync::Mutex;

use crate::args::{check_freq, get_freq, get_len, parse_len_frac, strtol, Getopt};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::rng::{pm_rand1_r, PM_RAND_MAX};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "decorrelate",
    usage:
        "[-m] [-s seed] [-d delay_min] [-D delay_max] [-f fc[k]] [-l rt60_lf] [-h rt60_hf] [stages]",
    init,
    number: 0,
}];

/// The random sequence of the delays (`static uint32_t seed = 1` in the original).
static SEED: Mutex<u32> = Mutex::new(1);

/// Starts the delay sequence over.
pub(crate) fn reset_seed() {
    *SEED.lock().unwrap_or_else(|e| e.into_inner()) = 1;
}

struct SchAp {
    len: usize,
    p: usize,
    mx: Vec<f64>,
    my: Vec<f64>,
    b0: f64,
    b1: f64,
    a0: f64,
    a1: f64,
}

impl SchAp {
    fn new(fs: u32, delay_samples: i64, fc: f64, rt60_lf: f64, rt60_hf: f64) -> Self {
        let len = (delay_samples + 1).max(1) as usize;
        let fs = f64::from(fs);
        let d = delay_samples as f64;
        let gain_lf = -60.0 / (rt60_lf * fs) * d;
        let gain_hf = -60.0 / (rt60_hf * fs) * d;
        let w0 = 2.0 * PI * fc / fs;
        let t = (w0 / 2.0).tan();
        let g_hf = 10f64.powf(gain_hf / 20.0);
        let gd = 10f64.powf((gain_lf - gain_hf) / 20.0);
        let sgd = gd.sqrt();
        let a0 = t + sgd;
        Self {
            len,
            p: 0,
            mx: vec![0.0; len],
            my: vec![0.0; len],
            a1: (t - sgd) / a0,
            b0: (gd * t - sgd) / a0 * g_hf,
            b1: (gd * t + sgd) / a0 * g_hf,
            a0: 1.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let i0 = if self.p < 1 { self.len } else { self.p } - 1;
        let i_n1 = self.p;
        let i_n2 = if self.p + 1 >= self.len {
            0
        } else {
            self.p + 1
        };
        let r =
            self.b1 * x + self.b0 * self.mx[i0] + self.a1 * self.mx[i_n2] + self.a0 * self.mx[i_n1]
                - self.a1 * self.my[i0]
                - self.b0 * self.my[i_n2]
                - self.b1 * self.my[i_n1];
        self.mx[self.p] = x;
        self.my[self.p] = r;
        self.p = if self.p + 1 >= self.len {
            0
        } else {
            self.p + 1
        };
        r
    }

    fn reset(&mut self) {
        self.p = 0;
        self.mx.fill(0.0);
        self.my.fill(0.0);
    }
}

struct Decorrelate {
    stream: Stream,
    /// Per channel, the chain of all-pass stages (empty for channels that are not selected).
    ap: Vec<Vec<SchAp>>,
}

impl Effect for Decorrelate {
    fn name(&self) -> &'static str {
        "decorrelate"
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
            for (s, stages) in frame.iter_mut().zip(self.ap.iter_mut()) {
                for ap in stages.iter_mut() {
                    *s = ap.run(*s);
                }
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.ap.iter_mut().flatten().for_each(SchAp::reset);
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
    let name = argv[0];
    let argc = argv.len();
    let fs = istream.fs;
    let range = |param: &str| Error::Range {
        effect: name.into(),
        param: param.into(),
    };
    let mut g = Getopt::default();
    let (mut mono, mut n_stages) = (false, 5i64);
    let mut new_seed: Option<u32> = None;
    let mut delay_min = (0.83333e-3 * f64::from(fs)).round() as i64;
    let mut delay_max = (3.12503e-3 * f64::from(fs)).round() as i64;
    let (mut filter_fc, mut rt60_lf, mut rt60_hf) = (1100.0, 0.1, 0.008);
    let parse_rt60 = |text: &str, param: &str| -> Result<f64> {
        let samples = parse_len_frac(text, f64::from(fs)).ok_or_else(|| Error::Parse {
            effect: name.into(),
            param: param.into(),
            text: text.into(),
        })?;
        Ok(samples / f64::from(fs))
    };
    while let Some(opt) = g.next(argc, argv, "ms:d:D:f:l:h:") {
        let arg = g.arg.clone().unwrap_or_default();
        match opt {
            'm' => mono = true,
            's' => {
                let (v, end) = strtol(&arg);
                if end == 0 || end != arg.len() {
                    return Err(Error::Parse {
                        effect: name.into(),
                        param: "seed".into(),
                        text: arg,
                    });
                }
                if v <= 0 || v > i64::from(PM_RAND_MAX) {
                    return Err(range("seed"));
                }
                new_seed = Some(v as u32);
            }
            'd' => {
                delay_min = get_len(name, &arg, fs, "delay_min")?;
                if delay_min <= 0 || delay_min > i64::from(fs) * 2 {
                    return Err(range("delay_min"));
                }
            }
            'D' => {
                delay_max = get_len(name, &arg, fs, "delay_max")?;
                if delay_max <= 0 || delay_max > i64::from(fs) * 2 {
                    return Err(range("delay_max"));
                }
            }
            'f' => {
                filter_fc = get_freq(name, &arg, "fc")?;
                check_freq(name, filter_fc, fs, "fc")?;
            }
            'l' => {
                rt60_lf = parse_rt60(&arg, "rt60_lf")?;
                if rt60_lf <= 0.0 {
                    return Err(range("rt60_lf"));
                }
            }
            'h' => {
                rt60_hf = parse_rt60(&arg, "rt60_hf")?;
                if rt60_hf <= 0.0 {
                    return Err(range("rt60_hf"));
                }
            }
            _ => return Err(info.usage_error()),
        }
    }
    if delay_max <= delay_min {
        return Err(Error::Message(format!(
            "{name}: error: delay_max must be greater than delay_min"
        )));
    }
    if g.ind + 1 < argc {
        return Err(info.usage_error());
    } else if g.ind + 1 == argc {
        let text = argv[g.ind];
        let (v, end) = strtol(text);
        if end == 0 || end != text.len() {
            return Err(Error::Parse {
                effect: name.into(),
                param: "stages".into(),
                text: text.into(),
            });
        }
        if v <= 0 || v > 100 {
            return Err(range("stages"));
        }
        n_stages = v;
    }
    let n_stages = n_stages as usize;
    let selected = default_selector(selector, istream.channels);
    let mut seed = SEED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = new_seed {
        *seed = s;
    }
    let mut random_delay = || -> i64 {
        let r = f64::from(pm_rand1_r(&mut seed)) / f64::from(PM_RAND_MAX)
            * (delay_max - delay_min) as f64
            + delay_min as f64;
        r.round() as i64
    };
    let mut ap: Vec<Vec<SchAp>> = selected
        .iter()
        .map(|_| Vec::with_capacity(n_stages))
        .collect();
    for _ in 0..n_stages {
        let d = if mono { random_delay() } else { 0 };
        for (k, on) in selected.iter().enumerate() {
            if *on {
                let delay = if mono { d } else { random_delay() };
                ap[k].push(SchAp::new(fs, delay, filter_fc, rt60_lf, rt60_hf));
            }
        }
    }
    Ok(Box::new(Decorrelate {
        stream: *istream,
        ap,
    }))
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    fn noise(n: usize) -> Vec<f64> {
        let mut s = 12345u32;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                f64::from(s >> 16) / 32768.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn it_keeps_the_level_and_makes_the_channels_unlike_each_other() {
        let mono = noise(48_000);
        let x: Vec<f64> = mono.iter().flat_map(|v| [*v, *v]).collect();
        let y = Chain::from_args(S, &["decorrelate", "-s", "1"])
            .unwrap()
            .run(&x);
        let (l, r): (Vec<f64>, Vec<f64>) =
            y.as_chunks::<2>().0.iter().map(|f| (f[0], f[1])).unzip();
        let power = |v: &[f64]| v.iter().map(|a| a * a).sum::<f64>() / v.len() as f64;
        // All-pass: the power is kept (after the start-up).
        assert!((power(&l[8000..]) / power(&mono[8000..]) - 1.0).abs() < 0.15);
        // Two different delay chains: the two channels no longer agree.
        let corr = l[8000..]
            .iter()
            .zip(&r[8000..])
            .map(|(a, b)| a * b)
            .sum::<f64>()
            / (l.len() - 8000) as f64
            / power(&l[8000..]);
        assert!(corr.abs() < 0.3, "{corr}");
        // Mono mode gives both channels the same delays, so they stay the same.
        let y = Chain::from_args(S, &["decorrelate", "-m", "-s", "1"])
            .unwrap()
            .run(&x);
        assert!(y
            .as_chunks::<2>()
            .0
            .iter()
            .all(|f| (f[0] - f[1]).abs() < 1e-12));
    }

    #[test]
    fn the_seed_makes_it_repeatable_and_options_are_checked() {
        let x = noise(2 * 2000);
        let a = Chain::from_args(S, &["decorrelate", "-s", "42", "3"])
            .unwrap()
            .run(&x);
        let b = Chain::from_args(S, &["decorrelate", "-s", "42", "3"])
            .unwrap()
            .run(&x);
        assert_eq!(a, b);
        for bad in [
            vec!["decorrelate", "-s", "0"],
            vec!["decorrelate", "-d", "5m", "-D", "4m"],
            vec!["decorrelate", "-f", "30k"],
            vec!["decorrelate", "-l", "0"],
            vec!["decorrelate", "0"],
            vec!["decorrelate", "101"],
            vec!["decorrelate", "-z"],
            vec!["decorrelate", "1", "2"],
        ] {
            assert!(Chain::from_args(S, &bad).is_err(), "{bad:?}");
        }
    }
}
