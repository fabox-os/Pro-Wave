//! `sinc` (`sinc.c`): a windowed-sinc low-pass or high-pass FIR filter.

use crate::args::{get_freq, get_number, Getopt};
use crate::effect::{default_selector, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir::Filter;
use crate::fir_util::{init_convolver, Convolver};
use crate::window::{norm_sinc, window, window_width, Window};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "sinc",
    usage: "[-hpzc] [-t width] [-w window] f0[k]",
    init,
    number: 0,
}];

const KAISER_BETA_DEFAULT: f64 = 12.0;
/// Filters longer than this are refused (a cut-off of a few millihertz would ask for gigabytes).
const MAX_TAPS: usize = 1 << 26;

/// The coefficients of the filter and how it is lined up: `(taps, reference)`.
fn design(
    fs: f64,
    f0: f64,
    mut tbw: f64,
    wn: Window,
    param: f64,
    highpass: bool,
    align: bool,
) -> Option<(Vec<f64>, isize)> {
    if f0 * (2.0 + tbw) > fs {
        tbw = (fs - 2.0 * f0) / f0;
    }
    let m_f = (window_width(wn, param) / (2.0 * tbw * f0 / fs)).round() * 2.0;
    if !m_f.is_finite() || m_f < 0.0 || m_f >= MAX_TAPS as f64 {
        return None;
    }
    let m = m_f as usize;
    let taps = m + 1;
    let fc = 2.0 * f0 / fs;
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            norm_sinc((i as f64 * 2.0 - m as f64) / 2.0, fc)
                * window(wn, i as f64 / m as f64, param)
        })
        .collect();
    if highpass {
        for v in h.iter_mut() {
            *v = -*v;
        }
        h[taps / 2] += 1.0;
    }
    Some((h, if align { (taps / 2) as isize } else { 0 }))
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let argc = argv.len();
    let range = |param: &str| Error::Range {
        effect: name.into(),
        param: param.into(),
    };
    let mut g = Getopt::default();
    let mut conv = Convolver::Fir;
    let mut wn = Window::Kaiser;
    let (mut do_align, mut highpass) = (false, false);
    let (mut tbw, mut param) = (0.5, KAISER_BETA_DEFAULT);
    while let Some(opt) = g.next(argc - 1, argv, "hpzct:w:") {
        let arg = g.arg.clone().unwrap_or_default();
        match opt {
            'h' => highpass = true,
            'p' => conv = Convolver::FirP,
            'z' => conv = Convolver::Zita,
            'c' => do_align = true,
            't' => {
                tbw = get_number(name, &arg, "width")?;
                if !(0.001..=2.0).contains(&tbw) {
                    return Err(range("width"));
                }
            }
            'w' => match arg.as_str() {
                "rect" => wn = Window::Rect,
                "hann" => wn = Window::Hann,
                "hamming" => wn = Window::Hamming,
                "blackman" => wn = Window::Blackman,
                "nuttall4c1" => wn = Window::Nuttall4c1,
                "albrecht9c3" => wn = Window::Albrecht9c3,
                a if a.starts_with("kaiser") => {
                    wn = Window::Kaiser;
                    if a.as_bytes().get(6) == Some(&b':') {
                        param = get_number(name, &a[7..], "beta")?;
                        if !(0.0..=32.0).contains(&param) {
                            return Err(range("beta"));
                        }
                    } else {
                        param = KAISER_BETA_DEFAULT;
                    }
                }
                _ => {
                    return Err(Error::Message(format!(
                        "{}: error: unknown window: {arg}",
                        info.name
                    )))
                }
            },
            _ => return Err(info.usage_error()),
        }
    }
    if g.ind != argc - 1 {
        return Err(info.usage_error());
    }
    let f0 = get_freq(name, argv[g.ind], "f0")?;
    let fs = f64::from(istream.fs);
    if !(f0 >= 0.0 && f0 < fs * 0.9995 / 2.0) {
        return Err(range("f0"));
    }
    // A cut-off of 0 Hz would need a filter of infinite length.
    let (h, reference) =
        design(fs, f0, tbw, wn, param, highpass, do_align).ok_or_else(|| range("f0"))?;
    let selector = default_selector(selector, istream.channels);
    let filter = Filter {
        data: &h,
        channels: 1,
        frames: h.len(),
        reference,
    };
    init_convolver(conv, info, istream, &selector, &filter).map(|e| vec![e])
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 1,
    };

    #[test]
    fn low_pass_has_unity_gain_at_dc_and_kills_the_nyquist() {
        let mut c = Chain::from_args(S, &["sinc", "-c", "4k"]).unwrap();
        let x = vec![1.0; 6000];
        let y = c.process(&x);
        assert!((y[3000] - 1.0).abs() < 1e-6, "{}", y[3000]);
        let nyq: Vec<f64> = (0..6000)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let mut c = Chain::from_args(S, &["sinc", "-c", "4k"]).unwrap();
        let y = c.process(&nyq);
        assert!(y[3000].abs() < 1e-5);
    }

    #[test]
    fn high_pass_is_the_complement() {
        let mut c = Chain::from_args(S, &["sinc", "-hc", "4k"]).unwrap();
        let y = c.process(&vec![1.0; 6000]);
        assert!(y[3000].abs() < 1e-6);
    }

    #[test]
    fn bad_arguments_are_errors() {
        assert!(Chain::from_args(S, &["sinc"]).is_err());
        assert!(Chain::from_args(S, &["sinc", "30k"]).is_err());
        assert!(Chain::from_args(S, &["sinc", "-t", "5", "1k"]).is_err());
        assert!(Chain::from_args(S, &["sinc", "-w", "triangle", "1k"]).is_err());
        assert!(Chain::from_args(S, &["sinc", "0"]).is_err());
    }
}
