//! `resample` (`resample.c`): a sample-rate converter that convolves with a windowed sinc
//! in the frequency domain, in blocks that hold a whole number of the ratio's periods.
//!
//! The ratio `n/d` is the output rate over the input rate, in lowest terms; a block of
//! `d * len_mult` input frames makes a block of `n * len_mult` output frames. The filter is
//! an Albrecht 9-term cosine-sum windowed sinc, as wide as the transition band
//! (`bandwidth`, default 0.939) asks for. Pro Wave's engine has its own converters; this one
//! keeps the original's numbers.

use realfft::num_complex::Complex;

use crate::args::{get_freq, get_number, strtol};
use crate::effect::{gcd, ratio_mult_ceil, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir::{next_fast_len, Fft};
use crate::window::{albrecht9c3, norm_sinc};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "resample",
    usage: "[bandwidth] fs[k]|x{mult}|/{div}",
    init,
    number: 0,
}];

const DEFAULT_BANDWIDTH: f64 = 0.939;
const SINC_MAX_OVERSAMPLE: i64 = 2;
const M_FACT: f64 = 17.7822;
/// Longest block the converter will make (a coprime pair of rates such as 44101 and 48000
/// would need blocks of millions of frames).
const MAX_BLOCK: i64 = 1 << 24;

struct Resample {
    stream_in: Stream,
    stream_out: Stream,
    ratio_n: i64,
    ratio_d: i64,
    sinc_fr_len: usize,
    in_len: usize,
    out_len: usize,
    in_buf_pos: usize,
    out_buf_pos: usize,
    drain_pos: isize,
    drain_frames: isize,
    out_delay: usize,
    sinc_fr: Vec<Complex<f64>>,
    tmp_fr: Vec<Complex<f64>>,
    tmp_fr_2: Vec<Complex<f64>>,
    input: Vec<Vec<f64>>,
    work: Vec<f64>,
    output: Vec<Vec<f64>>,
    overlap: Vec<Vec<f64>>,
    fft: Fft,
    has_output: bool,
    is_draining: bool,
}

impl Resample {
    /// Resamples the block in `input[ch]` into `output[ch]` and the overlap.
    fn convert_block(&mut self, ch: usize) {
        let (in_len, out_len) = (self.in_len, self.out_len);
        // FFT(input) -> tmp_fr (the transform uses up its input, so it works on a copy)
        self.work[..in_len].copy_from_slice(&self.input[ch][..in_len]);
        self.work[in_len..].fill(0.0);
        let bins = in_len + 1;
        self.fft.forward(&mut self.work, &mut self.tmp_fr[..bins]);
        self.tmp_fr_2.fill(Complex::new(0.0, 0.0));
        // convolve the input with the sinc filter
        self.tmp_fr_2[0] = self.tmp_fr[0] * self.sinc_fr[0];
        let (mut k, mut j, mut l) = (1usize, 1isize, 1isize);
        let (mut d1, mut d2) = (1isize, 1isize);
        loop {
            let s = if d1 == 1 {
                self.tmp_fr[j as usize]
            } else {
                self.tmp_fr[j as usize].conj()
            };
            let prod = s * self.sinc_fr[k];
            self.tmp_fr_2[l as usize] += if d2 == 1 { prod } else { prod.conj() };
            if k + 1 == self.sinc_fr_len {
                break;
            }
            if l == out_len as isize {
                self.tmp_fr_2[l as usize] += prod;
            } else if l == 0 {
                self.tmp_fr_2[l as usize] += prod.conj();
            }
            j += d1;
            l += d2;
            if j == 0 {
                d1 = 1;
            } else if j == in_len as isize {
                d1 = -1;
            }
            if l == 0 {
                d2 = 1;
            } else if l == out_len as isize {
                d2 = -1;
            }
            k += 1;
        }
        // IFFT(tmp_fr_2) -> output
        let obins = out_len + 1;
        self.fft
            .inverse(&mut self.tmp_fr_2[..obins], &mut self.output[ch]);
        // normalise and add the overlap of the block before
        let norm = (in_len * 2) as f64;
        for v in self.output[ch].iter_mut() {
            *v /= norm;
        }
        let (head, tail) = self.output[ch].split_at_mut(out_len);
        for ((h, o), t) in head.iter_mut().zip(self.overlap[ch].iter_mut()).zip(tail) {
            *h += *o;
            *o = *t;
        }
    }
}

impl Effect for Resample {
    fn name(&self) -> &'static str {
        "resample"
    }
    fn istream(&self) -> Stream {
        self.stream_in
    }
    fn ostream(&self) -> Stream {
        self.stream_out
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf {
        let ch = self.stream_out.channels;
        let (mut iframes, mut oframes) = (0usize, 0usize);
        let max_oframes = ratio_mult_ceil(*frames as i64, self.ratio_n, self.ratio_d) as usize;
        while iframes < *frames {
            while self.in_buf_pos < self.in_len && iframes < *frames {
                for i in 0..ch {
                    self.input[i][self.in_buf_pos] = ibuf[iframes * ch + i];
                }
                iframes += 1;
                self.in_buf_pos += 1;
            }
            while self.out_buf_pos < self.out_len && oframes < max_oframes && self.has_output {
                for i in 0..ch {
                    obuf[oframes * ch + i] = self.output[i][self.out_buf_pos];
                }
                oframes += 1;
                self.out_buf_pos += 1;
            }
            if self.in_buf_pos == self.in_len
                && (!self.has_output || self.out_buf_pos == self.out_len)
            {
                for i in 0..ch {
                    self.convert_block(i);
                }
                self.in_buf_pos = 0;
                self.out_buf_pos = 0;
                if !self.has_output {
                    self.out_buf_pos = self.out_delay;
                    self.has_output = true;
                }
            }
        }
        *frames = oframes;
        Buf::Output
    }
    fn reset(&mut self) {
        self.in_buf_pos = 0;
        self.out_buf_pos = 0;
        self.has_output = false;
        for o in self.overlap.iter_mut() {
            o.fill(0.0);
        }
    }
    fn drain2(&mut self, frames: &mut isize, buf1: &mut [f64], buf2: &mut [f64]) -> Option<Buf> {
        let ch = self.stream_out.channels;
        let mut result = Buf::Input;
        if !self.has_output && self.in_buf_pos == 0 {
            *frames = -1;
        } else {
            if !self.is_draining {
                if self.has_output {
                    // the filter's delay and the output frames not yet taken
                    self.drain_frames += self.out_delay as isize;
                    self.drain_frames += (self.out_len - self.out_buf_pos) as isize;
                }
                // the input frames not yet converted
                self.drain_frames +=
                    ratio_mult_ceil(self.in_buf_pos as i64, self.ratio_n, self.ratio_d) as isize;
                self.is_draining = true;
            }
            if self.drain_pos < self.drain_frames {
                let n = (*frames).max(0) as usize;
                buf1[..n * ch].fill(0.0);
                let mut f = n;
                result = self.run(&mut f, buf1, buf2);
                *frames = f as isize;
                self.drain_pos += *frames;
                if self.drain_pos > self.drain_frames {
                    *frames -= self.drain_pos - self.drain_frames;
                }
            } else {
                *frames = -1;
            }
        }
        Some(result)
    }
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    _selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let name = argv[0];
    let argc = argv.len();
    if !(2..=3).contains(&argc) {
        return Err(info.usage_error());
    }
    let range = |param: &str| Error::Range {
        effect: name.into(),
        param: param.into(),
    };
    let (bw_arg, rate_arg) = if argc == 3 {
        (Some(argv[1]), argv[2])
    } else {
        (None, argv[1])
    };
    let mut bw = DEFAULT_BANDWIDTH;
    if let Some(a) = bw_arg {
        bw = get_number(name, a, "bandwidth")?;
        if !(0.0..=0.999).contains(&bw) {
            return Err(range("bandwidth"));
        }
    }
    let fs = i64::from(istream.fs);
    let whole = |text: &str, param: &str| -> Result<i64> {
        let (v, end) = strtol(text);
        if end == 0 || end != text.len() {
            return Err(Error::Parse {
                effect: name.into(),
                param: param.into(),
                text: rate_arg.into(),
            });
        }
        Ok(v)
    };
    let rate: i64 = if let Some(t) = rate_arg.strip_prefix('x') {
        fs.saturating_mul(whole(t, "fs multiplier")?)
    } else if let Some(t) = rate_arg.strip_prefix('/') {
        let div = whole(t, "fs divisor")?;
        if div == 0 || fs % div != 0 {
            return Err(Error::Message(format!(
                "{name}: error: {div} is not a factor of {fs}"
            )));
        }
        fs / div
    } else {
        get_freq(name, rate_arg, "fs")?.round() as i64
    };
    if rate <= 0 || rate > i64::from(u32::MAX) {
        return Err(range("rate"));
    }
    if rate == fs {
        // The sample rates match: no processing (the original drops the effect).
        return Ok(Vec::new());
    }
    let (max_rate, min_rate) = (rate.max(fs), rate.min(fs));
    let g = i64::from(gcd(rate as u32, istream.fs));
    let (ratio_n, ratio_d) = (rate / g, fs / g);
    let max_factor = ratio_n.max(ratio_d);
    let min_factor = ratio_n.min(ratio_d);

    // the windowed sinc function
    let m = ((2.0 * M_FACT * max_rate as f64) / (min_rate as f64 * (1.0 - bw))).round() as i64;
    let width = M_FACT * max_rate as f64 / m as f64;
    let fc = (min_rate as f64 - width) / max_rate as f64;
    let sinc_os = min_factor.min(SINC_MAX_OVERSAMPLE);
    let fc_os = fc / sinc_os as f64;
    let m_os = (m + 1) * sinc_os - 1;

    // array lengths
    let mut len_mult = (m + 1) / max_factor;
    if (m + 1) % max_factor != 0 {
        len_mult += 1;
    }
    if len_mult > 16 {
        // 17 is the first slow size
        let fast = next_fast_len(len_mult as usize) as i64;
        if fast != len_mult
            && (ratio_n <= 16
                || ratio_d <= 16
                || next_fast_len(ratio_n as usize) as i64 == ratio_n
                || next_fast_len(ratio_d as usize) as i64 == ratio_d)
        {
            len_mult = fast;
        }
    }
    let sinc_len = max_factor * len_mult * sinc_os;
    let in_len = ratio_d * len_mult;
    let out_len = ratio_n * len_mult;
    if sinc_len * 2 > MAX_BLOCK || in_len * 2 > MAX_BLOCK || out_len * 2 > MAX_BLOCK {
        return Err(Error::Message(format!(
            "{name}: error: the ratio {ratio_n}/{ratio_d} needs blocks that are too long"
        )));
    }
    let tmp_fr_len = (max_factor * len_mult + 1) as usize;
    let sinc_fr_len = (sinc_len + 1) as usize;
    let out_delay = if rate == max_rate {
        m / 2
    } else {
        ((m / 2) as f64 * (ratio_n as f64 / ratio_d as f64)).round() as i64
    };

    // the filter, in the frequency domain
    let mut sinc = vec![0.0; (sinc_len * 2) as usize];
    for i in 1..m_os {
        sinc[i as usize] =
            norm_sinc((i * 2 - m_os) as f64 / 2.0, fc_os) * albrecht9c3(i as f64 / m_os as f64);
    }
    let mut sinc_fr = vec![Complex::new(0.0, 0.0); sinc_fr_len];
    let mut sinc_fft = Fft::new((sinc_len * 2) as usize);
    sinc_fft.forward(&mut sinc, &mut sinc_fr);

    let ch = istream.channels;
    let (in_len, out_len) = (in_len as usize, out_len as usize);
    Ok(vec![Box::new(Resample {
        stream_in: *istream,
        stream_out: Stream {
            fs: rate as u32,
            channels: ch,
        },
        ratio_n,
        ratio_d,
        sinc_fr_len,
        in_len,
        out_len,
        in_buf_pos: 0,
        out_buf_pos: 0,
        drain_pos: 0,
        drain_frames: 0,
        out_delay: out_delay as usize,
        sinc_fr,
        tmp_fr: vec![Complex::new(0.0, 0.0); tmp_fr_len],
        tmp_fr_2: vec![Complex::new(0.0, 0.0); tmp_fr_len],
        input: vec![vec![0.0; in_len * 2]; ch],
        work: vec![0.0; in_len * 2],
        output: vec![vec![0.0; out_len * 2]; ch],
        overlap: vec![vec![0.0; out_len]; ch],
        fft: Fft::with_sizes(in_len * 2, out_len * 2),
        has_output: false,
        is_draining: false,
    })])
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    fn tone(fs: f64, f: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|i| 0.5 * (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin())
            .collect()
    }

    /// Amplitude of the `f` Hz component of `y` (sampled at `fs`) over whole cycles.
    fn amplitude(y: &[f64], fs: f64, f: f64) -> f64 {
        let n = (fs / f).round() as usize * (y.len() as f64 * f / fs).floor() as usize;
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in y.iter().take(n).enumerate() {
            let a = 2.0 * std::f64::consts::PI * f * i as f64 / fs;
            s += v * a.sin();
            c += v * a.cos();
        }
        2.0 / n as f64 * (s * s + c * c).sqrt()
    }

    #[test]
    fn the_rate_changes_and_a_tone_keeps_its_pitch_and_level() {
        let s = Stream {
            fs: 44_100,
            channels: 1,
        };
        let mut c = Chain::from_args(s, &["resample", "48k"]).unwrap();
        assert_eq!(c.ostream().fs, 48_000);
        let x = tone(44_100.0, 1000.0, 20_000);
        let y = c.process(&x);
        let want = (20_000.0 * 48_000.0 / 44_100.0f64).ceil() as usize;
        assert!(y.len().abs_diff(want) <= 2, "{} vs {want}", y.len());
        let mid = &y[4800..9600];
        assert!((amplitude(mid, 48_000.0, 1000.0) - 0.5).abs() < 1e-4);
        assert!(amplitude(mid, 48_000.0, 1100.0) < 1e-3);
    }

    #[test]
    fn multiplier_divisor_and_equal_rates() {
        let s = Stream {
            fs: 48_000,
            channels: 2,
        };
        assert_eq!(
            Chain::from_args(s, &["resample", "x2"])
                .unwrap()
                .ostream()
                .fs,
            96_000
        );
        assert_eq!(
            Chain::from_args(s, &["resample", "/2"])
                .unwrap()
                .ostream()
                .fs,
            24_000
        );
        assert_eq!(
            Chain::from_args(s, &["resample", "0.9", "x2"])
                .unwrap()
                .ostream()
                .fs,
            96_000
        );
        let mut same = Chain::from_args(s, &["resample", "48000"]).unwrap();
        assert_eq!(same.run(&[0.25, -0.25]), vec![0.25, -0.25]);
        assert!(Chain::from_args(s, &["resample", "/5000"]).is_err());
        assert!(Chain::from_args(s, &["resample"]).is_err());
        assert!(Chain::from_args(s, &["resample", "2", "x2"]).is_err());
        assert!(Chain::from_args(s, &["resample", "-5"]).is_err());
    }
}
