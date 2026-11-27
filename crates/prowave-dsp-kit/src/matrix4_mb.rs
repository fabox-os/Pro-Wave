//! `matrix4_mb` (`matrix4_mb.c`): the multiband matrix decoder. The input is split into 13 or
//! 21 bands and every band is steered on its own, so a sound in one part of the spectrum can
//! sit somewhere else in the room than a sound in another.
//!
//! The effect is three stages: a copy of the input pair is appended as analysis channels
//! (`matrix4_mb_input`, which asks the chain to line the pair up so the analysis runs ahead
//! of the audio), a short linear-phase FIR that undoes the phase shift of the sum of the
//! bands, and the decoder proper, which runs the analysis through a decimating filter bank,
//! finds the steering of every band (see [`crate::matrix4_common`]), and mixes the bands of
//! the synthesis filter bank with the matrix of the band.
//!
//! When the two channels of an event are out of step by a delay of up to 1 ms, the decoder
//! estimates it ([`crate::delay_est`]) and, with `unaligned_event_action=align`, delays one
//! channel per band to line them up before the matrix is applied.
//!
//! Logic Pro equivalent: none; Logic's Surround Panner does not follow the stereo image.

use std::f64::consts::{FRAC_PI_4, PI};

use crate::allpass::Ap1;
use crate::capn::FilterType;
use crate::delay_est::{DelayEst, DelayEstHist, DELAY_EST_MAX_DELTA};
use crate::effect::{Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::filter_bank::{FShape, FilterBank, FilterBankParams, FB_MAX_BANDS, FB_PARAMS};
use crate::fir::{fir_effect, Filter};
use crate::fir_p::fir_p_effect;
use crate::matrix4_common::{
    calc_matrix_coefs, calc_norm_mult, config_init, phase_flip_ap1_c0, phase_flip_params,
    phase_flip_pos_rs, smoothstep, time_to_frames, Axes, CmcParams, Config, CsInterp, DirectPath,
    DirectPathMode, Envs, EventConfig, EventState, SmoothState, UnalignedEventAction,
    CS_INTERP_DELAY_FRAMES, DOWNSAMPLE_FACTOR, EVENT_SAMPLE_TIME, EVENT_THRESH, RISE_TIME_FAST,
};
use crate::smoothing::{rise_time, Ewma};

const NORM_ACCOM_FACTOR: f64 = 0.6;
const DIFF_OVERSHOOT: f64 = 1.02;
const BASE_ORD_NOTCH_SCALE_F0: f64 = 700.0; // -3dB point; Gaussian lowpass
const EVENT_THRESH_MAX: f64 = 3.6;
const EVENT_THRESH_MIN: f64 = 1.4;
const PHASE_LIN_TRUNC_THRESH: f64 = 1e-6;
const CH_ALIGN_LATENCY: i32 = 1;
/// The original's rounded `log10(e)`, kept as it is.
#[allow(clippy::approx_constant)]
const XTC_LOG10_E: f64 = 0.4343;

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "matrix4_mb",
    usage: "[options ...] [surround_level][/surround_level_rear]",
    init,
    number: 0,
}];

static INPUT_NAME: &str = "matrix4_mb_input";

// ----- the input stage -----

/// `matrix4_mb_input`: appends the left and right input as two more channels.
struct Input {
    istream: Stream,
    ostream: Stream,
    selector: Vec<bool>,
    c0: usize,
    c1: usize,
    lookahead_frames: isize,
}

impl Effect for Input {
    fn name(&self) -> &'static str {
        INPUT_NAME
    }
    fn istream(&self) -> Stream {
        self.istream
    }
    fn ostream(&self) -> Stream {
        self.ostream
    }
    fn buffer_frames(&self, frames: usize) -> usize {
        frames
    }
    fn ch_deps_identity(&self) -> bool {
        false
    }
    fn channel_selector(&self) -> Option<&[bool]> {
        Some(&self.selector)
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf {
        let (ich, och) = (self.istream.channels, self.ostream.channels);
        for i in 0..*frames {
            let (src, dst) = (
                &ibuf[i * ich..(i + 1) * ich],
                &mut obuf[i * och..(i + 1) * och],
            );
            dst[..ich].copy_from_slice(src);
            dst[ich] = src[self.c0];
            dst[ich + 1] = src[self.c1];
        }
        Buf::Output
    }
    fn channel_offsets(&self, _latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        let n = self.istream.channels;
        req_delay[n] -= self.lookahead_frames;
        req_delay[n + 1] -= self.lookahead_frames;
        true
    }
}

// ----- channel alignment -----

/// `ch_align_set`: the whole and the fractional delay for `dt` frames.
fn ch_align_set(dt: f64, ap: &mut Ap1) -> i32 {
    let r_dt = (dt - 0.1) as i32;
    let dt_frac = dt - f64::from(r_dt);
    ap.c0 = (1.0 - dt_frac) / (1.0 + dt_frac);
    r_dt
}

/// `struct ch_align_an_state`: aligns the analysis channels of a band.
#[derive(Clone, Copy, Default)]
struct AlignAn {
    ap: Ap1,
    dt: i32,
}

impl AlignAn {
    fn new() -> Self {
        let mut a = Self::default();
        a.dt = ch_align_set(f64::from(CH_ALIGN_LATENCY), &mut a.ap);
        a.ap.reset();
        a
    }
    /// `ch_align_an_set`
    fn set(&mut self, dt: f64) {
        self.dt = ch_align_set(dt.abs() + f64::from(CH_ALIGN_LATENCY), &mut self.ap);
        if dt.is_sign_negative() {
            self.dt = -self.dt;
        }
        self.ap.reset();
    }
    /// `ch_align_an_run`
    fn run(&mut self, de: &DelayEst) -> (f64, f64) {
        if self.dt < 0 {
            let s0 = de.back(CH_ALIGN_LATENCY as isize)[0];
            let s1 = self.ap.run(de.back(-self.dt as isize)[1]);
            (s0, s1)
        } else {
            let s0 = self.ap.run(de.back(self.dt as isize)[0]);
            let s1 = de.back(CH_ALIGN_LATENCY as isize)[1];
            (s0, s1)
        }
    }
}

/// `struct ch_align_syn_state`: aligns the synthesis channels of a band, cross-fading from
/// the previous delay (`[0]`, `[1]`) to the new one (`[2]`, `[3]`).
#[derive(Clone, Copy, Default)]
struct AlignSyn {
    ap: [Ap1; 4],
    dt: [i32; 4],
}

impl AlignSyn {
    fn new(reference: i32) -> Self {
        let mut a = Self::default();
        for i in 0..4 {
            a.dt[i] = ch_align_set(f64::from(reference), &mut a.ap[i]);
            a.ap[i].reset();
        }
        a
    }
    /// `ch_align_syn_xfade_start`
    fn xfade_start(&mut self, reference: i32, dt: f64) {
        self.ap[0] = self.ap[2];
        self.ap[1] = self.ap[3];
        self.dt[0] = self.dt[2];
        self.dt[1] = self.dt[3];
        self.dt[2] = ch_align_set(f64::from(reference) - dt, &mut self.ap[2]);
        self.dt[3] = ch_align_set(f64::from(reference) + dt, &mut self.ap[3]);
        self.ap[2].reset();
        self.ap[3].reset();
    }
}

// ----- crosstalk cancellation -----

/// `XTC_FTZ`: keeps the all-pass state out of the denormal range.
const XTC_FTZ: f64 = (2e3 * f64::MIN_POSITIVE) / f64::EPSILON;

/// `struct xtc_state`: feeds a delayed, attenuated copy of each channel to the other one with
/// the opposite sign (the sound of a speaker reaching the far ear).
struct Xtc {
    ap: [Ap1; 2],
    g: f64,
    buf: Vec<[f64; 2]>,
    p: usize,
}

impl Xtc {
    /// `xtc_state_init`; `None` when the level is too low to matter.
    fn new(itd: f64, ild: f64) -> Option<Self> {
        if ild < 1e-3 {
            return None;
        }
        let itd_int = (itd - 0.1) as i32;
        let itd_frac = itd - f64::from(itd_int);
        let mut ap0 = Ap1::new((1.0 - itd_frac) / (1.0 + itd_frac));
        ap0.o0 = XTC_FTZ;
        ap0.i0 = XTC_FTZ;
        Some(Self {
            ap: [ap0, ap0],
            g: ild,
            buf: vec![[0.0; 2]; itd_int.max(1) as usize],
            p: 0,
        })
    }
    fn run(&mut self, g: f64, l: &mut f64, r: &mut f64) {
        let s = [*l, *r];
        let d = [
            (self.ap[0].run(self.buf[self.p][0] + XTC_FTZ) - XTC_FTZ) * self.g,
            (self.ap[1].run(self.buf[self.p][1] + XTC_FTZ) - XTC_FTZ) * self.g,
        ];
        self.buf[self.p][0] = s[0] * g - d[1];
        *l = s[0] - d[1];
        self.buf[self.p][1] = s[1] * g - d[0];
        *r = s[1] - d[0];
        self.p = if self.p + 1 < self.buf.len() {
            self.p + 1
        } else {
            0
        };
    }
}

/// `gauss_mag`: the magnitude of a Gaussian low-pass (or high-pass) with its −3 dB point at `f0`.
fn gauss_mag(f0: f64, f: f64, hp: bool) -> f64 {
    let r = f / f0;
    if hp {
        1.0 - (-1.227947177299516 * r * r).exp()
    } else {
        (-3.465735902799727e-01 * r * r).exp()
    }
}

// ----- the decoder -----

/// `struct ax_buf_frame`: what a band found at one step, delayed until the slower bands
/// catch up.
#[derive(Clone, Copy, Default)]
struct AxFrame {
    ax: Axes,
    ax_ev: Axes,
    ax_dpwr: Axes,
    diff_last: Axes,
    pwrcmp_factor: f64,
    ev_ds_diff: f64,
    align_syn_dt: f64,
    rs_scale: f64,
    ev_hold: bool,
    ev_maybe: bool,
    ev_unaligned: bool,
    ev_realign: bool,
}

#[derive(Default)]
struct Interp {
    ll: CsInterp,
    lr: CsInterp,
    rl: CsInterp,
    rr: CsInterp,
    lsl: CsInterp,
    lsr: CsInterp,
    rsl: CsInterp,
    rsr: CsInterp,
    sg: CsInterp,
    rg: CsInterp,
    xg: CsInterp,
    amb: CsInterp,
    sdir: CsInterp,
    rdir: CsInterp,
}

struct Band {
    sm: SmoothState,
    env: Envs,
    pwr_env: Envs,
    de: Option<DelayEst>,
    ev: EventState,
    ax_f: AxFrame,
    align_an: AlignAn,
    align_syn: AlignSyn,
    m: Interp,
    pf_ap_c0: [CsInterp; 2],
    pf_ap: [Ap1; 2],
    dp: DirectPath,
    ev_thresh: Ewma,
    xtc: Option<Xtc>,
    ax_buf: Vec<AxFrame>,
    ax_buf_len: isize,
    ax_buf_p: usize,
    ev_thresh_max: f64,
    ev_thresh_min: f64,
    contour: f64,
    rear_shelf: f64,
    syn_dt: f64,
    syn_tol_r: f64,
    align_xfade_scale: f64,
    align_xfade_p: i64,
}

struct Matrix4Mb {
    istream: Stream,
    ostream: Stream,
    selector: Vec<bool>,
    cfg: Config,
    s: usize,
    n_bands: usize,
    have_rears: bool,
    fshape: [FShape; 2],
    fb: FilterBank,
    band: Vec<Band>,
    evc: EventConfig,
    de_hist: DelayEstHist,
    pf_params: [f64; 2],
    /// The history of the synthesis bands (left bands then right bands per frame), for the
    /// alignment; empty without it.
    syn_buf: Vec<f64>,
    syn_buf_p: usize,
    syn_buf_len: usize,
    align_ref_syn: i32,
    drain_frames: isize,
    latency_frames: isize,
    align_latency_frames: isize,
}

impl Matrix4Mb {
    /// `syn_buf_get_frame`: the offset of the frame `dt` frames back.
    fn syn_frame(&self, dt: i32) -> usize {
        let p = (self.syn_buf_p as isize - dt as isize).rem_euclid(self.syn_buf_len as isize);
        p as usize * self.n_bands * 2
    }

    /// `ch_align_syn_run`: the aligned pair of band `k`; the first value comes from the right
    /// channel's history and the second from the left's.
    fn align_syn_run(&mut self, k: usize, band: usize, t: f64) -> (f64, f64) {
        let nb = self.n_bands;
        let a = self.band[band].align_syn;
        let at = |s: &Self, i: usize, right: bool| {
            s.syn_buf[s.syn_frame(a.dt[i]) + if right { nb + k } else { k }]
        };
        let (v2, v3) = (at(self, 2, true), at(self, 3, false));
        let b = &mut self.band[band].align_syn;
        let mut s0 = b.ap[2].run(v2);
        let mut s1 = b.ap[3].run(v3);
        if t < 1.0 {
            let (v0, v1) = (at(self, 0, true), at(self, 1, false));
            let b = &mut self.band[band].align_syn;
            s0 = s0 * t + b.ap[0].run(v0) * (1.0 - t);
            s1 = s1 * t + b.ap[1].run(v1) * (1.0 - t);
        }
        (s0, s1)
    }

    /// Everything that happens once every `DOWNSAMPLE_FACTOR` samples.
    fn update(&mut self) {
        let n_bands = self.n_bands;
        // find the bands with possible events
        let mut angles: Vec<(Axes, f64, f64)> = Vec::with_capacity(FB_MAX_BANDS);
        for b in &self.band {
            if b.ax_f.ev_maybe {
                angles.push((b.ax_f.diff_last, b.syn_dt, b.syn_tol_r));
            }
        }
        let fs = f64::from(self.istream.fs);
        for k in 0..n_bands {
            let mask = self.fb.an_mask(k);
            let band = &mut self.band[k];
            // modulate the event threshold by the number of bands with similar steering
            let mut ev_thresh_fact = 0.0;
            if band.ax_f.ev_maybe {
                for (ax, dt, tol_r) in &angles {
                    let d_lr = (ax.lr - band.ax_f.diff_last.lr).abs();
                    let d_cs = (ax.cs - band.ax_f.diff_last.cs).abs();
                    ev_thresh_fact += smoothstep(1.0 - d_lr.max(d_cs) * (16.0 / PI))
                        * smoothstep(2.0 - (band.syn_dt - dt).abs() * band.syn_tol_r.min(*tol_r));
                }
                ev_thresh_fact -= 1.0;
            }
            let ev_thresh = band.ev_thresh.run_set_max(
                band.ev_thresh_max
                    - (band.ev_thresh_max - band.ev_thresh_min) * ev_thresh_fact
                        / (n_bands - 1) as f64,
            );

            let mut ax = Axes::default();
            let mut ax_ev = Axes::default();
            let mut ax_dpwr = Axes::default();
            let (env, pwr_env) = (band.env, band.pwr_env);
            let hist = &mut self.de_hist;
            band.ev.process(
                &self.evc,
                &env,
                &pwr_env,
                band.de.as_mut().map(|d| (d, hist)),
                NORM_ACCOM_FACTOR,
                ev_thresh * (1.0 / EVENT_THRESH),
                &mut ax,
                &mut ax_ev,
                &mut ax_dpwr,
            );
            let ev = &mut band.ev;
            let mut ax_f = AxFrame {
                ax,
                ax_ev,
                ax_dpwr,
                diff_last: ev.diff_last,
                pwrcmp_factor: ev.pwrcmp_factor.last(),
                ev_ds_diff: ev.ds_diff,
                rs_scale: ev.rs_scale.last(),
                ev_hold: ev.t_hold != 0,
                ev_maybe: (ev.slope_last[0] > 0.0 && ev.last[0] > band.ev_thresh_min)
                    || (ev.slope_last[1] > 0.0 && ev.last[1] > band.ev_thresh_min),
                ev_unaligned: ev.unaligned,
                ev_realign: ev.realign,
                align_syn_dt: 0.0,
            };
            if ax_f.ev_realign {
                ax_f.align_syn_dt = ev.align_dt[0] * f64::from(mask + 1);
                if band.de.is_some() {
                    band.align_an.set(ev.align_dt[0]);
                }
                ev.realign = false;
            }
            if band.ax_buf_len > 0 {
                band.ax_f = band.ax_buf[band.ax_buf_p];
                band.ax_buf[band.ax_buf_p] = ax_f;
                band.ax_buf_p = if band.ax_buf_p + 1 < band.ax_buf.len() {
                    band.ax_buf_p + 1
                } else {
                    0
                };
            } else {
                band.ax_f = ax_f;
            }
            let f = band.ax_f;
            let mut matrix_adj = self.cfg.matrix_adj[0];
            if self.cfg.dp_mode != DirectPathMode::None {
                let r_pan = band.dp.pan(
                    &f.ax,
                    &f.ax_ev,
                    f.ev_hold,
                    f.ev_ds_diff,
                    f.ev_unaligned,
                    self.have_rears,
                );
                band.m.amb.insert(r_pan[0]);
                band.m.sdir.insert(r_pan[1]);
                if self.have_rears {
                    band.m.rdir.insert(r_pan[2]);
                }
                matrix_adj =
                    self.cfg.matrix_adj[0] * (1.0 - r_pan[3]) + self.cfg.matrix_adj[1] * r_pan[3];
            }

            // The effect is never switched off here: no fade.
            let cur_fade_mult = 1.0;
            let w = smoothstep(f.ax.cs * (-2.0 / FRAC_PI_4));
            let rs_scale = f.rs_scale;
            let wct = w * rs_scale;
            let sm_f = [
                self.cfg.surr_mult[0] * cur_fade_mult,
                self.cfg.surr_mult[1] * cur_fade_mult,
            ];
            let surr_mult_rear = rs_scale * sm_f[1] + (1.0 - rs_scale) * sm_f[0];
            let surr_mult = w * surr_mult_rear + (1.0 - w) * sm_f[0];
            let ct_pcf = self.cfg.contour_pwrcmp * f.pwrcmp_factor;
            let ct0 = wct + (1.0 - wct) * band.contour;
            let ct1 = (ct0 - 1.0) * ct_pcf + 1.0;
            let ct2 = ct0 / ct1;

            let ax_dpwr = if self.cfg.do_dpwr_decouple {
                f.ax_dpwr
            } else {
                f.ax
            };
            let p = CmcParams {
                surr_mult: [surr_mult * ct1, surr_mult_rear],
                rs: self.cfg.matrix_rs * rs_scale,
                adj: matrix_adj,
            };
            let (m, _) = calc_matrix_coefs(&f.ax, &ax_dpwr, &p, &[]);
            band.m.ll.insert(m.ll);
            band.m.lr.insert(m.lr);
            band.m.rl.insert(m.rl);
            band.m.rr.insert(m.rr);
            band.m.lsl.insert(m.lsl * ct2);
            band.m.lsr.insert(m.lsr * ct2);
            band.m.rsl.insert(m.rsl * ct2);
            band.m.rsr.insert(m.rsr * ct2);

            if self.have_rears {
                let rsw = smoothstep(f.ax.cs * (-2.0 / FRAC_PI_4) - 1.0) * rs_scale;
                let sg = rsw * band.rear_shelf + (1.0 - rsw);
                let s_norm = calc_norm_mult(sg);
                band.m.sg.insert(sg * s_norm);
                band.m.rg.insert(s_norm);
            }
            if band.xtc.is_some() {
                let ax_cs = f.ax.cs;
                let xg = (2.0
                    * if ax_cs >= 0.0 {
                        ax_cs
                    } else {
                        (rs_scale - 1.0) * ax_cs
                    })
                .sin();
                band.m.xg.insert(xg * cur_fade_mult);
            }
            if self.cfg.do_phase_flip {
                let pf_pos_rs = phase_flip_pos_rs(&f.ax, rs_scale);
                band.pf_ap_c0[0].insert(phase_flip_ap1_c0(&self.pf_params, 1.0 - pf_pos_rs));
                band.pf_ap_c0[1].insert(phase_flip_ap1_c0(&self.pf_params, pf_pos_rs));
            }
            if !self.syn_buf.is_empty() && f.ev_realign {
                band.syn_dt = f.align_syn_dt;
                band.align_syn.xfade_start(self.align_ref_syn, band.syn_dt);
                let align_xfade_frames = fs * (RISE_TIME_FAST * 1.183 / 1000.0) / f.ev_ds_diff;
                band.align_xfade_p =
                    align_xfade_frames.round_ties_even() as i64 + CS_INTERP_DELAY_FRAMES as i64;
                band.align_xfade_scale = 1.0 / align_xfade_frames;
            }
        }
    }
}

impl Effect for Matrix4Mb {
    fn name(&self) -> &'static str {
        "matrix4_mb"
    }
    fn istream(&self) -> Stream {
        self.istream
    }
    fn ostream(&self) -> Stream {
        self.ostream
    }
    fn buffer_frames(&self, frames: usize) -> usize {
        frames
    }
    fn ch_deps_identity(&self) -> bool {
        false
    }
    fn channel_selector(&self) -> Option<&[bool]> {
        Some(&self.selector)
    }
    fn opt_selector(&self) -> Option<&[bool]> {
        Some(&self.selector)
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf {
        let (ich, och) = (self.istream.channels, self.ostream.channels);
        let (c0, c1) = (self.cfg.c0, self.cfg.c1);
        let n_bands = self.n_bands;
        for i in 0..*frames {
            let (mut out_l, mut out_r) = (0.0, 0.0);
            let (mut out_ls, mut out_rs, mut out_lr, mut out_rr) = (0.0, 0.0, 0.0, 0.0);
            let (mut out_ls_dir, mut out_rs_dir, mut out_lr_dir, mut out_rr_dir) =
                (0.0, 0.0, 0.0, 0.0);
            let s_syn = [ibuf[i * ich + c0], ibuf[i * ich + c1]];
            let s_an = [
                self.fshape[0].run(ibuf[(i + 1) * ich - 2]),
                self.fshape[1].run(ibuf[(i + 1) * ich - 1]),
            ];
            self.fb.run_syn(s_syn);
            self.fb.run_an(s_an);
            self.s = if self.s + 1 >= DOWNSAMPLE_FACTOR {
                0
            } else {
                self.s + 1
            };

            let an_p = self.fb.an_p();
            for k in 0..n_bands {
                if an_p & self.fb.an_mask(k) == 0 {
                    let an = self.fb.an_s(k);
                    let band = &mut self.band[k];
                    let (mut s0a, mut s1a) = (an[0], an[1]);
                    if let Some(de) = band.de.as_mut() {
                        de.insert(an[0], an[1]);
                        if !self.syn_buf.is_empty() {
                            (s0a, s1a) = band.align_an.run(de);
                        }
                    }
                    let (env, pwr_env) = band.sm.calc_input_envs(s0a, s1a);
                    band.env = env;
                    band.pwr_env = pwr_env;
                }
            }

            if self.s == 0 {
                self.update();
            }

            let has_syn_buf = !self.syn_buf.is_empty();
            let mut syn_ref = 0;
            if has_syn_buf {
                let off = self.syn_buf_p * n_bands * 2;
                for k in 0..n_bands {
                    self.syn_buf[off + k] = self.fb.syn_s(0, k);
                    self.syn_buf[off + n_bands + k] = self.fb.syn_s(1, k);
                }
                syn_ref = self.syn_frame(self.align_ref_syn);
            }
            let s = self.s;
            for k in 0..n_bands {
                let (s0_d_fb, s1_d_fb, s0x_d_fb, s1x_d_fb);
                if has_syn_buf {
                    s0_d_fb = self.syn_buf[syn_ref + k];
                    s1_d_fb = self.syn_buf[syn_ref + n_bands + k];
                    let band = &mut self.band[k];
                    let mut t = 1.0;
                    if band.align_xfade_p > 0 {
                        let x = band.align_xfade_p as f64 * band.align_xfade_scale;
                        t = if x < 1.0 { 1.0 - x * x } else { 0.0 };
                        band.align_xfade_p -= 1;
                    }
                    (s1x_d_fb, s0x_d_fb) = self.align_syn_run(k, k, t);
                } else {
                    s0_d_fb = self.fb.syn_s(0, k);
                    s0x_d_fb = s0_d_fb;
                    s1_d_fb = self.fb.syn_s(1, k);
                    s1x_d_fb = s1_d_fb;
                }
                let band = &mut self.band[k];
                let m = &band.m;
                let mut b_l = s0_d_fb * m.ll.at(s) + s1x_d_fb * m.lr.at(s);
                let mut b_r = s0x_d_fb * m.rl.at(s) + s1_d_fb * m.rr.at(s);
                let b_ls = s0_d_fb * m.lsl.at(s) + s1x_d_fb * m.lsr.at(s);
                let b_rs = s0x_d_fb * m.rsl.at(s) + s1_d_fb * m.rsr.at(s);

                if let Some(xtc) = band.xtc.as_mut() {
                    xtc.run(band.m.xg.at(s), &mut b_l, &mut b_r);
                }
                out_l += b_l;
                out_r += b_r;
                let (mut b_ls_pf, mut b_rs_pf) = (b_ls, b_rs);
                if self.cfg.do_phase_flip {
                    band.pf_ap[0].c0 = band.pf_ap_c0[0].at(s);
                    band.pf_ap[1].c0 = band.pf_ap_c0[1].at(s);
                    b_ls_pf = band.pf_ap[0].run(b_ls_pf + 1e-15) - 1e-15;
                    b_rs_pf = band.pf_ap[1].run(b_rs_pf + 1e-15) - 1e-15;
                }
                let (b_lr, b_rr) = (b_ls, b_rs);
                let (mut b_lr_pf, mut b_rr_pf) = (0.0, 0.0);
                if self.have_rears {
                    let m_sg = band.m.sg.at(s);
                    let m_rg = band.m.rg.at(s);
                    b_lr_pf = b_ls_pf * m_rg;
                    b_rr_pf = b_rs_pf * m_rg;
                    b_ls_pf *= m_sg;
                    b_rs_pf *= m_sg;
                }
                if self.cfg.dp_mode != DirectPathMode::None {
                    let m_amb = band.m.amb.at(s);
                    let m_sdir = band.m.sdir.at(s);
                    out_ls += b_ls_pf * m_amb;
                    out_rs += b_rs_pf * m_amb;
                    out_ls_dir += b_ls * m_sdir;
                    out_rs_dir -= b_rs * m_sdir;
                    if self.have_rears {
                        let m_rdir = band.m.rdir.at(s);
                        out_lr += b_lr_pf * m_amb;
                        out_rr += b_rr_pf * m_amb;
                        out_lr_dir += b_lr * m_rdir;
                        out_rr_dir -= b_rr * m_rdir;
                    }
                } else {
                    out_ls += b_ls_pf;
                    out_rs += b_rs_pf;
                    out_lr += b_lr_pf;
                    out_rr += b_rr_pf;
                }
            }

            let o = &mut obuf[i * och..(i + 1) * och];
            for k in 0..ich - 2 {
                o[k] = if k == c0 {
                    out_l
                } else if k == c1 {
                    out_r
                } else {
                    ibuf[i * ich + k]
                };
            }
            let o = &mut o[ich - 2..];
            o[0] = out_ls;
            o[1] = out_rs;
            if self.have_rears {
                o[2] = out_lr;
                o[3] = out_rr;
                if self.cfg.dp_mode != DirectPathMode::None {
                    o[4] = out_ls_dir;
                    o[5] = out_rs_dir;
                    o[6] = out_lr_dir;
                    o[7] = out_rr_dir;
                }
            } else if self.cfg.dp_mode != DirectPathMode::None {
                o[2] = out_ls_dir;
                o[3] = out_rs_dir;
            }

            if self.syn_buf_len > 0 {
                self.syn_buf_p = if self.syn_buf_p + 1 < self.syn_buf_len {
                    self.syn_buf_p + 1
                } else {
                    0
                };
            }
        }
        Buf::Output
    }
    fn reset(&mut self) {
        self.fb.reset();
        if !self.syn_buf.is_empty() {
            self.syn_buf.fill(0.0);
            for b in &mut self.band {
                b.align_xfade_p = 0;
            }
        }
    }
    fn channel_offsets(&self, latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        let (c0, c1) = (self.cfg.c0, self.cfg.c1);
        let l = self.latency_frames + self.align_latency_frames;
        latency[c0] += l;
        latency[c1] += l;
        req_delay[c0] += self.latency_frames;
        req_delay[c1] += self.latency_frames;
        for i in self.istream.channels - 2..self.ostream.channels {
            latency[i] += l;
            req_delay[i] += self.latency_frames;
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        samples[self.cfg.c0] += self.drain_frames;
        samples[self.cfg.c1] += self.drain_frames;
        for d in samples
            .iter_mut()
            .take(self.ostream.channels)
            .skip(self.istream.channels - 2)
        {
            *d += self.drain_frames;
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
    let name = info.name;
    let selector = crate::effect::default_selector(selector, istream.channels);
    let cfg = config_init(info, istream, &selector, argv, true)?;
    let fs = f64::from(istream.fs);
    let mut fbp: &FilterBankParams = &FB_PARAMS[0];
    if !cfg.fb_id.is_empty() {
        fbp = FB_PARAMS
            .iter()
            .find(|p| p.id_str == cfg.fb_id)
            .ok_or_else(|| {
                Error::Message(format!(
                    "{name}: error: unknown filter bank id: {}",
                    cfg.fb_id
                ))
            })?;
    }
    if istream.fs < fbp.min_fs {
        return Err(Error::Range {
            effect: name.into(),
            param: "input sample rate".into(),
        });
    }
    let n_in = istream.channels;
    let have_dp = cfg.dp_mode != DirectPathMode::None;
    let ns = cfg.layout.ns() * if have_dp { 2 } else { 1 };
    let mb_istream = Stream {
        fs: istream.fs,
        channels: n_in + 2,
    };
    let ostream = Stream {
        fs: istream.fs,
        channels: n_in - 2 + cfg.layout.nf() + ns,
    };
    let n_bands = fbp.n_bands;
    let ds_fs = fs / DOWNSAMPLE_FACTOR as f64;

    let mut fb = FilterBank::new(fbp, fs, cfg.fb_type, cfg.fb_stop)
        .ok_or_else(|| Error::Message(format!("{name}: BUG: failed to initialize filter bank")))?;
    let uev = cfg.uev_action;
    let mut max_syn_dt = 0i32;
    let mut syn_buf = Vec::new();
    let mut syn_buf_len = 0usize;
    let mut align_ref_syn = 0;
    if uev == UnalignedEventAction::Align {
        max_syn_dt = (DELAY_EST_MAX_DELTA / 1000.0 * fs).ceil() as i32;
        align_ref_syn = max_syn_dt + CH_ALIGN_LATENCY;
        syn_buf_len = (max_syn_dt * 2 + CH_ALIGN_LATENCY) as usize;
        syn_buf = vec![0.0; syn_buf_len * n_bands * 2];
    }
    let have_syn_buf = !syn_buf.is_empty();
    let pf_params = phase_flip_params(fs);
    let ms_per_frame = 1000.0 / fs;
    let syn_extra = |k: usize| {
        if have_syn_buf {
            f64::from(fb.an_mask(k) + 1) * ms_per_frame
        } else {
            0.0
        }
    };
    let an_tpeak_ref = fbp.an_tpeak[0] + syn_extra(0);

    let shelf_mult2 = cfg.shelf_mult * cfg.shelf_mult;
    let shelf_f02 = cfg.shelf_f0 * cfg.shelf_f0;
    let lowpass_f02 = cfg.lowpass_f0 * cfg.lowpass_f0;
    let mut band = Vec::with_capacity(n_bands);
    for k in 0..n_bands {
        let fc = fbp.fc[k];
        let an_fs = fs / f64::from(fb.an_mask(k) + 1);
        let de = if uev != UnalignedEventAction::None {
            Some(DelayEst::new(
                an_fs,
                fc,
                fbp.fdiv(k) - fbp.fdiv_lo(k),
                (2.0 - 0.241 * (fc.ln() - 3.4)).clamp(0.5, 2.0),
                ds_fs,
            ))
        } else {
            None
        };
        let thresh_scale = if fbp.thresh_scale[k] > 0.0 {
            fbp.thresh_scale[k]
        } else {
            1.0
        };
        let ev_thresh_max = EVENT_THRESH_MAX * thresh_scale;
        let ev_thresh_min = EVENT_THRESH_MIN * thresh_scale;
        let ev = EventState::new(
            ds_fs,
            ev_thresh_max * (1.0 / EVENT_THRESH),
            gauss_mag(BASE_ORD_NOTCH_SCALE_F0, fc, false),
        );
        let mut ev_thresh = Ewma::new(ds_fs, rise_time(EVENT_SAMPLE_TIME));
        ev_thresh.set(ev_thresh_max);
        let an_tpeak = fbp.an_tpeak[k] + syn_extra(k);
        let ax_buf_len = time_to_frames((an_tpeak_ref - an_tpeak) * 0.7, ds_fs);
        let ax_buf = if ax_buf_len > 0 {
            vec![
                AxFrame {
                    pwrcmp_factor: ev.pwrcmp_factor.last(),
                    rs_scale: ev.rs_scale.last(),
                    ..AxFrame::default()
                };
                ax_buf_len as usize
            ]
        } else {
            Vec::new()
        };
        let ax_f = AxFrame::default();
        let pf_pos_rs = phase_flip_pos_rs(&ax_f.ax, ev.rs_scale.last());
        let mut pf_ap_c0 = [CsInterp::default(); 2];
        pf_ap_c0[0].set(phase_flip_ap1_c0(&pf_params, 1.0 - pf_pos_rs));
        pf_ap_c0[1].set(phase_flip_ap1_c0(&pf_params, pf_pos_rs));
        let mut m = Interp::default();
        m.amb.set(1.0);
        m.sdir.set(0.0);
        m.rdir.set(0.0);
        let xtc = if cfg.do_xtc {
            Xtc::new(
                cfg.xtc_itd
                    * 1e-6
                    * fs
                    * (1.0 + smoothstep(1.0 - (fc.ln() - 5.704) * XTC_LOG10_E) * 0.47),
                cfg.xtc_gain
                    * gauss_mag(cfg.xtc_bp[0], fc, true)
                    * gauss_mag(cfg.xtc_bp[1], fc, false),
            )
        } else {
            None
        };
        let contour = {
            let fc2 = fc * fc;
            let shelf_norm_f2 = fc2 / shelf_f02;
            let mut c = ((1.0 + shelf_mult2 * shelf_norm_f2) / (1.0 + shelf_norm_f2)).sqrt();
            if lowpass_f02 > 0.0 {
                let lowpass_norm_f2 = fc2 / lowpass_f02;
                c *= (1.0 / (1.0 + lowpass_norm_f2)).powf(cfg.lowpass_order / 2.0);
            }
            c
        };
        let rear_shelf = {
            let shelf_norm_f2 = fc * fc / shelf_f02;
            ((1.0 + 0.1 * shelf_norm_f2) / (1.0 + shelf_norm_f2)).sqrt()
        };
        band.push(Band {
            sm: SmoothState::new(an_fs),
            env: Envs::default(),
            pwr_env: Envs::default(),
            de,
            ev,
            ax_f,
            align_an: if have_syn_buf {
                AlignAn::new()
            } else {
                AlignAn::default()
            },
            align_syn: if have_syn_buf {
                AlignSyn::new(align_ref_syn)
            } else {
                AlignSyn::default()
            },
            m,
            pf_ap_c0,
            pf_ap: [Ap1::default(); 2],
            dp: DirectPath::new(ds_fs, cfg.dp_mode),
            ev_thresh,
            xtc,
            ax_buf,
            ax_buf_len,
            ax_buf_p: 0,
            ev_thresh_max,
            ev_thresh_min,
            contour,
            rear_shelf,
            syn_dt: 0.0,
            syn_tol_r: (fc * 8.0) / fs, // ±45°; for event threshold modulation
            align_xfade_scale: 0.0,
            align_xfade_p: 0,
        });
    }

    // The linear-phase FIR that undoes the phase shift of the sum of the bands: the impulse
    // response of the all-pass chain run backwards, truncated where it has died away.
    let mut phase_lin_frames = time_to_frames(fbp.fir_len, fs).max(1) as usize;
    let mut filter = vec![0.0; phase_lin_frames];
    filter[phase_lin_frames - 1] = 1.0;
    for i in (0..phase_lin_frames).rev() {
        filter[i] = fb.sum(filter[i]);
    }
    let mut zx = 0;
    let mut integ = filter[0].abs();
    let trunc_thresh = PHASE_LIN_TRUNC_THRESH * PHASE_LIN_TRUNC_THRESH * fs;
    let mut k = 1;
    while integ < trunc_thresh && k < phase_lin_frames {
        if filter[k].is_sign_negative() != filter[k - 1].is_sign_negative() {
            zx = k;
            integ = 0.0;
        }
        integ += filter[k].abs();
        k += 1;
    }
    phase_lin_frames -= zx;
    fb.reset();

    let mut mb_selector = vec![false; n_in + 2];
    mb_selector[cfg.c0] = true;
    mb_selector[cfg.c1] = true;
    static FIR_INFO: EffectInfo = EffectInfo {
        name: "fir (matrix4_mb)",
        usage: "",
        init,
        number: 0,
    };
    static FIR_P_INFO: EffectInfo = EffectInfo {
        name: "fir_p (matrix4_mb)",
        usage: "",
        init,
        number: 0,
    };
    let fir_filter = Filter {
        data: &filter[zx..],
        channels: 1,
        frames: phase_lin_frames,
        reference: phase_lin_frames as isize - 1,
    };
    let e_fir = if cfg.use_fir_p {
        fir_p_effect(&FIR_P_INFO, &mb_istream, &mb_selector, &fir_filter, 0)?
    } else {
        fir_effect(&FIR_INFO, &mb_istream, &mb_selector, &fir_filter, false)?
    };

    let lookahead_frames = cfg.lookahead_frames
        + CS_INTERP_DELAY_FRAMES
        + band[n_bands - 1].ax_buf_len * DOWNSAMPLE_FACTOR as isize
        - max_syn_dt as isize;
    let e_input = Input {
        istream: *istream,
        ostream: mb_istream,
        selector: selector.clone(),
        c0: cfg.c0,
        c1: cfg.c1,
        lookahead_frames,
    };

    // the analysis inputs
    mb_selector[n_in] = true;
    mb_selector[n_in + 1] = true;
    let fir_latency = phase_lin_frames as isize - 1;
    let mut drain_frames = (lookahead_frames - fir_latency).max(0);
    let latency_frames = lookahead_frames.max(fir_latency);
    let mut align_latency_frames = 0;
    if have_syn_buf {
        align_latency_frames = (max_syn_dt + CH_ALIGN_LATENCY) as isize;
        drain_frames += align_latency_frames;
    }
    let evc = EventConfig::new(ds_fs, cfg.rear_ev_mask, DIFF_OVERSHOOT, uev);
    let fshape = [FShape::new(fs), FShape::new(fs)];
    let _: FilterType = cfg.fb_type;
    let mb = Matrix4Mb {
        istream: mb_istream,
        ostream,
        selector: mb_selector,
        s: 0,
        n_bands,
        have_rears: cfg.layout.ns() >= 4,
        fshape,
        fb,
        band,
        evc,
        de_hist: DelayEstHist::new(n_bands),
        pf_params,
        syn_buf,
        syn_buf_p: 0,
        syn_buf_len,
        align_ref_syn,
        drain_frames,
        latency_frames,
        align_latency_frames,
        cfg,
    };
    Ok(vec![Box::new(e_input), e_fir, Box::new(mb)])
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    #[test]
    fn the_layouts_set_the_number_of_output_channels() {
        let o = |a: &[&str]| Chain::from_args(S, a).unwrap().ostream().channels;
        assert_eq!(o(&["matrix4_mb"]), 4);
        assert_eq!(o(&["matrix4_mb", "layout=2/4"]), 6);
        assert_eq!(o(&["matrix4_mb", "layout=2/4,direct_path"]), 10);
    }

    #[test]
    fn options_are_checked() {
        assert!(Chain::from_args(S, &["matrix4_mb", "bogus"]).is_err());
        assert!(Chain::from_args(S, &["matrix4_mb", "filter_bank=nope"]).is_err());
        assert!(Chain::from_args(S, &["matrix4_mb", "filter_type=sinc"]).is_err());
        assert!(Chain::from_args(S, &["matrix4_mb", "unaligned_event_action=maybe"]).is_err());
        assert!(Chain::from_args(S, &["matrix4_mb", "xtc=-30"]).is_err());
        assert!(Chain::from_args(
            Stream {
                fs: 32_000,
                channels: 2
            },
            &["matrix4_mb"]
        )
        .is_err());
        assert!(Chain::from_args(
            Stream {
                fs: 32_000,
                channels: 2
            },
            &["matrix4_mb", "filter_bank=13band"]
        )
        .is_ok());
    }

    #[test]
    fn silence_stays_silent_and_a_centre_signal_stays_in_front() {
        let mut c = Chain::from_args(S, &["matrix4_mb", "filter_bank=13band"]).unwrap();
        let y = c.process(&vec![0.0; 2 * 4000]);
        assert!(y.iter().all(|v| v.abs() < 1e-9));
        let x: Vec<f64> = (0..2 * 48_000)
            .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * (i / 2) as f64 / 48_000.0).sin())
            .collect();
        let mut c = Chain::from_args(S, &["matrix4_mb", "filter_bank=13band"]).unwrap();
        let y = c.process(&x);
        let tail = &y[4 * 30_000..4 * 40_000];
        let rms = |ch: usize| {
            (tail.iter().skip(ch).step_by(4).map(|v| v * v).sum::<f64>() / 10_000.0).sqrt()
        };
        assert!(rms(0) > 0.2 && rms(1) > 0.2, "{} {}", rms(0), rms(1));
        assert!(rms(2) < rms(0) && rms(3) < rms(1));
    }
}
