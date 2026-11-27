//! The filter bank of the multiband matrix decoder (`matrix4_filter_bank.c`): 13 or 21
//! bands split by a tree of all-pass crossovers.
//!
//! Two banks run side by side, one per channel, for the signal that is decoded (synthesis;
//! the band outputs add back to a phase-shifted copy of the input) and a decimating
//! analysis bank whose bands are sampled at a lower rate the lower their frequency. A
//! short linear-phase FIR (see `matrix4_mb`) undoes the phase shift of the sum.
//!
//! Logic Pro equivalent: none; Logic's multiband tools use plain crossovers.

use std::f64::consts::PI;

use crate::biquad::{self, Biquad, Width};
use crate::capn::{capn_ap, Cap3, Cap5, Cap7, Crossover, FilterType, Section};
use crate::matrix4_common::DOWNSAMPLE_FACTOR;

/// The most bands of any filter bank.
pub(crate) const FB_MAX_BANDS: usize = 21;

const FB_FDIV_13BAND: [f64; 14] = [
    62.914, 170.0, 316.39, 516.52, 790.1, 1164.1, 1675.4, 2374.3, 3329.8, 4636.1, 6421.7, 8862.9,
    12200.0, 16762.0,
];
const FB_FC_13BAND: [f64; 13] = [
    112.28, 237.49, 408.65, 642.64, 962.52, 1399.8, 1997.6, 2814.8, 3932.0, 5459.3, 7547.1,
    10401.0, 14303.0,
];
const FB_AP_IDX_13BAND: [usize; 25] = [
    6, 4, 7, 3, 8, 2, 9, 1, 10, 0, 11, 3, 1, 9, 7, 4, 0, 10, 6, 11, 1, 4, 7, 11, 9,
];
const FB_STOP_SCALE_13BAND: [f64; 12] =
    [1.5, 1.2, 1.1, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
const FB_THRESH_SCALE_13BAND: [f64; 13] = [
    1.06, 0.936, 0.792, 0.695, 0.634, 0.585, 0.539, 0.512, 0.484, 0.451, 0.433, 0.431, 0.4,
];
const AN_FB_TPEAK_13BAND: [f64; 13] = [
    5.98, 4.31, 2.98, 1.98, 1.31, 0.896, 0.646, 0.479, 0.312, 0.229, 0.146, 0.104, 0.0625,
];
const FB_FDIV_13BAND_LFX: [f64; 14] = [
    20.05, 115.0, 246.17, 427.39, 677.74, 1023.6, 1501.4, 2161.5, 3073.4, 4333.3, 6073.8, 8478.2,
    11800.0, 16389.0,
];
const FB_FC_13BAND_LFX: [f64; 13] = [
    63.698, 175.3, 329.48, 542.48, 836.73, 1243.3, 1804.9, 2580.7, 3652.6, 5133.4, 7179.1, 10005.0,
    13910.0,
];
const FB_STOP_SCALE_13BAND_LFX: [f64; 12] =
    [1.7, 1.3, 1.2, 1.1, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
const FB_THRESH_SCALE_13BAND_LFX: [f64; 13] = [
    1.22, 1.01, 0.841, 0.721, 0.649, 0.599, 0.549, 0.517, 0.49, 0.458, 0.435, 0.432, 0.4,
];
const AN_FB_TPEAK_13BAND_LFX: [f64; 13] = [
    7.31, 5.31, 3.31, 2.15, 1.48, 1.06, 0.729, 0.479, 0.354, 0.229, 0.167, 0.104, 0.0625,
];
const FB_FDIV_21BAND: [f64; 22] = [
    20.021, 78.0, 149.49, 237.63, 346.3, 480.3, 645.52, 849.22, 1100.4, 1410.1, 1791.9, 2262.7,
    2843.2, 3558.9, 4441.4, 5529.5, 6871.1, 8525.2, 10565.0, 13079.0, 16180.0, 20003.0,
];
const FB_FC_21BAND: [f64; 21] = [
    47.494, 111.87, 191.25, 289.12, 409.8, 558.59, 742.04, 968.24, 1247.1, 1591.0, 2015.0, 2537.8,
    3182.3, 3977.1, 4957.0, 6165.2, 7654.9, 9491.6, 11756.0, 14549.0, 17991.0,
];
const FB_AP_IDX_21BAND: [usize; 54] = [
    10, 8, 11, 7, 12, 6, 13, 5, 14, 4, 15, 3, 16, 2, 17, 1, 18, 0, 19, 5, 3, 15, 13, 6, 2, 16, 12,
    7, 1, 17, 11, 8, 0, 18, 10, 19, 2, 0, 7, 5, 12, 10, 18, 16, 3, 8, 13, 19, 15, 3, 8, 13, 16, 19,
];
const FB_STOP_SCALE_21BAND: [f64; 20] = [
    2.0, 1.5, 1.3, 1.2, 1.1, 1.1, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0,
];
const FB_THRESH_SCALE_21BAND: [f64; 21] = [
    1.31, 1.19, 1.1, 0.986, 0.899, 0.844, 0.795, 0.739, 0.719, 0.68, 0.637, 0.627, 0.609, 0.579,
    0.56, 0.53, 0.503, 0.502, 0.502, 0.5, 0.5,
];
const AN_FB_TPEAK_21BAND: [f64; 21] = [
    18.0, 15.3, 12.6, 8.65, 6.31, 5.65, 4.31, 3.31, 2.65, 2.15, 1.65, 1.31, 1.06, 0.812, 0.646,
    0.438, 0.396, 0.25, 0.229, 0.208, 0.0625,
];
const FB_AP_IDX_13BAND_LFX: [usize; 25] = FB_AP_IDX_13BAND;

/// Which filter bank (`enum filter_bank_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FbId {
    Band21,
    Band13,
}

/// `struct filter_bank_params`
#[derive(Debug)]
pub(crate) struct FilterBankParams {
    pub id_str: &'static str,
    pub id: FbId,
    pub n_bands: usize,
    pub min_fs: u32,
    /// The crossover frequencies, with one extra at the start: `fdiv(i)` is the upper edge
    /// of band `i`.
    fdiv_all: &'static [f64],
    pub fc: &'static [f64],
    stop_scale: &'static [f64],
    pub thresh_scale: &'static [f64],
    pub an_tpeak: &'static [f64],
    ap_idx: &'static [usize],
    /// Length in ms of the linear-phase correction.
    pub fir_len: f64,
    an_stop: f64,
}

impl FilterBankParams {
    /// `fdiv[i]`: the upper edge of band `i`.
    pub fn fdiv(&self, i: usize) -> f64 {
        self.fdiv_all[i + 1]
    }
    /// `fdiv[i-1]`, which is the lower edge of the first band at `i = 0`.
    pub fn fdiv_lo(&self, i: usize) -> f64 {
        self.fdiv_all[i]
    }
    fn order(&self) -> usize {
        match self.id {
            FbId::Band21 => 7,
            FbId::Band13 => 5,
        }
    }
}

pub(crate) static FB_PARAMS: [FilterBankParams; 3] = [
    FilterBankParams {
        id_str: "21band",
        id: FbId::Band21,
        n_bands: 21,
        min_fs: 40000,
        fdiv_all: &FB_FDIV_21BAND,
        fc: &FB_FC_21BAND,
        stop_scale: &FB_STOP_SCALE_21BAND,
        thresh_scale: &FB_THRESH_SCALE_21BAND,
        an_tpeak: &AN_FB_TPEAK_21BAND,
        ap_idx: &FB_AP_IDX_21BAND,
        fir_len: 160.0,
        an_stop: 27.0,
    },
    FilterBankParams {
        id_str: "13band",
        id: FbId::Band13,
        n_bands: 13,
        min_fs: 32000,
        fdiv_all: &FB_FDIV_13BAND,
        fc: &FB_FC_13BAND,
        stop_scale: &FB_STOP_SCALE_13BAND,
        thresh_scale: &FB_THRESH_SCALE_13BAND,
        an_tpeak: &AN_FB_TPEAK_13BAND,
        ap_idx: &FB_AP_IDX_13BAND,
        fir_len: 50.0,
        an_stop: 18.0,
    },
    FilterBankParams {
        id_str: "13band_lfx",
        id: FbId::Band13,
        n_bands: 13,
        min_fs: 32000,
        fdiv_all: &FB_FDIV_13BAND_LFX,
        fc: &FB_FC_13BAND_LFX,
        stop_scale: &FB_STOP_SCALE_13BAND_LFX,
        thresh_scale: &FB_THRESH_SCALE_13BAND_LFX,
        an_tpeak: &AN_FB_TPEAK_13BAND_LFX,
        ap_idx: &FB_AP_IDX_13BAND_LFX,
        fir_len: 70.0,
        an_stop: 18.0,
    },
];

/// The filter that shapes the analysis input (`struct fshape_state`): a high-pass at 180 Hz
/// and a gentle low-pass transform near Nyquist.
#[derive(Clone, Debug)]
pub(crate) struct FShape {
    lf: Biquad,
    hf: Biquad,
}

impl FShape {
    pub fn new(fs: f64) -> Self {
        Self {
            lf: Biquad::from_type(biquad::HIGHPASS, fs, 180.0, 0.4, 0.0, 0.0, Width::Q),
            hf: Biquad::from_type(
                biquad::LOWPASS_TRANSFORM,
                fs,
                0.46 * fs,
                0.35,
                (0.9 * 0.46 * fs).min(16000.0),
                0.35,
                Width::Q,
            ),
        }
    }
    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let v = self.lf.run(s);
        self.hf.run(v)
    }
}

/// `get_stop_scale`
fn get_stop_scale(fbp: &FilterBankParams, i: usize, fs: f64) -> f64 {
    let mut hf_warp = ((PI * fbp.fdiv(i) / fs).tan() + 1.9).ln() * 1.142245;
    if hf_warp < 1.0 {
        hf_warp = 1.0;
    }
    let sc = fbp.stop_scale.get(i).copied().unwrap_or(1.0);
    (sc * hf_warp * 10.0).round_ties_even() / 10.0
}

/// One synthesis tree (`struct syn_filter_bank`).
#[derive(Clone)]
struct Syn<C: Crossover> {
    f: Vec<C>,
    ap: Vec<C::A1>,
    s: [f64; FB_MAX_BANDS],
}

/// The filter bank with crossovers of order `S` for synthesis and of the lower order `A` for
/// analysis.
#[derive(Clone)]
pub(crate) struct Bank<S: Crossover, A: Crossover> {
    syn: [Syn<S>; 2],
    an_f: Vec<[A; 2]>,
    /// The band signals of the analysis bank (valid when `p & mask[band] == 0`).
    pub an_s: [[f64; 2]; FB_MAX_BANDS],
    pub an_p: u32,
    pub an_mask: [u32; FB_MAX_BANDS],
    id: FbId,
    n_bands: usize,
}

/// Either of the two orders in use.
#[derive(Clone)]
pub(crate) enum FilterBank {
    Order5(Box<Bank<Cap5, Cap3>>),
    Order7(Box<Bank<Cap7, Cap5>>),
}

impl FilterBank {
    /// `filter_bank_init`. `None` when a filter could not be designed.
    pub fn new(
        fbp: &FilterBankParams,
        fs: f64,
        fb_type: FilterType,
        fb_stop: [f64; 2],
    ) -> Option<Self> {
        Some(match fbp.order() {
            5 => Self::Order5(Box::new(Bank::new(fbp, fs, fb_type, fb_stop)?)),
            _ => Self::Order7(Box::new(Bank::new(fbp, fs, fb_type, fb_stop)?)),
        })
    }
}

macro_rules! each {
    ($self:expr, $b:ident => $e:expr) => {
        match $self {
            FilterBank::Order5($b) => $e,
            FilterBank::Order7($b) => $e,
        }
    };
}

impl FilterBank {
    pub fn reset(&mut self) {
        each!(self, b => b.reset())
    }
    pub fn run_syn(&mut self, s: [f64; 2]) {
        each!(self, b => b.run_syn(s))
    }
    pub fn run_an(&mut self, s: [f64; 2]) {
        each!(self, b => b.run_an(s))
    }
    /// The output of band `k` of synthesis bank `ch`.
    pub fn syn_s(&self, ch: usize, k: usize) -> f64 {
        each!(self, b => b.syn[ch].s[k])
    }
    pub fn sum(&mut self, s: f64) -> f64 {
        each!(self, b => b.sum(s))
    }
    pub fn an_s(&self, k: usize) -> [f64; 2] {
        each!(self, b => b.an_s[k])
    }
    pub fn an_p(&self) -> u32 {
        each!(self, b => b.an_p)
    }
    pub fn an_mask(&self, k: usize) -> u32 {
        each!(self, b => b.an_mask[k])
    }
}

impl<S: Crossover, A: Crossover> Bank<S, A> {
    fn new(
        fbp: &FilterBankParams,
        fs: f64,
        fb_type: FilterType,
        fb_stop: [f64; 2],
    ) -> Option<Self> {
        let n = fbp.n_bands;
        let an_tr_bw = match S::ORDER {
            5 => 2.4,
            _ => 1.6,
        };
        let mut an_mask = [0u32; FB_MAX_BANDS];
        let mut an_fs_div = 1u32;
        for i in (0..n - 2).rev() {
            while fbp.fdiv(i) * an_tr_bw * f64::from(an_fs_div * 4) < fs
                && an_fs_div <= (DOWNSAMPLE_FACTOR / 2) as u32
            {
                an_fs_div *= 2;
            }
            an_mask[i] = an_fs_div - 1;
        }
        let mut f = Vec::with_capacity(n - 1);
        let mut an_f = Vec::with_capacity(n - 1);
        let mut prev_sc = 0.0;
        let mut ap = Vec::new();
        for i in 0..n - 1 {
            let an_fs = fs / f64::from(an_mask[i + 1] + 1);
            let an_st_sc = [fbp.an_stop * get_stop_scale(fbp, i, an_fs), 0.0];
            let an_ap = capn_ap(FilterType::Chebyshev2, S::ORDER - 2, an_st_sc)?;
            let sc = get_stop_scale(fbp, i, fs);
            if sc != prev_sc {
                ap = capn_ap(fb_type, S::ORDER, [fb_stop[0] * sc, fb_stop[1] * sc])?;
                prev_sc = sc;
            }
            f.push(S::new(fs, fbp.fdiv(i), &ap));
            let a = A::new(an_fs, fbp.fdiv(i), &an_ap);
            an_f.push([a, a]);
        }
        let ap_states: Vec<S::A1> = fbp.ap_idx.iter().map(|&i| f[i].a1()).collect();
        let syn0 = Syn {
            f,
            ap: ap_states,
            s: [0.0; FB_MAX_BANDS],
        };
        Some(Self {
            syn: [syn0.clone(), syn0],
            an_f,
            an_s: [[0.0; 2]; FB_MAX_BANDS],
            an_p: 0,
            an_mask,
            id: fbp.id,
            n_bands: n,
        })
    }

    fn reset(&mut self) {
        self.an_p = 0;
        for syn in &mut self.syn {
            for f in &mut syn.f {
                f.reset();
            }
            for a in &mut syn.ap {
                a.reset();
            }
            syn.s = [0.0; FB_MAX_BANDS];
        }
        for f in &mut self.an_f {
            f[0].reset();
            f[1].reset();
        }
        self.an_s = [[0.0; 2]; FB_MAX_BANDS];
    }

    fn run_syn(&mut self, s: [f64; 2]) {
        for (ch, syn) in self.syn.iter_mut().enumerate() {
            match self.id {
                FbId::Band21 => syn_21band(syn, s[ch]),
                FbId::Band13 => syn_13band(syn, s[ch]),
            }
        }
    }

    /// `filter_bank_sum`: runs `s` through the first all-pass of every crossover, which is the
    /// phase response of the sum of the bands.
    fn sum(&mut self, mut s: f64) -> f64 {
        for f in &mut self.syn[0].f[..self.n_bands - 1] {
            s = f.a1_mut().run(s);
        }
        s
    }

    fn run_an(&mut self, s: [f64; 2]) {
        self.an_p = self.an_p.wrapping_add(1);
        let top = self.n_bands - 1;
        self.an_s[top] = s;
        let mut i = top;
        while i > 0 && self.an_p & self.an_mask[i] == 0 {
            for ch in 0..2 {
                let (lp, hp) = self.an_f[i - 1][ch].run(self.an_s[i][ch]);
                self.an_s[i - 1][ch] = lp;
                self.an_s[i][ch] = hp;
            }
            i -= 1;
        }
    }
}

#[inline]
fn syn_21band<C: Crossover>(fb: &mut Syn<C>, s: f64) {
    let (lp, hp) = fb.f[9].run(s); // split at xover 9 (1791.9Hz)
    fb.s[9] = lp;
    fb.s[10] = hp;
    fb.s[9] = fb.ap[0].run(fb.s[9]); // xover 10 ap
    fb.s[10] = fb.ap[1].run(fb.s[10]); // xover 8 ap
    fb.s[9] = fb.ap[2].run(fb.s[9]); // xover 11 ap
    fb.s[10] = fb.ap[3].run(fb.s[10]); // xover 7 ap
    fb.s[9] = fb.ap[4].run(fb.s[9]); // xover 12 ap
    fb.s[10] = fb.ap[5].run(fb.s[10]); // xover 6 ap
    fb.s[9] = fb.ap[6].run(fb.s[9]); // xover 13 ap
    fb.s[10] = fb.ap[7].run(fb.s[10]); // xover 5 ap
    fb.s[9] = fb.ap[8].run(fb.s[9]); // xover 14 ap
    fb.s[10] = fb.ap[9].run(fb.s[10]); // xover 4 ap
    fb.s[9] = fb.ap[10].run(fb.s[9]); // xover 15 ap
    fb.s[10] = fb.ap[11].run(fb.s[10]); // xover 3 ap
    fb.s[9] = fb.ap[12].run(fb.s[9]); // xover 16 ap
    fb.s[10] = fb.ap[13].run(fb.s[10]); // xover 2 ap
    fb.s[9] = fb.ap[14].run(fb.s[9]); // xover 17 ap
    fb.s[10] = fb.ap[15].run(fb.s[10]); // xover 1 ap
    fb.s[9] = fb.ap[16].run(fb.s[9]); // xover 18 ap
    fb.s[10] = fb.ap[17].run(fb.s[10]); // xover 0 ap
    fb.s[9] = fb.ap[18].run(fb.s[9]); // xover 19 ap

    let (lp, hp) = fb.f[4].run(fb.s[9]); // split at xover 4 (480.3Hz)
    fb.s[4] = lp;
    fb.s[5] = hp;
    let (lp, hp) = fb.f[14].run(fb.s[10]); // split at xover 14 (5529.5Hz)
    fb.s[14] = lp;
    fb.s[15] = hp;
    fb.s[4] = fb.ap[19].run(fb.s[4]); // xover 5 ap
    fb.s[5] = fb.ap[20].run(fb.s[5]); // xover 3 ap
    fb.s[14] = fb.ap[21].run(fb.s[14]); // xover 15 ap
    fb.s[15] = fb.ap[22].run(fb.s[15]); // xover 13 ap
    fb.s[4] = fb.ap[23].run(fb.s[4]); // xover 6 ap
    fb.s[5] = fb.ap[24].run(fb.s[5]); // xover 2 ap
    fb.s[14] = fb.ap[25].run(fb.s[14]); // xover 16 ap
    fb.s[15] = fb.ap[26].run(fb.s[15]); // xover 12 ap
    fb.s[4] = fb.ap[27].run(fb.s[4]); // xover 7 ap
    fb.s[5] = fb.ap[28].run(fb.s[5]); // xover 1 ap
    fb.s[14] = fb.ap[29].run(fb.s[14]); // xover 17 ap
    fb.s[15] = fb.ap[30].run(fb.s[15]); // xover 11 ap
    fb.s[4] = fb.ap[31].run(fb.s[4]); // xover 8 ap
    fb.s[5] = fb.ap[32].run(fb.s[5]); // xover 0 ap
    fb.s[14] = fb.ap[33].run(fb.s[14]); // xover 18 ap
    fb.s[15] = fb.ap[34].run(fb.s[15]); // xover 10 ap
    fb.s[14] = fb.ap[35].run(fb.s[14]); // xover 19 ap

    let (lp, hp) = fb.f[1].run(fb.s[4]); // split at xover 1 (149.49Hz)
    fb.s[1] = lp;
    fb.s[2] = hp;
    let (lp, hp) = fb.f[6].run(fb.s[5]); // split at xover 6 (849.22Hz)
    fb.s[6] = lp;
    fb.s[7] = hp;
    let (lp, hp) = fb.f[11].run(fb.s[14]); // split at xover 11 (2843.2Hz)
    fb.s[11] = lp;
    fb.s[12] = hp;
    let (lp, hp) = fb.f[17].run(fb.s[15]); // split at xover 17 (10565Hz)
    fb.s[17] = lp;
    fb.s[18] = hp;
    fb.s[1] = fb.ap[36].run(fb.s[1]); // xover 2 ap
    fb.s[2] = fb.ap[37].run(fb.s[2]); // xover 0 ap
    fb.s[6] = fb.ap[38].run(fb.s[6]); // xover 7 ap
    fb.s[7] = fb.ap[39].run(fb.s[7]); // xover 5 ap
    fb.s[11] = fb.ap[40].run(fb.s[11]); // xover 12 ap
    fb.s[12] = fb.ap[41].run(fb.s[12]); // xover 10 ap
    fb.s[17] = fb.ap[42].run(fb.s[17]); // xover 18 ap
    fb.s[18] = fb.ap[43].run(fb.s[18]); // xover 16 ap
    fb.s[1] = fb.ap[44].run(fb.s[1]); // xover 3 ap
    fb.s[6] = fb.ap[45].run(fb.s[6]); // xover 8 ap
    fb.s[11] = fb.ap[46].run(fb.s[11]); // xover 13 ap
    fb.s[17] = fb.ap[47].run(fb.s[17]); // xover 19 ap
    fb.s[18] = fb.ap[48].run(fb.s[18]); // xover 15 ap

    let (lp, hp) = fb.f[0].run(fb.s[1]); // split at xover 0 (78Hz)
    fb.s[0] = lp;
    fb.s[1] = hp;
    let (lp, hp) = fb.f[2].run(fb.s[2]); // split at xover 2 (237.63Hz)
    fb.s[2] = lp;
    fb.s[3] = hp;
    let (lp, hp) = fb.f[5].run(fb.s[6]); // split at xover 5 (645.52Hz)
    fb.s[5] = lp;
    fb.s[6] = hp;
    let (lp, hp) = fb.f[7].run(fb.s[7]); // split at xover 7 (1100.4Hz)
    fb.s[7] = lp;
    fb.s[8] = hp;
    let (lp, hp) = fb.f[10].run(fb.s[11]); // split at xover 10 (2262.7Hz)
    fb.s[10] = lp;
    fb.s[11] = hp;
    let (lp, hp) = fb.f[12].run(fb.s[12]); // split at xover 12 (3558.9Hz)
    fb.s[12] = lp;
    fb.s[13] = hp;
    let (lp, hp) = fb.f[15].run(fb.s[17]); // split at xover 15 (6871.1Hz)
    fb.s[15] = lp;
    fb.s[16] = hp;
    let (lp, hp) = fb.f[18].run(fb.s[18]); // split at xover 18 (13079Hz)
    fb.s[18] = lp;
    fb.s[19] = hp;
    fb.s[2] = fb.ap[49].run(fb.s[2]); // xover 3 ap
    fb.s[7] = fb.ap[50].run(fb.s[7]); // xover 8 ap
    fb.s[12] = fb.ap[51].run(fb.s[12]); // xover 13 ap
    fb.s[15] = fb.ap[52].run(fb.s[15]); // xover 16 ap
    fb.s[18] = fb.ap[53].run(fb.s[18]); // xover 19 ap

    let (lp, hp) = fb.f[3].run(fb.s[3]); // split at xover 3 (346.3Hz)
    fb.s[3] = lp;
    fb.s[4] = hp;
    let (lp, hp) = fb.f[8].run(fb.s[8]); // split at xover 8 (1410.1Hz)
    fb.s[8] = lp;
    fb.s[9] = hp;
    let (lp, hp) = fb.f[13].run(fb.s[13]); // split at xover 13 (4441.4Hz)
    fb.s[13] = lp;
    fb.s[14] = hp;
    let (lp, hp) = fb.f[16].run(fb.s[16]); // split at xover 16 (8525.2Hz)
    fb.s[16] = lp;
    fb.s[17] = hp;
    let (lp, hp) = fb.f[19].run(fb.s[19]); // split at xover 19 (16180Hz)
    fb.s[19] = lp;
    fb.s[20] = hp;
}

#[inline]
fn syn_13band<C: Crossover>(fb: &mut Syn<C>, s: f64) {
    let (lp, hp) = fb.f[5].run(s); // split at xover 5 (1675.4Hz)
    fb.s[5] = lp;
    fb.s[6] = hp;
    fb.s[5] = fb.ap[0].run(fb.s[5]); // xover 6 ap
    fb.s[6] = fb.ap[1].run(fb.s[6]); // xover 4 ap
    fb.s[5] = fb.ap[2].run(fb.s[5]); // xover 7 ap
    fb.s[6] = fb.ap[3].run(fb.s[6]); // xover 3 ap
    fb.s[5] = fb.ap[4].run(fb.s[5]); // xover 8 ap
    fb.s[6] = fb.ap[5].run(fb.s[6]); // xover 2 ap
    fb.s[5] = fb.ap[6].run(fb.s[5]); // xover 9 ap
    fb.s[6] = fb.ap[7].run(fb.s[6]); // xover 1 ap
    fb.s[5] = fb.ap[8].run(fb.s[5]); // xover 10 ap
    fb.s[6] = fb.ap[9].run(fb.s[6]); // xover 0 ap
    fb.s[5] = fb.ap[10].run(fb.s[5]); // xover 11 ap

    let (lp, hp) = fb.f[2].run(fb.s[5]); // split at xover 2 (516.52Hz)
    fb.s[2] = lp;
    fb.s[3] = hp;
    let (lp, hp) = fb.f[8].run(fb.s[6]); // split at xover 8 (4636.1Hz)
    fb.s[8] = lp;
    fb.s[9] = hp;
    fb.s[2] = fb.ap[11].run(fb.s[2]); // xover 3 ap
    fb.s[3] = fb.ap[12].run(fb.s[3]); // xover 1 ap
    fb.s[8] = fb.ap[13].run(fb.s[8]); // xover 9 ap
    fb.s[9] = fb.ap[14].run(fb.s[9]); // xover 7 ap
    fb.s[2] = fb.ap[15].run(fb.s[2]); // xover 4 ap
    fb.s[3] = fb.ap[16].run(fb.s[3]); // xover 0 ap
    fb.s[8] = fb.ap[17].run(fb.s[8]); // xover 10 ap
    fb.s[9] = fb.ap[18].run(fb.s[9]); // xover 6 ap
    fb.s[8] = fb.ap[19].run(fb.s[8]); // xover 11 ap

    let (lp, hp) = fb.f[0].run(fb.s[2]); // split at xover 0 (170Hz)
    fb.s[0] = lp;
    fb.s[1] = hp;
    let (lp, hp) = fb.f[3].run(fb.s[3]); // split at xover 3 (790.1Hz)
    fb.s[3] = lp;
    fb.s[4] = hp;
    let (lp, hp) = fb.f[6].run(fb.s[8]); // split at xover 6 (2374.3Hz)
    fb.s[6] = lp;
    fb.s[7] = hp;
    let (lp, hp) = fb.f[10].run(fb.s[9]); // split at xover 10 (8862.9Hz)
    fb.s[10] = lp;
    fb.s[11] = hp;
    fb.s[0] = fb.ap[20].run(fb.s[0]); // xover 1 ap
    fb.s[3] = fb.ap[21].run(fb.s[3]); // xover 4 ap
    fb.s[6] = fb.ap[22].run(fb.s[6]); // xover 7 ap
    fb.s[10] = fb.ap[23].run(fb.s[10]); // xover 11 ap
    fb.s[11] = fb.ap[24].run(fb.s[11]); // xover 9 ap

    let (lp, hp) = fb.f[1].run(fb.s[1]); // split at xover 1 (316.39Hz)
    fb.s[1] = lp;
    fb.s[2] = hp;
    let (lp, hp) = fb.f[4].run(fb.s[4]); // split at xover 4 (1164.1Hz)
    fb.s[4] = lp;
    fb.s[5] = hp;
    let (lp, hp) = fb.f[7].run(fb.s[7]); // split at xover 7 (3329.8Hz)
    fb.s[7] = lp;
    fb.s[8] = hp;
    let (lp, hp) = fb.f[9].run(fb.s[10]); // split at xover 9 (6421.7Hz)
    fb.s[9] = lp;
    fb.s[10] = hp;
    let (lp, hp) = fb.f[11].run(fb.s[11]); // split at xover 11 (12200Hz)
    fb.s[11] = lp;
    fb.s[12] = hp;
}
