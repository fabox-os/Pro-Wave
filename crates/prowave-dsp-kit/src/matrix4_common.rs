//! What the matrix decoder shares (`matrix4_common.c` and `.h` of the original): the event
//! detector that tells how the stereo image moves, the steering angles, the matrix
//! coefficients, the direct path and the options.
//!
//! The decoder steers a 2-channel signal into front and surround channels. It works on the
//! band 500–5000 Hz of the input, follows the level differences between left and right
//! (`lr`) and between sum and difference (`cs`) as angles, spots events (onsets) by the rise
//! of the smoothed power and holds the direction an event had for up to 200 ms.
#![allow(clippy::manual_clamp)] // min(max(x, 0), hi) is what the original does with NaN

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_2_PI, FRAC_PI_2, FRAC_PI_4, PI, SQRT_2};

use crate::args::{get_freq, strtod};
use crate::biquad::{Biquad, Width};
use crate::capn::FilterType;
use crate::delay_est::{DelayEst, DelayEstHist};
use crate::effect::{EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::smoothing::{rise_time, Ewma};

pub(crate) const EVENT_THRESH: f64 = 1.8;
pub(crate) const ENV_SMOOTH_TIME: f64 = 30.0;
pub(crate) const EVENT_SMOOTH_TIME: f64 = 30.0;
pub(crate) const ACCOM_TIME: f64 = 300.0;
pub(crate) const RISE_TIME_FAST: f64 = 30.0;
pub(crate) const RISE_TIME_SLOW: f64 = 100.0;
pub(crate) const NORM_TIME: f64 = 160.0;
const NORM_CROSSFEED: f64 = 0.1;
const ORD_FACTOR_DECAY: f64 = 10.0;
pub(crate) const EVENT_SAMPLE_TIME: f64 = 30.0;
pub(crate) const EVENT_MAX_HOLD_TIME: f64 = 200.0;
const EVENT_MASK_TIME: f64 = 100.0;
const ORD_SENS_ERR: f64 = 2.0;
const ORD_SENS_WEIGHT: f64 = 3.0;
const ORD_WEIGHT_THRESH: f64 = 0.3;
const ORD_OFFSET_THRESH: f64 = 0.6;
const ORD_NOTCH_FREQ_1: f64 = 4.0;
const ORD_NOTCH_GAIN_1: f64 = -10.3;
const ORD_NOTCH_FREQ_2: f64 = 12.0;
const ORD_NOTCH_GAIN_2: f64 = -10.3;
const ORD_NOTCH_SCALE_RT: f64 = 2.0;
const DIFF_SENS_WEIGHT: f64 = 2.0;
const DIFF_WEIGHT_SCALE: f64 = 2.5;
const ORD_DPWR_SENS_ERR: f64 = 2.0;
const PWRCMP_RISE_TIME: f64 = 160.0;
const PWRCMP_FALL_TIME: f64 = 15.0;
const PWRCMP_FACTOR_SENS: f64 = 0.1;
const OFFSET_BASE_SCALE: f64 = 1.0;
pub(crate) const BASE_ORD_NOTCH_SCALE: f64 = 0.7;
pub(crate) const NORM_ACCOM_FACTOR: f64 = 0.9;
pub(crate) const DIFF_OVERSHOOT: f64 = 1.001;

const MATRIX_RS_DEFAULT: f64 = 1.0;
const MATRIX_ADJ0_DEFAULT: f64 = 0.5;
const MATRIX_ADJ1_DEFAULT: f64 = 1.0;
const SURR_MULT_DEFAULT: f64 = FRAC_1_SQRT_2;
const SURR_MULT_REAR_DEFAULT: f64 = 1.0;
const LOOKAHEAD_DEFAULT: f64 = 0.5;
const SHELF_MULT_DEFAULT: f64 = FRAC_1_SQRT_2;
const SHELF_F0_DEFAULT: f64 = 500.0;
const CONTOUR_PWRCMP_DEFAULT: f64 = 1.0;
const LOWPASS_F0_DEFAULT: f64 = 6000.0;
const LOWPASS_ORDER_DEFAULT: f64 = 0.5;
const REAR_EVENT_MASK_DEFAULT: f64 = 1.0;

const DS_OFF_SCALE: f64 = OFFSET_BASE_SCALE * 2.0 * ACCOM_TIME / EVENT_MAX_HOLD_TIME;

const FLAG_L: u32 = 1 << 0;
const FLAG_R: u32 = 1 << 1;
const FLAG_USE_ORD: u32 = 1 << 2;
const FLAG_FUSE: u32 = 1 << 3;
const FLAG_UNALIGNED: u32 = 1 << 4;
const DELAY_EST_AVG_THRESH: f64 = -0.3;
const OFF_FLAG_L: u32 = 1 << 0;
const OFF_FLAG_R: u32 = 1 << 1;

/// `TIME_TO_FRAMES`
pub(crate) fn time_to_frames(ms: f64, fs: f64) -> isize {
    (ms / 1000.0 * fs).round() as isize
}

/// Envelopes of left, right, sum and difference.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Envs {
    pub l: f64,
    pub r: f64,
    pub sum: f64,
    pub diff: f64,
}

/// The steering angles: left–right (`lr`) and centre–surround (`cs`), in radians within ±π/4.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Axes {
    pub lr: f64,
    pub cs: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MatrixCoefs {
    pub ll: f64,
    pub lr: f64,
    pub rl: f64,
    pub rr: f64,
    pub lsl: f64,
    pub lsr: f64,
    pub rsl: f64,
    pub rsr: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectPathMode {
    None,
    Event,
    Static,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    L2_2,
    L2_4,
}

impl Layout {
    pub fn nf(self) -> usize {
        2
    }
    pub fn ns(self) -> usize {
        match self {
            Layout::L2_2 => 2,
            Layout::L2_4 => 4,
        }
    }
}

/// The options of `matrix4` (`struct matrix4_config`).
#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub c0: usize,
    pub c1: usize,
    pub do_phase_flip: bool,
    pub do_dpwr_decouple: bool,
    pub lookahead_frames: isize,
    pub surr_mult: [f64; 2],
    pub rear_ev_mask: f64,
    pub shelf_mult: f64,
    pub shelf_f0: f64,
    pub lowpass_f0: f64,
    pub lowpass_order: f64,
    pub contour_pwrcmp: f64,
    pub dp_mode: DirectPathMode,
    pub matrix_rs: f64,
    pub matrix_adj: [f64; 2],
    pub layout: Layout,
    /// Options of the multiband version.
    pub use_fir_p: bool,
    pub fb_id: String,
    pub fb_type: FilterType,
    pub fb_stop: [f64; 2],
    pub uev_action: UnalignedEventAction,
    pub do_xtc: bool,
    pub xtc_gain: f64,
    pub xtc_itd: f64,
    pub xtc_bp: [f64; 2],
}

const REAR_EVENT_MASK_MB_DEFAULT: f64 = 0.3;
const XTC_GAIN_DEFAULT: f64 = 0.2;
const XTC_ITD_DEFAULT: f64 = 250.0;
const XTC_BP_FL_DEFAULT: f64 = 0.0;
const XTC_BP_FH_DEFAULT: f64 = 4000.0;

/// `set_fb_stop_default`
fn fb_stop_default(t: FilterType) -> [f64; 2] {
    match t {
        FilterType::Butterworth => [0.0, 0.0],
        FilterType::Chebyshev1 | FilterType::Chebyshev2 => [25.0, 0.0],
        FilterType::Elliptic => [35.0, 50.0],
    }
}

fn calc_lookahead_frames(x: f64, fs: f64) -> isize {
    time_to_frames(EVENT_SAMPLE_TIME + RISE_TIME_FAST * x, fs)
}

/// `isolate`: the text before `sep` and the text after it (empty if there is no `sep`).
fn isolate(s: &str, sep: char) -> (&str, &str) {
    s.split_once(sep).unwrap_or((s, ""))
}

/// `optcmp` with `:` as the separator: `opt` is `name` or `name:…`.
fn is_opt_arg(opt: &str, name: &str) -> bool {
    match name.strip_suffix(':') {
        Some(n) if !n.is_empty() => {
            opt.starts_with(n) && (opt.len() == n.len() || opt.as_bytes()[n.len()] == b':')
        }
        _ => opt == name,
    }
}

/// `HANDLE_BOOLEAN_ARG2`: empty or a prefix of "true" is true, a prefix of "false" is false.
fn boolean_arg(opt: &str, arg: &str, name: &str) -> Result<bool> {
    let a = arg.to_ascii_lowercase();
    if a.is_empty() || "true".starts_with(&a) {
        Ok(true)
    } else if "false".starts_with(&a) {
        Ok(false)
    } else {
        Err(Error::Message(format!(
            "{name}: error: unrecognized argument to option '{opt}': {arg}"
        )))
    }
}

fn number(name: &str, text: &str, param: &str) -> Result<f64> {
    let (v, end) = strtod(text);
    if end == 0 || end != text.len() {
        return Err(Error::Parse {
            effect: name.into(),
            param: param.into(),
            text: text.into(),
        });
    }
    Ok(v)
}

fn ranged(name: &str, v: f64, lo: f64, hi: f64, param: &str) -> Result<f64> {
    if v >= lo && v <= hi {
        Ok(v)
    } else {
        Err(Error::Range {
            effect: name.into(),
            param: param.into(),
        })
    }
}

/// `matrix4_config_init`; `is_mb` for the multiband version, which has more options (they are
/// ignored, with no effect, by `matrix4`).
pub(crate) fn config_init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
    is_mb: bool,
) -> Result<Config> {
    let name = argv[0];
    let argc = argv.len();
    let fs = f64::from(istream.fs);
    if istream.fs < 32000 {
        return Err(Error::Range {
            effect: name.into(),
            param: "input sample rate".into(),
        });
    }
    if selector.iter().filter(|s| **s).count() != 2 {
        return Err(Error::Range {
            effect: name.into(),
            param: "input channels must be 2".into(),
        });
    }
    let mut surr_level = [f64::NAN, f64::NAN];
    let mut c = Config {
        c0: 0,
        c1: 0,
        do_phase_flip: false,
        do_dpwr_decouple: true,
        lookahead_frames: calc_lookahead_frames(LOOKAHEAD_DEFAULT, fs),
        surr_mult: [0.0; 2],
        rear_ev_mask: if is_mb {
            REAR_EVENT_MASK_MB_DEFAULT
        } else {
            REAR_EVENT_MASK_DEFAULT
        },
        shelf_mult: SHELF_MULT_DEFAULT,
        shelf_f0: SHELF_F0_DEFAULT,
        lowpass_f0: LOWPASS_F0_DEFAULT,
        lowpass_order: LOWPASS_ORDER_DEFAULT,
        contour_pwrcmp: CONTOUR_PWRCMP_DEFAULT,
        dp_mode: DirectPathMode::None,
        matrix_rs: MATRIX_RS_DEFAULT,
        matrix_adj: [MATRIX_ADJ0_DEFAULT, MATRIX_ADJ1_DEFAULT],
        layout: Layout::L2_2,
        use_fir_p: false,
        fb_id: String::new(),
        fb_type: FilterType::Elliptic,
        fb_stop: fb_stop_default(FilterType::Elliptic),
        uev_action: if is_mb {
            UnalignedEventAction::Align
        } else {
            UnalignedEventAction::None
        },
        do_xtc: false,
        xtc_gain: XTC_GAIN_DEFAULT,
        xtc_itd: XTC_ITD_DEFAULT,
        xtc_bp: [XTC_BP_FL_DEFAULT, XTC_BP_FH_DEFAULT],
    };
    let mut phase_flip: Option<bool> = None;
    for (i, arg) in argv.iter().enumerate().skip(1) {
        let (v, end) = strtod(arg);
        let rest = &arg[end..];
        if rest.is_empty() || rest.starts_with('/') {
            if end != 0 {
                surr_level[0] = v;
            }
            if let Some(sub) = rest.strip_prefix('/') {
                surr_level[1] = number(name, sub, "surround_level_rear")?;
            } else if !surr_level[0].is_nan() {
                surr_level[1] = (surr_level[0] + 6.02).min(0.0);
            }
            if i != argc - 1 {
                return Err(info.usage_error());
            }
            continue;
        }
        let mut opts = *arg;
        while !opts.is_empty() {
            let (opt_full, next) = isolate(opts, ',');
            opts = next;
            let opt = opt_full.trim();
            if opt.is_empty() {
                continue;
            }
            let (oname, oarg) = isolate(opt, '=');
            let needs_arg =
                || Error::Message(format!("{name}: error: option requires argument: {opt}"));
            match oname {
                "status" => {
                    if !(oarg.is_empty() || oarg == "bars" || oarg == "text" || oarg == "none") {
                        return Err(Error::Message(format!(
                            "{name}: error: unrecognized status type: {oarg}"
                        )));
                    }
                }
                "layout" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.layout = match oarg {
                        "2/2" => Layout::L2_2,
                        "2/4" => Layout::L2_4,
                        _ => {
                            return Err(Error::Message(format!(
                                "{name}: error: unrecognized channel layout: {oarg}"
                            )))
                        }
                    };
                }
                "matrix" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    if is_opt_arg(oarg, "v1") {
                        c.matrix_rs = 0.0;
                    } else if is_opt_arg(oarg, "v2") {
                        c.matrix_rs = 1.0;
                        c.matrix_adj = [0.0, 0.0];
                    } else if is_opt_arg(oarg, "v3") {
                        c.matrix_rs = 1.0;
                        c.matrix_adj = [1.0, 1.0];
                    } else if is_opt_arg(oarg, "v4:") {
                        c.matrix_rs = 1.0;
                        c.matrix_adj = [MATRIX_ADJ0_DEFAULT, MATRIX_ADJ1_DEFAULT];
                        let (_, sub) = isolate(oarg, ':');
                        let (sub0, sub1) = isolate(sub, ':');
                        if !sub0.is_empty() {
                            let v = number(name, sub0, "matrix: v4: adj")?;
                            c.matrix_adj[0] = ranged(name, v, 0.0, 1.0, "matrix: v4: adj")?;
                        }
                        if !sub1.is_empty() {
                            let v = number(name, sub1, "matrix: v4: adj_dir")?;
                            c.matrix_adj[1] = ranged(name, v, 0.0, 1.0, "matrix: v4: adj_dir")?;
                        }
                    } else {
                        return Err(Error::Message(format!(
                            "{name}: error: unrecognized matrix identifier: {oarg}"
                        )));
                    }
                }
                "shelf" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    let (gain, sub) = isolate(oarg, ':');
                    let (sub0, sub1) = isolate(sub, ':');
                    if !gain.is_empty() {
                        if gain == "none" {
                            c.shelf_mult = 1.0;
                        } else {
                            let g = number(name, gain, "shelf: gain")?;
                            c.shelf_mult = 10f64.powf(g / 20.0);
                        }
                    }
                    if !sub0.is_empty() {
                        c.shelf_f0 = get_freq(name, sub0, "shelf: f0")?;
                        ranged(name, c.shelf_f0, 100.0, 6000.0, "shelf: f0")?;
                    }
                    if !sub1.is_empty() {
                        c.contour_pwrcmp = number(name, sub1, "shelf: pwrcmp")?;
                        ranged(name, c.contour_pwrcmp, 0.0, 1.0, "shelf: pwrcmp")?;
                    }
                }
                "lowpass" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    let (f0, sub) = isolate(oarg, ':');
                    if f0 == "none" {
                        c.lowpass_f0 = 0.0;
                    } else if !f0.is_empty() {
                        c.lowpass_f0 = get_freq(name, f0, "lowpass: f0")?;
                        if !(c.lowpass_f0 >= 0.0 && c.lowpass_f0 < fs / 2.0) {
                            return Err(Error::Range {
                                effect: name.into(),
                                param: "lowpass: f0".into(),
                            });
                        }
                    }
                    if !sub.is_empty() {
                        c.lowpass_order = number(name, sub, "lowpass: order")?;
                        ranged(name, c.lowpass_order, 0.1, 1.0, "lowpass: order")?;
                    }
                }
                "contour_pwrcmp" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.contour_pwrcmp = number(name, oarg, opt)?;
                    ranged(name, c.contour_pwrcmp, 0.0, 1.0, opt)?;
                }
                "phase_flip" => phase_flip = Some(boolean_arg(opt, oarg, name)?),
                "signal" => {
                    boolean_arg(opt, oarg, name)?;
                }
                "direct_path" => {
                    c.dp_mode = match oarg {
                        "event" => DirectPathMode::Event,
                        "static" => DirectPathMode::Static,
                        _ => {
                            if boolean_arg(opt, oarg, name)? {
                                DirectPathMode::Event
                            } else {
                                DirectPathMode::None
                            }
                        }
                    };
                }
                "rear_event_mask" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.rear_ev_mask = number(name, oarg, opt)?;
                    ranged(name, c.rear_ev_mask, 0.0, 100.0, opt)?;
                }
                "use_fir_p" => c.use_fir_p = boolean_arg(opt, oarg, name)?,
                // The other options of the multiband version are ignored here.
                "unaligned_event_action" | "xtc" | "filter_bank" | "filter_type" if !is_mb => {}
                "unaligned_event_action" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.uev_action = match oarg {
                        "none" => UnalignedEventAction::None,
                        "neutralize" => UnalignedEventAction::Neutralize,
                        "align" => UnalignedEventAction::Align,
                        _ => {
                            return Err(Error::Message(format!(
                                "{name}: error: unrecognized unaligned event action: {oarg}"
                            )))
                        }
                    };
                }
                "xtc" => {
                    if oarg == "none" {
                        c.do_xtc = false;
                    } else {
                        c.do_xtc = true;
                        let (ild, rest) = isolate(oarg, ':');
                        let (itd, rest) = isolate(rest, ':');
                        let (fl, fh) = isolate(rest, '-');
                        if !ild.is_empty() {
                            let v = number(name, ild, "xtc: ild")?;
                            c.xtc_gain = 10f64.powf(v / -20.0);
                            if !(0.01..=0.8).contains(&c.xtc_gain) {
                                return Err(Error::Range {
                                    effect: name.into(),
                                    param: "xtc: ild".into(),
                                });
                            }
                        }
                        if !itd.is_empty() {
                            c.xtc_itd = number(name, itd, "xtc: itd")?;
                            ranged(name, c.xtc_itd, 40.0, 1000.0, "xtc: itd")?;
                        }
                        if !fl.is_empty() {
                            c.xtc_bp[0] = get_freq(name, fl, "xtc: fl")?;
                            ranged(name, c.xtc_bp[0], 0.0, 1000.0, "xtc: fl")?;
                        }
                        if !fh.is_empty() {
                            c.xtc_bp[1] = get_freq(name, fh, "xtc: fh")?;
                            ranged(name, c.xtc_bp[1], 2000.0, 10000.0, "xtc: fh")?;
                        }
                    }
                }
                "filter_bank" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.fb_id = oarg.chars().take(15).collect();
                }
                "filter_type" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    c.fb_type = if is_opt_arg(oarg, "butterworth") {
                        FilterType::Butterworth
                    } else if is_opt_arg(oarg, "chebyshev1:") {
                        FilterType::Chebyshev1
                    } else if is_opt_arg(oarg, "chebyshev2:") {
                        FilterType::Chebyshev2
                    } else if is_opt_arg(oarg, "elliptic:") {
                        FilterType::Elliptic
                    } else {
                        return Err(Error::Message(format!(
                            "{name}: error: unrecognized filter bank type: {oarg}"
                        )));
                    };
                    c.fb_stop = fb_stop_default(c.fb_type);
                    let (_, sub) = isolate(oarg, ':');
                    if !sub.is_empty() {
                        match c.fb_type {
                            FilterType::Chebyshev1 | FilterType::Chebyshev2 => {
                                c.fb_stop[0] = number(name, sub, "stop_dB")?;
                                if c.fb_stop[0] < 10.0 {
                                    return Err(Error::Message(format!(
                                        "{name}: error: {oarg}: stopband attenuation must be at least 10dB"
                                    )));
                                }
                            }
                            FilterType::Elliptic => {
                                let (sub0, sub1) = isolate(sub, ':');
                                c.fb_stop[0] = number(name, sub0, "stop_dB")?;
                                c.fb_stop[1] = if sub1.is_empty() {
                                    c.fb_stop[0]
                                } else {
                                    number(name, sub1, "stop_dB")?
                                };
                                if c.fb_stop[0] < 20.0 || c.fb_stop[1] < 20.0 {
                                    return Err(Error::Message(format!(
                                        "{name}: error: {oarg}: stopband attenuation must be at least 20dB"
                                    )));
                                }
                            }
                            FilterType::Butterworth => {}
                        }
                    }
                }
                "lookahead" => {
                    if oarg.is_empty() {
                        return Err(needs_arg());
                    }
                    let v = number(name, oarg, opt)?;
                    ranged(name, v, 0.0, 2.0, opt)?;
                    c.lookahead_frames = calc_lookahead_frames(v, fs);
                }
                "dpwr_decouple" => c.do_dpwr_decouple = boolean_arg(opt, oarg, name)?,
                _ => {
                    return Err(Error::Message(format!(
                        "{name}: error: unrecognized option: {opt}"
                    )))
                }
            }
        }
    }
    c.do_phase_flip = phase_flip.unwrap_or(false);
    c.surr_mult[0] = if surr_level[0].is_nan() {
        SURR_MULT_DEFAULT
    } else {
        10f64.powf(surr_level[0] / 20.0)
    };
    c.surr_mult[1] = if surr_level[1].is_nan() {
        SURR_MULT_REAR_DEFAULT
    } else {
        10f64.powf(surr_level[1] / 20.0)
    };
    let mut on = selector
        .iter()
        .enumerate()
        .filter(|(_, s)| **s)
        .map(|(i, _)| i);
    c.c0 = on.next().unwrap_or(0);
    c.c1 = on.next().unwrap_or(0);
    Ok(c)
}

/// The input smoothers (`struct smooth_state`).
#[derive(Clone, Debug)]
pub(crate) struct SmoothState {
    env: [Ewma; 4],
    pwr_env: [Ewma; 4],
}

impl SmoothState {
    pub fn new(fs: f64) -> Self {
        let e = Ewma::new(fs, rise_time(ENV_SMOOTH_TIME));
        Self {
            env: [e; 4],
            pwr_env: [e; 4],
        }
    }

    /// `calc_input_envs`
    pub fn calc_input_envs(&mut self, l: f64, r: f64) -> (Envs, Envs) {
        let (sum, diff) = (l + r, l - r);
        let env = Envs {
            l: self.env[0].run(l.abs()),
            r: self.env[1].run(r.abs()),
            sum: self.env[2].run(sum.abs()),
            diff: self.env[3].run(diff.abs()),
        };
        let pwr = Envs {
            l: self.pwr_env[0].run(l * l),
            r: self.pwr_env[1].run(r * r),
            sum: self.pwr_env[2].run(sum * sum),
            diff: self.pwr_env[3].run(diff * diff),
        };
        (env, pwr)
    }
}

/// A peaking filter as a state-variable filter (after F. Adriaensen, "Digital State-Variable
/// Filters").
#[derive(Clone, Copy, Debug, Default)]
struct SvfPk {
    m0: f64,
    m1: f64,
    a0: f64,
    alpha: f64,
    beta: f64,
}

impl SvfPk {
    fn new(fs: f64, f0: f64, q: f64, g0: f64) -> Self {
        let w0 = 2.0 * PI * f0 / fs;
        Self {
            a0: 10f64.powf(g0 / 40.0),
            alpha: w0.sin() / (2.0 * q),
            beta: w0.cos() - 1.0,
            m0: 0.0,
            m1: 0.0,
        }
    }
    fn run(&mut self, s: f64, scale: f64) -> f64 {
        let (alpha, beta) = (self.alpha, self.beta);
        let a = (self.a0 - 1.0) * scale + 1.0;
        let (k0, k1) = (a * alpha, a * beta);
        let (g0, g1) = (1.0 / (alpha + a), a / (k1 - alpha));
        let (c1, c2) = (2.0 * g0 * (alpha - k1), g1 * beta);
        let (d0, d1) = (g0 * a * (k0 + 1.0), g1 * (beta - k0));
        let x = s - self.m0 - self.m1;
        let y = d0 * x + d1 * self.m0 + self.m1;
        self.m1 += c2 * self.m0;
        self.m0 += c1 * x;
        y
    }
}

/// Event-detection settings (`struct event_config`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct EventConfig {
    pub sample_frames: isize,
    pub max_hold_frames: isize,
    pub ord_factor_c: f64,
    pub diff_lim: f64,
    pub rear_ev_mask: f64,
    pub offset_scale_slope: f64,
    pub uev_action: UnalignedEventAction,
}

/// What to do about an event whose channels are out of step (`enum unaligned_event_action`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnalignedEventAction {
    /// Decode it as if the channels were in step.
    None,
    /// Steer to the centre.
    Neutralize,
    /// Delay one channel to line the pair up and steer with the angles found.
    Align,
}

impl EventConfig {
    /// `event_config_init_priv`
    pub fn new(
        fs: f64,
        rear_ev_mask: f64,
        diff_overshoot: f64,
        uev_action: UnalignedEventAction,
    ) -> Self {
        Self {
            sample_frames: time_to_frames(EVENT_SAMPLE_TIME, fs),
            max_hold_frames: time_to_frames(EVENT_MAX_HOLD_TIME, fs),
            ord_factor_c: (-1.0 / (fs * ORD_FACTOR_DECAY)).exp(),
            diff_lim: FRAC_PI_4 * diff_overshoot,
            rear_ev_mask,
            offset_scale_slope: (EVENT_MAX_HOLD_TIME / EVENT_SAMPLE_TIME - 1.0)
                / ((EVENT_MAX_HOLD_TIME - EVENT_SAMPLE_TIME) * fs / 1000.0),
            uev_action,
        }
    }
}

/// The state of the event detector (`struct event_state`).
pub(crate) struct EventState {
    accom: [Ewma; 6],
    norm: [Ewma; 4],
    slow: [Ewma; 2],
    smooth: [Ewma; 2],
    avg: [Ewma; 4],
    drift: [Ewma; 4],
    drift_scale: [Ewma; 2],
    drift_err: [Ewma; 4],
    offset: [Ewma; 4],
    pub pwrcmp_factor: Ewma,
    ord_notch_scale: Ewma,
    pub rs_scale: Ewma,
    ord_lp: [Biquad; 2],
    ord_notch: [SvfPk; 4],
    pub dir: Axes,
    pub diff_last: Axes,
    align_max: Axes,
    align_avg: Ewma,
    pub align_dt: [f64; 2],
    pub realign: bool,
    t_delay_est: isize,
    pub slope_last: [f64; 2],
    ord_buf: Vec<Axes>,
    ord_lp_buf: Vec<Axes>,
    diff_buf: Vec<Axes>,
    slope_buf: Vec<[f64; 2]>,
    pub last: [f64; 2],
    clip_thresh: f64,
    pcf_sens: f64,
    max: [f64; 2],
    ord_factor: f64,
    base_ord_ns: f64,
    adj: f64,
    pub ds_diff: f64,
    off_thresh: f64,
    rts_off: f64,
    ds_ord_buf: Vec<f64>,
    max_buf: Vec<f64>,
    t: isize,
    t_sample: isize,
    pub t_hold: isize,
    t_end: [isize; 2],
    t_off_ord: isize,
    buf_len: usize,
    buf_p: usize,
    pub unaligned: bool,
    flags: [u32; 2],
    off_flags: u32,
}

pub(crate) fn square(x: f64) -> f64 {
    x * x
}
fn pwr_sum(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}
fn safe_sqrt(x: f64) -> f64 {
    if x > 0.0 {
        x.sqrt()
    } else {
        0.0
    }
}
pub(crate) fn axes_mag(ax: &Axes) -> f64 {
    ax.lr.abs() + ax.cs.abs()
}

/// `smoothstep` clamped to 0..1.
pub(crate) fn smoothstep(x: f64) -> f64 {
    if x >= 1.0 {
        return 1.0;
    }
    if x <= 0.0 {
        return 0.0;
    }
    x * x * (3.0 - 2.0 * x)
}

/// Approximation of atan for 0 <= x <= 1.
fn atan_approx(x: f64) -> f64 {
    FRAC_PI_4 * x + 0.2777 * x * (1.0 - x)
}

fn calc_angle(a: f64, b: f64) -> f64 {
    if a <= f64::MIN_POSITIVE && b <= f64::MIN_POSITIVE {
        return 0.0;
    }
    let r = atan_approx(if a < b { a / b } else { b / a });
    if a < b {
        r - FRAC_PI_4
    } else {
        FRAC_PI_4 - r
    }
}

pub(crate) fn calc_angle_pwr(a: f64, b: f64) -> f64 {
    if a <= f64::MIN_POSITIVE && b <= f64::MIN_POSITIVE {
        return 0.0;
    }
    let r = atan_approx((if a < b { a / b } else { b / a }).sqrt());
    if a < b {
        r - FRAC_PI_4
    } else {
        FRAC_PI_4 - r
    }
}

fn norm_axes(ax: &mut Axes) {
    let mag = axes_mag(ax);
    if mag > FRAC_PI_4 {
        let norm = FRAC_PI_4 / mag;
        ax.lr *= norm;
        ax.cs *= norm;
    }
}

fn ord_notch_scale(ax: &Axes) -> f64 {
    let mut z = axes_mag(ax) * (2.0 / FRAC_PI_4) - 1.0;
    if z < 0.0 {
        z = 0.0;
    }
    1.0 - z * z * 0.99
}

fn run_scale_set_abs_max(e: &mut Ewma, s: f64, sf: f64) -> f64 {
    let s_1 = e.last();
    if s.is_sign_negative() != s_1.is_sign_negative() || s.abs() >= s_1.abs() {
        e.run_scale(s, sf)
    } else {
        e.set(s)
    }
}

fn prev(x: usize, len: usize) -> usize {
    if x > 0 {
        x - 1
    } else {
        len - 1
    }
}
fn next(x: usize, len: usize) -> usize {
    if x + 1 < len {
        x + 1
    } else {
        0
    }
}

impl EventState {
    /// `event_state_init_priv`
    pub fn new(fs: f64, base_thresh_scale: f64, base_ord_notch_scale: f64) -> Self {
        let ew = |ms: f64| Ewma::new(fs, rise_time(ms));
        let mut drift_scale = [ew(RISE_TIME_FAST), ew(EVENT_MAX_HOLD_TIME)];
        drift_scale.iter_mut().for_each(|d| {
            d.set(1.0);
        });
        let mut ord_notch_scale = Ewma::new(fs, rise_time(ORD_NOTCH_SCALE_RT * 1000.0));
        ord_notch_scale.set(1.0);
        let mut rs_scale = ew(RISE_TIME_FAST);
        rs_scale.set(1.0);
        let ord_lp_f = (0.34 * 1000.0 * 1.5) / RISE_TIME_FAST;
        let lp = || {
            Biquad::from_type(
                crate::biquad::LOWPASS,
                fs,
                ord_lp_f,
                0.577,
                0.0,
                0.0,
                Width::Q,
            )
        };
        let buf_len = time_to_frames(EVENT_SAMPLE_TIME * 0.5, fs).max(0) as usize;
        Self {
            accom: [ew(ACCOM_TIME); 6],
            norm: [
                ew(NORM_TIME),
                ew(NORM_TIME),
                ew(NORM_TIME * 0.625),
                ew(NORM_TIME * 0.625),
            ],
            slow: [ew(RISE_TIME_SLOW); 2],
            smooth: [ew(EVENT_SMOOTH_TIME); 2],
            avg: [ew(EVENT_SAMPLE_TIME); 4],
            drift: [
                ew(ACCOM_TIME * 2.0),
                ew(ACCOM_TIME * 2.0),
                ew(RISE_TIME_FAST),
                ew(RISE_TIME_FAST),
            ],
            drift_scale,
            drift_err: [
                ew(ACCOM_TIME),
                ew(ACCOM_TIME),
                ew(RISE_TIME_FAST),
                ew(RISE_TIME_FAST),
            ],
            offset: [
                ew(NORM_TIME * 1.25),
                ew(NORM_TIME * 1.25),
                ew(NORM_TIME),
                ew(NORM_TIME),
            ],
            pwrcmp_factor: ew(PWRCMP_RISE_TIME),
            ord_notch_scale,
            rs_scale,
            ord_lp: [lp(), lp()],
            ord_notch: [
                SvfPk::new(fs, ORD_NOTCH_FREQ_1, 0.5, ORD_NOTCH_GAIN_1),
                SvfPk::new(fs, ORD_NOTCH_FREQ_1, 0.5, ORD_NOTCH_GAIN_1),
                SvfPk::new(fs, ORD_NOTCH_FREQ_2, 0.5, ORD_NOTCH_GAIN_2),
                SvfPk::new(fs, ORD_NOTCH_FREQ_2, 0.5, ORD_NOTCH_GAIN_2),
            ],
            dir: Axes::default(),
            diff_last: Axes::default(),
            align_max: Axes::default(),
            align_avg: Ewma::new(1.0, 10.0),
            align_dt: [0.0; 2],
            realign: false,
            t_delay_est: 0,
            slope_last: [0.0; 2],
            ord_buf: vec![Axes::default(); buf_len],
            ord_lp_buf: vec![Axes::default(); buf_len],
            diff_buf: vec![Axes::default(); buf_len],
            slope_buf: vec![[0.0; 2]; buf_len],
            last: [0.0; 2],
            clip_thresh: EVENT_THRESH * base_thresh_scale * 100.0,
            pcf_sens: PWRCMP_FACTOR_SENS / base_thresh_scale,
            max: [0.0; 2],
            ord_factor: 0.0,
            base_ord_ns: if base_ord_notch_scale >= 1e-6 {
                base_ord_notch_scale
            } else {
                0.0
            },
            adj: 0.0,
            ds_diff: 0.0,
            off_thresh: ORD_OFFSET_THRESH * base_thresh_scale,
            rts_off: 0.0,
            ds_ord_buf: vec![0.0; buf_len],
            max_buf: vec![0.0; buf_len],
            t: 1,
            t_sample: 0,
            t_hold: 0,
            t_end: [0; 2],
            t_off_ord: 0,
            buf_len,
            buf_p: 0,
            unaligned: false,
            flags: [0; 2],
            off_flags: 0,
        }
    }

    /// `process_events_priv`: updates the steering angles `ax` (and `ax_ev` while an event is
    /// held, `ax_dpwr` for the power correction). `de` is the delay estimator of the band and
    /// the history shared by all bands; without it events are never "unaligned".
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        evc: &EventConfig,
        env: &Envs,
        pwr_env: &Envs,
        mut de: Option<(&mut DelayEst, &mut DelayEstHist)>,
        norm_accom_factor: f64,
        thresh_scale: f64,
        ax: &mut Axes,
        ax_ev: &mut Axes,
        ax_dpwr: &mut Axes,
    ) {
        let bp = self.buf_p;
        let ord = Axes {
            lr: calc_angle(env.l, env.r),
            cs: calc_angle(env.sum, env.diff),
        };
        let ord_lp = Axes {
            lr: self.ord_lp[0].run(ord.lr),
            cs: self.ord_lp[1].run(ord.cs),
        };
        let ord_lp_d = self.ord_lp_buf[bp];
        let ord_ns = self.ord_notch_scale.last() * self.base_ord_ns;
        let ord_lp_d_notched = Axes {
            lr: {
                let v = self.ord_notch[0].run(ord_lp_d.lr, ord_ns);
                self.ord_notch[2].run(v, ord_ns)
            },
            cs: {
                let v = self.ord_notch[1].run(ord_lp_d.cs, ord_ns);
                self.ord_notch[3].run(v, ord_ns)
            },
        };
        let adapt = Envs {
            l: pwr_env.l - self.accom[0].run_set_max(pwr_env.l),
            r: pwr_env.r - self.accom[1].run_set_max(pwr_env.r),
            sum: pwr_env.sum - self.accom[2].run_set_max(pwr_env.sum),
            diff: pwr_env.diff - self.accom[3].run_set_max(pwr_env.diff),
        };
        let diff = Axes {
            lr: calc_angle_pwr(adapt.l, adapt.r),
            cs: calc_angle_pwr(adapt.sum, adapt.diff),
        };
        self.diff_last = diff;
        self.ord_buf[bp] = ord;
        self.ord_lp_buf[bp] = ord_lp;
        self.diff_buf[bp] = diff;

        self.adj = 1.0 - self.ord_factor / 20.0;
        self.adj = if self.adj > 0.5 { self.adj } else { 0.5 };
        self.ord_factor *= evc.ord_factor_c;

        let thresh = EVENT_THRESH * thresh_scale;
        let l_pwr_xf = pwr_env.l * (1.0 - NORM_CROSSFEED) + pwr_env.r * NORM_CROSSFEED;
        let r_pwr_xf = pwr_env.r * (1.0 - NORM_CROSSFEED) + pwr_env.l * NORM_CROSSFEED;
        let l_norm_div = {
            let inner = self.norm[2].run(l_pwr_xf) * norm_accom_factor * self.adj;
            self.norm[0].run((l_pwr_xf - inner).abs())
        };
        let r_norm_div = {
            let inner = self.norm[3].run(r_pwr_xf) * norm_accom_factor * self.adj;
            self.norm[1].run((r_pwr_xf - inner).abs())
        };
        let l_mask =
            pwr_env.l - self.accom[4].run_scale_asym(pwr_env.l, 1.0, ACCOM_TIME / EVENT_MASK_TIME);
        let r_mask =
            pwr_env.r - self.accom[5].run_scale_asym(pwr_env.r, 1.0, ACCOM_TIME / EVENT_MASK_TIME);
        let l_mask_norm = l_mask / (l_norm_div + f64::MIN_POSITIVE);
        let r_mask_norm = r_mask / (r_norm_div + f64::MIN_POSITIVE);
        let l_mask_norm_sm = self.smooth[0].run(l_mask_norm.max(0.0).min(self.clip_thresh));
        let r_mask_norm_sm = self.smooth[1].run(r_mask_norm.max(0.0).min(self.clip_thresh));
        let l_event = (l_mask_norm_sm - self.slow[0].run(l_mask_norm_sm)) * self.adj;
        let r_event = (r_mask_norm_sm - self.slow[1].run(r_mask_norm_sm)) * self.adj;
        let l_slope = l_event - self.last[0];
        let r_slope = r_event - self.last[1];
        self.last = [l_event, r_event];
        self.slope_last = [l_slope, r_slope];
        self.slope_buf[bp] = [l_slope, r_slope];

        if self.t_sample == 0
            && ((l_slope > 0.0 && l_event > thresh) || (r_slope > 0.0 && r_event > thresh))
        {
            self.flags[1] = 0;
            if l_event >= r_event {
                self.flags[1] |= FLAG_L;
            }
            if r_event >= l_event {
                self.flags[1] |= FLAG_R;
            }
            self.t_end[1] = 0;
            self.t_sample = self.t;
            if self.t_hold == 0 || self.t_end[0] != 0 {
                self.max[1] = 0.0;
                self.avg[0].set(ord.lr);
                self.avg[1].set(ord.cs);
                self.avg[2].set(diff.lr);
                self.avg[3].set(diff.cs);
                // look back to where the event began
                let mut i = prev(bp, self.buf_len);
                let mut k = bp;
                let ch = if self.flags[1] & FLAG_L != 0 { 0 } else { 1 };
                while self.slope_buf[i][ch] > self.slope_buf[k][ch] {
                    self.t_sample -= 1;
                    k = i;
                    i = prev(i, self.buf_len);
                }
                i = k;
                while i != bp {
                    self.avg[0].run(self.ord_buf[i].lr);
                    self.avg[1].run(self.ord_buf[i].cs);
                    self.avg[2].run(self.diff_buf[i].lr);
                    self.avg[3].run(self.diff_buf[i].cs);
                    i = next(i, self.buf_len);
                }
                if de.is_some() {
                    self.t_delay_est = self.t_sample + de.as_ref().map_or(0, |d| d.0.ev_lag);
                }
            } else {
                self.flags[1] |= FLAG_FUSE;
                if self.flags[0] & FLAG_UNALIGNED != 0 {
                    if let Some((d, _)) = de.as_ref() {
                        self.t_delay_est = self.t_sample + d.ev_lag;
                    }
                }
            }
            if self.t_delay_est != 0 {
                if let Some((d, _)) = de.as_mut() {
                    d.set_norm();
                }
            }
            self.align_max = Axes::default();
        }

        if self.t_sample != 0 {
            if self.t_end[1] == 0
                && ((self.flags[1] & FLAG_L != 0 && adapt.l < f64::MIN_POSITIVE)
                    || (self.flags[1] & FLAG_R != 0 && adapt.r < f64::MIN_POSITIVE))
            {
                self.t_end[1] = self.t + evc.sample_frames;
            }
            if self.t_end[1] == 0 {
                self.avg[0].run(ord.lr);
                self.avg[1].run(ord.cs);
                self.avg[2].run(diff.lr);
                self.avg[3].run(diff.cs);
                if l_event > self.max[1] {
                    self.max[1] = l_event;
                }
                if r_event > self.max[1] {
                    self.max[1] = r_event;
                }
            }
            if self.t_delay_est != 0 && self.t >= self.t_delay_est {
                if let Some((d, hist)) = de.as_mut() {
                    let (dt, found) = d.estimate(self.align_dt[0]);
                    if let Some(m) = found {
                        self.align_max = m;
                    }
                    self.align_dt[1] = dt;
                    let tol = if evc.uev_action == UnalignedEventAction::Align {
                        d.ra_tol
                    } else {
                        d.tol
                    };
                    let avg_v = if dt.is_nan() || dt.abs() > d.tol {
                        -1.0
                    } else {
                        1.0
                    };
                    let align_wavg = (self.align_avg.run(avg_v) + hist.align_avg.run(avg_v)) * 0.5;
                    if dt.is_nan()
                        || (dt - self.align_dt[0]).abs() > tol
                        || (axes_rear(&self.align_max)
                            && (dt.abs() > d.tol || align_wavg < DELAY_EST_AVG_THRESH))
                    {
                        self.flags[1] |= FLAG_UNALIGNED;
                    }
                }
                self.t_delay_est = 0;
            }
            if self.t - self.t_sample >= evc.sample_frames
                || (self.flags[1] & FLAG_FUSE != 0 && self.t_end[1] != 0)
            {
                self.t_delay_est = 0;
                self.t_sample = 0;
                let align_wavg = de.as_ref().map_or(0.0, |(_, h)| {
                    (self.align_avg.last() + h.align_avg.last()) * 0.5
                });
                let mut ev_dt = self.align_dt[0];
                let mut ev_dir = Axes {
                    lr: self.avg[2].last(),
                    cs: self.avg[3].last(),
                };
                if self.flags[1] & FLAG_UNALIGNED != 0 {
                    // the flag is only ever set when a delay estimator is present
                    let (tol, thresh_de) =
                        de.as_ref().map_or((0.0, 0.0), |(d, _)| (d.tol, d.thresh));
                    let dt_valid =
                        !self.align_dt[1].is_nan() && axes_mag(&self.align_max) >= thresh_de;
                    if evc.uev_action == UnalignedEventAction::Align
                        && dt_valid
                        && !(axes_rear(&self.align_max) && self.align_dt[1].abs() > tol)
                    {
                        ev_dir = self.align_max;
                        ev_dt = self.align_dt[1];
                    } else if evc.uev_action != UnalignedEventAction::None {
                        if dt_valid || (align_wavg >= DELAY_EST_AVG_THRESH && axes_rear(&ev_dir)) {
                            ev_dir.cs = 0.0;
                            ev_dir.lr = 0.0;
                            if align_wavg >= DELAY_EST_AVG_THRESH {
                                ev_dt = 0.0;
                            }
                        } else {
                            // indeterminate/unreliable dt; use the ordinary angles
                            ev_dir.lr = self.avg[0].last();
                            ev_dir.cs = self.avg[1].last();
                        }
                    }
                } else if self.flags[1] & FLAG_USE_ORD == 0
                    && axes_mag(&self.align_max) > axes_mag(&ev_dir)
                {
                    ev_dir = self.align_max;
                } else if axes_mag(&ev_dir) > evc.diff_lim {
                    self.flags[1] |= FLAG_USE_ORD;
                    ev_dir.lr = self.avg[0].last();
                    ev_dir.cs = self.avg[1].last();
                }
                if self.flags[1] & FLAG_FUSE != 0
                    && self.flags[1] & FLAG_USE_ORD != 0
                    && self.flags[0] & FLAG_USE_ORD == 0
                {
                    // ignored
                } else if evc.rear_ev_mask > 0.0
                    && axes_rear(&ev_dir)
                    && ((self.flags[1] & FLAG_L != 0 && l_event < thresh * evc.rear_ev_mask)
                        || (self.flags[1] & FLAG_R != 0 && r_event < thresh * evc.rear_ev_mask))
                {
                    // short-duration rear event: ignored
                } else {
                    self.dir = ev_dir;
                    if self.flags[1] & FLAG_FUSE == 0 {
                        self.t_hold = self.t;
                    }
                    if self.flags[1] & FLAG_USE_ORD != 0 {
                        self.ord_factor += 1.0;
                    }
                    if ev_dt != self.align_dt[0] {
                        self.align_dt[0] = ev_dt;
                        self.realign = true;
                    }
                    self.flags[0] = self.flags[1];
                    self.max[0] = self.max[1];
                    self.t_end[0] = self.t_end[1];
                    self.ds_diff = 1.0
                        + smoothstep((self.max[1] - thresh) / (thresh * DIFF_WEIGHT_SCALE))
                            * DIFF_SENS_WEIGHT;
                }
                self.unaligned = de.as_ref().is_some_and(|(d, _)| {
                    align_wavg < DELAY_EST_AVG_THRESH || self.align_dt[0].abs() > d.tol
                });
            }
        }

        let l_off_norm = (-pwr_env.l + self.offset[0].run(pwr_env.l))
            / (self.offset[2].run(l_pwr_xf) + f64::MIN_POSITIVE);
        let r_off_norm = (-pwr_env.r + self.offset[1].run(pwr_env.r))
            / (self.offset[3].run(r_pwr_xf) + f64::MIN_POSITIVE);
        if self.t_off_ord != 0 {
            if (self.off_flags & OFF_FLAG_L != 0 && l_off_norm <= 0.0)
                || (self.off_flags & OFF_FLAG_R != 0 && r_off_norm <= 0.0)
            {
                self.t_off_ord = 0;
                self.off_flags = 0;
            }
            if self.t == self.t_off_ord && self.drift_scale[1].last() < DS_OFF_SCALE {
                self.drift_scale[1].set(DS_OFF_SCALE);
                self.rts_off = 1.0;
            }
        } else if l_off_norm > self.off_thresh || r_off_norm > self.off_thresh {
            self.t_off_ord = self.t + evc.sample_frames;
            if l_off_norm > self.off_thresh {
                self.off_flags |= OFF_FLAG_L;
            }
            if r_off_norm > self.off_thresh {
                self.off_flags |= OFF_FLAG_R;
            }
        }

        let ax_ord_last = Axes {
            lr: self.drift[0].last(),
            cs: self.drift[1].last(),
        };
        let ord_lr_err = ax_ord_last.lr - ord_lp_d_notched.lr;
        let ord_cs_err = ax_ord_last.cs - ord_lp_d_notched.cs;
        let ord_abs_err = (ord_lr_err.abs() + ord_cs_err.abs()) * FRAC_2_PI;
        let ds_ord = {
            let a = self.drift_scale[0]
                .run_set_max(self.ds_ord_buf[bp] * (1.0 + ord_abs_err * ORD_SENS_ERR));
            let b = self.drift_scale[1].run_scale(1.0, self.rts_off);
            a * b
        };
        let max_d = self.max_buf[bp];
        self.max_buf[bp] = l_event.max(r_event);
        self.pwrcmp_factor.run_scale_asym(
            1.0 - smoothstep(max_d * self.pcf_sens),
            self.drift_scale[1].last(),
            PWRCMP_RISE_TIME / PWRCMP_FALL_TIME,
        );
        if self.t_hold != 0 {
            ax.lr = self.drift[2].run_scale(self.dir.lr, self.ds_diff);
            ax_ev.lr = ax.lr;
            ax.cs = self.drift[3].run_scale(self.dir.cs, self.ds_diff);
            ax_ev.cs = ax.cs;
            self.drift[0].set(ax.lr);
            self.drift[1].set(ax.cs);
            if self.t_end[0] == 0
                && ((self.flags[0] & FLAG_L != 0 && adapt.l < f64::MIN_POSITIVE)
                    || (self.flags[0] & FLAG_R != 0 && adapt.r < f64::MIN_POSITIVE))
            {
                self.t_end[0] = self.t + evc.sample_frames;
            }
            if (self.t - self.t_hold >= evc.max_hold_frames)
                || (self.t_end[0] != 0 && self.t >= self.t_end[0])
            {
                let ev_d = (if self.t_end[0] != 0 {
                    self.t_end[0]
                } else {
                    self.t
                }) - self.t_hold;
                if self.t_end[0] == 0 {
                    self.drift_scale[0].set(1.0);
                }
                if self.t_end[0] != 0 && self.t >= self.t_end[0] {
                    let rts_off =
                        1.0 + (evc.max_hold_frames - ev_d) as f64 * evc.offset_scale_slope;
                    let ds_off = rts_off * DS_OFF_SCALE;
                    if self.drift_scale[1].last() < ds_off {
                        self.drift_scale[1].set(ds_off);
                        self.rts_off = rts_off;
                    }
                }
                self.t_hold = 0;
            }
            let dpwr_lr_err = self.dir.lr - ax.lr;
            let dpwr_cs_err = self.dir.cs - ax.cs;
            ax_dpwr.lr =
                ax.lr + run_scale_set_abs_max(&mut self.drift_err[2], dpwr_lr_err, self.ds_diff);
            ax_dpwr.cs =
                ax.cs + run_scale_set_abs_max(&mut self.drift_err[3], dpwr_cs_err, self.ds_diff);
            let (e2, e3) = (self.drift_err[2].last(), self.drift_err[3].last());
            self.drift_err[0].set(e2);
            self.drift_err[1].set(e3);
        } else {
            ax.lr = self.drift[0].run_scale(ord_lp_d_notched.lr, ds_ord);
            ax.cs = self.drift[1].run_scale(ord_lp_d_notched.cs, ds_ord);
            self.drift[2].set(ax.lr);
            self.drift[3].set(ax.cs);
            ax_ev.lr = 0.0;
            ax_ev.cs = 0.0;
            let dpwr_lr_err = ord_lp_d.lr - ax.lr;
            let dpwr_cs_err = ord_lp_d.cs - ax.cs;
            let dpwr_abs_err = (dpwr_lr_err.abs() + dpwr_cs_err.abs()) * FRAC_2_PI;
            let ds_dpwr = (1.0 + dpwr_abs_err * ORD_DPWR_SENS_ERR)
                * self.ds_ord_buf[bp]
                * self.drift_scale[1].last();
            ax_dpwr.lr = ax.lr + self.drift_err[0].run_scale(dpwr_lr_err, ds_dpwr);
            ax_dpwr.cs = ax.cs + self.drift_err[1].run_scale(dpwr_cs_err, ds_dpwr);
            let (e0, e1) = (self.drift_err[0].last(), self.drift_err[1].last());
            self.drift_err[2].set(e0);
            self.drift_err[3].set(e1);
        }
        norm_axes(ax);
        norm_axes(ax_dpwr);
        if self.unaligned && self.rs_scale.last() > 0.0 {
            if ax.cs >= 0.0 || ax_ord_last.cs >= 0.0 || self.rs_scale.last() < 0.0001 {
                self.rs_scale.set(0.0);
            } else {
                let sf = if self.t_hold != 0 {
                    self.ds_diff
                } else {
                    (RISE_TIME_FAST / ACCOM_TIME) * ds_ord
                };
                self.rs_scale.run_scale(0.0, sf);
            }
        } else if !self.unaligned && self.rs_scale.last() < 1.0 {
            if ax.cs >= 0.0 || ax_ord_last.cs >= 0.0 || self.rs_scale.last() > 0.9999 {
                self.rs_scale.set(1.0);
            } else {
                let sf = if self.t_hold != 0 {
                    self.ds_diff
                } else {
                    (RISE_TIME_FAST / ACCOM_TIME) * ds_ord
                };
                self.rs_scale.run_scale(1.0, sf);
            }
        }
        self.ord_notch_scale.run_set_max(ord_notch_scale(ax));
        let ds_ord_thresh = thresh * ORD_WEIGHT_THRESH;
        self.ds_ord_buf[bp] = 1.0;
        if l_mask_norm_sm > ds_ord_thresh || r_mask_norm_sm > ds_ord_thresh {
            let x = (l_mask_norm_sm.max(r_mask_norm_sm) - ds_ord_thresh)
                / (thresh * 1.5 - ds_ord_thresh);
            self.ds_ord_buf[bp] = smoothstep(x) * ORD_SENS_WEIGHT + 1.0;
        }
        self.t += 1;
        self.buf_p = next(bp, self.buf_len);
    }
}

pub(crate) fn axes_rear(ax: &Axes) -> bool {
    ax.cs < 0.06 * square(ax.lr) - (PI / 64.0)
}

/// The matrix settings of `calc_matrix_coefs`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CmcParams {
    pub surr_mult: [f64; 2],
    pub rs: f64,
    pub adj: f64,
}

/// `calc_matrix_coefs`: the matrix for the steering angles `ax` (power correction from
/// `ax_dpwr`). `shelf_args` are the surround levels at the shelf's high end; the result has
/// the matching front and surround gain ratios.
pub(crate) fn calc_matrix_coefs(
    ax: &Axes,
    ax_dpwr: &Axes,
    p: &CmcParams,
    shelf_args: &[f64],
) -> (MatrixCoefs, Vec<(f64, f64)>) {
    let mut m = MatrixCoefs::default();
    let (lr, cs) = (ax.lr, ax.cs);
    let (abs_lr, abs_cs) = (lr.abs(), cs.abs());

    // initial surround elements
    m.rsr = 1.0;
    m.lsl = 1.0;
    m.rsl = 0.0;
    m.lsr = 0.0;
    let gl = 1.0 + (abs_lr - FRAC_PI_4).tan();
    let gl2 = gl * gl;
    let gc_2 = 0.5 + 0.5 * (abs_cs - FRAC_PI_4).tan();
    if lr > 0.0 {
        m.lsl -= gl2;
        m.lsr -= gl;
    } else if lr < 0.0 {
        m.rsl -= gl;
        m.rsr -= gl2;
    }
    if cs > 0.0 {
        m.lsl -= gc_2;
        m.lsr -= gc_2;
        m.rsl -= gc_2;
        m.rsr -= gc_2;
    } else if cs < 0.0 {
        let sgc_2 = 0.5
            + 0.5
                * (if cs > -FRAC_PI_4 / 2.0 {
                    abs_cs - FRAC_PI_4
                } else {
                    cs
                })
                .tan();
        m.lsl -= sgc_2;
        m.lsr += sgc_2;
        m.rsl += sgc_2;
        m.rsr -= sgc_2;
    }

    // power correction for uncorrelated input
    let pu_sl = pwr_sum(m.lsl, m.lsr);
    m.lsl /= pu_sl;
    m.lsr /= pu_sl;
    let pu_sr = pwr_sum(m.rsl, m.rsr);
    m.rsl /= pu_sr;
    m.rsr /= pu_sr;

    // initial front elements
    if cs >= 0.0 {
        m.rr = 1.0;
        m.ll = 1.0;
        m.rl = 0.0;
        m.lr = 0.0;
    } else {
        let front_cs = if cs > -FRAC_PI_4 / 2.0 {
            4.0 * abs_cs
        } else {
            FRAC_PI_2
        };
        let front_lr_mult = p.adj
            * (if abs_lr <= FRAC_PI_4 / 2.0 {
                1.0
            } else {
                square((FRAC_PI_2 - 2.0 * abs_lr).tan())
            });
        m.ll = -gc_2;
        m.rr = -gc_2;
        m.rl = gc_2;
        m.lr = gc_2;
        if lr > 0.0 {
            m.ll -= gl2 * front_cs.sin() * front_lr_mult;
            m.lr += gl * (1.0 - front_cs.cos()) * front_lr_mult;
        } else if lr < 0.0 {
            m.rl += gl * (1.0 - front_cs.cos()) * front_lr_mult;
            m.rr -= gl2 * front_cs.sin() * front_lr_mult;
        }
        let cf_sm2 = square(p.surr_mult[1].min(1.0));
        let cf = (1.0 - ((1.0 - cf_sm2) / (1.0 + cf_sm2)).sqrt()) * p.rs;
        m.ll = 1.0 + m.ll * cf;
        m.lr *= cf;
        m.rl *= cf;
        m.rr = 1.0 + m.rr * cf;

        let pu_fl = pwr_sum(m.ll, m.lr);
        m.ll /= pu_fl;
        m.lr /= pu_fl;
        let pu_fr = pwr_sum(m.rl, m.rr);
        m.rl /= pu_fr;
        m.rr /= pu_fr;
    }

    // input phasor steering angles
    let (ph_lr, ph_cs) = (ax_dpwr.lr, ax_dpwr.cs);
    let (abs_ph_lr, abs_ph_cs) = (ph_lr.abs(), ph_cs.abs());

    // pre-emphasis of the directional component
    let pe_gl = 1.0 + (abs_ph_lr - FRAC_PI_4).tan();
    let pe_gl2 = pe_gl * pe_gl;
    let pe = [
        pe_gl2 * (1.0 + (abs_ph_lr + FRAC_PI_4).cos()),
        pe_gl * abs_ph_cs.tan(),
    ];
    let pe_sf = (1.0 + square(p.surr_mult[0].min(1.0))).sqrt() - 1.0;
    if ph_cs >= 0.0 {
        let pe_gcs = (0.5 + 0.5 * (abs_ph_cs - FRAC_PI_4).tan()) * pe_sf;
        m.ll += pe_gcs;
        m.lr += pe_gcs;
        m.rl += pe_gcs;
        m.rr += pe_gcs;
        if ph_lr > 0.0 {
            m.ll += pe[0] * pe_sf;
            m.lr += pe[1] * pe_sf;
        } else if ph_lr < 0.0 {
            m.rl += pe[1] * pe_sf;
            m.rr += pe[0] * pe_sf;
        }
    } else if ph_cs >= -FRAC_PI_4 / 2.0 {
        let pe_fsfw = pe_sf * (4.0 * abs_ph_cs).cos();
        let pe_ssfw = FRAC_1_SQRT_2 * (4.0 * abs_ph_cs).sin() * p.rs;
        if ph_lr > 0.0 {
            m.ll += pe[0] * pe_fsfw;
            m.lr += pe[1] * pe_fsfw;
            m.lsl += pe[0] * pe_ssfw;
            m.lsr -= pe[1] * pe_ssfw;
        } else if ph_lr < 0.0 {
            m.rl += pe[1] * pe_fsfw;
            m.rr += pe[0] * pe_fsfw;
            m.rsl -= pe[1] * pe_ssfw;
            m.rsr += pe[0] * pe_ssfw;
        }
    } else {
        let pe_ssf = FRAC_1_SQRT_2 * p.rs;
        let pe_gcs = (0.5 + 0.5 * (2.0 * abs_ph_cs - FRAC_PI_2).tan()) * (2.0 - SQRT_2) * pe_ssf;
        m.lsl += pe_gcs;
        m.lsr -= pe_gcs;
        m.rsl -= pe_gcs;
        m.rsr += pe_gcs;
        if ph_lr > 0.0 {
            m.lsl += pe[0] * pe_ssf;
            m.lsr -= pe[1] * pe_ssf;
        } else if ph_lr < 0.0 {
            m.rsl -= pe[1] * pe_ssf;
            m.rsr += pe[0] * pe_ssf;
        }
    }

    // input phasors
    let sin_ph_lr = (ph_lr + FRAC_PI_4).sin();
    let cos_ph_lr = (ph_lr + FRAC_PI_4).cos();
    let (sin_theta, cos_theta);
    if abs_ph_lr + abs_ph_cs < FRAC_PI_4 {
        let alpha = (1.0 - square((2.0 * ph_cs).sin() / (2.0 * ph_lr).cos())).sqrt();
        let beta = (1.0 + alpha).sqrt();
        let gamma = (1.0 - alpha).sqrt();
        sin_theta = if ph_cs < 0.0 {
            0.5 * (beta + gamma)
        } else {
            0.5 * (beta - gamma)
        };
        cos_theta = if ph_cs < 0.0 {
            0.5 * (beta - gamma)
        } else {
            0.5 * (beta + gamma)
        };
    } else {
        sin_theta = if ph_cs < 0.0 { 1.0 } else { 0.0 };
        cos_theta = if ph_cs < 0.0 { 0.0 } else { 1.0 };
    }
    let (l_real, l_imag) = (sin_ph_lr * cos_theta, sin_ph_lr * sin_theta);
    let (r_real, r_imag) = (cos_ph_lr * cos_theta, cos_ph_lr * -sin_theta);

    // squared level for directional input
    let gd_fl2 = square(m.ll * l_real + m.lr * r_real) + square(m.ll * l_imag + m.lr * r_imag);
    let gd_fr2 = square(m.rl * l_real + m.rr * r_real) + square(m.rl * l_imag + m.rr * r_imag);
    let gd_sl2 = square(m.lsl * l_real + m.lsr * r_real) + square(m.lsl * l_imag + m.lsr * r_imag);
    let gd_sr2 = square(m.rsl * l_real + m.rsr * r_real) + square(m.rsl * l_imag + m.rsr * r_imag);

    // power for directional input
    let pd_f = gd_fl2 + gd_fr2;
    let pd_s = gd_sl2 + gd_sr2;

    // directional power correction weighting
    let surr_mult2 = square(p.surr_mult[0]);
    let mut pdc_wf = 1.0;
    if ph_cs < 0.0 {
        if abs_ph_cs < abs_ph_lr {
            let w = if abs_ph_lr + abs_ph_cs > f64::MIN_POSITIVE {
                (abs_ph_lr - abs_ph_cs) / (abs_ph_lr + abs_ph_cs)
            } else {
                0.0
            };
            let pd_fw = pd_f * w * surr_mult2;
            let pd_sw = pd_s * (1.0 - w);
            pdc_wf = (if pd_fw >= pd_sw {
                FRAC_PI_2 - atan_approx(pd_sw / pd_fw)
            } else {
                atan_approx(pd_fw / pd_sw)
            }) * FRAC_2_PI;
            if pdc_wf < 1e-6 {
                pdc_wf = 1e-6;
            }
        } else {
            pdc_wf = 1e-6;
        }
    }
    let pdc_ws = 1.0 - pdc_wf;

    // directional power correction and normalisation
    let pd_ss = pd_s * surr_mult2;
    let norm_mult2 = 1.0 / (1.0 + surr_mult2);
    let pdc_x = ((1.0 + surr_mult2) - pd_f - pd_ss) / (pd_f * pdc_wf + pd_ss * pdc_ws);
    let pdc_f = safe_sqrt(norm_mult2 * (1.0 + pdc_x * pdc_wf));
    let pdc_s = safe_sqrt(norm_mult2 * surr_mult2 * (1.0 + pdc_x * pdc_ws));

    let shelf = shelf_args
        .iter()
        .map(|arg| {
            let surr_mult_hf2 = square(*arg);
            let pd_ss_hf = pd_s * surr_mult_hf2;
            let pdc_x_hf =
                ((1.0 + surr_mult_hf2) - pd_f - pd_ss_hf) / (pd_f * pdc_wf + pd_ss_hf * pdc_ws);
            let norm_mult_hf2 = 1.0 / (1.0 + surr_mult_hf2);
            (
                safe_sqrt(norm_mult_hf2 * (1.0 + pdc_x_hf * pdc_wf)) / pdc_f,
                safe_sqrt(norm_mult_hf2 * surr_mult_hf2 * (1.0 + pdc_x_hf * pdc_ws))
                    / pdc_s.max(f64::MIN_POSITIVE),
            )
        })
        .collect();

    m.ll *= pdc_f;
    m.lr *= pdc_f;
    m.rl *= pdc_f;
    m.rr *= pdc_f;
    m.lsl *= pdc_s;
    m.lsr *= pdc_s;
    m.rsl *= pdc_s;
    m.rsr *= pdc_s;
    (m, shelf)
}

/// The direct path of the surround channels (`struct direct_path_state`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DirectPath {
    zs: Ewma,
    enable: i32,
    mode: DirectPathMode,
}

impl DirectPath {
    /// `direct_path_state_init`
    pub fn new(fs: f64, mode: DirectPathMode) -> Self {
        Self {
            zs: Ewma::new(fs, rise_time(RISE_TIME_FAST)),
            enable: i32::from(mode == DirectPathMode::Static),
            mode,
        }
    }

    fn finish(&mut self) -> [f64; 4] {
        self.enable = 0;
        self.zs.set(0.0);
        [1.0, 0.0, 0.0, 0.0]
    }

    /// `surr_direct_pan`: `[ambience gain, direct gain (front surround), direct gain (rear),
    /// the position 0..1]`.
    pub fn pan(
        &mut self,
        ax: &Axes,
        ax_ev: &Axes,
        ev_hold: bool,
        ev_ds_diff: f64,
        ev_unaligned: bool,
        have_rears: bool,
    ) -> [f64; 4] {
        let x = ax.lr.abs();
        let mut r = [1.0, 0.0, 0.0, 0.0];
        if self.mode == DirectPathMode::Event {
            let x_ev = ax_ev.lr.abs();
            if self.enable == 0 && ev_hold && !ev_unaligned {
                let eb = (-0.45 * x_ev + 0.8) * x_ev * x_ev - (PI / 8.0);
                if ax_ev.cs <= eb {
                    if ax.cs >= 0.0 {
                        self.zs.set(1.0);
                    }
                    self.enable = 1;
                }
            }
            if self.enable != 0 {
                let y0 = (-0.05 * x + 0.17) * x * x - (PI / 32.0);
                let y1 = (-0.15 * x + 0.37) * x * x - (PI / 12.0);
                if ev_hold {
                    let y1_ev = (-0.15 * x_ev + 0.37) * x_ev * x_ev - (PI / 12.0);
                    if ax_ev.cs >= y1_ev || ev_unaligned {
                        if ax.cs >= 0.0 {
                            return self.finish();
                        }
                        self.enable = 2;
                    } else if self.enable > 1 && ax_ev.cs < y1_ev && !ev_unaligned {
                        self.enable = 1;
                    }
                } else if ax.cs >= y0 {
                    return self.finish();
                }
                let zs = self
                    .zs
                    .run_scale(if self.enable == 1 { 1.0 } else { 0.0 }, ev_ds_diff);
                if self.enable > 1 && zs < 1e-4 {
                    return self.finish();
                }
                let m = FRAC_PI_2 / (y1 - y0);
                let z = ((ax.cs - y0) * m).max(0.0).min(FRAC_PI_2) * zs;
                r[0] = z.cos();
                r[1] = z.sin();
                r[3] = z * (1.0 / FRAC_PI_2);
                if have_rears {
                    let y2 = x * (-1.22) + (PI / 16.0);
                    let y3 = -x - (PI / 8.0);
                    let m2 = FRAC_PI_2 / (y3 - y2);
                    let g = r[1];
                    let z2 = ((ax.cs - y2) * m2).max(0.0).min(FRAC_PI_2);
                    r[1] = g * z2.cos();
                    r[2] = g * z2.sin();
                }
            }
        } else if self.mode == DirectPathMode::Static {
            let y0 = (-0.45 * x + 0.8) * x * x - (PI / 11.0);
            let y1 = (-0.52 * x + 0.93) * x * x - (PI / 8.0);
            let m = FRAC_PI_2 / (y1 - y0);
            let z = ((ax.cs - y0) * m).max(0.0).min(FRAC_PI_2);
            r[0] = z.cos();
            r[1] = z.sin();
            r[3] = z * (1.0 / FRAC_PI_2);
            if have_rears {
                let y2 = x * (-1.0 / 2.0) - (PI / 66.0);
                let y3 = x * (-1.0 / 3.0) - (PI / 6.0);
                let m2 = FRAC_PI_2 / (y3 - y2);
                let g = r[1];
                let z2 = ((ax.cs - y2) * m2).max(0.0).min(FRAC_PI_2);
                r[1] = g * z2.cos();
                r[2] = g * z2.sin();
            }
        }
        r
    }
}

/// `phase_flip_init_params`
pub(crate) fn phase_flip_params(fs: f64) -> [f64; 2] {
    [0.667829372575655, (0.0005 * (44100.0 / fs)).ln()]
}

/// `phase_flip_pos_rs`
pub(crate) fn phase_flip_pos_rs(ax: &Axes, rs: f64) -> f64 {
    if ax.cs >= 0.0 {
        return 0.5;
    }
    let x = ax.lr.abs();
    let y = ax.cs * rs / ((-0.45 * x + 0.8) * x * x - (PI / 8.0));
    let z = y * y * 0.5 + 0.5;
    z.min(rs)
}

/// `phase_flip_ap1_c0`
pub(crate) fn phase_flip_ap1_c0(pf: &[f64; 2], pos: f64) -> f64 {
    (pos * (pf[1] - pf[0]) + pf[0]).exp() - 1.0
}

/// `CALC_NORM_MULT`
pub(crate) fn calc_norm_mult(x: f64) -> f64 {
    1.0 / (1.0 + x * x).sqrt()
}

/// A slowly changing coefficient made smooth by interpolating between the values given every
/// `DOWNSAMPLE_FACTOR` samples (`cs_interp`, parabolic 2x; Niemitalo 2001).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CsInterp {
    c: [f64; 3],
    y: [f64; 4],
}

pub(crate) const DOWNSAMPLE_FACTOR: usize = 32;
/// `CS_INTERP_DELAY_FRAMES`
pub(crate) const CS_INTERP_DELAY_FRAMES: isize = 3 * DOWNSAMPLE_FACTOR as isize;

impl CsInterp {
    pub fn insert(&mut self, x: f64) {
        self.y.copy_within(1..4, 0);
        self.y[3] = x;
        let y = &self.y;
        let a = y[2] - y[0];
        self.c[0] = (1.0 / 2.0) * y[1] + (1.0 / 4.0) * (y[0] + y[2]);
        self.c[1] = (1.0 / 2.0) * a;
        self.c[2] = (1.0 / 4.0) * (y[3] - y[1] - a);
    }
    pub fn set(&mut self, x: f64) {
        for _ in 0..4 {
            self.insert(x);
        }
    }
    pub fn at(&self, x: usize) -> f64 {
        let t = x as f64 * (1.0 / DOWNSAMPLE_FACTOR as f64);
        (self.c[2] * t + self.c[1]) * t + self.c[0]
    }
}

/// A shelf whose gain follows the signal (`dyn_shelf_state`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DynShelf {
    c2: f64,
    norm: f64,
    sin_w0: f64,
    cos_w0_p1: f64,
    m0: f64,
}

impl DynShelf {
    pub fn new(fs: f64, f0: f64) -> Self {
        let w0 = 2.0 * PI * f0 / fs;
        let sin_w0 = w0.sin();
        let cos_w0_p1 = w0.cos() + 1.0;
        let norm = 1.0 / (sin_w0 + cos_w0_p1);
        Self {
            sin_w0,
            cos_w0_p1,
            norm,
            c2: (sin_w0 - cos_w0_p1) * norm,
            m0: 0.0,
        }
    }
    pub fn run(&mut self, s: f64, g: f64) -> f64 {
        let sn = s * self.norm;
        let gcp1 = g * self.cos_w0_p1;
        let c0s = (self.sin_w0 + gcp1) * sn;
        let c1s = (self.sin_w0 - gcp1) * sn;
        let r = c0s + self.m0;
        self.m0 = c1s - (self.c2 * r);
        r
    }
}
