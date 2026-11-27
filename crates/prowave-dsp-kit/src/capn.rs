//! Crossovers made of two all-pass branches (`capn.c` and `capn.h`): the sum of the branches
//! is the low-pass output and their difference the high-pass one, so the two always add back
//! to an all-pass response. The poles come from a Butterworth, Chebyshev or elliptic design
//! of odd order (3, 5 or 7), mapped to the z-plane with the bilinear transform.
//!
//! Logic Pro equivalent: the crossover of a multiband plug-in (Multipressor's band splits).

use realfft::num_complex::Complex64 as Cx;
use std::f64::consts::PI;

use crate::allpass::{Ap1, Ap2};

/// `CAPN_MAX_ORDER`
pub const CAPN_MAX_ORDER: usize = 7;

/// The kind of prototype filter (`enum capn_filter_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Butterworth,
    Chebyshev1,
    Chebyshev2,
    Elliptic,
}

/// Three cascaded all-passes: a second order and a first order section (`struct ap3_state`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Ap3 {
    ap2: Ap2,
    ap1: Ap1,
}

impl Ap3 {
    fn new(p0: Cx, p1: f64) -> Self {
        Self {
            ap2: ap2_coefs(p0),
            ap1: Ap1::new(-p1),
        }
    }
    pub fn reset(&mut self) {
        self.ap2.reset();
        self.ap1.reset();
    }
    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        self.ap1.run(self.ap2.run(s))
    }
}

/// Four cascaded all-passes: two second order sections (`struct ap4_state`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Ap4 {
    ap2: [Ap2; 2],
}

impl Ap4 {
    fn new(p0: Cx, p1: Cx) -> Self {
        Self {
            ap2: [ap2_coefs(p0), ap2_coefs(p1)],
        }
    }
    pub fn reset(&mut self) {
        self.ap2[0].reset();
        self.ap2[1].reset();
    }
    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let v = self.ap2[0].run(s);
        self.ap2[1].run(v)
    }
}

/// `ap2_init_coefs`
fn ap2_coefs(p0: Cx) -> Ap2 {
    Ap2::new(-2.0 * p0.re, p0.re * p0.re + p0.im * p0.im)
}

/// A single all-pass section that can be run and cleared.
pub trait Section: Copy {
    fn run(&mut self, s: f64) -> f64;
    fn reset(&mut self);
}

impl Section for Ap2 {
    #[inline]
    fn run(&mut self, s: f64) -> f64 {
        Ap2::run(self, s)
    }
    fn reset(&mut self) {
        Ap2::reset(self);
    }
}

impl Section for Ap3 {
    #[inline]
    fn run(&mut self, s: f64) -> f64 {
        Ap3::run(self, s)
    }
    fn reset(&mut self) {
        Ap3::reset(self);
    }
}

/// A crossover of odd order built of two all-pass branches.
pub trait Crossover: Copy {
    /// The type of the branch `a1`, which is also the type of the phase-matching all-passes.
    type A1: Section;
    /// The order of the filter.
    const ORDER: usize;
    /// `capN_init`
    fn new(fs: f64, fc: f64, ap: &[Cx]) -> Self;
    fn reset(&mut self);
    /// `capN_run`: the low-pass and the high-pass output.
    fn run(&mut self, s: f64) -> (f64, f64);
    /// The state of the branch `a1`.
    fn a1(&self) -> Self::A1;
    fn a1_mut(&mut self) -> &mut Self::A1;
}

/// A third-order crossover (`struct cap3_state`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Cap3 {
    a1: Ap1,
    a2: Ap2,
}

/// A fifth-order crossover (`struct cap5_state`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Cap5 {
    a1: Ap2,
    a2: Ap3,
}

/// A seventh-order crossover (`struct cap7_state`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Cap7 {
    a1: Ap3,
    a2: Ap4,
}

impl Section for Ap1 {
    #[inline]
    fn run(&mut self, s: f64) -> f64 {
        Ap1::run(self, s)
    }
    fn reset(&mut self) {
        Ap1::reset(self);
    }
}

impl Crossover for Cap3 {
    type A1 = Ap1;
    const ORDER: usize = 3;
    fn new(fs: f64, fc: f64, ap: &[Cx]) -> Self {
        let p = bilinear(3, fs, fc, ap);
        Self {
            a1: Ap1::new(-p[1].re),
            a2: ap2_coefs(p[0]),
        }
    }
    fn reset(&mut self) {
        self.a1.reset();
        self.a2.reset();
    }
    #[inline]
    fn run(&mut self, s: f64) -> (f64, f64) {
        let a1 = self.a1.run(s);
        let a2 = self.a2.run(s);
        ((a1 + a2) * 0.5, (a1 - a2) * 0.5)
    }
    fn a1(&self) -> Ap1 {
        self.a1
    }
    fn a1_mut(&mut self) -> &mut Ap1 {
        &mut self.a1
    }
}

impl Crossover for Cap5 {
    type A1 = Ap2;
    const ORDER: usize = 5;
    fn new(fs: f64, fc: f64, ap: &[Cx]) -> Self {
        let p = bilinear(5, fs, fc, ap);
        Self {
            a1: ap2_coefs(p[1]),
            a2: Ap3::new(p[0], p[2].re),
        }
    }
    fn reset(&mut self) {
        self.a1.reset();
        self.a2.reset();
    }
    #[inline]
    fn run(&mut self, s: f64) -> (f64, f64) {
        let a1 = self.a1.run(s);
        let a2 = self.a2.run(s);
        ((a1 + a2) * 0.5, (a1 - a2) * 0.5)
    }
    fn a1(&self) -> Ap2 {
        self.a1
    }
    fn a1_mut(&mut self) -> &mut Ap2 {
        &mut self.a1
    }
}

impl Crossover for Cap7 {
    type A1 = Ap3;
    const ORDER: usize = 7;
    fn new(fs: f64, fc: f64, ap: &[Cx]) -> Self {
        let p = bilinear(7, fs, fc, ap);
        Self {
            a1: Ap3::new(p[1], p[3].re),
            a2: Ap4::new(p[0], p[2]),
        }
    }
    fn reset(&mut self) {
        self.a1.reset();
        self.a2.reset();
    }
    #[inline]
    fn run(&mut self, s: f64) -> (f64, f64) {
        let a1 = self.a1.run(s);
        let a2 = self.a2.run(s);
        ((a1 + a2) * 0.5, (a1 - a2) * 0.5)
    }
    fn a1(&self) -> Ap3 {
        self.a1
    }
    fn a1_mut(&mut self) -> &mut Ap3 {
        &mut self.a1
    }
}

/// `capn_bilinear`: the poles at the corner frequency `fc` in the z-plane.
fn bilinear(n: usize, fs: f64, fc: f64, ap: &[Cx]) -> Vec<Cx> {
    let n_ap = n / 2 + 1;
    let fc_w = 2.0 * fs * (PI * fc / fs).tan(); // pre-warped corner frequency
    (0..n_ap)
        .map(|i| {
            let p = ap[i] * fc_w;
            (Cx::new(2.0 * fs, 0.0) + p) / (Cx::new(2.0 * fs, 0.0) - p)
        })
        .collect()
}

fn butterworth_ap(n: usize) -> Vec<Cx> {
    (0..n / 2 + 1)
        .map(|i| {
            let theta = (2 * i + 1) as f64 * PI / (2.0 * n as f64);
            Cx::new(-theta.sin(), theta.cos()) // normalized pole in the s-plane
        })
        .collect()
}

fn chebyshev_ap(n: usize, type2: bool, stop_db: f64) -> Vec<Cx> {
    if stop_db > 100.0 {
        return butterworth_ap(n);
    }
    let epsilon = (10f64.powf(stop_db / 10.0) - 1.0).sqrt();
    let sigma = epsilon.asinh() / n as f64;
    let scale = (epsilon.acosh() / n as f64).cosh();
    (0..n / 2 + 1)
        .map(|i| {
            let theta = (2 * i + 1) as f64 * PI / (2.0 * n as f64);
            let mut p = Cx::new(-sigma.sinh() * theta.sin(), sigma.cosh() * theta.cos());
            p /= scale; // so that H(1) = sqrt(0.5)
            if type2 {
                p = Cx::new(1.0, 0.0) / p;
            }
            p
        })
        .collect()
}

fn fz_sgn(x: f64) -> i32 {
    if x < 0.0 {
        -1
    } else if x > 0.0 {
        1
    } else {
        0
    }
}

/// `find_zero`: the false-position (Illinois) search for a zero of `f` within `[a, b]`; NaN
/// when there is none or it does not converge.
fn find_zero(f: &dyn Fn(f64) -> f64, mut a: f64, mut b: f64, mut tol: f64) -> f64 {
    let mut c = a;
    let (mut fn_a, mut fn_b) = (f(a), f(b));
    if tol < f64::EPSILON {
        tol = f64::EPSILON * 2.0;
    }
    let mut side = 0;
    for i in 0..100 {
        c = (fn_a * b - fn_b * a) / (fn_a - fn_b);
        if (b - a).abs() < tol * (b + a).abs() {
            return c;
        }
        let fn_c = f(c);
        if fz_sgn(fn_b) == fz_sgn(fn_c) {
            b = c;
            fn_b = fn_c;
            if side == -1 {
                fn_a /= 2.0;
            }
            side = -1;
        } else if fz_sgn(fn_a) == fz_sgn(fn_c) {
            a = c;
            fn_a = fn_c;
            if side == 1 {
                fn_b /= 2.0;
            }
            side = 1;
        } else {
            if i == 0 {
                return f64::NAN; // no zero within the interval
            }
            return c;
        }
    }
    let _ = c;
    f64::NAN // failed to converge
}

fn ellip_q_err(k: f64, target_q: f64) -> f64 {
    let kp = (1.0 - k * k).sqrt().sqrt();
    let l = (1.0 - kp) / ((1.0 + kp) * 2.0);
    (l + 2.0 * l.powi(5) + 15.0 * l.powi(9) + 150.0 * l.powi(13)) - target_q
}

/// `eval_allpass_ap`: the all-pass given by the poles `ap[off]`, `ap[off+stride]`… at `jw`.
fn eval_allpass_ap(ap: &[Cx], off: usize, n: usize, stride: usize, jw: Cx) -> Cx {
    let one = Cx::new(1.0, 0.0);
    if n == 0 {
        return one;
    }
    let mut num = one;
    let mut den = one;
    let mut end = ((n - 1) * stride) as isize;
    let at = |i: isize| ap[off + i as usize];
    if at(end).im == 0.0 {
        // a real root is always last
        num *= jw + at(end);
        den *= jw - at(end);
        end -= stride as isize;
    }
    let mut i = end;
    while i >= 0 {
        num *= (jw + at(i)) * (jw + at(i).conj());
        den *= (jw - at(i)) * (jw - at(i).conj());
        i -= stride as isize;
    }
    num / den
}

fn geom_dot(a: Cx, b: Cx) -> f64 {
    a.re * b.re + a.im * b.im
}

fn elliptic_ap(n: usize, stop_db_lp: f64, stop_db_hp: f64) -> Vec<Cx> {
    if stop_db_lp > 100.0 {
        return chebyshev_ap(n, false, stop_db_hp);
    } else if stop_db_hp > 100.0 {
        return chebyshev_ap(n, true, stop_db_lp);
    }
    let n_ap = n / 2 + 1;
    let nf = n as f64;
    let e2 = 1.0 / (10f64.powf(stop_db_hp / 10.0) - 1.0);
    let d = (10f64.powf(stop_db_lp / 10.0) - 1.0) / e2;
    let q = 1.0 / (2f64.powf(4.0 / nf) * d.powf(1.0 / nf));
    let k = find_zero(&|k| ellip_q_err(k, q), 0.0, 1.0, 0.0);
    if !k.is_normal() {
        return butterworth_ap(n);
    }
    let l = (((1.0 + e2).sqrt() + 1.0) / ((1.0 + e2).sqrt() - 1.0)).ln() / (2.0 * nf);
    let mut sigma0_s0 = l.sinh();
    let mut sigma0_s1 = 0.0;
    for m in 1..6 {
        let sgn = if m & 1 != 0 { -1.0 } else { 1.0 };
        let mf = m as f64;
        sigma0_s0 += sgn * q.powf(mf * (mf + 1.0)) * ((2.0 * mf + 1.0) * l).sinh();
        sigma0_s1 += sgn * q.powf(mf * mf) * (2.0 * mf * l).cosh();
    }
    let sigma0 = ((2.0 * q.sqrt().sqrt() * sigma0_s0) / (1.0 + 2.0 * sigma0_s1)).abs();
    let sigma02 = sigma0 * sigma0;
    let w = ((1.0 + k * sigma02) * (1.0 + sigma02 / k)).sqrt();
    let mut ap = vec![Cx::new(0.0, 0.0); n_ap];
    for (i, a) in ap.iter_mut().enumerate().take(n_ap - 1) {
        let mu = ((n_ap - 1) - i) as f64;
        let mut omega_s0 = (PI * mu / nf).sin();
        let mut omega_s1 = 0.0;
        for m in 1..6 {
            let sgn = if m & 1 != 0 { -1.0 } else { 1.0 };
            let mf = m as f64;
            omega_s0 += sgn * q.powf(mf * (mf + 1.0)) * ((2.0 * mf + 1.0) * PI * mu / nf).sin();
            omega_s1 += sgn * q.powf(mf * mf) * (2.0 * mf * PI * mu / nf).cos();
        }
        let omega = (2.0 * q.sqrt().sqrt() * omega_s0) / (1.0 + 2.0 * omega_s1);
        let omega2 = omega * omega;
        let vi = ((1.0 - k * omega2) * (1.0 - omega2 / k)).sqrt();
        let den = 2.0 * (1.0 + sigma02 * omega2);
        // complex poles (the conjugates are not stored)
        *a = Cx::new((-2.0 * sigma0 * vi) / den, (2.0 * omega * w) / den);
    }
    ap[n_ap - 1] = Cx::new(-sigma0, 0.0); // real pole

    if (stop_db_lp - stop_db_hp).abs() > 0.01 {
        // scale so that the magnitude at w = 1 is sqrt(0.5)
        let n1 = n_ap / 2;
        let n0 = n1 + (n_ap & 1);
        let half_width = (1.0 / k).sqrt();
        let err = |w: f64| {
            let jw = Cx::new(0.0, w);
            geom_dot(
                eval_allpass_ap(&ap, 0, n0, 2, jw),
                eval_allpass_ap(&ap, 1, n1, 2, jw),
            )
        };
        let wc = find_zero(&err, 1.0 / half_width, half_width, 0.0);
        if !wc.is_normal() {
            return butterworth_ap(n);
        }
        for a in ap.iter_mut() {
            *a /= wc;
        }
    }
    ap
}

/// `capn_ap`: the s-plane poles (one of each conjugate pair) of the prototype of order `n`,
/// which must be odd. `stop_db` holds the stopband attenuations (one for Chebyshev, the
/// low-pass and high-pass ones for elliptic). `None` for an even order.
pub fn capn_ap(f: FilterType, n: usize, stop_db: [f64; 2]) -> Option<Vec<Cx>> {
    if n & 1 == 0 {
        return None;
    }
    Some(match f {
        FilterType::Butterworth => butterworth_ap(n),
        FilterType::Chebyshev1 => chebyshev_ap(n, false, stop_db[0]),
        FilterType::Chebyshev2 => chebyshev_ap(n, true, stop_db[0]),
        FilterType::Elliptic => elliptic_ap(n, stop_db[0], stop_db[1]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs: f64, f: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f64 / fs).sin())
            .collect()
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn split<C: Crossover>(c: &mut C, x: &[f64]) -> (Vec<f64>, Vec<f64>) {
        x.iter().map(|&s| c.run(s)).unzip()
    }

    #[test]
    fn even_orders_are_refused() {
        assert!(capn_ap(FilterType::Elliptic, 4, [35.0, 50.0]).is_none());
        assert_eq!(
            capn_ap(FilterType::Elliptic, 5, [35.0, 50.0])
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn the_branches_of_every_design_cross_at_minus_three_decibels() {
        let fs = 48_000.0;
        for (ty, stop) in [
            (FilterType::Butterworth, [0.0, 0.0]),
            (FilterType::Chebyshev1, [25.0, 0.0]),
            (FilterType::Chebyshev2, [25.0, 0.0]),
            (FilterType::Elliptic, [35.0, 50.0]),
        ] {
            let ap = capn_ap(ty, 5, stop).unwrap();
            let x = tone(fs, 1000.0, 20_000);
            let mut c = Cap5::new(fs, 1000.0, &ap);
            let (lp, hp) = split(&mut c, &x);
            let (l, h) = (rms(&lp[10_000..]), rms(&hp[10_000..]));
            assert!((l - 0.5).abs() < 0.05, "{ty:?} {l}");
            assert!((l - h).abs() < 0.02, "{ty:?} {l} {h}");
        }
    }

    #[test]
    fn low_and_high_pass_add_up_to_an_all_pass() {
        let fs = 48_000.0;
        let ap = capn_ap(FilterType::Elliptic, 7, [35.0, 50.0]).unwrap();
        for f in [100.0, 1000.0, 9000.0] {
            let x = tone(fs, f, 20_000);
            let mut c = Cap7::new(fs, 1000.0, &ap);
            let (lp, hp) = split(&mut c, &x);
            let sum: Vec<f64> = lp.iter().zip(&hp).map(|(a, b)| a + b).collect();
            assert!(
                (rms(&sum[10_000..]) - rms(&x[10_000..])).abs() < 1e-3,
                "{f}"
            );
        }
    }
}
