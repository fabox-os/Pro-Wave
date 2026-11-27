//! `gain`, `mult` and `add` (`gain.c`): multiply or add a constant per channel.

use crate::args::get_number;
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::Result;

const GAIN: i32 = 1;
const MULT: i32 = 2;
const ADD: i32 = 3;

pub(crate) static INFO: [EffectInfo; 3] = [
    EffectInfo {
        name: "gain",
        usage: "gain_dB",
        init,
        number: GAIN,
    },
    EffectInfo {
        name: "mult",
        usage: "multiplier",
        init,
        number: MULT,
    },
    EffectInfo {
        name: "add",
        usage: "value",
        init,
        number: ADD,
    },
];

struct GainEffect {
    name: &'static str,
    stream: Stream,
    /// The factor (or the value to add) of each channel; the neutral value where the channel
    /// is not selected.
    state: Vec<f64>,
    add: bool,
}

impl Effect for GainEffect {
    fn reorderable(&self) -> bool {
        !self.add
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
            for (s, v) in frame.iter_mut().zip(&self.state) {
                if self.add {
                    *s += *v;
                } else {
                    *s *= *v;
                }
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
    if argv.len() != 2 {
        return Err(info.usage_error());
    }
    let arg = argv[1];
    let v = match info.number {
        GAIN => 10f64.powf(get_number(argv[0], arg, "gain")? / 20.0),
        MULT => get_number(argv[0], arg, "multiplier")?,
        _ => get_number(argv[0], arg, "value")?,
    };
    let add = info.number == ADD;
    let neutral = if add { 0.0 } else { 1.0 };
    let sel = default_selector(selector, istream.channels);
    Ok(Box::new(GainEffect {
        name: info.name,
        stream: *istream,
        state: sel.iter().map(|s| if *s { v } else { neutral }).collect(),
        add,
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
    fn gain_is_in_decibels_mult_is_a_factor_and_add_is_an_offset() {
        let mut c = Chain::from_args(S, &["gain", "-6.020599913279624"]).unwrap();
        let y = c.run(&[1.0, -1.0]);
        assert!((y[0] - 0.5).abs() < 1e-12 && (y[1] + 0.5).abs() < 1e-12);
        let mut c = Chain::from_args(S, &["mult", "3"]).unwrap();
        assert_eq!(c.run(&[1.0, -2.0]), vec![3.0, -6.0]);
        let mut c = Chain::from_args(S, &["add", "0.25"]).unwrap();
        assert_eq!(c.run(&[1.0, -2.0]), vec![1.25, -1.75]);
    }

    #[test]
    fn a_channel_selector_leaves_the_other_channels_alone() {
        let mut c = Chain::from_args(S, &[":1", "mult", "2"]).unwrap();
        assert_eq!(c.run(&[1.0, 1.0, 2.0, 2.0]), vec![1.0, 2.0, 2.0, 4.0]);
        let mut c = Chain::from_args(S, &[":0", "add", "1", ":1", "mult", "-1"]).unwrap();
        assert_eq!(c.run(&[1.0, 1.0]), vec![2.0, -1.0]);
    }

    #[test]
    fn wrong_arguments_are_errors() {
        assert!(Chain::from_args(S, &["gain"]).is_err());
        assert!(Chain::from_args(S, &["gain", "1", "2"]).is_err());
        assert!(Chain::from_args(S, &["gain", "x"]).is_err());
        assert!(Chain::from_args(S, &["gain", "3dB"]).is_err());
    }
}
