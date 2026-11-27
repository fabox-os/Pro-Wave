//! Latency alignment and draining of a chain (`align.c` and the post-processing in
//! `effects_chain.c`).
//!
//! Some effects delay some channels: a delay, a convolution with latency. If one channel of a
//! stream is late, the others must wait for it or the channels drift apart. After the effects
//! are made the chain walks through them, adds up the latency and the delay each channel has
//! gathered, and wherever channels meet (a mixing effect, the end of the chain) puts in an
//! `align` effect that delays the early channels. It also works out how many frames of silence
//! must follow the input to flush what the effects still hold, and where the beginning of the
//! signal is cut off because every channel is late by the same amount.

use crate::effect::{Buf, Effect, Stream};
use crate::error::{Error, Result};

/// The delay buffer of one channel of an [`Align`] effect.
struct AlignChannel {
    buf: Vec<f64>,
    p: usize,
}

/// The `align` effect: delays some channels so that all line up, and drops leading frames that
/// every channel would otherwise have to wait for.
pub struct Align {
    stream: Stream,
    cs: Vec<AlignChannel>,
    /// Frames still to drop (negative) or frames passed since (positive).
    frames: isize,
    discard_frames: usize,
}

impl Effect for Align {
    fn name(&self) -> &'static str {
        "align"
    }
    fn istream(&self) -> Stream {
        self.stream
    }
    fn ostream(&self) -> Stream {
        self.stream
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], _obuf: &mut [f64]) -> Buf {
        let ch = self.stream.channels;
        for (k, c) in self.cs.iter_mut().enumerate() {
            if c.buf.is_empty() {
                continue;
            }
            let len = c.buf.len();
            let mut idx = k;
            for _ in 0..*frames {
                std::mem::swap(&mut ibuf[idx], &mut c.buf[c.p]);
                idx += ch;
                c.p = if c.p + 1 >= len { 0 } else { c.p + 1 };
            }
        }
        if self.frames < 0 {
            let end = *frames * ch;
            self.frames += *frames as isize;
            *frames = self.frames.max(0) as usize;
            let samples = *frames * ch;
            ibuf.copy_within(end - samples..end, 0);
        } else {
            self.frames += *frames as isize;
        }
        Buf::Input
    }
    fn reset(&mut self) {
        self.frames = -(self.discard_frames as isize);
        for c in self.cs.iter_mut() {
            c.p = 0;
            c.buf.fill(0.0);
        }
    }
    fn drain_samples(&self, samples: &mut [isize]) -> bool {
        for (s, c) in samples.iter_mut().zip(&self.cs) {
            *s += c.buf.len() as isize;
        }
        true
    }
}

/// `align_effect_insert`: the effect that brings the channels of `prev` (with `offsets` of
/// latency) to `align_refs`, or to the largest offset when there are none. `has_next` is false
/// at the end of the chain, where negative offsets are made zero. Updates `offsets` and
/// returns the effect, or `None` when the channels already line up.
fn insert(
    prev: Stream,
    has_next: bool,
    offsets: &mut [isize],
    align_refs: Option<&[isize]>,
) -> Option<Align> {
    let n = prev.channels;
    let do_align = match align_refs {
        Some(refs) => offsets[..n] != refs[..n],
        None => offsets[..n].iter().any(|o| *o != 0),
    };
    if !do_align {
        return None;
    }
    let mut max_offset = if has_next { offsets[0] } else { 0 };
    for o in &offsets[..n] {
        max_offset = max_offset.max(*o);
    }
    let mut min_ref = max_offset;
    let mut cs = Vec::with_capacity(n);
    for k in 0..n {
        let reference = align_refs.map_or(max_offset, |r| r[k]);
        min_ref = min_ref.min(reference);
        let len = if offsets[k] != reference {
            (reference - offsets[k]).max(0) as usize
        } else {
            0
        };
        cs.push(AlignChannel {
            buf: vec![0.0; len],
            p: 0,
        });
        offsets[k] = reference;
    }
    let mut discard_frames = 0;
    if min_ref > 0 {
        for o in offsets[..n].iter_mut() {
            *o -= min_ref;
        }
        discard_frames = min_ref as usize;
    }
    Some(Align {
        stream: prev,
        cs,
        frames: -(discard_frames as isize),
        discard_frames,
    })
}

fn sel_is_ident(s: &[bool], n: usize, i: usize) -> bool {
    s[i] && (0..n).all(|k| k == i || !s[k])
}

/// `query_channel_deps`: fills `deps` for the effect; returns whether it is a square identity.
pub(crate) fn query_deps(e: &dyn Effect, is_align: bool, deps: &mut [Vec<bool>]) -> bool {
    let (n_in, n_out) = (e.istream().channels, e.ostream().channels);
    for row in deps.iter_mut().take(n_out) {
        row[..n_in].iter_mut().for_each(|b| *b = false);
    }
    for (i, row) in deps.iter_mut().enumerate().take(n_in.min(n_out)) {
        row[i] = true;
    }
    let is_square = n_in == n_out;
    if e.channel_deps(deps) {
        if is_square && (0..n_out).any(|i| !sel_is_ident(&deps[i], n_in, i)) {
            return false;
        }
    } else if !e.ch_deps_identity() || (is_align && e.align_barrier()) {
        for (i, row) in deps.iter_mut().enumerate().take(n_out) {
            match e.channel_selector() {
                Some(sel) => {
                    if i >= n_in || sel[i] {
                        row[..n_in].copy_from_slice(&sel[..n_in]);
                    }
                }
                None => row[..n_in].iter_mut().for_each(|b| *b = true),
            }
        }
        return false;
    }
    is_square
}

/// `find_input_deps`: the input channels that must stay together with channel `ch`.
fn find_input_deps(ch: usize, deps: &[Vec<bool>], n_in: usize, n_out: usize) -> Vec<bool> {
    let mut r = vec![false; n_in];
    r[ch] = true;
    let mut i = 0;
    while i < n_out {
        for k in 0..n_in {
            if r[k] && deps[i][k] {
                let mut modified: Option<usize> = None;
                for j in 0..n_in {
                    if !r[j] && deps[i][j] {
                        r[j] = true;
                        modified.get_or_insert(j);
                    }
                }
                if let Some(m) = modified {
                    if m > 0 && m < i {
                        i = m - 1;
                        break;
                    }
                }
            }
        }
        i += 1;
    }
    r
}

fn ratio(from: u32, to: u32) -> (i64, i64) {
    let (mut a, mut b) = (i64::from(to), i64::from(from));
    let (n, d) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    (n / a, d / a)
}

fn ratio_mult_ceil(v: isize, n: i64, d: i64) -> isize {
    let r = v as i64 * n;
    (if r % d != 0 { r / d + 1 } else { r / d }) as isize
}

/// `ec_align_channels`: walks the effects, inserts `align` effects where channels have to meet
/// and returns the delay no channel can lose (`zero_ref`).
pub(crate) fn align_channels(effects: &mut Vec<Box<dyn Effect>>) -> Result<isize> {
    let max_in = effects
        .iter()
        .map(|e| e.istream().channels)
        .max()
        .unwrap_or(1);
    let max_out = effects
        .iter()
        .map(|e| e.ostream().channels)
        .max()
        .unwrap_or(1);
    let max_ch = max_in.max(max_out);
    let mut deps = vec![vec![false; max_in]; max_out];
    let mut offsets = vec![0isize; max_ch];
    let mut delays = vec![0isize; max_ch];
    let mut nd_part = 0isize;
    let mut prev: Option<Stream> = None;
    let mut i = 0;
    while i < effects.len() {
        let did_remap = !query_deps(effects[i].as_ref(), true, &mut deps);
        let (is_, os_) = (effects[i].istream(), effects[i].ostream());
        if let Some(prev_stream) = prev {
            if is_.fs != os_.fs || did_remap {
                let refs = if is_.fs != os_.fs {
                    None
                } else {
                    let mut refs = offsets[..is_.channels].to_vec();
                    let mut done = vec![false; is_.channels];
                    for k in 0..is_.channels {
                        if done[k] {
                            continue;
                        }
                        let in_deps = find_input_deps(k, &deps, is_.channels, os_.channels);
                        let mut max_offset = offsets[k];
                        for j in 0..is_.channels {
                            if in_deps[j] {
                                done[j] = true;
                                max_offset = max_offset.max(offsets[j]);
                            }
                        }
                        for j in 0..is_.channels {
                            if in_deps[j] {
                                refs[j] = max_offset;
                            }
                        }
                    }
                    Some(refs)
                };
                if let Some(a) = insert(prev_stream, true, &mut offsets, refs.as_deref()) {
                    effects.insert(i, Box::new(a));
                    i += 1;
                }
            }
        }
        let e = effects[i].as_ref();
        if did_remap {
            let tmp_offsets = offsets.clone();
            let tmp_delays = delays.clone();
            let max_offset = tmp_offsets[..is_.channels]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            for o in 0..os_.channels {
                let ref_idx = deps[o][..is_.channels].iter().position(|b| *b);
                delays[o] = ref_idx.map_or(0, |r| tmp_delays[r]);
                if let Some(r) = ref_idx {
                    for k in r + 1..is_.channels {
                        if !deps[o][k] {
                            continue;
                        }
                        if tmp_offsets[k] != tmp_offsets[r] {
                            return Err(Error::Message(format!(
                                "align_channels(): BUG: channel {k} offset incorrect: {}!={}",
                                tmp_offsets[k], tmp_offsets[r]
                            )));
                        }
                        delays[o] = delays[o].min(tmp_delays[k]);
                    }
                }
                offsets[o] = ref_idx.map_or(max_offset, |r| tmp_offsets[r]);
            }
        }
        for o in os_.channels..is_.channels {
            delays[o] = 0;
            offsets[o] = 0;
        }
        for o in 0..os_.channels {
            offsets[o] += delays[o] - nd_part;
        }
        if !e.channel_offsets(&mut offsets, &mut delays) && os_.fs != is_.fs {
            let (n, d) = ratio(is_.fs, os_.fs);
            for o in 0..os_.channels {
                delays[o] = ratio_mult_ceil(delays[o], n, d);
                offsets[o] = ratio_mult_ceil(offsets[o], n, d);
            }
        }
        nd_part = delays[..os_.channels].iter().copied().fold(0, isize::min);
        for o in 0..os_.channels {
            offsets[o] -= delays[o] - nd_part;
        }
        prev = Some(os_);
        i += 1;
    }
    if let Some(p) = prev {
        if let Some(a) = insert(p, false, &mut offsets, None) {
            effects.push(Box::new(a));
        }
    }
    Ok(-nd_part)
}

/// `ec_set_drain_frames`: how many frames of silence after the input flush the chain, counted
/// in input frames.
pub(crate) fn drain_frames(effects: &[Box<dyn Effect>], istream: Stream, ostream: Stream) -> usize {
    let max_in = effects
        .iter()
        .map(|e| e.istream().channels)
        .max()
        .unwrap_or(1);
    let max_out = effects
        .iter()
        .map(|e| e.ostream().channels)
        .max()
        .unwrap_or(1);
    let max_ch = max_in.max(max_out);
    let mut deps = vec![vec![false; max_in]; max_out];
    let mut samples = vec![0isize; max_ch];
    for e in effects {
        let (is_, os_) = (e.istream(), e.ostream());
        if !query_deps(e.as_ref(), false, &mut deps) {
            let tmp = samples.clone();
            for o in 0..os_.channels {
                samples[o] = (0..is_.channels)
                    .filter(|k| deps[o][*k])
                    .map(|k| tmp[k])
                    .fold(0, isize::max);
            }
        }
        if !e.drain_samples(&mut samples) && os_.fs != is_.fs {
            let (n, d) = ratio(is_.fs, os_.fs);
            for s in samples[..os_.channels].iter_mut() {
                *s = ratio_mult_ceil(*s, n, d);
            }
        }
        for s in samples[os_.channels..is_.channels.max(os_.channels)].iter_mut() {
            *s = 0;
        }
    }
    let mut drain = samples[..ostream.channels]
        .iter()
        .copied()
        .fold(0, isize::max)
        .max(0) as i64;
    if istream.fs != ostream.fs {
        let (n, d) = ratio(ostream.fs, istream.fs);
        drain = drain * n / d;
    }
    drain as usize
}

#[cfg(test)]
mod tests {
    use crate::{Chain, Stream};

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    fn ramp(frames: usize) -> Vec<f64> {
        (0..frames)
            .flat_map(|i| [i as f64 + 1.0, -(i as f64 + 1.0)])
            .collect()
    }

    #[test]
    fn a_delay_shifts_the_signal_and_the_output_grows_by_the_delay() {
        let x = ramp(300);
        let mut c = Chain::from_args(S, &["delay", "40S"]).unwrap();
        let y = c.process(&x);
        assert_eq!(y.len() / 2, 340);
        assert!(y[..2 * 40].iter().all(|v| *v == 0.0));
        assert_eq!(&y[2 * 40..2 * 340], &x[..]);
    }

    #[test]
    fn a_delay_on_one_channel_leaves_the_other_where_it_is() {
        let x = ramp(200);
        let mut c = Chain::from_args(S, &[":0", "delay", "25S"]).unwrap();
        let y = c.process(&x);
        assert_eq!(y.len() / 2, 225);
        for i in 0..200 {
            assert_eq!(y[2 * i + 1], x[2 * i + 1], "right channel at {i}");
            let left = if i >= 25 { x[2 * (i - 25)] } else { 0.0 };
            assert_eq!(y[2 * i], left, "left channel at {i}");
        }
    }
}
