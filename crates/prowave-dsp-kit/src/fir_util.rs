//! What `fir`, `fir_p`, `sinc` and `hilbert` have in common (`fir_util.c` of the original):
//! the options of the filter commands, reading the filter, the alignment offset and the choice
//! of convolver.

use crate::args::{get_freq, strtod, Getopt};
use crate::effect::{default_selector, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir::{fir_effect, Filter};
use crate::fir_p::fir_p_effect;

/// The options common to the filter commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirConfig {
    /// `-a`: line the filter's reference sample up with the input.
    pub do_align: bool,
    /// The offset given to `-a` in samples; 0 means "the largest coefficient".
    pub offset: isize,
}

/// Which convolver does the work (`enum conv_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convolver {
    /// `fir`: direct form up to 16 taps, FFT overlap-add with one block of latency otherwise.
    Fir,
    /// `fir_p`: partitioned convolution without latency.
    FirP,
    /// `zita-convolver` in the original. It is not part of this port, and the original falls
    /// back to `fir_p` when it is not built in; so does this.
    Zita,
}

/// `fir_parse_opts`: reads the options of the filter commands from `argv[1..argc-1]`, with
/// the option string of `fir` (`a::t:e:BLNr:c:`). The options that describe a filter file
/// (`-t -e -B -L -N -c`) are checked and ignored, as no files are read. Returns the config
/// and the index of the first word that is not an option.
pub(crate) fn parse_opts(
    info: &EffectInfo,
    istream: &Stream,
    argv: &[&str],
) -> Result<(FirConfig, usize)> {
    let mut config = FirConfig::default();
    let mut g = Getopt::default();
    let name = info.name;
    while let Some(opt) = g.next(argv.len() - 1, argv, "a::t:e:BLNr:c:") {
        let arg = g.arg.clone();
        let parse = |param: &str, text: &str| Error::Parse {
            effect: name.into(),
            param: param.into(),
            text: text.into(),
        };
        match opt {
            'a' => {
                config.do_align = true;
                if let Some(a) = arg {
                    config.offset = crate::args::parse_len(&a, istream.fs)
                        .ok_or_else(|| parse("offset", &a))?
                        as isize;
                }
            }
            't' | 'e' | 'B' | 'L' | 'N' => {}
            'r' => {
                let a = arg.unwrap_or_default();
                if a != "any" {
                    let f = get_freq(name, &a, "sample rate")?.round() as i64;
                    if f <= 0 {
                        return Err(Error::Message(format!(
                            "{name}: error: sample rate must be > 0"
                        )));
                    }
                    if f != i64::from(istream.fs) {
                        return Err(Error::Message(format!(
                            "{name}: error: sample rate mismatch: stream_fs={} requested_fs={f}",
                            istream.fs
                        )));
                    }
                }
            }
            'c' => {
                let a = arg.unwrap_or_default();
                let (v, end) = crate::args::strtol(&a);
                if end == 0 || end != a.len() {
                    return Err(parse("number of channels", &a));
                }
                if v <= 0 {
                    return Err(Error::Message(format!(
                        "{name}: error: number of channels must be > 0"
                    )));
                }
            }
            _ => return Err(info.usage_error()),
        }
    }
    Ok((config, g.ind))
}

/// `fir_read_filter`: the filter of a filter command, as `coefs:a,b,c/d,e,f` (channels
/// separated by `/`, coefficients by `,`; missing ones are 0). Returns the interleaved
/// coefficients, the number of channels and the number of frames.
///
/// Filter files are not read here: pass the coefficients with `coefs:`, or build the effect
/// with [`fir_effect`] or [`fir_p_effect`] from samples loaded by Pro Wave's own file readers.
pub fn read_filter(name: &str, path: &str) -> Result<(Vec<f64>, usize, usize)> {
    let Some(list) = path.strip_prefix("coefs:") else {
        let file = path.strip_prefix("file:").unwrap_or(path);
        return Err(Error::Unsupported(format!(
            "{name}: reading the filter file {file}: use coefs:..., or give the samples with fir_effect()"
        )));
    };
    let mut channels = 1usize;
    let mut frames = 1usize;
    let mut i = 1usize;
    for c in list.chars() {
        if c == ',' {
            i += 1;
        } else if c == '/' {
            channels += 1;
            frames = frames.max(i);
            i = 1;
        }
    }
    frames = frames.max(i);
    let mut data = vec![0.0; frames * channels];
    let mut ch = list;
    let mut c = 0;
    while !ch.is_empty() {
        let (this, next) = ch.split_once('/').unwrap_or((ch, ""));
        let mut coef = this;
        let mut i = 0;
        while !coef.is_empty() {
            let (text, rest) = coef.split_once(',').unwrap_or((coef, ""));
            let text = text.trim();
            if !text.is_empty() {
                let (v, end) = strtod(text);
                if end == 0 || end != text.len() {
                    return Err(Error::Parse {
                        effect: name.into(),
                        param: "coefficient".into(),
                        text: text.into(),
                    });
                }
                data[channels * i + c] = v;
            }
            coef = rest;
            i += 1;
        }
        ch = next;
        c += 1;
    }
    Ok((data, channels, frames))
}

/// `fir_get_offset`: the sample of the filter that `-a` lines up with the input: the offset
/// given (negative: counted from the end), or the position of the largest coefficient.
/// (As in the original, the position is an index into the interleaved coefficients.)
pub fn get_offset(
    config: &FirConfig,
    filter_data: &[f64],
    filter_channels: usize,
    filter_frames: usize,
) -> isize {
    if !config.do_align {
        return 0;
    }
    if config.offset > 0 {
        config.offset
    } else if config.offset < 0 {
        filter_frames as isize + config.offset
    } else {
        let mut peak = 0.0;
        let mut offset = 0;
        for (i, &v) in filter_data
            .iter()
            .take(filter_frames * filter_channels)
            .enumerate()
        {
            if v > peak {
                peak = v;
                offset = i as isize;
            }
        }
        offset
    }
}

/// `init_convolver`: makes the effect that convolves the stream with the filter.
pub fn init_convolver(
    conv: Convolver,
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    filter: &Filter,
) -> Result<Box<dyn Effect>> {
    let selector = default_selector(selector, istream.channels);
    match conv {
        Convolver::Fir => fir_effect(info, istream, &selector, filter, false),
        Convolver::FirP | Convolver::Zita => fir_p_effect(info, istream, &selector, filter, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coefficient_lists_fill_missing_values_with_zero() {
        let (d, ch, n) = read_filter("fir", "coefs:1,2,3/4,5").unwrap();
        assert_eq!((ch, n), (2, 3));
        assert_eq!(d, vec![1.0, 4.0, 2.0, 5.0, 3.0, 0.0]);
        let (d, ch, n) = read_filter("fir", "coefs: 0.5 , ,1").unwrap();
        assert_eq!((ch, n), (1, 3));
        assert_eq!(d, vec![0.5, 0.0, 1.0]);
        let (d, ch, n) = read_filter("fir", "coefs:1/").unwrap();
        assert_eq!((ch, n), (2, 1));
        assert_eq!(d, vec![1.0, 0.0]);
        assert!(read_filter("fir", "coefs:1,x").is_err());
        assert!(matches!(
            read_filter("fir", "file:a.wav"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn the_alignment_offset_is_the_peak_unless_given() {
        let c = FirConfig {
            do_align: true,
            offset: 0,
        };
        assert_eq!(get_offset(&c, &[0.1, 0.9, 0.3], 1, 3), 1);
        let c = FirConfig {
            do_align: true,
            offset: -1,
        };
        assert_eq!(get_offset(&c, &[0.1, 0.9, 0.3], 1, 3), 2);
        assert_eq!(get_offset(&FirConfig::default(), &[1.0], 1, 1), 0);
    }
}
