//! `hilbert` (`hilbert.c`): a Hilbert transform (90° phase shift) FIR filter.

use std::f64::consts::PI;

use crate::args::{get_number, strtol, Getopt};
use crate::effect::{default_selector, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir::Filter;
use crate::fir_util::{init_convolver, Convolver};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "hilbert",
    usage: "[-pzc] [-a angle] taps",
    init,
    number: 0,
}];

/// The coefficients of a Blackman-windowed Hilbert filter of `taps` taps that shifts the phase
/// by `angle` radians (-π/2 is the Hilbert transform; 0 passes the signal unchanged).
pub fn design(taps: usize, angle: f64) -> Vec<f64> {
    let (w_h, w_d) = ((-angle).sin(), (-angle).cos());
    let mut h = vec![0.0; taps];
    for (i, v) in h.iter_mut().enumerate() {
        let k = i as i64 - (taps / 2) as i64;
        if k == 0 {
            *v = w_d;
        } else if k % 2 != 0 {
            let x = 2.0 * PI * i as f64 / (taps - 1) as f64;
            *v = w_h * 2.0 / (PI * k as f64) * (0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos());
        }
    }
    h
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let argc = argv.len();
    let mut g = Getopt::default();
    let mut conv = Convolver::Fir;
    let mut do_align = false;
    let mut angle = -PI / 2.0;
    while let Some(opt) = g.next(argc - 1, argv, "pzca:") {
        match opt {
            'p' => conv = Convolver::FirP,
            'z' => conv = Convolver::Zita,
            'c' => do_align = true,
            'a' => {
                angle = get_number(name, g.arg.as_deref().unwrap_or(""), "angle")? / 180.0 * PI;
            }
            _ => return Err(info.usage_error()),
        }
    }
    if g.ind != argc - 1 {
        return Err(info.usage_error());
    }
    let text = argv[g.ind];
    let (taps, end) = strtol(text);
    if end == 0 || end != text.len() {
        return Err(Error::Parse {
            effect: name.into(),
            param: "taps".into(),
            text: text.into(),
        });
    }
    if taps <= 3 {
        return Err(Error::Message(format!("{name}: error: taps must be > 3")));
    }
    if taps % 2 == 0 {
        return Err(Error::Message(format!("{name}: error: taps must be odd")));
    }
    if taps > 1 << 26 {
        return Err(Error::Range {
            effect: name.into(),
            param: "taps".into(),
        });
    }
    let taps = taps as usize;
    let h = design(taps, angle);
    let reference = if do_align { (taps / 2) as isize } else { 0 };
    let selector = default_selector(selector, istream.channels);
    let filter = Filter {
        data: &h,
        channels: 1,
        frames: taps,
        reference,
    };
    init_convolver(conv, info, istream, &selector, &filter).map(|e| vec![e])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chain, Stream};

    #[test]
    fn the_filter_is_antisymmetric_with_zero_even_taps() {
        let h = design(21, -PI / 2.0);
        for i in 0..21 {
            assert!((h[i] + h[20 - i]).abs() < 1e-12);
        }
        assert_eq!(h[10], (PI / 2.0).cos());
        assert_eq!(h[8], 0.0);
        assert!(h[9].abs() > 0.1);
    }

    #[test]
    fn angle_zero_passes_the_signal_through_with_the_filters_delay() {
        let s = Stream {
            fs: 48_000,
            channels: 1,
        };
        // A mono signal cannot be advanced, so `-c` leaves the delay of half the filter.
        let mut c = Chain::from_args(s, &["hilbert", "-c", "-a", "0", "101"]).unwrap();
        let x: Vec<f64> = (0..500)
            .map(|i| ((i * 13 % 7) as f64 - 3.0) / 4.0)
            .collect();
        let y = c.process(&x);
        assert!(y.len() >= x.len() + 50);
        for (i, a) in x.iter().enumerate() {
            assert!((a - y[i + 50]).abs() < 1e-12, "{i}");
        }
        assert!(Chain::from_args(s, &["hilbert", "100"]).is_err());
        assert!(Chain::from_args(s, &["hilbert", "3"]).is_err());
    }
}
