//! `fir` (`fir.c`): FIR filtering by convolution with a filter given as coefficients.
//!
//! Short filters (16 taps or fewer) are convolved directly. Longer ones use the overlap-add
//! method with blocks as long as the filter, which delays the signal by one block: the
//! effect reports that latency so the chain can keep the channels aligned.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir_util::{get_offset, parse_opts, read_filter};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "fir",
    usage: "[-a[offset[s|m|S]]] [input_options] [file:][~/]filter_path|coefs:list[/list...]",
    init,
    number: 0,
}];

/// Filters up to this many taps are convolved directly.
pub(crate) const MAX_DIRECT_LEN: usize = 1 << 4;

/// The smallest length of the form 2^a 3^b 5^c 7^d that is at least `min_len`
/// (`next_fast_fftw_len`): lengths a fast FFT likes.
pub fn next_fast_len(min_len: usize) -> usize {
    let mut best = min_len * 7;
    let bound = min_len * 2;
    let mut p2 = 1usize;
    while p2 <= bound {
        let mut p3 = p2;
        while p3 <= bound {
            let mut p5 = p3;
            while p5 <= bound {
                let mut p7 = p5;
                while p7 <= bound {
                    if p7 < best && p7 >= min_len {
                        best = p7;
                    }
                    p7 *= 7;
                }
                p5 *= 5;
            }
            p3 *= 3;
        }
        p2 *= 2;
    }
    best
}

/// Checks the filter against the channels it will work on; returns how many those are.
pub(crate) fn check_filter(
    name: &str,
    selector: &[bool],
    filter_channels: usize,
    filter_frames: usize,
) -> Result<usize> {
    let n_channels = selector.iter().filter(|s| **s).count();
    if filter_channels != 1 && filter_channels != n_channels {
        return Err(Error::Message(format!(
            "{name}: error: channels mismatch: channels={n_channels} filter_channels={filter_channels}"
        )));
    }
    if filter_frames < 1 {
        return Err(Error::Message(format!(
            "{name}: error: filter length must be >= 1"
        )));
    }
    Ok(n_channels)
}

/// The taps of the filter for the `k`-th selected channel (`filter_data` is interleaved).
pub(crate) fn taps_of<'a>(
    data: &'a [f64],
    filter_channels: usize,
    k: usize,
    frames: usize,
) -> impl Iterator<Item = f64> + 'a {
    let c = if filter_channels == 1 { 0 } else { k };
    (0..frames).map(move |j| data[j * filter_channels + c])
}

struct Direct {
    name: &'static str,
    stream: Stream,
    len: usize,
    mask: usize,
    p: usize,
    filter_frames: usize,
    reference: isize,
    filter: Vec<Option<Vec<f64>>>,
    buf: Vec<Option<Vec<f64>>>,
}

/// A direct-form FIR filter; the filter is `filter_data` with `filter_channels` interleaved
/// channels (one, or one per selected channel). `reference` is the sample of the filter that
/// should line up with the input (`-a`): the effect asks the chain to delay the other channels.
pub(crate) fn direct(
    name: &'static str,
    stream: Stream,
    selector: &[bool],
    filter_data: &[f64],
    filter_channels: usize,
    filter_frames: usize,
    reference: isize,
) -> Result<Box<dyn Effect>> {
    check_filter(name, selector, filter_channels, filter_frames)?;
    let mut len = 1usize;
    while len < filter_frames {
        len <<= 1;
    }
    let mut filter = vec![None; stream.channels];
    let mut buf = vec![None; stream.channels];
    let mut k = 0;
    for i in 0..stream.channels {
        if selector.get(i).copied().unwrap_or(false) {
            let mut f = vec![0.0; len];
            for (d, t) in f
                .iter_mut()
                .zip(taps_of(filter_data, filter_channels, k, filter_frames))
            {
                *d = t;
            }
            filter[i] = Some(f);
            buf[i] = Some(vec![0.0; len]);
            k += 1;
        }
    }
    Ok(Box::new(Direct {
        name,
        stream,
        len,
        mask: len - 1,
        p: 0,
        filter_frames,
        reference,
        filter,
        buf,
    }))
}

impl Effect for Direct {
    fn name(&self) -> &'static str {
        self.name
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for i in 0..*frames {
            for k in 0..ch {
                if let (Some(buf), Some(filter)) = (&mut self.buf[k], &self.filter[k]) {
                    let s = ibuf[i * ch + k];
                    let mut n = self.p;
                    for &h in filter.iter().take(self.len) {
                        buf[n] += s * h;
                        n = (n + 1) & self.mask;
                    }
                    ibuf[i * ch + k] = buf[self.p];
                    buf[self.p] = 0.0;
                }
            }
            self.p = (self.p + 1) & self.mask;
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.p = 0;
        for b in self.buf.iter_mut().flatten() {
            b.fill(0.0);
        }
    }
    fn channel_offsets(&self, _latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        for (r, b) in req_delay.iter_mut().zip(&self.buf) {
            if b.is_some() {
                *r -= self.reference;
            }
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (d, b) in samples.iter_mut().zip(&self.buf) {
            if b.is_some() {
                *d += self.filter_frames as isize - 1;
            }
        }
        true
    }
}

/// Plans for real FFTs of one size, and the scratch space they need.
pub(crate) struct Fft {
    pub r2c: Arc<dyn RealToComplex<f64>>,
    pub c2r: Arc<dyn ComplexToReal<f64>>,
    scratch: Vec<Complex<f64>>,
}

impl Fft {
    /// Transforms of `n` real samples (n even).
    pub fn new(n: usize) -> Self {
        Self::with_sizes(n, n)
    }

    /// A forward transform of `forward` real samples and an inverse one of `inverse`.
    pub fn with_sizes(forward: usize, inverse: usize) -> Self {
        let mut planner = RealFftPlanner::<f64>::new();
        let r2c = planner.plan_fft_forward(forward);
        let c2r = planner.plan_fft_inverse(inverse);
        let scratch =
            vec![Complex::new(0.0, 0.0); r2c.get_scratch_len().max(c2r.get_scratch_len())];
        Self { r2c, c2r, scratch }
    }

    /// Forward transform; `input` is used up.
    pub fn forward(&mut self, input: &mut [f64], output: &mut [Complex<f64>]) {
        let n = self.r2c.get_scratch_len();
        // The lengths are fixed when the effect is made, so this cannot fail.
        let _ = self
            .r2c
            .process_with_scratch(input, output, &mut self.scratch[..n]);
    }

    /// Inverse transform (not scaled); `input` is used up.
    pub fn inverse(&mut self, input: &mut [Complex<f64>], output: &mut [f64]) {
        let n = self.c2r.get_scratch_len();
        if let Some(first) = input.first_mut() {
            first.im = 0.0;
        }
        if let Some(last) = input.last_mut() {
            last.im = 0.0;
        }
        let _ = self
            .c2r
            .process_with_scratch(input, output, &mut self.scratch[..n]);
    }
}

struct Overlap {
    name: &'static str,
    stream: Stream,
    len: usize,
    p: usize,
    filter_frames: usize,
    reference: isize,
    /// The spectra of the filter, one per filter channel.
    filters: Vec<Vec<Complex<f64>>>,
    /// Which spectrum each selected channel uses.
    spectrum: Vec<Option<usize>>,
    buf: Vec<Option<Vec<f64>>>,
    olap: Vec<Option<Vec<f64>>>,
    tmp: Vec<Complex<f64>>,
    fft: Fft,
}

/// An FFT convolution FIR filter (see [`direct`] for the arguments).
pub(crate) fn overlap_add(
    name: &'static str,
    stream: Stream,
    selector: &[bool],
    filter_data: &[f64],
    filter_channels: usize,
    filter_frames: usize,
    reference: isize,
) -> Result<Box<dyn Effect>> {
    check_filter(name, selector, filter_channels, filter_frames)?;
    let len = next_fast_len(filter_frames);
    let mut fft = Fft::new(len * 2);
    let bins = len + 1;
    let mut tmp = vec![Complex::new(0.0, 0.0); bins];
    let mut filters = Vec::new();
    for c in 0..filter_channels {
        let mut t = vec![0.0; len * 2];
        for (d, v) in t
            .iter_mut()
            .zip(taps_of(filter_data, filter_channels, c, filter_frames))
        {
            *d = v;
        }
        fft.forward(&mut t, &mut tmp);
        filters.push(tmp.clone());
    }
    let mut spectrum = vec![None; stream.channels];
    let mut buf = vec![None; stream.channels];
    let mut olap = vec![None; stream.channels];
    let mut k = 0;
    for i in 0..stream.channels {
        if selector.get(i).copied().unwrap_or(false) {
            spectrum[i] = Some(if filter_channels == 1 { 0 } else { k });
            buf[i] = Some(vec![0.0; len * 2]);
            olap[i] = Some(vec![0.0; len]);
            k += 1;
        }
    }
    Ok(Box::new(Overlap {
        name,
        stream,
        len,
        p: 0,
        filter_frames,
        reference,
        filters,
        spectrum,
        buf,
        olap,
        tmp,
        fft,
    }))
}

impl Effect for Overlap {
    fn name(&self) -> &'static str {
        self.name
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        let len = self.len;
        for i in 0..*frames {
            for k in 0..ch {
                if let Some(buf) = &mut self.buf[k] {
                    let s = ibuf[i * ch + k];
                    ibuf[i * ch + k] = std::mem::replace(&mut buf[self.p], s);
                }
            }
            self.p += 1;
            if self.p == len {
                let out_norm = 1.0 / (len as f64 * 2.0);
                for k in 0..ch {
                    let (Some(buf), Some(olap), Some(sp)) =
                        (&mut self.buf[k], &mut self.olap[k], self.spectrum[k])
                    else {
                        continue;
                    };
                    self.fft.forward(buf, &mut self.tmp);
                    for (t, f) in self.tmp.iter_mut().zip(&self.filters[sp]) {
                        *t *= *f;
                    }
                    self.fft.inverse(&mut self.tmp, buf);
                    for v in buf.iter_mut() {
                        *v *= out_norm;
                    }
                    let (head, tail) = buf.split_at_mut(len);
                    for ((h, o), t) in head.iter_mut().zip(olap.iter_mut()).zip(tail.iter_mut()) {
                        *h += *o;
                        *o = *t;
                        *t = 0.0;
                    }
                }
                self.p = 0;
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.p = 0;
        for b in self.buf.iter_mut().flatten() {
            b.fill(0.0);
        }
        for o in self.olap.iter_mut().flatten() {
            o.fill(0.0);
        }
    }
    fn channel_offsets(&self, latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        for ((l, r), b) in latency.iter_mut().zip(req_delay.iter_mut()).zip(&self.buf) {
            if b.is_some() {
                *l += self.len as isize;
                *r -= self.reference;
            }
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (d, b) in samples.iter_mut().zip(&self.buf) {
            if b.is_some() {
                *d += (self.len + self.filter_frames - 1) as isize;
            }
        }
        true
    }
}

/// A filter in memory: `frames` frames of `channels` interleaved channels (one, or one per
/// channel the effect works on).
#[derive(Clone, Copy, Debug)]
pub struct Filter<'a> {
    pub data: &'a [f64],
    pub channels: usize,
    pub frames: usize,
    /// The sample of the filter that `-a` lines up with the input.
    pub reference: isize,
}

/// `fir` with the filter already in memory: direct form for up to 16 taps (or when
/// `force_direct`), FFT convolution otherwise.
pub fn fir_effect(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    filter: &Filter,
    force_direct: bool,
) -> Result<Box<dyn Effect>> {
    let make = if filter.frames <= MAX_DIRECT_LEN || force_direct {
        direct
    } else {
        overlap_add
    };
    make(
        info.name,
        *istream,
        selector,
        filter.data,
        filter.channels,
        filter.frames,
        filter.reference,
    )
}

fn init(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    argv: &[&str],
) -> Result<Vec<Box<dyn Effect>>> {
    let (config, ind) = parse_opts(info, istream, argv)?;
    if ind != argv.len() - 1 {
        return Err(info.usage_error());
    }
    let (data, channels, frames) = read_filter(info.name, argv[ind])?;
    let offset = get_offset(&config, &data, channels, frames);
    let selector = default_selector(selector, istream.channels);
    let filter = Filter {
        data: &data,
        channels,
        frames,
        reference: offset,
    };
    fir_effect(info, istream, &selector, &filter, false).map(|e| vec![e])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_lengths_are_smooth_numbers() {
        assert_eq!(next_fast_len(1), 1);
        assert_eq!(next_fast_len(11), 12);
        assert_eq!(next_fast_len(17), 18);
        assert_eq!(next_fast_len(1000), 1000);
        assert_eq!(next_fast_len(1009), 1024);
    }

    fn convolve(x: &[f64], h: &[f64], n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| {
                h.iter()
                    .enumerate()
                    .filter(|(m, _)| *m <= i && i - *m < x.len())
                    .map(|(m, v)| v * x[i - m])
                    .sum()
            })
            .collect()
    }

    #[test]
    fn direct_and_fft_forms_give_the_same_convolution() {
        let info = EffectInfo {
            name: "fir",
            usage: "",
            init: |_, _, _, _| Ok(vec![]),
            number: 0,
        };
        let s = Stream {
            fs: 48_000,
            channels: 1,
        };
        let h: Vec<f64> = (0..40)
            .map(|i| ((i * 7 % 13) as f64 - 6.0) / 10.0)
            .collect();
        let x: Vec<f64> = (0..300)
            .map(|i| ((i * 5 % 11) as f64 - 5.0) / 7.0)
            .collect();
        let want = convolve(&x, &h, x.len());
        let f = Filter {
            data: &h,
            channels: 1,
            frames: h.len(),
            reference: 0,
        };
        let mut a = fir_effect(&info, &s, &[true], &f, true).unwrap();
        let mut b = fir_effect(&info, &s, &[true], &f, false).unwrap();
        let mut xa = x.clone();
        let mut n = x.len();
        a.run(&mut n, &mut xa, &mut []);
        for (g, w) in xa.iter().zip(&want) {
            assert!((g - w).abs() < 1e-12);
        }
        // The FFT form is late by one block.
        let mut lat = [0isize];
        let mut dl = [0isize];
        b.channel_offsets(&mut lat, &mut dl);
        let mut xb = x.clone();
        xb.extend(std::iter::repeat_n(0.0, lat[0] as usize));
        let mut n = xb.len();
        b.run(&mut n, &mut xb, &mut []);
        for (i, w) in want.iter().enumerate() {
            assert!((xb[i + lat[0] as usize] - w).abs() < 1e-9, "{i}");
        }
    }
}
