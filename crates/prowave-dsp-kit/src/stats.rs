//! `stats` (`stats.c`): statistics of each selected channel over the whole signal: DC
//! offset, minimum, maximum, peak and RMS level, crest factor, how often the peak was reached
//! and where. The signal passes through unchanged. With `-i` the peaks are those of the
//! signal between the samples (4x oversampling and a quadratic fit), the true peak.
//!
//! The original prints the table when the chain is destroyed; here
//! [`crate::Chain::reports`] returns it as a [`StatsReport`], with the same text in
//! [`StatsReport::text`]. `ref_level` adds the levels relative to it (dBr).
//!
//! Logic Pro equivalent: the Loudness Meter's and Level Meter's peak and RMS readings, and
//! the Audio File Editor's "Search Peak".

use crate::args::{strtod, strtol, Getopt};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Report, Stream};
use crate::error::{Error, Result};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "stats",
    usage: "[-w width|auto] [-i] [ref_level]",
    init,
    number: 0,
}];

const STATS_DEFAULT_WIDTH: i64 = 80;
/// `STATS_INTERP_DELAY`: 7.75 + 1 + 9 samples (FIR, quadratic fit, lookahead).
const STATS_INTERP_DELAY: i64 = 18;

/// The statistics of one channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelStats {
    /// The channel number in the stream.
    pub channel: usize,
    pub dc_offset: f64,
    pub min: f64,
    pub max: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub crest_factor_db: f64,
    /// How many times the peak value was reached.
    pub peak_count: i64,
    /// The frame where the peak was first reached.
    pub peak_frame: i64,
}

/// What `stats` found over the whole signal.
#[derive(Clone, Debug, PartialEq)]
pub struct StatsReport {
    pub channels: Vec<ChannelStats>,
    /// The number of frames measured.
    pub frames: i64,
    pub fs: u32,
    /// The reference level in dB, when one was given.
    pub ref_level: Option<f64>,
    /// The table as the original prints it.
    pub text: String,
}

#[derive(Clone, Copy)]
struct Interp {
    m: [f64; 64],
    y: [f64; 6],
    tmin: f64,
    tmax: f64,
    z: [f64; 9],
    p: usize,
    zp: usize,
    n: i64,
}

#[derive(Clone, Copy, Default)]
struct ChState {
    sum: f64,
    sum_sq: f64,
    min: f64,
    max: f64,
    peak: f64,
    peak_count: i64,
    peak_frame: i64,
    interp: Interp,
    ch: usize,
}

/// 4x half filter with every 4th coefficient left out.
#[allow(clippy::excessive_precision)]
const R: [f64; 24] = [
    -9.353493881474939e-04,
    -2.811275711123766e-03,
    -3.165361696477658e-03,
    5.929994218827107e-03,
    1.065865725083938e-02,
    9.308373173634579e-03,
    -1.340062089976642e-02,
    -2.227979776029874e-02,
    -1.833945608477310e-02,
    2.430932418366197e-02,
    3.925899279385184e-02,
    3.157919724264597e-02,
    -4.056172445833198e-02,
    -6.489751870004079e-02,
    -5.192701793078084e-02,
    6.684049697012354e-02,
    1.078342211598459e-01,
    8.751763525896815e-02,
    -1.187292496637064e-01,
    -2.001458972657618e-01,
    -1.729186314209981e-01,
    2.957854651930789e-01,
    6.325370350028462e-01,
    8.988707620097378e-01,
];

/// The groups of four slots `stats_interp_insert` adds to after the first: a coefficient
/// index, 24 for the sample itself, or -1 for a slot left alone.
const GROUPS: [[i8; 4]; 15] = [
    [3, 4, 5, -1],
    [6, 7, 8, -1],
    [9, 10, 11, -1],
    [12, 13, 14, -1],
    [15, 16, 17, -1],
    [18, 19, 20, -1],
    [21, 22, 23, 24],
    [23, 22, 21, -1],
    [20, 19, 18, -1],
    [17, 16, 15, -1],
    [14, 13, 12, -1],
    [11, 10, 9, -1],
    [8, 7, 6, -1],
    [5, 4, 3, -1],
    [2, 1, 0, -1],
];

impl Default for Interp {
    fn default() -> Self {
        Self {
            m: [0.0; 64],
            y: [0.0; 6],
            tmin: 0.0,
            tmax: 0.0,
            z: [0.0; 9],
            p: 0,
            zp: 0,
            n: 0,
        }
    }
}

impl Interp {
    /// `stats_interp_insert`: pushes `x` into the polyphase interpolator; `y[2..6]` get the
    /// next four interpolated samples.
    fn insert(&mut self, x: f64) {
        let r: [f64; 25] = std::array::from_fn(|i| if i < 24 { R[i] * x } else { x });
        let mut p = self.p;
        self.y[0] = self.y[4];
        self.y[1] = self.y[5];
        self.y[2] = self.m[p] + r[0];
        self.y[3] = self.m[p + 1] + r[1];
        self.y[4] = self.m[p + 2] + r[2];
        self.y[5] = self.m[p + 3];
        self.m[p..p + 4].fill(0.0);
        p = (p + 4) & 0x3f;
        self.p = p;
        for g in &GROUPS {
            for &c in g {
                if c >= 0 {
                    self.m[p] += r[c as usize];
                }
                p += 1;
            }
            p &= 0x3f;
        }
    }
}

struct Stats {
    stream: Stream,
    selector: Vec<bool>,
    cs: Vec<ChState>,
    ref_level: f64,
    samples: i64,
    width: i64,
    interp: bool,
    report: Option<StatsReport>,
}

impl Stats {
    /// `stats_interp_peak`
    fn interp_peak(samples: i64, cs: &mut ChState) {
        let y = cs.interp.y;
        let mut r = 0;
        for i in 1..y.len() - 1 {
            let d0 = y[i] - y[i - 1];
            let d1 = y[i] - y[i + 1];
            if (d0 > 0.0 && d1 < 0.0) || (d0 < 0.0 && d1 > 0.0) || (d0 == 0.0 && d1 == 0.0) {
                continue; // no extremum
            }
            let dy = y[i - 1] - y[i + 1];
            // quadratic peak estimation
            let p_4 = dy / (8.0 * (y[i - 1] - 2.0 * y[i] + y[i + 1]));
            let yq = y[i] - dy * p_4;
            let mut pk = false;
            if yq <= cs.min {
                cs.min = yq;
                cs.interp.tmin = 0.5 * yq;
                pk = true;
            } else if yq >= cs.max {
                cs.max = yq;
                cs.interp.tmax = 0.5 * yq;
                pk = true;
            }
            if pk {
                let ayq = yq.abs();
                if ayq > 0.0 && ayq == cs.peak {
                    r = 1;
                } else if ayq > cs.peak {
                    cs.peak = ayq;
                    r = 2;
                }
            }
        }
        if r == 2 {
            cs.peak_frame = samples - (STATS_INTERP_DELAY - 1);
            cs.peak_count = 1;
        } else if r == 1 {
            cs.peak_count += 1;
        }
    }

    fn step_interp(samples: i64, cs: &mut ChState, s: f64) {
        let cis = &mut cs.interp;
        if cis.n > 0 {
            let z = cis.z[cis.zp];
            cis.insert(z);
            Self::interp_peak(samples, cs);
            cs.interp.n -= 1;
        }
        let cis = &mut cs.interp;
        cis.z[cis.zp] = s;
        cis.zp += 1;
        if cis.zp >= cis.z.len() {
            cis.zp = 0;
        }
    }

    /// The end of the signal: flushes the interpolator and makes the table
    /// (`stats_effect_destroy`).
    fn finish(&mut self) -> StatsReport {
        if self.interp {
            for _ in 0..STATS_INTERP_DELAY {
                for cs in &mut self.cs {
                    Self::step_interp(self.samples, cs, 0.0);
                }
                self.samples += 1;
            }
            self.samples -= STATS_INTERP_DELAY;
        }
        let n = self.samples as f64;
        let channels: Vec<ChannelStats> = self
            .cs
            .iter()
            .map(|c| {
                let rms = (c.sum_sq / n).sqrt();
                ChannelStats {
                    channel: c.ch,
                    dc_offset: c.sum / n,
                    min: c.min,
                    max: c.max,
                    peak_dbfs: 20.0 * c.peak.log10(),
                    rms_dbfs: 20.0 * rms.log10(),
                    crest_factor_db: 20.0 * (c.peak / rms).log10(),
                    peak_count: c.peak_count,
                    peak_frame: c.peak_frame,
                }
            })
            .collect();
        let width = if self.width < 0 {
            STATS_DEFAULT_WIDTH // no terminal to measure
        } else {
            self.width
        };
        let cols = if width > 0 {
            ((width - 18) / 13).max(1) as usize
        } else {
            channels.len().max(1)
        };
        let ref_level = (self.ref_level != f64::NEG_INFINITY).then_some(self.ref_level);
        let mut text = String::new();
        for chunk in channels.chunks(cols) {
            print_channels(&mut text, chunk, self.samples, self.stream.fs, ref_level);
        }
        StatsReport {
            channels,
            frames: self.samples,
            fs: self.stream.fs,
            ref_level,
            text,
        }
    }
}

/// `printf("%W.Pf")` as glibc does it, `nan` and `inf` included.
fn cf(v: f64, width: usize, prec: usize) -> String {
    let t = if v.is_nan() {
        if v.is_sign_negative() { "-nan" } else { "nan" }.to_string()
    } else if v.is_infinite() {
        if v < 0.0 { "-inf" } else { "inf" }.to_string()
    } else {
        format!("{v:.prec$}")
    };
    format!(" {t:>width$}")
}

/// `stats_print_channels`
fn print_channels(
    out: &mut String,
    cs: &[ChannelStats],
    samples: i64,
    fs: u32,
    ref_level: Option<f64>,
) {
    use std::fmt::Write;
    let mut row = |label: &str, cell: &dyn Fn(&ChannelStats) -> String| {
        let _ = write!(out, "\n{label:<18}");
        for c in cs {
            out.push_str(&cell(c));
        }
    };
    row("Channel", &|c| format!(" {:>12}", c.channel));
    row("DC offset", &|c| cf(c.dc_offset, 12, 8));
    row("Minimum", &|c| cf(c.min, 12, 8));
    row("Maximum", &|c| cf(c.max, 12, 8));
    row("Peak level (dBFS)", &|c| cf(c.peak_dbfs, 12, 4));
    if let Some(r) = ref_level {
        row("Peak level (dBr)", &|c| cf(r + c.peak_dbfs, 12, 4));
    }
    row("RMS level (dBFS)", &|c| cf(c.rms_dbfs, 12, 4));
    if let Some(r) = ref_level {
        row("RMS level (dBr)", &|c| cf(r + c.rms_dbfs, 12, 4));
    }
    row("Crest factor (dB)", &|c| cf(c.crest_factor_db, 12, 4));
    row("Peak count", &|c| format!(" {:>12}", c.peak_count));
    row("Peak sample", &|c| format!(" {:>12}", c.peak_frame));
    row("Samples", &|_| format!(" {samples:>12}"));
    row("Length (s)", &|_| cf(samples as f64 / f64::from(fs), 12, 2));
    out.push('\n');
}

impl Effect for Stats {
    fn name(&self) -> &'static str {
        "stats"
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
            for cs in &mut self.cs {
                let s = frame[cs.ch];
                cs.sum += s;
                cs.sum_sq += s * s;
                if self.interp {
                    if s < cs.interp.tmin || s > cs.interp.tmax {
                        cs.interp.n = STATS_INTERP_DELAY;
                    }
                    Self::step_interp(self.samples, cs, s);
                } else {
                    let mut pk = false;
                    if s <= cs.min {
                        cs.min = s;
                        pk = true;
                    } else if s >= cs.max {
                        cs.max = s;
                        pk = true;
                    }
                    if pk {
                        let abs_s = s.abs();
                        if abs_s > 0.0 && abs_s == cs.peak {
                            cs.peak_count += 1;
                        } else if abs_s > cs.peak {
                            cs.peak = abs_s;
                            cs.peak_frame = self.samples;
                            cs.peak_count = 1;
                        }
                    }
                }
            }
            self.samples += 1;
        }
        Buf::Input
    }
    /// The statistics so far; the first call ends the measurement (as the end of the
    /// signal does in the original), later calls give the same report.
    fn report(&mut self) -> Option<Report> {
        if self.report.is_none() {
            self.report = Some(self.finish());
        }
        self.report.clone().map(Report::Stats)
    }
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let argc = argv.len();
    let mut width = STATS_DEFAULT_WIDTH;
    let mut interp = false;
    let mut ref_level = f64::NEG_INFINITY;
    let mut g = Getopt::default();
    while let Some(opt) = g.next(argc, argv, "w:i") {
        match opt {
            'w' => {
                let text = g.arg.clone().unwrap_or_default();
                if text == "auto" {
                    width = -1;
                } else {
                    let (v, end) = strtol(&text);
                    if end == 0 || end != text.len() {
                        return Err(Error::Parse {
                            effect: name.into(),
                            param: "width".into(),
                            text,
                        });
                    }
                    if v < 0 {
                        return Err(Error::Message(format!(
                            "{name}: error: width must be positive or zero"
                        )));
                    }
                    width = v;
                }
            }
            'i' => interp = true,
            _ => return Err(info.usage_error()),
        }
    }
    if g.ind + 1 == argc {
        // The original reads argv[1] here, which is the reference level only when no option
        // comes before it; the port reads the word that is left.
        let text = argv[g.ind];
        let (v, end) = strtod(text);
        if end == 0 || end != text.len() {
            return Err(Error::Parse {
                effect: name.into(),
                param: "ref_level".into(),
                text: text.into(),
            });
        }
        ref_level = v;
    } else if g.ind != argc {
        return Err(info.usage_error());
    }
    let selector = default_selector(selector, istream.channels);
    let cs = selector
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(ch, _)| ChState {
            ch,
            ..ChState::default()
        })
        .collect();
    Ok(vec![Box::new(Stats {
        stream: *istream,
        selector,
        cs,
        ref_level,
        samples: 0,
        width,
        interp,
        report: None,
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

    fn stats_of(args: &[&str], x: &[f64]) -> StatsReport {
        let mut c = Chain::from_args(S, args).unwrap();
        let y = c.process(x);
        assert_eq!(y, x);
        match c.reports().pop() {
            Some(Report::Stats(r)) => r,
            _ => panic!("no stats"),
        }
    }

    #[test]
    fn a_square_wave_has_a_crest_factor_of_zero_and_the_peak_count_adds_up() {
        let x: Vec<f64> = (0..2 * 1000)
            .map(|i| if (i / 2) % 10 < 5 { 0.5 } else { -0.5 })
            .collect();
        let r = stats_of(&["stats"], &x);
        let c = r.channels[0];
        assert!((c.peak_dbfs - 20.0 * 0.5f64.log10()).abs() < 1e-12);
        assert!(c.crest_factor_db.abs() < 1e-9);
        assert!(c.dc_offset.abs() < 1e-12);
        assert_eq!(c.peak_frame, 0);
        // the first +0.5 sets the peak; every later new maximum or minimum of 0.5 counts
        assert!(c.peak_count >= 2);
        assert_eq!(r.frames, 1000);
        assert!(r.text.contains("Crest factor (dB)"));
    }

    #[test]
    fn the_true_peak_of_a_sine_between_samples_is_found() {
        // fs/4 sine sampled at 45°: the samples reach 0.707 but the signal reaches 1.0
        let x: Vec<f64> = (0..2 * 4000)
            .map(|i| {
                let n = (i / 2) as f64;
                (std::f64::consts::FRAC_PI_2 * n + std::f64::consts::FRAC_PI_4).sin()
            })
            .collect();
        let plain = stats_of(&["stats"], &x).channels[0];
        let tp = stats_of(&["stats", "-i"], &x).channels[0];
        assert!(
            (plain.peak_dbfs + 3.0103).abs() < 1e-3,
            "{}",
            plain.peak_dbfs
        );
        assert!(tp.peak_dbfs.abs() < 0.2, "{}", tp.peak_dbfs);
    }

    #[test]
    fn options_are_checked() {
        assert!(Chain::from_args(S, &["stats", "-w", "-3"]).is_err());
        // a negative level after options needs `--`, as with the original
        assert!(Chain::from_args(S, &["stats", "-w", "auto", "-i", "--", "-6"]).is_ok());
        assert!(Chain::from_args(S, &["stats", "-i", "-6"]).is_err());
        assert!(Chain::from_args(S, &["stats", "x"]).is_err());
        assert!(Chain::from_args(S, &["stats", "1", "2"]).is_err());
    }
}
