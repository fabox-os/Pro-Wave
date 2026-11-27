//! `fir_p` (`fir_p.c`): FIR filtering with no latency, by partitioned convolution.
//!
//! The first 32 taps are convolved directly; the rest of the filter is cut into partitions
//! that grow with the distance from the start (up to 16384 samples), each group of equal
//! partitions convolved by FFT with a frequency-domain delay line. The result is the plain
//! convolution, with no delay of its own.
//!
//! The original runs the long partition groups on helper threads for filters of 4096 taps and
//! more. Pro Wave's engine does its own threading and this crate does no platform calls, so
//! every group is computed in the audio thread, in the way the original does for short filters
//! (the numbers are the same).

use realfft::num_complex::Complex;

use crate::args::strtol;
use crate::effect::{default_selector, Buf, Effect, EffectInfo, Stream};
use crate::error::{Error, Result};
use crate::fir::{check_filter, fir_effect, taps_of, Fft, Filter};
use crate::fir_util::{get_offset, parse_opts, read_filter};

pub(crate) static INFO: [EffectInfo; 1] = [EffectInfo {
    name: "fir_p",
    usage: "[-a[offset[s|m|S]]] [input_options] [max_part_len] [file:][~/]filter_path|coefs:list[/list...]",
    init,
    number: 0,
}];

const DIRECT_LEN: usize = 1 << 5;
const FFT_LEN_STEP_DEFAULT: usize = 1 << 2;
const MAX_FFT_GROUPS: usize = 4;
const MAX_PART_LEN_LIMIT: usize = i32::MAX as usize;
const MAX_PART_LEN_DEFAULT: usize = 1 << 14;

/// A group of `n` partitions of `len` taps each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Part {
    len: usize,
    n: usize,
}

/// Chooses the partitions (`find_partitions`) for a filter of `filter_frames` taps, the first
/// `DIRECT_LEN` being convolved directly.
fn find_partitions(filter_frames: usize, max_part_len: usize) -> Vec<Part> {
    let mut step = FFT_LEN_STEP_DEFAULT;
    'again: loop {
        let mut groups: Vec<Part> = Vec::new();
        let (mut j, mut k) = (DIRECT_LEN, DIRECT_LEN);
        while k < filter_frames {
            if groups.len() + 1 > MAX_FFT_GROUPS {
                step <<= 1;
                continue 'again;
            }
            let mut g = Part { len: j, n: 1 };
            k += g.len;
            while k < filter_frames && k < j * step {
                g.n += 1;
                k += g.len;
            }
            j *= step;
            if j > max_part_len || k + j * step > filter_frames {
                while k < filter_frames {
                    g.n += 1;
                    k += g.len;
                }
                groups.push(g);
                break;
            }
            groups.push(g);
        }
        // Try to optimise a bit.
        for k in (1..groups.len()).rev() {
            while groups[k].len * 2 <= max_part_len {
                let prev = groups[k - 1];
                let new_n = prev.n + groups[k].len / prev.len;
                if groups[k].n <= new_n {
                    break;
                }
                groups[k - 1].n = new_n;
                groups[k].len *= 2;
                groups[k].n -= 1;
                groups[k].n = groups[k].n / 2 + (groups[k].n & 1);
            }
        }
        return groups;
    }
}

/// `verify_and_print_partitions`: the groups must follow each other without a gap.
fn verify_partitions(name: &str, groups: &[Part], filter_frames: usize) -> Result<()> {
    let mut total = DIRECT_LEN;
    for (k, g) in groups.iter().enumerate() {
        if total != g.len {
            return Err(Error::Message(format!(
                "{name}: BUG: invalid partitioning: group={} len={} delay={}",
                k + 1,
                g.len,
                total as isize - g.len as isize
            )));
        }
        total += g.len * g.n;
    }
    if total < filter_frames {
        return Err(Error::Message(format!(
            "{name}: BUG: invalid partitioning: total={total} filter_frames={filter_frames}"
        )));
    }
    Ok(())
}

struct Group {
    len: usize,
    n: usize,
    /// Number of frequency bins of one partition.
    bins: usize,
    p: usize,
    fdl_p: usize,
    /// `filter_fr[c]`: the spectra of the `n` partitions for filter channel `c`, one after the other.
    filter_fr: Vec<Vec<Complex<f64>>>,
    /// Per selected channel: the frequency-domain delay line, the block buffer, the overlap.
    fdl: Vec<Vec<Complex<f64>>>,
    fft_buf: Vec<Vec<f64>>,
    fft_olap: Vec<Vec<f64>>,
    tmp: Vec<Complex<f64>>,
    fft: Fft,
}

impl Group {
    fn compute(&mut self, one_filter: bool) {
        let out_norm = 1.0 / (self.len as f64 * 2.0);
        let bins = self.bins;
        for k in 0..self.fft_buf.len() {
            let filter = &self.filter_fr[if one_filter { 0 } else { k }];
            let fdl = &mut self.fdl[k];
            let buf = &mut self.fft_buf[k];
            let olap = &mut self.fft_olap[k];
            self.fft.forward(buf, &mut self.tmp);
            let mut slot = self.fdl_p;
            fdl[slot * bins..(slot + 1) * bins].copy_from_slice(&self.tmp);
            for (t, f) in self.tmp.iter_mut().zip(&filter[..bins]) {
                *t *= *f;
            }
            for q in 1..self.n {
                slot = if slot == 0 { self.n - 1 } else { slot - 1 };
                let d = &fdl[slot * bins..(slot + 1) * bins];
                let f = &filter[q * bins..(q + 1) * bins];
                for ((t, d), f) in self.tmp.iter_mut().zip(d).zip(f) {
                    *t += *d * *f;
                }
            }
            self.fft.inverse(&mut self.tmp, buf);
            for v in buf.iter_mut() {
                *v *= out_norm;
            }
            let (head, tail) = buf.split_at_mut(self.len);
            for ((h, o), t) in head.iter_mut().zip(olap.iter_mut()).zip(tail.iter_mut()) {
                *h += *o;
                *o = *t;
                *t = 0.0;
            }
        }
        self.fdl_p = if self.fdl_p + 1 < self.n {
            self.fdl_p + 1
        } else {
            0
        };
    }
}

struct Partitioned {
    name: &'static str,
    stream: Stream,
    one_filter: bool,
    p0_p: usize,
    p0_filter: Vec<Option<Vec<f64>>>,
    p0_buf: Vec<Option<Vec<f64>>>,
    /// The position of each channel among the selected ones.
    slot: Vec<Option<usize>>,
    groups: Vec<Group>,
    filter_frames: usize,
    reference: isize,
}

/// `fir_p` with the filter already in memory (`fir_p_effect_init_with_filter`). `max_part_len`
/// is the longest partition, a power of two (0 for the default of 16384).
pub fn fir_p_effect(
    info: &EffectInfo,
    istream: &Stream,
    selector: &[bool],
    filter: &Filter,
    max_part_len: usize,
) -> Result<Box<dyn Effect>> {
    let Filter {
        data: filter_data,
        channels: filter_channels,
        frames: filter_frames,
        reference,
    } = *filter;
    if filter_frames <= DIRECT_LEN {
        return fir_effect(info, istream, selector, filter, true);
    }
    let name = info.name;
    let n_channels = check_filter(name, selector, filter_channels, filter_frames)?;
    let max_part_len = if max_part_len == 0 {
        MAX_PART_LEN_DEFAULT
    } else {
        max_part_len
    };
    if !max_part_len.is_power_of_two() {
        return Err(Error::Message(format!(
            "{name}: error: max_part_len must be a power of two"
        )));
    }
    if !(DIRECT_LEN..=MAX_PART_LEN_LIMIT).contains(&max_part_len) {
        return Err(Error::Message(format!(
            "{name}: error: max_part_len must be within [{DIRECT_LEN},{MAX_PART_LEN_LIMIT}] or 0 for default"
        )));
    }
    let parts = find_partitions(filter_frames, max_part_len);
    verify_partitions(name, &parts, filter_frames)?;

    let ch = istream.channels;
    let mut p0_filter = vec![None; ch];
    let mut p0_buf = vec![None; ch];
    let mut slot = vec![None; ch];
    let mut k = 0;
    for i in 0..ch {
        if selector.get(i).copied().unwrap_or(false) {
            p0_filter[i] =
                Some(taps_of(filter_data, filter_channels, k, DIRECT_LEN).collect::<Vec<f64>>());
            p0_buf[i] = Some(vec![0.0; DIRECT_LEN]);
            slot[i] = Some(k);
            k += 1;
        }
    }

    let mut groups = Vec::new();
    let mut filter_pos = DIRECT_LEN;
    for part in &parts {
        let bins = part.len + 1;
        let mut fft = Fft::new(part.len * 2);
        let mut tmp = vec![Complex::new(0.0, 0.0); bins];
        let mut filter_fr = Vec::new();
        for c in 0..filter_channels {
            let mut spectra = Vec::with_capacity(bins * part.n);
            for q in 0..part.n {
                let mut t = vec![0.0; part.len * 2];
                let start = filter_pos + q * part.len;
                for (l, d) in t.iter_mut().take(part.len).enumerate() {
                    if start + l < filter_frames {
                        *d = filter_data[(start + l) * filter_channels + c];
                    }
                }
                fft.forward(&mut t, &mut tmp);
                spectra.extend_from_slice(&tmp);
            }
            filter_fr.push(spectra);
        }
        filter_pos += part.len * part.n;
        groups.push(Group {
            len: part.len,
            n: part.n,
            bins,
            p: 0,
            fdl_p: 0,
            filter_fr,
            fdl: vec![vec![Complex::new(0.0, 0.0); bins * part.n]; n_channels],
            fft_buf: vec![vec![0.0; part.len * 2]; n_channels],
            fft_olap: vec![vec![0.0; part.len]; n_channels],
            tmp,
            fft,
        });
    }
    Ok(Box::new(Partitioned {
        name,
        stream: *istream,
        one_filter: filter_channels == 1,
        p0_p: 0,
        p0_filter,
        p0_buf,
        slot,
        groups,
        filter_frames,
        reference,
    }))
}

impl Effect for Partitioned {
    fn reorderable(&self) -> bool {
        true
    }
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
                let (Some(buf), Some(filter), Some(slot)) =
                    (&mut self.p0_buf[k], &self.p0_filter[k], self.slot[k])
                else {
                    continue;
                };
                let s = ibuf[i * ch + k];
                let mut n = self.p0_p;
                for &h in filter.iter() {
                    buf[n] += s * h;
                    n = (n + 1) & (DIRECT_LEN - 1);
                }
                let mut y = buf[self.p0_p];
                buf[self.p0_p] = 0.0;
                for g in self.groups.iter_mut() {
                    let b = &mut g.fft_buf[slot];
                    y += b[g.p + self.p0_p];
                    b[g.p + self.p0_p] = s;
                }
                ibuf[i * ch + k] = y;
            }
            self.p0_p = (self.p0_p + 1) & (DIRECT_LEN - 1);
            // All partition lengths are multiples of DIRECT_LEN.
            if self.p0_p == 0 {
                let one = self.one_filter;
                for g in self.groups.iter_mut() {
                    g.p += DIRECT_LEN;
                    if g.p == g.len {
                        g.p = 0;
                        g.compute(one);
                    }
                }
            }
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.p0_p = 0;
        for b in self.p0_buf.iter_mut().flatten() {
            b.fill(0.0);
        }
        for g in self.groups.iter_mut() {
            g.p = 0;
            g.fdl_p = 0;
            for f in g.fdl.iter_mut() {
                f.fill(Complex::new(0.0, 0.0));
            }
            for b in g.fft_buf.iter_mut() {
                b.fill(0.0);
            }
            for o in g.fft_olap.iter_mut() {
                o.fill(0.0);
            }
        }
    }
    fn channel_offsets(&self, _latency: &mut [isize], req_delay: &mut [isize]) -> bool {
        for (r, b) in req_delay.iter_mut().zip(&self.p0_buf) {
            if b.is_some() {
                *r -= self.reference;
            }
        }
        true
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (d, b) in samples.iter_mut().zip(&self.p0_buf) {
            if b.is_some() {
                *d += self.filter_frames as isize - 1;
            }
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
    let argc = argv.len();
    let (config, mut ind) = parse_opts(info, istream, argv)?;
    if ind + 2 < argc || ind + 1 > argc {
        return Err(info.usage_error());
    }
    let mut max_part_len = 0usize;
    if ind + 2 == argc {
        let (v, end) = strtol(argv[ind]);
        if end == 0 || end != argv[ind].len() {
            return Err(Error::Parse {
                effect: info.name.into(),
                param: "max_part_len".into(),
                text: argv[ind].into(),
            });
        }
        max_part_len = usize::try_from(v).map_err(|_| {
            Error::Message(format!(
                "{}: error: max_part_len must be a power of two",
                info.name
            ))
        })?;
        ind += 1;
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
    fir_p_effect(info, istream, &selector, &filter, max_part_len).map(|e| vec![e])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_cover_the_filter_without_a_gap() {
        for frames in [33usize, 100, 513, 4096, 5000, 20_000, 100_000, 1 << 20] {
            let parts = find_partitions(frames, MAX_PART_LEN_DEFAULT);
            verify_partitions("fir_p", &parts, frames).unwrap();
            assert!(parts.len() <= MAX_FFT_GROUPS, "{frames}");
        }
    }

    #[test]
    fn the_result_is_the_plain_convolution_with_no_delay() {
        let info = EffectInfo {
            name: "fir_p",
            usage: "",
            init: |_, _, _, _| Ok(vec![]),
            number: 0,
        };
        let s = Stream {
            fs: 48_000,
            channels: 2,
        };
        for taps in [33usize, 200, 1500, 9000] {
            let h: Vec<f64> = (0..taps)
                .map(|i| (((i * 7919) % 101) as f64 - 50.0) / 5000.0)
                .collect();
            let frames = 3 * taps.max(2000);
            let x: Vec<f64> = (0..frames * 2)
                .map(|i| (((i * 31) % 17) as f64 - 8.0) / 9.0)
                .collect();
            let f = Filter {
                data: &h,
                channels: 1,
                frames: taps,
                reference: 0,
            };
            let mut e = fir_p_effect(&info, &s, &[true, true], &f, 0).unwrap();
            let mut y = x.clone();
            let mut n = frames;
            e.run(&mut n, &mut y, &mut []);
            for &i in &[0usize, 1, 31, 32, 33, 500, frames / 2, frames - 1] {
                for c in 0..2 {
                    let want: f64 = (0..taps.min(i + 1))
                        .map(|m| h[m] * x[(i - m) * 2 + c])
                        .sum();
                    assert!(
                        (y[i * 2 + c] - want).abs() < 1e-9,
                        "taps={taps} frame={i} ch={c}: {} vs {want}",
                        y[i * 2 + c]
                    );
                }
            }
        }
    }
}
