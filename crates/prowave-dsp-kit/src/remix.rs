//! `remix` (`remix.c`): builds each output channel as the sum of the input channels named by a
//! selector (`0,1` sums channels 0 and 1, `.` makes silence). Selectors count only the channels
//! of the active selector. Channels outside the selector pass through; more selectors than
//! selected channels add channels at the end, fewer remove them.

use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::selector::{num_bits_set, parse_selector_masked};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "remix",
    usage: "channel_selector|. ...",
    init,
    number: 0,
}];

struct Remix {
    istream: Stream,
    ostream: Stream,
    /// For each output channel, the input channels summed into it.
    sources: Vec<Vec<usize>>,
    no_dither: bool,
}

impl Effect for Remix {
    fn name(&self) -> &'static str {
        "remix"
    }
    fn istream(&self) -> Stream {
        self.istream
    }
    fn ostream(&self) -> Stream {
        self.ostream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf {
        let (ich, och) = (self.istream.channels, self.ostream.channels);
        for (i, o) in ibuf[..*frames * ich]
            .chunks_exact(ich)
            .zip(obuf[..*frames * och].chunks_exact_mut(och))
        {
            for (out, src) in o.iter_mut().zip(&self.sources) {
                let mut sum = 0.0;
                for j in src {
                    sum += i[*j];
                }
                *out = sum;
            }
        }
        Buf::Output
    }
    fn buffer_frames(&self, frames: usize) -> usize {
        frames
    }
    fn no_dither(&self) -> bool {
        self.no_dither
    }
    fn channel_deps(&self, deps: &mut [Vec<bool>]) -> bool {
        for (row, src) in deps.iter_mut().zip(&self.sources) {
            row.iter_mut().for_each(|b| *b = false);
            for j in src {
                row[*j] = true;
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
    if argv.len() <= 1 {
        return Err(info.usage_error());
    }
    let in_ch = istream.channels;
    let sel = default_selector(selector, in_ch);
    let n_selectors = argv.len() - 1;
    let mask_bits = num_bits_set(&sel);
    let out_channels = (in_ch + n_selectors)
        .checked_sub(mask_bits)
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::Message(format!("{}: no output channels", argv[0])))?;
    let mut maps: Vec<Vec<bool>> = Vec::with_capacity(out_channels);
    let (mut i, mut ch) = (0usize, 0usize);
    let mut no_dither = true;
    for _ in 0..out_channels {
        let mut cs = vec![false; in_ch];
        if ch >= in_ch || sel[ch] {
            if i < n_selectors {
                if argv[i + 1] != "." {
                    cs = parse_selector_masked(argv[i + 1], &sel)?;
                }
                if num_bits_set(&cs) > 1 {
                    no_dither = false;
                }
                i += 1;
            } else {
                while ch < in_ch && sel[ch] {
                    ch += 1;
                }
                if ch < in_ch {
                    cs[ch] = true;
                }
            }
        } else {
            cs[ch] = true;
        }
        maps.push(cs);
        ch += 1;
    }
    let sources = maps
        .iter()
        .map(|m| {
            m.iter()
                .enumerate()
                .filter(|(_, s)| **s)
                .map(|(j, _)| j)
                .collect()
        })
        .collect();
    Ok(Box::new(Remix {
        istream: *istream,
        ostream: Stream {
            fs: istream.fs,
            channels: out_channels,
        },
        sources,
        no_dither,
    }))
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    fn s(channels: usize) -> Stream {
        Stream {
            fs: 48_000,
            channels,
        }
    }

    #[test]
    fn selectors_swap_sum_silence_and_add_channels() {
        let x = [1.0, 2.0, 4.0];
        let run = |ch: usize, words: &[&str]| Chain::from_args(s(ch), words).unwrap().run(&x[..ch]);
        assert_eq!(run(2, &["remix", "1", "0"]), vec![2.0, 1.0]);
        assert_eq!(run(2, &["remix", "0,1"]), vec![3.0]);
        assert_eq!(run(2, &["remix", "0", "."]), vec![1.0, 0.0]);
        assert_eq!(run(2, &["remix", "0", "1", "0,1"]), vec![1.0, 2.0, 3.0]);
        // Channels outside the active selector pass through at the end.
        assert_eq!(
            Chain::from_args(s(3), &[":0,1", "remix", "0,1"])
                .unwrap()
                .run(&x),
            vec![3.0, 4.0]
        );
    }

    #[test]
    fn it_reports_whether_dither_is_pointless() {
        // Pure routing leaves the signal bit-exact; summing does not.
        assert!(!Chain::from_args(s(2), &["remix", "1", "0"])
            .unwrap()
            .needs_dither());
        assert!(Chain::from_args(s(2), &["remix", "0,1"])
            .unwrap()
            .needs_dither());
        assert!(Chain::from_args(s(2), &["remix"]).is_err());
        assert!(Chain::from_args(s(2), &["remix", "5"]).is_err());
    }
}
