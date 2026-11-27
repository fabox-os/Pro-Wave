//! `crossfeed` (`crossfeed.c`): a simple headphone crossfeed on two selected channels, very
//! like the Linkwitz, Meier, CMoy and bs2b ones: each ear hears its own side directly plus the
//! other side low-passed and its own side high-passed, with first-order filters at `f0`.

use crate::args::{check_freq, get_freq, get_number};
use crate::biquad::{Biquad, Width, HIGHPASS_1, LOWPASS_1};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::selector::num_bits_set;

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "crossfeed",
    usage: "f0[k] separation",
    init,
    number: 0,
}];

struct Crossfeed {
    stream: Stream,
    c0: usize,
    c1: usize,
    direct_gain: f64,
    cross_gain: f64,
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Effect for Crossfeed {
    fn name(&self) -> &'static str {
        "crossfeed"
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
            let (s0, s1) = (frame[self.c0], frame[self.c1]);
            frame[self.c0] = s0 * self.direct_gain
                + self.lp[0].run(s1) * self.cross_gain
                + self.hp[0].run(s0) * self.cross_gain;
            frame[self.c1] = s1 * self.direct_gain
                + self.lp[1].run(s0) * self.cross_gain
                + self.hp[1].run(s1) * self.cross_gain;
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.lp
            .iter_mut()
            .chain(self.hp.iter_mut())
            .for_each(Biquad::reset);
    }
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Box<dyn Effect>> {
    if argv.len() != 3 {
        return Err(info.usage_error());
    }
    let sel = default_selector(selector, istream.channels);
    if num_bits_set(&sel) != 2 {
        return Err(Error::Message(format!(
            "{}: input channels must be 2: value out of range",
            argv[0]
        )));
    }
    let freq = get_freq(argv[0], argv[1], "f0")?;
    check_freq(argv[0], freq, istream.fs, "f0")?;
    let sep_db = get_number(argv[0], argv[2], "separation")?;
    if sep_db < 0.0 {
        return Err(Error::Range {
            effect: argv[0].into(),
            param: "separation".into(),
        });
    }
    let mut chans = sel.iter().enumerate().filter(|(_, s)| **s).map(|(i, _)| i);
    let (c0, c1) = (chans.next().unwrap_or(0), chans.next().unwrap_or(1));
    let sep = 10f64.powf(sep_db / 20.0);
    let fs = f64::from(istream.fs);
    let lp = Biquad::from_type(LOWPASS_1, fs, freq, 0.0, 0.0, 0.0, Width::Q);
    let hp = Biquad::from_type(HIGHPASS_1, fs, freq, 0.0, 0.0, 0.0, Width::Q);
    Ok(Box::new(Crossfeed {
        stream: *istream,
        c0,
        c1,
        direct_gain: sep / (1.0 + sep),
        cross_gain: 1.0 / (1.0 + sep),
        lp: [lp; 2],
        hp: [hp; 2],
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
    fn a_left_only_signal_leaks_into_the_right_by_the_separation_at_low_frequencies() {
        let mut c = Chain::from_args(S, &["crossfeed", "700", "4.5"]).unwrap();
        // 100 Hz in the left channel only, 4.5 dB of separation.
        let x: Vec<f64> = (0..48_000)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * 100.0 * i as f64 / 48_000.0).sin();
                [v, 0.0]
            })
            .collect();
        let y = c.run(&x);
        let rms = |k: usize| {
            (y.iter()
                .skip(24_000 * 2 + k)
                .step_by(2)
                .map(|v| v * v)
                .sum::<f64>()
                / 24_000.0)
                .sqrt()
        };
        let db = 20.0 * (rms(0) / rms(1)).log10();
        assert!((db - 4.5).abs() < 0.5, "{db} dB between the ears");
    }

    #[test]
    fn it_needs_two_channels_and_a_valid_frequency_and_separation() {
        assert!(Chain::from_args(
            Stream {
                fs: 48_000,
                channels: 3
            },
            &["crossfeed", "700", "4.5"]
        )
        .is_err());
        assert!(Chain::from_args(S, &["crossfeed", "700"]).is_err());
        assert!(Chain::from_args(S, &["crossfeed", "30k", "4.5"]).is_err());
        assert!(Chain::from_args(S, &["crossfeed", "700", "-1"]).is_err());
        assert!(Chain::from_args(S, &["crossfeed", "700", "0"]).is_ok());
    }
}
