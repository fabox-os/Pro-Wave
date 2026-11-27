//! `st2ms` and `ms2st` (`st2ms.c`): stereo to mid/side and back, on exactly two selected
//! channels. Mid and side are half the sum and difference; going back takes the full sum and
//! difference.

use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::selector::num_bits_set;

const ST2MS: i32 = 1;
const MS2ST: i32 = 2;

pub(crate) static INFO: [EffectInfo; 2] = [
    EffectInfo {
        name: "st2ms",
        usage: "",
        init,
        number: ST2MS,
    },
    EffectInfo {
        name: "ms2st",
        usage: "",
        init,
        number: MS2ST,
    },
];

struct St2Ms {
    name: &'static str,
    stream: Stream,
    c0: usize,
    c1: usize,
    to_ms: bool,
}

impl Effect for St2Ms {
    fn reorderable(&self) -> bool {
        true
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
    fn channel_deps(&self, deps: &mut [Vec<bool>]) -> bool {
        deps[self.c0][self.c1] = true;
        deps[self.c1][self.c0] = true;
        true
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for frame in ibuf[..*frames * ch].chunks_exact_mut(ch) {
            let (s0, s1) = (frame[self.c0], frame[self.c1]);
            if self.to_ms {
                frame[self.c0] = (s0 + s1) * 0.5;
                frame[self.c1] = (s0 - s1) * 0.5;
            } else {
                frame[self.c0] = s0 + s1;
                frame[self.c1] = s0 - s1;
            }
        }
        Buf::Input
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
    if argv.len() != 1 {
        return Err(info.usage_error());
    }
    let sel = default_selector(selector, istream.channels);
    if num_bits_set(&sel) != 2 {
        return Err(Error::Message(format!(
            "{}: input channels must be 2: value out of range",
            argv[0]
        )));
    }
    let mut chans = sel.iter().enumerate().filter(|(_, s)| **s).map(|(i, _)| i);
    let (c0, c1) = (chans.next().unwrap_or(0), chans.next().unwrap_or(1));
    Ok(Box::new(St2Ms {
        name: info.name,
        stream: *istream,
        c0,
        c1,
        to_ms: info.number == ST2MS,
    }))
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    #[test]
    fn mid_and_side_are_half_the_sum_and_difference_and_come_back() {
        let s = Stream {
            fs: 48_000,
            channels: 2,
        };
        let mut c = Chain::from_args(s, &["st2ms"]).unwrap();
        assert_eq!(
            c.run(&[1.0, 0.5, -0.25, 0.75]),
            vec![0.75, 0.25, 0.25, -0.5]
        );
        // Mid/side with the sum and difference at full size: ms2st undoes st2ms.
        let mut c = Chain::from_args(s, &["st2ms", "ms2st"]).unwrap();
        let x = [0.3, -0.7, 1.0, 0.25];
        let y = c.run(&x);
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-15));
    }

    #[test]
    fn it_needs_exactly_two_selected_channels_and_works_on_any_two() {
        let s3 = Stream {
            fs: 48_000,
            channels: 3,
        };
        assert!(Chain::from_args(s3, &["st2ms"]).is_err());
        let mut c = Chain::from_args(s3, &[":0,2", "st2ms"]).unwrap();
        assert_eq!(c.run(&[1.0, 9.0, 0.0]), vec![0.5, 9.0, 0.5]);
        assert!(Chain::from_args(s3, &["st2ms", "x"]).is_err());
    }
}
