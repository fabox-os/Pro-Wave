//! `matrix4` (`matrix4.c`): an active matrix decoder that steers a stereo pair into front
//! and surround channels.
//!
//! It follows where in the stereo image the sound is and how it moves (see
//! [`crate::matrix4_common`]), looks ahead by `lookahead` so a matrix change can start just
//! before an event does, and writes the front pair back in place of the input pair and the
//! surround channels after the last input channel: LS RS (and LR RR with the 2/4 layout),
//! followed by the direct-path channels when `direct_path` is on.
//!
//! Logic Pro equivalent: Surround Panner / Direction Mixer upmixing; Logic has no
//! steering-logic decoder.

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_4, SQRT_2};

use crate::allpass::Ap1;
use crate::biquad::{Biquad, Width};
use crate::effect::{Buf, Effect, EffectInfo, Stream};
use crate::error::Result;
use crate::matrix4_common::{
    calc_matrix_coefs, calc_norm_mult, config_init, phase_flip_ap1_c0, phase_flip_params,
    phase_flip_pos_rs, smoothstep, Axes, CmcParams, Config, CsInterp, DirectPath, DirectPathMode,
    DynShelf, EventConfig, EventState, SmoothState, ACCOM_TIME, BASE_ORD_NOTCH_SCALE,
    CS_INTERP_DELAY_FRAMES, DIFF_OVERSHOOT, DOWNSAMPLE_FACTOR, NORM_ACCOM_FACTOR,
};
use crate::smoothing::{rise_time, Smf};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "matrix4",
    usage: "[options ...] [surround_level][/surround_level_rear]",
    init,
    number: 0,
}];

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
    g_surr_shelf: CsInterp,
    g_surr_lp: CsInterp,
    g_front_shelf: CsInterp,
    g_front_lp: CsInterp,
    g_side_shelf: CsInterp,
    g_rear_shelf: CsInterp,
    amb: CsInterp,
    sdir: CsInterp,
    rdir: CsInterp,
}

struct Matrix4 {
    istream: Stream,
    ostream: Stream,
    selector: Vec<bool>,
    cfg: Config,
    s: usize,
    have_rears: bool,
    bufs: [Vec<f64>; 2],
    in_hp: [Biquad; 2],
    in_lp: [Biquad; 2],
    surr_shelf: [DynShelf; 2],
    surr_lp: [DynShelf; 2],
    front_shelf: [DynShelf; 2],
    front_lp: [DynShelf; 2],
    sr_shelf: [DynShelf; 4],
    sm: SmoothState,
    ev: EventState,
    evc: EventConfig,
    ax: Axes,
    ax_ev: Axes,
    ax_dpwr: Axes,
    bg_cs: Smf,
    m: Interp,
    pf_ap_c0: [CsInterp; 2],
    pf_ap: [Ap1; 2],
    pf_params: [f64; 2],
    dp: DirectPath,
    shelf_mult: f64,
    lowpass_mult: f64,
    len: usize,
    p: usize,
}

impl Matrix4 {
    /// Everything that happens once every `DOWNSAMPLE_FACTOR` samples.
    fn update(&mut self, env: &crate::matrix4_common::Envs, pwr_env: &crate::matrix4_common::Envs) {
        self.ev.process(
            &self.evc,
            env,
            pwr_env,
            NORM_ACCOM_FACTOR,
            1.0,
            &mut self.ax,
            &mut self.ax_ev,
            &mut self.ax_dpwr,
        );
        let mut matrix_adj = self.cfg.matrix_adj[0];
        if self.cfg.dp_mode != DirectPathMode::None {
            let r_pan = self.dp.pan(
                &self.ax,
                &self.ax_ev,
                self.ev.t_hold != 0,
                self.ev.ds_diff,
                self.ev.unaligned,
                self.have_rears,
            );
            self.m.amb.insert(r_pan[0]);
            self.m.sdir.insert(r_pan[1]);
            if self.have_rears {
                self.m.rdir.insert(r_pan[2]);
            }
            matrix_adj =
                self.cfg.matrix_adj[0] * (1.0 - r_pan[3]) + self.cfg.matrix_adj[1] * r_pan[3];
        }
        // The effect is never switched off here: no fade.
        let cur_fade_mult = 1.0;
        let w_step = smoothstep(self.ax.cs * (-2.0 / FRAC_PI_4));
        let w = self.bg_cs.run_asym(w_step + 1.0) - 1.0;
        let rs_scale = self.ev.rs_scale.last();
        let wct = w * rs_scale;
        let sm_f = [
            self.cfg.surr_mult[0] * cur_fade_mult,
            self.cfg.surr_mult[1] * cur_fade_mult,
        ];
        let surr_mult_rear = rs_scale * sm_f[1] + (1.0 - rs_scale) * sm_f[0];
        let surr_mult = w * surr_mult_rear + (1.0 - w) * sm_f[0];
        let ct_pcf = self.cfg.contour_pwrcmp * self.ev.pwrcmp_factor.last();
        let shelf_ct0 = wct + (1.0 - wct) * self.shelf_mult;
        let shelf_ct1 = (shelf_ct0 - 1.0) * ct_pcf + 1.0;
        let lp_ct0 = wct + (1.0 - wct) * self.lowpass_mult;
        let lp_ct1 = (lp_ct0 - 1.0) * ct_pcf.powf(1.0 / self.shelf_mult) + 1.0;

        let ax_dpwr = if self.cfg.do_dpwr_decouple {
            self.ax_dpwr
        } else {
            self.ax
        };
        let p = CmcParams {
            surr_mult: [surr_mult, surr_mult_rear],
            rs: self.cfg.matrix_rs * rs_scale,
            adj: matrix_adj,
        };
        let args = [surr_mult * shelf_ct1, surr_mult * shelf_ct1 * lp_ct1];
        let (m, shelf) = calc_matrix_coefs(&self.ax, &ax_dpwr, &p, &args);

        self.m.ll.insert(m.ll);
        self.m.lr.insert(m.lr);
        self.m.rl.insert(m.rl);
        self.m.rr.insert(m.rr);
        self.m.lsl.insert(m.lsl);
        self.m.lsr.insert(m.lsr);
        self.m.rsl.insert(m.rsl);
        self.m.rsr.insert(m.rsr);

        self.m
            .g_surr_shelf
            .insert(shelf_ct0 / shelf_ct1 * shelf[0].1);
        self.m
            .g_surr_lp
            .insert(lp_ct0 / lp_ct1 * shelf[1].1 / shelf[0].1.max(f64::MIN_POSITIVE));
        self.m.g_front_shelf.insert(shelf[0].0);
        self.m.g_front_lp.insert(shelf[1].0 / shelf[0].0);

        if self.have_rears {
            let rsw = smoothstep(self.ax.cs * (-2.0 / FRAC_PI_4) - 1.0) * rs_scale;
            let sg = rsw * 0.316228 + (1.0 - rsw);
            let s_norm = calc_norm_mult(sg) * SQRT_2;
            self.m.g_side_shelf.insert(sg * s_norm);
            self.m.g_rear_shelf.insert(s_norm);
        }
        if self.cfg.do_phase_flip {
            let pf_pos_rs = phase_flip_pos_rs(&self.ax, rs_scale);
            self.pf_ap_c0[0].insert(phase_flip_ap1_c0(&self.pf_params, 1.0 - pf_pos_rs));
            self.pf_ap_c0[1].insert(phase_flip_ap1_c0(&self.pf_params, pf_pos_rs));
        }
    }
}

impl Effect for Matrix4 {
    fn name(&self) -> &'static str {
        "matrix4"
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
        for i in 0..*frames {
            let s0 = ibuf[i * ich + c0];
            let s1 = ibuf[i * ich + c1];
            let s0_bp = self.in_lp[0].run(self.in_hp[0].run(s0));
            let s1_bp = self.in_lp[1].run(self.in_hp[1].run(s1));
            let (env, pwr_env) = self.sm.calc_input_envs(s0_bp, s1_bp);

            self.s = if self.s + 1 >= DOWNSAMPLE_FACTOR {
                0
            } else {
                self.s + 1
            };
            if self.s == 0 {
                self.update(&env, &pwr_env);
            }

            let s = self.s;
            let (s0_d, s1_d) = (self.bufs[0][self.p], self.bufs[1][self.p]);
            let m = &self.m;
            let mut out_l = s0_d * m.ll.at(s) + s1_d * m.lr.at(s);
            let mut out_r = s0_d * m.rl.at(s) + s1_d * m.rr.at(s);
            let mut out_ls = s0_d * m.lsl.at(s) + s1_d * m.lsr.at(s) + 1e-15;
            let mut out_rs = s0_d * m.rsl.at(s) + s1_d * m.rsr.at(s) + 1e-15;

            if self.shelf_mult != 1.0 {
                let g_surr_shelf = m.g_surr_shelf.at(s);
                let g_front_shelf = m.g_front_shelf.at(s);
                out_l = self.front_shelf[0].run(out_l, g_front_shelf);
                out_r = self.front_shelf[1].run(out_r, g_front_shelf);
                out_ls = self.surr_shelf[0].run(out_ls, g_surr_shelf);
                out_rs = self.surr_shelf[1].run(out_rs, g_surr_shelf);
            }
            if self.lowpass_mult != 1.0 {
                let g_surr_lp = m.g_surr_lp.at(s);
                let g_front_lp = m.g_front_lp.at(s);
                out_l = self.front_lp[0].run(out_l, g_front_lp);
                out_r = self.front_lp[1].run(out_r, g_front_lp);
                out_ls = self.surr_lp[0].run(out_ls, g_surr_lp);
                out_rs = self.surr_lp[1].run(out_rs, g_surr_lp);
            }
            let (mut out_ls_pf, mut out_rs_pf) = (out_ls, out_rs);
            if self.cfg.do_phase_flip {
                self.pf_ap[0].c0 = self.pf_ap_c0[0].at(s);
                self.pf_ap[1].c0 = self.pf_ap_c0[1].at(s);
                out_ls_pf = self.pf_ap[0].run(out_ls_pf);
                out_rs_pf = self.pf_ap[1].run(out_rs_pf);
            }

            let o = &mut obuf[i * och..(i + 1) * och];
            for k in 0..ich {
                o[k] = if k == c0 {
                    out_l
                } else if k == c1 {
                    out_r
                } else {
                    ibuf[i * ich + k]
                };
            }
            let o = &mut o[ich..];
            out_ls -= 1e-15;
            out_rs -= 1e-15;
            let (out_lr, out_rr) = (out_ls, out_rs);
            let (mut out_lr_pf, mut out_rr_pf) = (0.0, 0.0);
            if self.have_rears {
                let m_gss = self.m.g_side_shelf.at(s);
                let m_grs = self.m.g_rear_shelf.at(s);
                out_lr_pf = (self.sr_shelf[2].run(out_ls_pf, m_grs) - 1e-15) * FRAC_1_SQRT_2;
                out_rr_pf = (self.sr_shelf[3].run(out_rs_pf, m_grs) - 1e-15) * FRAC_1_SQRT_2;
                out_ls_pf = (self.sr_shelf[0].run(out_ls_pf, m_gss) - 1e-15) * FRAC_1_SQRT_2;
                out_rs_pf = (self.sr_shelf[1].run(out_rs_pf, m_gss) - 1e-15) * FRAC_1_SQRT_2;
            } else {
                out_ls_pf -= 1e-15;
                out_rs_pf -= 1e-15;
            }
            if self.cfg.dp_mode != DirectPathMode::None {
                let m_amb = self.m.amb.at(s);
                let m_sdir = self.m.sdir.at(s);
                o[0] = out_ls_pf * m_amb;
                o[1] = out_rs_pf * m_amb;
                if self.have_rears {
                    let m_rdir = self.m.rdir.at(s);
                    o[2] = out_lr_pf * m_amb;
                    o[3] = out_rr_pf * m_amb;
                    o[4] = out_ls * m_sdir;
                    o[5] = -out_rs * m_sdir;
                    o[6] = out_lr * m_rdir;
                    o[7] = -out_rr * m_rdir;
                } else {
                    o[2] = out_ls * m_sdir;
                    o[3] = -out_rs * m_sdir;
                }
            } else {
                o[0] = out_ls_pf;
                o[1] = out_rs_pf;
                if self.have_rears {
                    o[2] = out_lr_pf;
                    o[3] = out_rr_pf;
                }
            }

            self.bufs[0][self.p] = s0;
            self.bufs[1][self.p] = s1;
            self.p = if self.p + 1 < self.len { self.p + 1 } else { 0 };
        }
        Buf::Output
    }
    fn reset(&mut self) {
        self.p = 0;
        self.bufs[0].fill(0.0);
        self.bufs[1].fill(0.0);
    }
    fn channel_offsets(&self, latency: &mut [isize], _req_delay: &mut [isize]) -> bool {
        let len = self.len as isize;
        latency[self.cfg.c0] += len;
        latency[self.cfg.c1] += len;
        for l in latency
            .iter_mut()
            .take(self.ostream.channels)
            .skip(self.istream.channels)
        {
            *l += len;
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        let len = self.len as isize;
        samples[self.cfg.c0] += len;
        samples[self.cfg.c1] += len;
        for d in samples
            .iter_mut()
            .take(self.ostream.channels)
            .skip(self.istream.channels)
        {
            *d += len;
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
    let selector = crate::effect::default_selector(selector, istream.channels);
    let cfg = config_init(info, istream, &selector, argv)?;
    let fs = f64::from(istream.fs);
    let ds_fs = fs / DOWNSAMPLE_FACTOR as f64;
    let ns = cfg.layout.ns()
        * if cfg.dp_mode != DirectPathMode::None {
            2
        } else {
            1
        };
    let ostream = Stream {
        fs: istream.fs,
        channels: istream.channels - 2 + cfg.layout.nf() + ns,
    };
    let hp = || Biquad::from_type(crate::biquad::HIGHPASS, fs, 500.0, 0.5, 0.0, 0.0, Width::Q);
    let lp = || Biquad::from_type(crate::biquad::LOWPASS, fs, 5000.0, 0.5, 0.0, 0.0, Width::Q);
    let mut bg_cs = Smf::new_asym(ds_fs, rise_time(ACCOM_TIME * 2.0), 0.01, 1e-6);
    bg_cs.set(1.0);
    let mut m = Interp::default();
    m.amb.set(1.0);
    m.sdir.set(0.0);
    m.rdir.set(0.0);
    let ev = EventState::new(ds_fs, 1.0, BASE_ORD_NOTCH_SCALE);
    let pf_params = phase_flip_params(fs);
    let ax = Axes::default();
    let pf_pos_rs = phase_flip_pos_rs(&ax, ev.rs_scale.last());
    let mut pf_ap_c0 = [CsInterp::default(); 2];
    pf_ap_c0[0].set(phase_flip_ap1_c0(&pf_params, 1.0 - pf_pos_rs));
    pf_ap_c0[1].set(phase_flip_ap1_c0(&pf_params, pf_pos_rs));
    let len = (cfg.lookahead_frames + CS_INTERP_DELAY_FRAMES).max(1) as usize;
    let lowpass_mult = if cfg.lowpass_f0 > 0.0 {
        let lp_f = (fs + cfg.lowpass_f0) / 2.0;
        (1.0 / (1.0 + (lp_f * lp_f / (cfg.lowpass_f0 * cfg.lowpass_f0))))
            .powf(cfg.lowpass_order / 2.0)
    } else {
        1.0
    };
    let sh = |f0: f64| DynShelf::new(fs, f0);
    Ok(vec![Box::new(Matrix4 {
        istream: *istream,
        ostream,
        selector,
        s: 0,
        have_rears: cfg.layout.ns() >= 4,
        bufs: [vec![0.0; len], vec![0.0; len]],
        in_hp: [hp(), hp()],
        in_lp: [lp(), lp()],
        surr_shelf: [sh(cfg.shelf_f0); 2],
        surr_lp: [sh(cfg.lowpass_f0); 2],
        front_shelf: [sh(cfg.shelf_f0); 2],
        front_lp: [sh(cfg.lowpass_f0); 2],
        sr_shelf: [sh(cfg.shelf_f0); 4],
        sm: SmoothState::new(fs),
        ev,
        evc: EventConfig::new(ds_fs, cfg.rear_ev_mask, DIFF_OVERSHOOT),
        ax,
        ax_ev: Axes::default(),
        ax_dpwr: Axes::default(),
        bg_cs,
        m,
        pf_ap_c0,
        pf_ap: [Ap1::default(), Ap1::default()],
        pf_params,
        dp: DirectPath::new(ds_fs, cfg.dp_mode),
        shelf_mult: cfg.shelf_mult,
        lowpass_mult,
        len,
        p: 0,
        cfg,
    })])
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
        assert_eq!(
            Chain::from_args(S, &["matrix4"])
                .unwrap()
                .ostream()
                .channels,
            4
        );
        let c = Chain::from_args(S, &["matrix4", "layout=2/4"]).unwrap();
        assert_eq!(c.ostream().channels, 6);
        let c = Chain::from_args(S, &["matrix4", "layout=2/4,direct_path"]).unwrap();
        assert_eq!(c.ostream().channels, 10);
        let c = Chain::from_args(S, &["matrix4", "direct_path=static"]).unwrap();
        assert_eq!(c.ostream().channels, 6);
    }

    #[test]
    fn silence_stays_silent_and_the_signal_is_delayed_by_the_lookahead() {
        let mut c = Chain::from_args(S, &["matrix4"]).unwrap();
        let y = c.process(&vec![0.0; 2 * 3000]);
        assert!(y.iter().all(|v| v.abs() < 1e-9));
        // A mono (centre) signal comes out of the front pair, not the surrounds.
        let x: Vec<f64> = (0..2 * 48_000)
            .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * (i / 2) as f64 / 48_000.0).sin())
            .collect();
        let mut c = Chain::from_args(S, &["matrix4"]).unwrap();
        let y = c.process(&x);
        let tail = &y[4 * 30_000..4 * 40_000];
        let rms = |ch: usize| {
            (tail.iter().skip(ch).step_by(4).map(|v| v * v).sum::<f64>() / 10_000.0).sqrt()
        };
        assert!(rms(0) > 0.2 && rms(1) > 0.2, "{} {}", rms(0), rms(1));
        assert!(rms(2) < rms(0) && rms(3) < rms(1));
    }

    #[test]
    fn options_are_checked() {
        assert!(Chain::from_args(S, &["matrix4", "bogus"]).is_err());
        assert!(Chain::from_args(S, &["matrix4", "layout=3/3"]).is_err());
        assert!(Chain::from_args(S, &["matrix4", "shelf=-3:50"]).is_err());
        assert!(Chain::from_args(S, &["matrix4", "lowpass=4k:3"]).is_err());
        assert!(Chain::from_args(S, &["matrix4", "-3/-2", "matrix=v3"]).is_err());
        assert!(Chain::from_args(S, &["matrix4", "phase_flip=maybe"]).is_err());
        assert!(Chain::from_args(
            Stream {
                fs: 16_000,
                channels: 2
            },
            &["matrix4"]
        )
        .is_err());
        assert!(Chain::from_args(
            Stream {
                fs: 48_000,
                channels: 3
            },
            &["matrix4"]
        )
        .is_err());
        assert!(Chain::from_args(S, &["matrix4", "-4/-1",]).is_ok());
    }
}
