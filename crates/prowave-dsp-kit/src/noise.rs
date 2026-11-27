//! `noise` (`noise.c`): adds TPDF noise to the selected channels. The level is a peak level in
//! dBFS, or the effective precision in bits with a `b` suffix.

use crate::args::strtod;
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::rng::{Tpdf, PM_RAND_MAX};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "noise",
    usage: "level",
    init,
    number: 0,
}];

struct Noise {
    stream: Stream,
    selected: Vec<bool>,
    mult: f64,
    rng: Tpdf,
}

impl Effect for Noise {
    fn name(&self) -> &'static str {
        "noise"
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
            for (s, on) in frame.iter_mut().zip(&self.selected) {
                if *on {
                    *s += self.rng.noise(self.mult);
                }
            }
        }
        Buf::Input
    }
}

/// `noise_parse_level`: the linear level, or `None` if the text does not parse.
fn parse_level(s: &str) -> Option<f64> {
    let (v, end) = strtod(s);
    if end == 0 {
        return None;
    }
    let mut l = 10f64.powf(v / 20.0);
    let mut rest = &s[end..];
    if rest.starts_with('b') {
        l = 2.0 / v.exp2();
        rest = &rest[1..];
    }
    rest.is_empty().then_some(l)
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
    if argv.len() != 2 {
        return Err(info.usage_error());
    }
    let level = parse_level(argv[1]).ok_or_else(|| Error::Parse {
        effect: argv[0].into(),
        param: "level".into(),
        text: argv[1].into(),
    })?;
    Ok(Box::new(Noise {
        stream: *istream,
        selected: default_selector(selector, istream.channels),
        mult: level / f64::from(PM_RAND_MAX),
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

    #[test]
    fn the_level_is_dbfs_or_bits() {
        let rms = |args: &[&str], sel: &str| {
            let mut c = Chain::from_args(S, &[sel, args[0], args[1]]).unwrap();
            let y = c.run(&vec![0.0; 2 * 48_000]);
            let r = |k: usize| {
                (y.iter().skip(k).step_by(2).map(|v| v * v).sum::<f64>() / 48_000.0).sqrt()
            };
            (r(0), r(1))
        };
        // TPDF with peak 0.1: rms is peak / sqrt(6).
        let (l, r) = rms(&["noise", "-20"], ":0");
        assert!((l - 0.1 / 6f64.sqrt()).abs() < 0.001, "{l}");
        assert_eq!(r, 0.0);
        // 8 bits: peak 2 / 2^8.
        let (l, _) = rms(&["noise", "8b"], ":-");
        assert!((l - 2.0 / 256.0 / 6f64.sqrt()).abs() < 0.0005, "{l}");
    }

    #[test]
    fn bad_levels_are_errors() {
        assert!(Chain::from_args(S, &["noise"]).is_err());
        assert!(Chain::from_args(S, &["noise", "x"]).is_err());
        assert!(Chain::from_args(S, &["noise", "-20x"]).is_err());
        assert!(Chain::from_args(S, &["noise", "8bb"]).is_err());
    }
}
