//! `delay` (`delay.c`): a delay line in seconds, milliseconds or samples, with three extras.
//!
//! - A whole number of samples is not done by the effect at all: it tells the chain how much
//!   each channel is delayed (`channel_offsets`) and the chain lines the channels up and lets
//!   the output grow by the delay ([`crate::align`]).
//! - `-f[order]` makes the delay fractional with an all-pass filter of the given order (first
//!   and second order in closed form, higher orders as Thiran filters; the default is second).
//!   The filter's own latency is moved into the whole part of the delay so the total is exact.
//! - `-m depth` / `-M depth` modulate the delay at random (a chorus or tape-flutter effect):
//!   smooth band-limited noise (`-b` bandwidth) moves the delay between 0 and twice the depth,
//!   read back with polyphase interpolation of quality `-q` 0 (cubic Hermite), 1 or 2. `-M` gives
//!   all channels the same modulation, `-m` independent modulation.

use std::sync::Mutex;

use crate::allpass::{Ap1, Ap2, Thiran};
use crate::args::{check_freq, get_freq, parse_len_frac, strtod, strtol, Getopt};
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::mod_tables::{MOD_FLT_Q1, MOD_FLT_Q2};
use crate::rng::{pm_rand1_r, pm_rand2_r, PM_RAND_MAX};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "delay",
    usage: "[-f[order]] [-m|M depth[s|m|S|%]] [-b bw[k]] [-q quality] delay[s|m|S]",
    init,
    number: 0,
}];

const DELAY_MIN_FRAC: f64 = 0.1;
const DELAY_FD_AP_N_DEFAULT: usize = 2;

enum FracAp {
    None,
    First(Ap1),
    Second(Ap2),
    Nth(Thiran),
}

struct DelayChannel {
    ap: FracAp,
    samples_frac: f64,
    samples_int: isize,
    fd_ap_n: usize,
}

struct Delay {
    stream: Stream,
    cs: Vec<DelayChannel>,
}

impl Effect for Delay {
    fn name(&self) -> &'static str {
        "delay"
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for (k, c) in self.cs.iter_mut().enumerate() {
            if matches!(c.ap, FracAp::None) {
                continue;
            }
            let mut idx = k;
            for _ in 0..*frames {
                ibuf[idx] = match &mut c.ap {
                    FracAp::First(a) => a.run(ibuf[idx]),
                    FracAp::Second(a) => a.run(ibuf[idx]),
                    FracAp::Nth(a) => a.run(ibuf[idx]),
                    FracAp::None => ibuf[idx],
                };
                idx += ch;
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        for c in self.cs.iter_mut() {
            match &mut c.ap {
                FracAp::First(a) => a.reset(),
                FracAp::Second(a) => a.reset(),
                FracAp::Nth(a) => a.reset(),
                FracAp::None => {}
            }
        }
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (s, c) in samples.iter_mut().zip(&self.cs) {
            *s += c.fd_ap_n as isize;
        }
        true
    }
    fn channel_offsets(&self, _latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        for (r, c) in req_delay.iter_mut().zip(&self.cs) {
            *r += c.samples_int;
        }
        true
    }
    fn prepare(&mut self) -> Result<()> {
        for c in self.cs.iter_mut() {
            if c.fd_ap_n < 1 {
                c.fd_ap_n = DELAY_FD_AP_N_DEFAULT;
            }
            // Ignore an extremely small fractional delay.
            if (c.samples_frac - c.samples_frac.round_ties_even()).abs() >= f64::EPSILON {
                let adj =
                    (c.fd_ap_n as isize - 1) - (c.samples_frac - DELAY_MIN_FRAC).floor() as isize;
                c.samples_int -= adj;
                c.samples_frac += adj as f64;
            } else {
                c.samples_int += c.samples_frac.round_ties_even() as isize;
                c.samples_frac = 0.0;
                c.fd_ap_n = 0;
            }
        }
        for c in self.cs.iter_mut() {
            if c.fd_ap_n > 0 {
                let delta = c.samples_frac.abs();
                c.ap = match c.fd_ap_n {
                    1 => FracAp::First(Ap1::new((1.0 - delta) / (1.0 + delta))),
                    2 => FracAp::Second(Ap2::new(
                        (4.0 - 2.0 * delta) / (1.0 + delta),
                        ((delta - 2.0) * (delta - 1.0)) / ((delta + 1.0) * (delta + 2.0)),
                    )),
                    n => FracAp::Nth(Thiran::new(n, delta).ok_or_else(|| {
                        Error::Message("delay: error: thiran_ap_new() failed".into())
                    })?),
                };
            } else {
                c.ap = FracAp::None;
            }
        }
        Ok(())
    }
}

/// `delay_effect_init_common`: nothing at all for a zero delay.
fn delay_effect(
    stream: &Stream,
    selector: &[bool],
    samples_int: isize,
    samples_frac: f64,
    fd_ap_n: usize,
) -> Option<Box<dyn Effect>> {
    if samples_int == 0 && samples_frac == 0.0 {
        return None;
    }
    let cs = selector
        .iter()
        .map(|on| DelayChannel {
            ap: FracAp::None,
            samples_frac: if *on { samples_frac } else { 0.0 },
            samples_int: if *on { samples_int } else { 0 },
            fd_ap_n: if *on { fd_ap_n } else { 0 },
        })
        .collect();
    Some(Box::new(Delay {
        stream: *stream,
        cs,
    }))
}

// ---- the modulated delay ----

/// Smooth random numbers between 0 and 1 (`mod_noise_state`): TPDF noise at a low rate joined
/// by a cubic B-spline.
struct ModNoise {
    c: [f64; 4],
    y: [f64; 4],
    t: f64,
    step: f64,
}

const MOD_NOISE_N: usize = 6;
const MOD_NOISE_SCALE: f64 = 0.77 / MOD_NOISE_N as f64 / PM_RAND_MAX as f64;

impl ModNoise {
    fn new(fs: f64, fc: f64) -> Self {
        Self {
            c: [0.5, 0.0, 0.0, 0.0],
            y: [0.0; 4],
            t: 0.0,
            step: 2.0 * fc / fs,
        }
    }

    fn next(&mut self, seeds: &mut [u32; 2]) {
        self.y.copy_within(1..4, 0);
        self.y[3] = 0.0;
        for _ in 0..MOD_NOISE_N {
            let n1 = pm_rand1_r(&mut seeds[0]) as i32;
            let n2 = pm_rand2_r(&mut seeds[1]) as i32;
            self.y[3] += f64::from(n1 - n2) * MOD_NOISE_SCALE;
        }
        let y = &self.y;
        let a = y[0] + y[2];
        self.c[0] = (1.0 / 6.0) * a + (2.0 / 3.0) * y[1] + 0.5;
        self.c[1] = 0.5 * (y[2] - y[0]);
        self.c[2] = 0.5 * a - y[1];
        self.c[3] = 0.5 * (y[1] - y[2]) + (1.0 / 6.0) * (y[3] - y[0]);
    }

    #[inline]
    fn value(&mut self, seeds: &mut [u32; 2]) -> f64 {
        let c = &self.c;
        let t = self.t;
        let z = ((c[3] * t + c[2]) * t + c[1]) * t + c[0];
        self.t += self.step;
        if self.t >= 1.0 {
            self.t -= 1.0;
            self.next(seeds);
        }
        z.clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    Hermite,
    Q1,
    Q2,
}

impl Quality {
    fn taps(self) -> usize {
        match self {
            Quality::Hermite => 3,
            Quality::Q1 => 16,
            Quality::Q2 => 32,
        }
    }
}

/// A polyphase filter's output at `buf[base]`, taps from the newest sample down, summed in
/// groups of four like the original.
#[inline]
fn fir_phase(buf: &[f64], base: usize, row: &[f64]) -> f64 {
    let n = row.len();
    let mut total = 0.0;
    for g in (0..n).step_by(4) {
        let mut x = 0.0;
        for j in g..g + 4 {
            // Tap j multiplies the sample j behind `base` with the coefficient (n - 1 - j).
            let term = buf[base - j] * row[n - 1 - j];
            x = if j == g { term } else { x + term };
        }
        total = if g == 0 { x } else { total + x };
    }
    total
}

/// `mod_interp`: the signal `t` of a sample behind `buf[y]` (newest sample), for `t` in `[0, 1)`.
fn mod_interp(buf: &[f64], y: usize, t: f64, q: Quality) -> f64 {
    let mut t = t;
    let c: [f64; 4] = match q {
        Quality::Hermite => {
            let at = |k: usize| buf[y - k];
            [
                at(1),
                0.5 * (at(2) - at(0)),
                at(0) - 2.5 * at(1) + 2.0 * at(2) - 0.5 * at(3),
                0.5 * (at(3) - at(0)) + 1.5 * (at(1) - at(2)),
            ]
        }
        _ => {
            let phases = if q == Quality::Q1 {
                MOD_FLT_Q1.len()
            } else {
                MOD_FLT_Q2.len()
            };
            let t_os = t * phases as f64;
            let mut ph = t_os as usize;
            let mut yb = y;
            let mut z = [0.0; 4];
            for zi in z.iter_mut() {
                *zi = if q == Quality::Q1 {
                    fir_phase(buf, yb, &MOD_FLT_Q1[ph])
                } else {
                    fir_phase(buf, yb, &MOD_FLT_Q2[ph])
                };
                if ph < phases - 1 {
                    ph += 1;
                } else {
                    ph = 0;
                    yb -= 1;
                }
            }
            t = t_os - (t_os as i64) as f64;
            let a = z[0] + z[2];
            [
                (1.0 / 6.0) * a + (2.0 / 3.0) * z[1],
                0.5 * (z[2] - z[0]),
                0.5 * a - z[1],
                0.5 * (z[1] - z[2]) + (1.0 / 6.0) * (z[3] - z[0]),
            ]
        }
    };
    ((c[3] * t + c[2]) * t + c[1]) * t + c[0]
}

struct ModChannel {
    buf: Vec<f64>,
    ns: ModNoise,
    len: usize,
    n: usize,
    p: usize,
    depth: f64,
    seeds: [u32; 2],
    q: Quality,
}

struct ModDelay {
    stream: Stream,
    /// `None` for channels that are not selected.
    cs: Vec<Option<ModChannel>>,
    seeds: [u32; 2],
    mono: bool,
}

impl Effect for ModDelay {
    fn name(&self) -> &'static str {
        "delay"
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for (k, c) in self.cs.iter_mut().enumerate() {
            let Some(c) = c else { continue };
            let mut idx = k;
            for _ in 0..*frames {
                let seeds = if self.mono {
                    &mut c.seeds
                } else {
                    &mut self.seeds
                };
                let m = c.ns.value(seeds) * c.depth;
                let d_int = m as usize;
                let d_frac = m - d_int as f64;
                let s = ibuf[idx];
                let op = c.p + c.n;
                let mut yp = op as isize - d_int as isize;
                if yp < c.n as isize {
                    yp += c.len as isize;
                }
                c.buf[op] = s;
                ibuf[idx] = mod_interp(&c.buf, yp as usize, d_frac, c.q);
                if op >= c.len {
                    c.buf[op - c.len] = s;
                }
                idx += ch;
                c.p = if c.p + 1 >= c.len { 0 } else { c.p + 1 };
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        for c in self.cs.iter_mut().flatten() {
            c.buf.fill(0.0);
            c.p = 0;
        }
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (s, c) in samples.iter_mut().zip(&self.cs) {
            if let Some(c) = c {
                *s += c.len as isize;
            }
        }
        true
    }
    fn channel_offsets(&self, latency: &mut [isize], _req_delay: &mut [isize]) -> bool {
        for (l, c) in latency.iter_mut().zip(&self.cs) {
            if let Some(c) = c {
                *l += (c.len / 2) as isize;
            }
        }
        true
    }
}

/// The seed of the modulation noise, shared by every modulated delay of the program (the
/// original's `static uint32_t seed = 1`).
static MOD_SEED: Mutex<u32> = Mutex::new(1);

/// Starts the modulation noise sequence over.
pub(crate) fn reset_seed() {
    *MOD_SEED.lock().unwrap_or_else(|e| e.into_inner()) = 1;
}

fn mod_effect(
    stream: &Stream,
    selector: &[bool],
    samples: f64,
    fc: f64,
    mono: bool,
    quality: i64,
) -> Result<Box<dyn Effect>> {
    let q = match quality {
        0 => Quality::Hermite,
        1 => Quality::Q1,
        2 => Quality::Q2,
        _ => {
            return Err(Error::Message(format!(
                "delay: error: invalid quality: {quality}"
            )))
        }
    };
    let seeds = {
        let mut seed = MOD_SEED.lock().unwrap_or_else(|e| e.into_inner());
        let s0 = pm_rand2_r(&mut seed);
        let s1 = pm_rand1_r(&mut seed);
        [s0, s1]
    };
    let cs = selector
        .iter()
        .map(|on| {
            on.then(|| {
                let n = q.taps();
                let len = samples.ceil().round_ties_even() as usize * 2 + n;
                ModChannel {
                    buf: vec![0.0; len + n],
                    ns: ModNoise::new(f64::from(stream.fs), fc),
                    len,
                    n,
                    p: 0,
                    depth: samples * 2.0,
                    seeds: if mono { seeds } else { [0, 0] },
                    q,
                }
            })
        })
        .collect();
    Ok(Box::new(ModDelay {
        stream: *stream,
        cs,
        seeds,
        mono,
    }))
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let argc = argv.len();
    let fs = istream.fs;
    let parse = |param: &str, text: &str| Error::Parse {
        effect: name.into(),
        param: param.into(),
        text: text.into(),
    };
    let mut g = Getopt::default();
    let (mut do_frac, mut fd_ap_n) = (false, 0usize);
    let mut mod_arg: Option<String> = None;
    let (mut mod_mono, mut mod_qual, mut mod_bw) = (false, 1i64, 1.0f64);
    while let Some(opt) = g.next(argc - 1, argv, "f::m:M:b:q:") {
        let arg = g.arg.clone();
        match opt {
            'f' => {
                do_frac = true;
                if let Some(a) = arg {
                    let (v, end) = strtol(&a);
                    if end == 0 || end != a.len() {
                        return Err(parse("order", &a));
                    }
                    if v <= 0 || v > 50 {
                        return Err(Error::Range {
                            effect: name.into(),
                            param: "order".into(),
                        });
                    }
                    fd_ap_n = v as usize;
                }
            }
            'm' | 'M' => {
                mod_arg = arg;
                mod_mono = opt == 'M';
            }
            'b' => {
                let a = arg.unwrap_or_default();
                mod_bw = get_freq(name, &a, "modulation bandwidth")?;
                if !(mod_bw > 0.0 && mod_bw < f64::from(fs) / 2.0) {
                    return Err(Error::Range {
                        effect: name.into(),
                        param: "modulation bandwidth".into(),
                    });
                }
                check_freq(name, mod_bw, fs, "modulation bandwidth")?;
            }
            'q' => {
                let a = arg.unwrap_or_default();
                let (v, end) = strtol(&a);
                if end == 0 || end != a.len() {
                    return Err(parse("quality", &a));
                }
                mod_qual = v;
            }
            _ => return Err(info.usage_error()),
        }
    }
    if g.ind != argc - 1 {
        return Err(info.usage_error());
    }
    let samples =
        parse_len_frac(argv[g.ind], f64::from(fs)).ok_or_else(|| parse("delay", argv[g.ind]))?;
    let mut mod_samples = 0.0;
    if let Some(a) = &mod_arg {
        let (v, end) = strtod(a);
        mod_samples = if end != 0 && a[end..] == *"%" {
            samples * (v / 100.0)
        } else {
            parse_len_frac(a, f64::from(fs)).ok_or_else(|| parse("modulation depth", a))?
        };
    }
    let sel = default_selector(selector, istream.channels);
    let mut out: Vec<Box<dyn Effect>> = Vec::new();
    let delay = if do_frac {
        delay_effect(istream, &sel, 0, samples, fd_ap_n)
    } else {
        delay_effect(istream, &sel, samples.round_ties_even() as isize, 0.0, 0)
    };
    out.extend(delay);
    if mod_samples > 0.0 {
        out.push(mod_effect(
            istream,
            &sel,
            mod_samples,
            mod_bw,
            mod_mono,
            mod_qual,
        )?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 1,
    };

    fn tone(n: usize, hz: f64) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn units_are_seconds_milliseconds_and_samples() {
        let x = vec![1.0; 10];
        for (arg, extra) in [("100S", 100), ("1m", 48), ("0.001", 48), ("2.4S", 2)] {
            let y = Chain::from_args(S, &["delay", arg]).unwrap().process(&x);
            assert_eq!(y.len(), 10 + extra, "{arg}");
        }
        // Nothing to do for zero.
        assert_eq!(Chain::from_args(S, &["delay", "0"]).unwrap().process(&x), x);
        assert!(Chain::from_args(S, &["delay"]).is_err());
        assert!(Chain::from_args(S, &["delay", "x"]).is_err());
        assert!(Chain::from_args(S, &["delay", "-z", "1"]).is_err());
    }

    #[test]
    fn a_fractional_delay_moves_a_tone_by_that_much_from_the_filter_order_up() {
        // The all-pass is stable for delays above its order minus one; below that the original
        // keeps the filter's own latency, so a delay of 0.5 samples comes out as 1.5.
        let hz = 200.0;
        let x = tone(4000, hz);
        let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
        for (args, delay) in [
            (vec!["delay", "-f", "5.5S"], 5.5),
            (vec!["delay", "-f1", "4.5S"], 4.5),
            (vec!["delay", "-f3", "6.25S"], 6.25),
            (vec!["delay", "-f", "0.5S"], 1.5),
        ] {
            let y = Chain::from_args(S, &args).unwrap().process(&x);
            let worst = (500..3500)
                .map(|i| (y[i] - (w * (i as f64 - delay)).sin()).abs())
                .fold(0.0, f64::max);
            assert!(worst < 0.01, "{args:?}: {worst}");
        }
        // Without -f the delay is rounded to a whole number of samples.
        let y = Chain::from_args(S, &["delay", "2.6S"]).unwrap().process(&x);
        assert_eq!(&y[3..3 + 100], &x[..100]);
    }

    #[test]
    fn modulation_keeps_the_signal_close_and_moves_with_the_depth() {
        let x = tone(24_000, 500.0);
        let run = |args: &[&str]| Chain::from_args(S, args).unwrap().process(&x);
        let none = run(&["delay", "0"]);
        assert_eq!(none, x);
        let y = run(&["delay", "-m", "1m", "0"]);
        assert!(y.len() > x.len());
        let peak = y.iter().fold(0f64, |a, v| a.max(v.abs()));
        assert!(peak > 0.9 && peak < 1.1, "{peak}");
        // A deeper modulation bends the pitch more: the output differs more from the delayed input.
        let mid = |y: &[f64]| y.len() / 2;
        assert!(y[mid(&y)..mid(&y) + 200].iter().any(|v| v.abs() > 0.1));
        for q in ["0", "1", "2"] {
            let y = run(&["delay", "-m", "5%", "-q", q, "2m"]);
            assert!(
                y.iter().all(|v| v.is_finite() && v.abs() < 1.5),
                "quality {q}"
            );
        }
        assert!(Chain::from_args(S, &["delay", "-m", "1m", "-q", "3", "0"]).is_err());
        assert!(Chain::from_args(S, &["delay", "-m", "1x", "0"]).is_err());
        assert!(Chain::from_args(S, &["delay", "-b", "30k", "-m", "1m", "0"]).is_err());
    }
}
