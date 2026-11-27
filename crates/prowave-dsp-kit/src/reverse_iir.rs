//! Reverse IIR filtering (`reverse_iir.c`), the `-r` option of the biquad filters: a filter
//! with the magnitude of the biquad and no phase shift (zero phase, so a delay of whole
//! blocks is all that is left).
//!
//! The method is M. Vicanek's, "A New Reverse IIR Filtering Algorithm" (2015, rev. 2022): the
//! filter is split into partial fractions, each pole `p` is run backwards in time by a
//! cascade of `N` stages that apply `p^(2^i)` to a delayed input, and the stages' sum is
//! what the reversed filter would give, with a delay of `2^N` samples. Sections whose poles
//! are close to each other, or whose partial-fraction residues would be too large for the
//! precision, are moved to a second filter that runs after the first (a cascade).

use std::any::Any;

use realfft::num_complex::Complex;

use crate::biquad::Biquad;
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};

const POLE_CMP_TOL: f64 = 1e-4;
const RES_LIM: f64 = 1e-8 / f64::EPSILON;
/// The most stages a filter may have: the memory is 2^N samples per pole.
const MAX_STAGES: i64 = 24;
const FIR_MAX: usize = 8;

type C64 = Complex<f64>;

/// What the roots of a polynomial are: nothing, one real, two real, or a complex pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pq {
    #[default]
    None,
    R1,
    R2,
    Cc,
}

impl Pq {
    fn n(self) -> usize {
        match self {
            Pq::Cc | Pq::R2 => 2,
            Pq::R1 => 1,
            Pq::None => 0,
        }
    }
    fn n_eval(self) -> usize {
        match self {
            Pq::Cc | Pq::R1 => 1,
            Pq::R2 => 2,
            Pq::None => 0,
        }
    }
}

/// The roots: two real numbers, or the complex number made of the same two numbers (the
/// original's `union`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Roots([f64; 2]);

impl Roots {
    fn c(&self) -> C64 {
        Complex::new(self.0[0], self.0[1])
    }
    fn set_c(&mut self, c: C64) {
        self.0 = [c.re, c.im];
    }
}

/// One biquad's poles (`p`) and zeros (`q`) with its gain, before the filter is made.
#[derive(Clone, Debug, Default)]
pub(crate) struct Section {
    pt: Pq,
    qt: Pq,
    p: Roots,
    q: Roots,
    res: Roots,
    g: f64,
    thresh: f64,
}

struct Stage<T> {
    p: T,
    m: Vec<T>,
}

/// One pole (or a complex pole, with its conjugate) as a cascade of stages.
struct Sec<T> {
    res: T,
    stages: Vec<Stage<T>>,
}

fn stages<T: Copy + Default + std::ops::Mul<Output = T>>(p0: T, n: usize) -> Vec<Stage<T>> {
    let mut p = p0;
    (0..n)
        .map(|i| {
            if i > 0 {
                p = p * p;
            }
            Stage {
                p,
                m: vec![T::default(); 1 << i],
            }
        })
        .collect()
}

fn run_real(sec: &mut Sec<f64>, s: f64, idx: usize) -> f64 {
    let mut x = s;
    for (i, st) in sec.stages.iter_mut().enumerate() {
        let mi = idx & ((1 << i) - 1);
        let y = st.p * x + st.m[mi];
        st.m[mi] = x;
        x = y;
    }
    sec.res * x
}

fn run_cc(sec: &mut Sec<C64>, s: f64, idx: usize) -> f64 {
    let mut x = Complex::new(s, 0.0);
    for (i, st) in sec.stages.iter_mut().enumerate() {
        let mi = idx & ((1 << i) - 1);
        let y = st.p * x + st.m[mi];
        st.m[mi] = x;
        x = y;
    }
    2.0 * (x * sec.res).re
}

#[derive(Default)]
struct Fir {
    c: [f64; FIR_MAX],
    m: [f64; FIR_MAX],
    buf: Vec<f64>,
    n: usize,
    idx: usize,
}

/// One filter of a cascade.
struct State {
    real: Vec<Sec<f64>>,
    cc: Vec<Sec<C64>>,
    idx: usize,
    mask: usize,
    n: usize,
    fir: Fir,
}

impl State {
    fn run_fir(&mut self, s: f64, idx: usize) -> f64 {
        let f = &mut self.fir;
        let x = f.buf[idx];
        f.buf[idx] = s;
        if f.n == 1 {
            return x * f.c[0];
        }
        let mi = f.idx;
        let mut n = mi;
        for m in 0..f.n {
            f.m[n] += x * f.c[m];
            n = if n + 1 < f.n { n + 1 } else { 0 };
        }
        let r = f.m[mi];
        f.m[mi] = 0.0;
        f.idx = if mi + 1 < f.n { mi + 1 } else { 0 };
        r
    }

    fn run(&mut self, buf: &mut [f64], first: usize, stride: usize, frames: usize) {
        for i in 0..frames {
            let at = first + i * stride;
            let s = buf[at];
            let mut out = 0.0;
            for sec in self.real.iter_mut() {
                out += run_real(sec, s, self.idx);
            }
            for sec in self.cc.iter_mut() {
                out += run_cc(sec, s, self.idx);
            }
            if self.fir.n > 0 {
                out += self.run_fir(s, self.idx);
            }
            buf[at] = out;
            self.idx = (self.idx + 1) & self.mask;
        }
    }

    fn reset(&mut self) {
        self.idx = 0;
        self.fir.idx = 0;
        self.fir.buf.fill(0.0);
        self.fir.m = [0.0; FIR_MAX];
        for s in self.real.iter_mut() {
            s.stages.iter_mut().for_each(|st| st.m.fill(0.0));
        }
        for s in self.cc.iter_mut() {
            s.stages
                .iter_mut()
                .for_each(|st| st.m.fill(Complex::new(0.0, 0.0)));
        }
    }
}

/// A channel's filter: the cascade and the delay it adds.
struct ChannelFilter {
    states: Vec<State>,
    latency: isize,
}

/// `riir_expand_pq`: the polynomial `z^2 + r[0] z + r[1]` that has the roots.
fn expand_pq(pq: &Roots, t: Pq) -> [f64; 2] {
    match t {
        Pq::Cc => {
            let c = pq.c();
            [-2.0 * c.re, (c * c.conj()).re]
        }
        Pq::R2 => [-pq.0[0] - pq.0[1], pq.0[0] * pq.0[1]],
        Pq::R1 => [-pq.0[0], 0.0],
        Pq::None => [0.0, 0.0],
    }
}

/// `riir_eval_pq`: the `i`-th factor `(z - root) / z` of the polynomial at `z`.
fn eval_pq(pq: &Roots, t: Pq, i: usize, z: C64) -> C64 {
    match t {
        Pq::Cc => (z - if i != 0 { pq.c().conj() } else { pq.c() }) / z,
        Pq::R2 => (z - pq.0[usize::from(i != 0)]) / z,
        Pq::R1 => {
            if i != 0 {
                Complex::new(1.0, 0.0)
            } else {
                (z - pq.0[0]) / z
            }
        }
        Pq::None => Complex::new(1.0, 0.0),
    }
}

/// `riir_pq_close`: whether a root of the second polynomial is close to one of the first.
fn pq_close(pq0: &Roots, t0: Pq, pq1: &Roots, t1: Pq) -> bool {
    for i in 0..t0.n() {
        match t1 {
            Pq::Cc => {
                if eval_pq(pq0, t0, i, pq1.c()).norm() < POLE_CMP_TOL {
                    return true;
                }
            }
            Pq::R2 | Pq::R1 => {
                if t1 == Pq::R2
                    && eval_pq(pq0, t0, i, Complex::new(pq1.0[1], 0.0)).norm() < POLE_CMP_TOL
                {
                    return true;
                }
                if eval_pq(pq0, t0, i, Complex::new(pq1.0[0], 0.0)).norm() < POLE_CMP_TOL {
                    return true;
                }
            }
            Pq::None => {}
        }
    }
    false
}

fn pq_max_abs(pq: &Roots, t: Pq) -> f64 {
    match t {
        Pq::Cc => pq.c().norm(),
        Pq::R2 => pq.0[0].abs().max(pq.0[1].abs()),
        Pq::R1 => pq.0[0].abs(),
        Pq::None => 0.0,
    }
}

/// `RIIR_POLE_MIN_STAGES`: how many stages bring the response of the pole below `thresh` dB.
/// A pole that is not inside the unit circle (or is 0) needs none of its own.
fn pole_min_stages(thresh: f64, abs_p: f64) -> i64 {
    let v = (-((thresh) + 6.02) / (20.0 * abs_p.log10())).log2().ceil();
    if v.is_finite() {
        v.clamp(-1.0, 1e6) as i64
    } else {
        i64::MIN
    }
}

fn prepare_channel(name: &str, k: usize, mut v: Vec<Section>) -> Result<ChannelFilter> {
    let mut cascade_v: Vec<Section> = Vec::new();
    // split sections with repeated real poles
    for sec in v.iter_mut() {
        if sec.pt == Pq::R2 && (sec.p.0[1] - sec.p.0[0]).abs() < POLE_CMP_TOL {
            let mut split = Section {
                thresh: sec.thresh,
                pt: Pq::R1,
                ..Section::default()
            };
            sec.pt = Pq::R1;
            split.p.0[0] = sec.p.0[1];
            if sec.qt == Pq::R2 {
                split.qt = Pq::R1;
                sec.qt = Pq::R1;
                split.q.0[0] = sec.q.0[1];
                sec.g = sec.g.sqrt();
                split.g = sec.g;
            } else {
                split.g = 1.0;
            }
            cascade_v.push(split);
        }
    }
    let mut states: Vec<State> = Vec::new();
    'recalc: loop {
        // move any other repeated poles
        let mut i = 0;
        while i < v.len() {
            if v[i].pt != Pq::None {
                let mut j = i + 1;
                while j < v.len() {
                    if pq_close(&v[i].p, v[i].pt, &v[j].p, v[j].pt) {
                        cascade_v.push(v.remove(j));
                    } else {
                        j += 1;
                    }
                }
            }
            i += 1;
        }
        // the minimum number of stages and the total number of poles and zeros
        let mut n_stages = 3i64;
        let (mut nq, mut np) = (0usize, 0usize);
        let mut g = 1.0;
        for sec in &v {
            let (mut p0, mut p1) = (0i64, 0i64);
            nq += sec.qt.n();
            np += sec.pt.n();
            g *= sec.g;
            match sec.pt {
                Pq::Cc => p0 = pole_min_stages(sec.thresh, sec.p.c().norm()),
                Pq::R2 => {
                    p1 = pole_min_stages(sec.thresh, sec.p.0[1].abs());
                    p0 = pole_min_stages(sec.thresh, sec.p.0[0].abs());
                }
                Pq::R1 => p0 = pole_min_stages(sec.thresh, sec.p.0[0].abs()),
                Pq::None => {}
            }
            n_stages = n_stages.max(p0.max(p1));
        }
        if n_stages > MAX_STAGES {
            return Err(Error::Message(format!(
                "{name}: error: channel {k}: the poles are too close to the unit circle ({n_stages} stages)"
            )));
        }
        let n_stages = n_stages as usize;
        if nq + 1 > np + FIR_MAX {
            return Err(Error::Message(format!(
                "{name}: error: channel {k}: too many zeros: {nq}-{np}+1 > {FIR_MAX}"
            )));
        }
        // partial fraction residues
        let mut do_cascade = false;
        for i in 0..v.len() {
            let is_cc = v[i].pt == Pq::Cc;
            for l in 0..v[i].pt.n_eval() {
                let p = if is_cc {
                    v[i].p.c()
                } else {
                    Complex::new(v[i].p.0[l], 0.0)
                };
                // z^(nq-np+1) (1 - p/z) H(z) at z = p
                let mut num = if nq < np {
                    Complex::new(1.0, 0.0)
                } else if nq == np {
                    p
                } else {
                    p.powf((nq - np + 1) as f64)
                };
                let mut den = Complex::new(1.0, 0.0);
                for (j, e) in v.iter().enumerate() {
                    num *= eval_pq(&e.q, e.qt, 0, p);
                    num *= eval_pq(&e.q, e.qt, 1, p);
                    if j != i {
                        den *= eval_pq(&e.p, e.pt, l, p);
                    }
                    den *= eval_pq(&e.p, e.pt, usize::from(l == 0), p);
                }
                let mut res = num / den;
                if res.norm().is_nan() {
                    res = Complex::new(f64::INFINITY, 0.0);
                }
                if res.norm() > RES_LIM {
                    do_cascade = true;
                }
                if is_cc {
                    v[i].res.set_c(res * g);
                } else {
                    v[i].res.0[l] = g * res.re;
                }
            }
        }
        if do_cascade {
            if v.len() < 2 {
                return Err(Error::Message(format!(
                    "{name}: error: something has gone terribly wrong; aborting..."
                )));
            }
            // move the section with the largest residue
            let mut rm_idx = 0;
            let mut max_res = pq_max_abs(&v[0].res, v[0].pt);
            for (i, s) in v.iter().enumerate().skip(1) {
                let r = pq_max_abs(&s.res, s.pt);
                if r > max_res {
                    rm_idx = i;
                    max_res = r;
                }
            }
            cascade_v.push(v.remove(rm_idx));
            continue 'recalc;
        }
        // sort the sections to keep the quantisation error small
        let mut sort_sum = 0.0;
        for i in 0..v.len() {
            let mut min_idx = i;
            let mut min_sum = f64::INFINITY;
            for (j, sec) in v.iter().enumerate().skip(i) {
                let mut sec_sum = sort_sum;
                match sec.pt {
                    Pq::Cc => sec_sum += 2.0 * sec.res.c().re,
                    Pq::R2 => sec_sum += sec.res.0[1] + sec.res.0[0],
                    Pq::R1 => sec_sum += sec.res.0[0],
                    Pq::None => {}
                }
                if sec_sum.abs() < min_sum.abs() {
                    min_sum = sec_sum;
                    min_idx = j;
                }
            }
            v.swap(i, min_idx);
            sort_sum = min_sum;
        }
        // the FIR part
        let mut fir = Fir::default();
        if nq >= np {
            fir.n = nq - np + 1;
            fir.c[nq - np] = g;
            if nq > np {
                for sec in &v {
                    let b = expand_pq(&sec.q, sec.qt);
                    let a = expand_pq(&sec.p, sec.pt);
                    let mut bq = Biquad::new(1.0, b[0], b[1], 1.0, a[0], a[1]);
                    for n in (0..=nq - np).rev() {
                        fir.c[n] = bq.run(fir.c[n]);
                    }
                }
            }
            fir.buf = vec![0.0; 1 << n_stages];
        }
        // the poles and residues
        let (mut real, mut cc) = (Vec::new(), Vec::new());
        for sec in &v {
            if sec.pt == Pq::Cc {
                cc.push(Sec {
                    res: sec.res.c(),
                    stages: stages(sec.p.c(), n_stages),
                });
            } else {
                for j in 0..sec.pt.n_eval() {
                    real.push(Sec {
                        res: sec.res.0[j],
                        stages: stages(sec.p.0[j], n_stages),
                    });
                }
            }
        }
        states.push(State {
            real,
            cc,
            idx: 0,
            mask: (1 << n_stages) - 1,
            n: n_stages,
            fir,
        });
        if cascade_v.is_empty() {
            break;
        }
        v = std::mem::take(&mut cascade_v);
    }
    let latency = states
        .iter()
        .map(|s| (1isize << s.n) + s.fir.n as isize - 1)
        .sum();
    Ok(ChannelFilter { states, latency })
}

struct ReverseIir {
    name: &'static str,
    stream: Stream,
    selector: Vec<bool>,
    /// The sections of each channel, until `prepare` makes the filters.
    init: Vec<Vec<Section>>,
    run: Vec<Option<ChannelFilter>>,
}

impl Effect for ReverseIir {
    fn name(&self) -> &'static str {
        self.name
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn prepare(&mut self) -> Result<()> {
        let init = std::mem::take(&mut self.init);
        let mut run = Vec::with_capacity(init.len());
        for (k, v) in init.into_iter().enumerate() {
            run.push(if v.is_empty() {
                None
            } else {
                Some(prepare_channel(self.name, k, v)?)
            });
        }
        self.run = run;
        Ok(())
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for (k, f) in self.run.iter_mut().enumerate() {
            if let Some(f) = f {
                for st in f.states.iter_mut() {
                    st.run(ibuf, k, ch, *frames);
                }
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        for f in self.run.iter_mut().flatten() {
            f.states.iter_mut().for_each(State::reset);
        }
    }
    fn channel_offsets(&self, _latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        for (r, f) in req_delay.iter_mut().zip(&self.run) {
            if let Some(f) = f {
                *r -= f.latency;
            }
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (d, f) in samples.iter_mut().zip(&self.run) {
            if let Some(f) = f {
                *d += f.latency;
            }
        }
        true
    }
    fn reorderable(&self) -> bool {
        true
    }
    fn opt_selector(&self) -> Option<&[bool]> {
        Some(&self.selector)
    }
    fn merge_id(&self) -> Option<&'static str> {
        Some("reverse_iir")
    }
    fn merge_take(&mut self) -> Option<Box<dyn Any>> {
        let sel = self.selector.clone();
        let init = std::mem::take(&mut self.init);
        Some(Box::new((sel, init)))
    }
    fn merge_put(&mut self, data: Box<dyn Any>) -> bool {
        let Ok(data) = data.downcast::<(Vec<bool>, Vec<Vec<Section>>)>() else {
            return false;
        };
        let (sel, init) = *data;
        for (k, secs) in init.into_iter().enumerate() {
            if sel.get(k).copied().unwrap_or(false) {
                self.init[k].extend(secs);
                self.selector[k] = true;
            }
        }
        true
    }
}

/// `calc_qroots`: the roots of `z^2 + b z + c`, as a complex pair or two real numbers.
fn calc_qroots(b: f64, c: f64, roots: &mut Roots) -> bool {
    let d = b * b - 4.0 * c;
    if d < 0.0 {
        let r = Complex::new(-b / 2.0, (-d).sqrt() / 2.0);
        if r.im.abs() >= 1e-6 {
            roots.set_c(r);
            return true;
        }
    }
    roots.0[0] = (d.max(0.0).sqrt() - b) / 2.0;
    roots.0[1] = (-d.max(0.0).sqrt() - b) / 2.0;
    false
}

/// `reverse_iir_effect_init_from_biquad`: the reverse filter with the response of `b`, which
/// attenuates the pole's tail by `thresh` dB before it is cut.
pub(crate) fn from_biquad(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    b: &Biquad,
    thresh: f64,
) -> Box<dyn Effect> {
    let mut sec = Section {
        thresh,
        g: b.c0,
        ..Section::default()
    };
    // poles
    if b.c4 == 0.0 {
        if b.c3 != 0.0 {
            sec.pt = Pq::R1;
            sec.p.0[0] = -b.c3;
        }
    } else {
        sec.pt = if calc_qroots(b.c3, b.c4, &mut sec.p) {
            Pq::Cc
        } else {
            Pq::R2
        };
    }
    // zeros
    if b.c2 == 0.0 {
        if b.c1 != 0.0 {
            sec.qt = Pq::R1;
            sec.q.0[0] = -b.c1 / b.c0;
        }
    } else {
        sec.qt = if calc_qroots(b.c1 / b.c0, b.c2 / b.c0, &mut sec.q) {
            Pq::Cc
        } else {
            Pq::R2
        };
    }
    let selector = default_selector(selector, istream.channels);
    let init = selector
        .iter()
        .map(|on| if *on { vec![sec.clone()] } else { Vec::new() })
        .collect();
    Box::new(ReverseIir {
        name: info.name,
        stream: *istream,
        selector,
        init,
        run: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};
    use std::f64::consts::PI;

    const S: Stream = Stream {
        fs: 48_000,
        channels: 1,
    };

    fn tone(f: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f64 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn the_impulse_response_is_symmetric_and_the_gain_is_the_biquads() {
        // Zero phase: the impulse response is symmetric about its peak.
        let mut imp = vec![0.0; 4000];
        imp[0] = 1.0;
        let mut c = Chain::from_args(S, &["lowpass", "-r", "1k", "0.7q"]).unwrap();
        let h = c.process(&imp);
        let symmetric = |c: usize| (1..30).all(|k| (h[c + k] - h[c - k]).abs() < 1e-9);
        assert!((60..200).any(symmetric), "not symmetric about any point");
        // The magnitude is the biquad's.
        let x = tone(1000.0, 48_000);
        let mut f = Chain::from_args(S, &["lowpass", "1k", "0.7q"]).unwrap();
        let mut r = Chain::from_args(S, &["lowpass", "-r", "1k", "0.7q"]).unwrap();
        let (yf, yr) = (f.process(&x), r.process(&x));
        let rms = |v: &[f64]| (v.iter().map(|a| a * a).sum::<f64>() / v.len() as f64).sqrt();
        let gf = rms(&yf[24_000..28_800]) / rms(&x[24_000..28_800]);
        let gr = rms(&yr[24_000..28_800]) / rms(&x[24_000..28_800]);
        assert!((gr - gf).abs() < 5e-3, "{gr} vs {gf}");
    }

    #[test]
    fn neighbouring_reverse_filters_are_merged_and_wrong_options_fail() {
        let x = tone(300.0, 4000);
        let mut a = Chain::from_args(
            S,
            &[
                "highpass", "-r", "100", "0.7q", "lowpass", "-r", "5k", "0.7q",
            ],
        )
        .unwrap();
        let mut b = Chain::from_args(S, &["highpass", "-r", "100", "0.7q"]).unwrap();
        // Merged, the pair has one delay; run separately each would add its own.
        assert!(a.process(&x).len() < x.len() + 2 * b.process(&x).len() - 2 * x.len());
        assert!(Chain::from_args(S, &["lowpass", "-r300", "1k", "0.7q"]).is_err());
    }
}
