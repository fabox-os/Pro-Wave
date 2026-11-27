//! The inter-channel delay estimator of the multiband matrix decoder (`delay_est_*` and
//! `estimate_delay` in `matrix4_common.c`).
//!
//! Each band keeps the last few milliseconds of its left and right analysis signals. When an
//! event starts, the estimator oversamples that window, runs a hair-cell (inner hair cell)
//! model over it and looks for the relative delay between the channels at which the steering
//! angles are the most pronounced. A delay that disagrees with the one in use marks the event
//! as "unaligned" so the decoder can re-align the channels or neutralise the event.

use std::f64::consts::{FRAC_PI_4, PI};

use crate::matrix4_common::{
    axes_mag, calc_angle_pwr, smoothstep, square, time_to_frames, Axes, EVENT_SAMPLE_TIME,
};
use crate::smoothing::{rise_time, Ewma};

/// `DELAY_EST_MAX_DELTA`: the largest delay between channels that is looked for, in ms.
pub(crate) const DELAY_EST_MAX_DELTA: f64 = 1.0;
/// `DELAY_EST_SAMPLE_TIME` in ms.
pub(crate) const DELAY_EST_SAMPLE_TIME: f64 = 15.0;
const DELAY_EST_WINDOW_ALPHA: f64 = 0.35;
const DELAY_EST_INDETERM: f64 = 1.05;
const DELAY_EST_OVERSAMPLE: usize = 2;
const DELAY_EST_POLYPHASE_TAPS: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
struct Bin {
    dt: f64,
    n: usize,
}

/// `struct delay_est_hist`: what all the bands share.
#[derive(Clone, Debug)]
pub(crate) struct DelayEstHist {
    pub align_avg: Ewma,
}

impl DelayEstHist {
    /// `delay_est_hist_init`
    pub fn new(n: usize) -> Self {
        Self {
            align_avg: Ewma::new(1.0, 80.0 * (n as f64).powf(0.7)),
        }
    }
}

/// `struct delay_est_state`
#[derive(Clone, Debug)]
pub(crate) struct DelayEst {
    agc: [Ewma; 2],
    sm: [Ewma; 2],
    buf: Vec<[f64; 2]>,
    rot: Vec<[f64; 2]>,
    os: Vec<[f64; 2]>,
    ihc: Vec<[f64; 2]>,
    pub tol: f64,
    pub ra_tol: f64,
    bin_tol: f64,
    pub thresh: f64,
    ws: f64,
    pub ihc_norm: [f64; 2],
    p: usize,
    len: usize,
    os_len: usize,
    dt: isize,
    pub ev_lag: isize,
    n_bins: usize,
    bin: [Bin; 8],
}

impl DelayEst {
    /// `delay_est_state_init`; `ev_fs` is the (downsampled) rate of the event detector.
    pub fn new(fs: f64, fc: f64, bw: f64, st_sc: f64, ev_fs: f64) -> Self {
        let tol = fs / (fc * 16.0); // ±22.5° at fc
        let ra_tol = fs / (fc * 64.0).max(8e4); // ±5.625° at fc or ±12.5µs
        let bin_tol = fs / (fc * 16.0).max(2e4); // ±22.5° at fc or ±50µs
        let mut thresh = if bw > 0.0 {
            let t = 1.2
                / (bw * st_sc * (DELAY_EST_SAMPLE_TIME / 1000.0))
                    .sqrt()
                    .sqrt();
            t.clamp(0.5, 0.95)
        } else {
            0.5
        };
        thresh *= FRAC_PI_4;
        let ws = 1000.0 / (DELAY_EST_MAX_DELTA * fs);
        let os_fs = fs * DELAY_EST_OVERSAMPLE as f64;
        let dt = time_to_frames(DELAY_EST_MAX_DELTA, os_fs) + 1;
        let ew = |ms: f64| Ewma::new(os_fs, rise_time(ms));
        let sample_time = st_sc * DELAY_EST_SAMPLE_TIME;
        let sample_frames = time_to_frames(sample_time, fs) as usize;
        let osr = DELAY_EST_OVERSAMPLE as isize;
        let len = sample_frames + DELAY_EST_POLYPHASE_TAPS + (dt / osr + (dt & (osr - 1))) as usize;
        let os_len = (len - DELAY_EST_POLYPHASE_TAPS) * DELAY_EST_OVERSAMPLE;
        let ev_lag = time_to_frames(
            (sample_time / 2.0 + (DELAY_EST_POLYPHASE_TAPS / 2) as f64 * 1000.0 / fs)
                .min(EVENT_SAMPLE_TIME),
            ev_fs,
        );
        let mut bin = [Bin::default(); 8];
        bin[0] = Bin { dt: 0.0, n: 1 };
        Self {
            agc: [ew(63.0); 2],
            sm: [ew(0.5); 2],
            buf: vec![[0.0; 2]; len],
            rot: vec![[0.0; 2]; len],
            os: vec![[0.0; 2]; os_len],
            ihc: vec![[0.0; 2]; os_len],
            tol,
            ra_tol,
            bin_tol,
            thresh,
            ws,
            ihc_norm: [0.0; 2],
            p: 0,
            len,
            os_len,
            dt,
            ev_lag,
            n_bins: 1,
            bin,
        }
    }

    /// `delay_est_insert`
    #[inline]
    pub fn insert(&mut self, l: f64, r: f64) {
        self.buf[self.p] = [l, r];
        self.p = if self.p + 1 < self.len { self.p + 1 } else { 0 };
    }

    /// The latest `c0` and `c1` samples back from the newest one: `buf[(p-1-back) mod len]`.
    #[inline]
    pub fn back(&self, back: isize) -> [f64; 2] {
        let len = self.len as isize;
        let i = (self.p as isize - 1 - back).rem_euclid(len);
        self.buf[i as usize]
    }

    /// `ihc_norm` for an event that has just started.
    pub fn set_norm(&mut self) {
        let mut n = [0.0f64; 2];
        for b in &self.buf {
            n[0] += square(b[0]);
            n[1] += square(b[1]);
        }
        let len = self.len as f64;
        self.ihc_norm = [
            1.0 / ((n[0] / len).sqrt() + f64::MIN_POSITIVE),
            1.0 / ((n[1] / len).sqrt() + f64::MIN_POSITIVE),
        ];
    }

    /// `delay_est_ihc_model`
    fn ihc_model(&mut self) {
        let v = self.agc[0].set(0.35);
        self.agc[1].set(v);
        let v = self.sm[0].set(0.0);
        self.sm[1].set(v);
        let norm = [self.ihc_norm[0] * 0.25, self.ihc_norm[1] * 0.25];
        for i in 0..self.os_len {
            let v = [self.os[i][0] * norm[0], self.os[i][1] * norm[1]];
            let x = [
                (smoothstep(v[0] + 0.1) + smoothstep(-v[0] + 0.1)) * (1.0 - self.agc[0].last()),
                (smoothstep(v[1] + 0.1) + smoothstep(-v[1] + 0.1)) * (1.0 - self.agc[1].last()),
            ];
            self.agc[0].run(x[0] * 10.0);
            self.agc[1].run(x[1] * 10.0);
            self.ihc[i][0] = self.sm[0].run(x[0] * 0.5);
            self.ihc[i][1] = self.sm[1].run(x[1] * 0.5);
        }
        let v = self.sm[0].set(0.0);
        self.sm[1].set(v);
        for i in (0..self.os_len).rev() {
            self.ihc[i][0] = self.sm[0].run(self.ihc[i][0]);
            self.ihc[i][1] = self.sm[1].run(self.ihc[i][1]);
        }
    }

    /// `delay_est_step`: the left–right and centre–surround angles and the weight at the
    /// trial delay `dt` (in oversampled frames).
    fn step(&self, dt: isize) -> (f64, f64, f64) {
        let mut avg = [0.0f64; 4];
        let mut wt = 0.0;
        let dt0 = (-dt).min(0);
        let dt1 = dt.min(0);
        for i in self.dt as usize..self.os_len {
            let i0 = (i as isize + dt0) as usize;
            let i1 = (i as isize + dt1) as usize;
            let w = self.ihc[i0][0] * self.ihc[i1][1];
            let v = [self.os[i0][0], self.os[i1][1]];
            avg[0] += square(v[0]) * w;
            avg[1] += square(v[1]) * w;
            avg[2] += square(v[0] + v[1]) * w;
            avg[3] += square(v[0] - v[1]) * w;
            wt += w;
        }
        (
            calc_angle_pwr(avg[0], avg[1]),
            calc_angle_pwr(avg[2], avg[3]),
            wt,
        )
    }

    /// `estimate_delay`: the best delay in frames (NaN when it cannot be told) and the
    /// steering angles found there.
    pub fn estimate(&mut self, ref_dt: f64) -> (f64, Option<Axes>) {
        // rotate the circular buffer so the oldest frame comes first
        let (p, len) = (self.p, self.len);
        self.rot[..len - p].copy_from_slice(&self.buf[p..len]);
        self.rot[len - p..len].copy_from_slice(&self.buf[..p]);
        for i in 0..len - DELAY_EST_POLYPHASE_TAPS {
            let z = oversample(&self.rot[i..i + DELAY_EST_POLYPHASE_TAPS]);
            self.os[i * DELAY_EST_OVERSAMPLE] = z[0];
            self.os[i * DELAY_EST_OVERSAMPLE + 1] = z[1];
        }
        self.ihc_model();
        self.search(ref_dt)
    }

    fn search(&mut self, ref_dt: f64) -> (f64, Option<Axes>) {
        let osr = DELAY_EST_OVERSAMPLE as f64;
        let ref_dt_step = (ref_dt * osr).round_ties_even() as isize;
        let mut max = Axes::default();
        let mut reference = Axes::default();
        let mut max_sum = [0.0f64; 3];
        let mut max_dt = f64::NAN;
        let mut lr = [0.0f64; 3];
        let mut cs = [0.0f64; 3];
        let mut w = [0.0f64; 3];
        let mut y = [0.0f64; 3];
        let (v, l, c, wt) = self.step_signed(-self.dt);
        y[1] = v;
        lr[1] = l;
        cs[1] = c;
        w[1] = wt;
        let (v, l, c, wt) = self.step_signed(-self.dt + 1);
        y[2] = v;
        lr[2] = l;
        cs[2] = c;
        w[2] = wt;
        for dt in -self.dt + 2..=self.dt {
            lr[0] = lr[1];
            lr[1] = lr[2];
            cs[0] = cs[1];
            cs[1] = cs[2];
            w[0] = w[1];
            w[1] = w[2];
            y[0] = y[1];
            y[1] = y[2];
            let (v, l, c, wt) = self.step_signed(dt);
            y[2] = v;
            lr[2] = l;
            cs[2] = c;
            w[2] = wt;
            let (x, iy) = interp_peak_quadratic(&y);
            if !x.is_nan() {
                let idt = ((dt - 1) as f64 + x) * (1.0 / osr);
                let abs_idt = idt.abs();
                let wx = abs_idt * self.ws;
                if wx < 1.0 {
                    let awiy = iy.abs()
                        * interp_pts_quadratic(&w, x)
                        * smoothstep((1.0 - wx) * (1.0 / DELAY_EST_WINDOW_ALPHA));
                    if awiy > max_sum[1] {
                        max_sum[0] = iy;
                        max_sum[2] = max_sum[1];
                        max_sum[1] = awiy;
                        max_dt = idt;
                        max.lr = interp_pts_quadratic(&lr, x);
                        max.cs = interp_pts_quadratic(&cs, x);
                    } else if awiy > max_sum[2] {
                        max_sum[2] = awiy;
                    }
                }
            }
            if dt - 1 == ref_dt_step {
                let x = ref_dt * osr - ref_dt_step as f64;
                reference.lr = interp_pts_quadratic(&lr, x);
                reference.cs = interp_pts_quadratic(&cs, x);
            }
        }
        if max_dt.is_nan() {
            return (f64::NAN, None);
        }
        if max.cs.abs() < PI / 32.0 {
            return (ref_dt, Some(reference));
        }
        if max_sum[2] > f64::MIN_POSITIVE
            && (max_dt - ref_dt).abs() > self.tol
            && max_sum[1] < max_sum[2] * DELAY_EST_INDETERM
        {
            return (f64::NAN, None);
        }
        if (reference.cs - max.cs).abs() < PI / 64.0 {
            max_sum[0] = axes_mag(&reference).copysign(reference.cs);
            max_dt = ref_dt;
            max = reference;
        }
        if max_sum[0].abs() > self.thresh {
            let mut b = 0;
            while b < self.n_bins && (max_dt - self.bin[b].dt).abs() > self.bin_tol {
                b += 1;
            }
            if b > 0 {
                if b < self.n_bins {
                    let tmp = self.bin[b];
                    self.bin.copy_within(0..b, 1);
                    self.bin[0] = tmp;
                } else {
                    if b < self.bin.len() {
                        self.n_bins += 1;
                    } else {
                        b = self.bin.len() - 1;
                    }
                    self.bin.copy_within(0..b, 1);
                    self.bin[0] = Bin::default();
                }
            }
            self.bin[0].n += 1;
            max_dt = self.bin[0].dt + (max_dt - self.bin[0].dt) / (self.bin[0].n.min(16) as f64);
            self.bin[0].dt = max_dt;
        }
        (max_dt, Some(max))
    }

    /// `delay_est_step` with its return value: `|lr| + |cs|` carrying the sign of `cs`.
    fn step_signed(&self, dt: isize) -> (f64, f64, f64, f64) {
        let (lr, cs, wt) = self.step(dt);
        ((lr.abs() + cs.abs()).copysign(cs), lr, cs, wt)
    }
}

/// `delay_est_oversample` (2 phases, 8 taps; windowed sinc, Lanczos): the sample at the
/// centre and the one half a frame later.
#[allow(clippy::excessive_precision)]
fn oversample(y: &[[f64; 2]]) -> [[f64; 2]; 2] {
    const FLT: [f64; 8] = [
        -2.39245719590458310e-02,
        7.18429587806505288e-02,
        -1.75493437951545650e-01,
        6.23769943523017845e-01,
        6.23769943523017845e-01,
        -1.75493437951545650e-01,
        7.18429587806505288e-02,
        -2.39245719590458310e-02,
    ];
    let mut z1 = [0.0f64; 2];
    for ch in 0..2 {
        z1[ch] = y[0][ch] * FLT[0]
            + y[1][ch] * FLT[1]
            + y[2][ch] * FLT[2]
            + y[3][ch] * FLT[3]
            + y[4][ch] * FLT[4]
            + y[5][ch] * FLT[5]
            + y[6][ch] * FLT[6]
            + y[7][ch] * FLT[7];
    }
    [y[3], z1]
}

/// `interp_peak_quadratic`: the position (NaN if there is no peak) and height of the peak
/// through three points.
fn interp_peak_quadratic(y: &[f64; 3]) -> (f64, f64) {
    if y[0] == 0.0 && y[1] == 0.0 && y[2] == 0.0 {
        return (f64::NAN, 0.0);
    }
    let num = y[0] - y[2];
    let den = y[0] + y[2] - 2.0 * y[1];
    if num.abs() > den.abs() {
        return (f64::NAN, 0.0);
    }
    let x = num / (2.0 * den);
    (x, y[1] - 0.25 * num * x)
}

/// `interp_pts_quadratic`
fn interp_pts_quadratic(y: &[f64; 3], t: f64) -> f64 {
    let c = [y[1], 0.5 * (y[2] - y[0]), 0.5 * (y[2] + y[0]) - y[1]];
    (c[2] * t + c[1]) * t + c[0]
}
