//! `levels` (`levels.c`): a level meter. The signal passes through unchanged; for every
//! selected channel the effect keeps an average power (time constant `-t`, 0.3 s by default)
//! and the peak of each block, which [`crate::Chain::reports`] returns as
//! [`LevelReading`]s. The original draws them as status lines; [`LevelReading::status_line`]
//! gives the same text.
//!
//! Logic Pro equivalent: the Level Meter plug-in (peak and RMS per channel).

use crate::args::{strtod, Getopt};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Report, Stream};
use crate::error::{Error, Result};
use crate::smoothing::Ewma;

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "levels",
    usage: "[-t time_const]",
    init,
    number: 0,
}];

/// The level of one channel at the end of the last block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelReading {
    /// The channel number in the stream.
    pub channel: usize,
    /// The average power in dBFS.
    pub avg_db: f64,
    /// The highest peak (of the peak follower) during the block, in dBFS.
    pub peak_db: f64,
}

impl LevelReading {
    /// `draw_levels_bar`: 60 characters, one per dB from −59 to 0; `#` up to the average,
    /// `|` at the peak, `.` every 5 dB.
    pub fn bar(&self) -> String {
        let mut s = [b' '; 60];
        let avg = if self.avg_db.is_infinite() {
            -200.0
        } else {
            self.avg_db
        };
        let peak = if self.peak_db.is_infinite() {
            -200.0
        } else {
            self.peak_db
        };
        for i in (4..59).step_by(5) {
            s[i] = b'.';
        }
        let idx_avg = 59 + avg.round_ties_even() as i64;
        if idx_avg >= 0 {
            let end = idx_avg.min(59) as usize + 1;
            s[..end].fill(b'#');
        }
        let idx_peak = 59 + peak.round_ties_even() as i64;
        if idx_peak >= 0 {
            s[idx_peak.min(59) as usize] = b'|';
        }
        String::from_utf8_lossy(&s).into_owned()
    }

    /// The original's status line; `wide` when the stream has more than 10 channels.
    pub fn status_line(&self, name: &str, wide: bool) -> String {
        let w = if wide { 2 } else { 1 };
        format!(
            "{name}: channel {:>w$}: [{}]  avg:{}; peak:{}",
            self.channel,
            self.bar(),
            signed(self.avg_db),
            signed(self.peak_db),
        )
    }
}

/// `%+6.1f`
fn signed(v: f64) -> String {
    let t = if v.is_nan() {
        if v.is_sign_negative() {
            "-nan".to_string()
        } else {
            "+nan".to_string()
        }
    } else if v.is_infinite() {
        if v < 0.0 { "-inf" } else { "+inf" }.to_string()
    } else {
        format!("{v:+.1}")
    };
    format!("{t:>6}")
}

struct ChState {
    block_peak: f64,
    avg: Ewma,
    peak: Ewma,
}

struct Levels {
    stream: Stream,
    selector: Vec<bool>,
    cs: Vec<Option<ChState>>,
    readings: Vec<LevelReading>,
}

impl Effect for Levels {
    fn name(&self) -> &'static str {
        "levels"
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn no_dither(&self) -> bool {
        true
    }
    fn align_barrier(&self) -> bool {
        true
    }
    fn channel_selector(&self) -> Option<&[bool]> {
        Some(&self.selector)
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let stride = self.stream.channels;
        for frame in ibuf[..*frames * stride].chunks_exact(stride) {
            for (cs, &x) in self.cs.iter_mut().zip(frame) {
                if let Some(cs) = cs {
                    let s2 = x * x;
                    cs.avg.run(s2);
                    let peak = cs.peak.run_set_min(s2);
                    if cs.block_peak < peak {
                        cs.block_peak = peak;
                    }
                }
            }
        }
        for (cs, r) in self.cs.iter_mut().flatten().zip(self.readings.iter_mut()) {
            r.avg_db = 10.0 * cs.avg.last().log10();
            r.peak_db = 10.0 * cs.block_peak.log10();
            cs.block_peak = 0.0;
        }
        Buf::Input
    }
    fn report(&mut self) -> Option<Report> {
        Some(Report::Levels(self.readings.clone()))
    }
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let mut tc = 0.3;
    let mut g = Getopt::default();
    while let Some(opt) = g.next(argv.len(), argv, "t:") {
        match opt {
            't' => {
                let text = g.arg.clone().unwrap_or_default();
                let (v, end) = strtod(&text);
                if end == 0 || end != text.len() {
                    return Err(Error::Parse {
                        effect: name.into(),
                        param: "time constant".into(),
                        text,
                    });
                }
                if !(0.01..=10.0).contains(&v) {
                    return Err(Error::Range {
                        effect: name.into(),
                        param: "time constant".into(),
                    });
                }
                tc = v;
            }
            _ => return Err(info.usage_error()),
        }
    }
    if g.ind != argv.len() {
        return Err(info.usage_error());
    }
    let selector = default_selector(selector, istream.channels);
    let fs = f64::from(istream.fs);
    let cs: Vec<Option<ChState>> = selector
        .iter()
        .map(|&on| {
            on.then(|| ChState {
                block_peak: 0.0,
                avg: Ewma::new(fs, tc),
                peak: Ewma::new(fs, tc),
            })
        })
        .collect();
    let readings = selector
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(channel, _)| LevelReading {
            channel,
            avg_db: f64::NEG_INFINITY,
            peak_db: f64::NEG_INFINITY,
        })
        .collect();
    Ok(vec![Box::new(Levels {
        stream: *istream,
        selector,
        cs,
        readings,
    })])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    #[test]
    fn the_signal_passes_through_and_a_full_scale_sine_reads_minus_three_db() {
        let x: Vec<f64> = (0..2 * 3 * 48_000)
            .map(|i| {
                if i % 2 == 0 {
                    (2.0 * std::f64::consts::PI * 1000.0 * (i / 2) as f64 / 48_000.0).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let mut c = Chain::from_args(S, &["levels"]).unwrap();
        let y = c.process(&x);
        assert_eq!(y, x);
        let r = match c.reports().pop() {
            Some(Report::Levels(r)) => r,
            _ => panic!("no levels"),
        };
        assert!((r[0].avg_db + 3.01).abs() < 0.05, "{}", r[0].avg_db);
        assert!(
            r[0].peak_db > -0.1 && r[0].peak_db <= 0.0,
            "{}",
            r[0].peak_db
        );
        assert_eq!(r[1].avg_db, f64::NEG_INFINITY);
    }

    #[test]
    fn the_bar_and_the_status_line_follow_the_original() {
        let r = LevelReading {
            channel: 1,
            avg_db: -20.0,
            peak_db: -6.2,
        };
        let bar = r.bar();
        assert_eq!(bar.len(), 60);
        assert_eq!(&bar[..40], &"#".repeat(40));
        assert_eq!(bar.as_bytes()[40], b' ');
        assert_eq!(bar.as_bytes()[44], b'.');
        assert_eq!(bar.as_bytes()[53], b'|');
        assert!(r
            .status_line("levels", false)
            .ends_with("]  avg: -20.0; peak:  -6.2"));
        let silent = LevelReading {
            channel: 0,
            avg_db: f64::NEG_INFINITY,
            peak_db: f64::NEG_INFINITY,
        };
        assert_eq!(
            silent.bar(),
            "    .    .    .    .    .    .    .    .    .    .    .     "
        );
    }

    #[test]
    fn options_are_checked() {
        assert!(Chain::from_args(S, &["levels", "-t", "0.001"]).is_err());
        assert!(Chain::from_args(S, &["levels", "-t", "1"]).is_ok());
        assert!(Chain::from_args(S, &["levels", "extra"]).is_err());
    }
}
