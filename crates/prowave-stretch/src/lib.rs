//! Time stretching and pitch shifting for audio regions (Logic: Flex Time / Time and Pitch
//! Machine).
//!
//! [`stretch`] changes duration without changing pitch with WSOLA (waveform-similarity
//! overlap-add): fixed synthesis hops of half a Hann-window, an analysis hop scaled by the
//! stretch factor, and a similarity search that picks the input segment that continues the
//! previous overlap most smoothly. [`shift_pitch`] stretches by the pitch ratio and resamples
//! back to the original length.
//!
//! Both functions work on interleaved samples for any channel count; stereo and multichannel
//! files keep their channels in step because the similarity search uses a mono mix and the
//! chosen offset is applied to every channel. The functions are offline; a streaming wrapper
//! for the audio thread is a separate step.

use std::f64::consts::TAU;

/// The longest analysis frame, in frames per channel. Short inputs use a smaller window.
const FRAME: usize = 1024;
/// How far (in frames) the similarity search may move the analysis position.
const SEARCH: isize = 256;
/// Coarse step of the similarity search; refined to single frames around the best candidate.
const SEARCH_STEP: isize = 4;

/// Stretches `input` by `factor` (> 1 longer, < 1 shorter) without changing its pitch.
///
/// Returns interleaved samples; the result is about `input`'s frame count × `factor`. Empty
/// input, zero channels or a non-positive/non-finite factor give an empty result.
pub fn stretch(input: &[f32], channels: usize, factor: f64) -> Vec<f32> {
    if channels == 0 || input.is_empty() || !factor.is_finite() || factor <= 0.0 {
        return Vec::new();
    }
    let frames_in = input.len() / channels;
    if frames_in == 0 {
        return Vec::new();
    }
    let target = ((frames_in as f64) * factor).round().max(1.0) as usize;

    // Short clips get a shorter window, still even.
    let frame = FRAME.min(frames_in.max(64)) & !1;
    let overlap = frame / 2;
    let hop_out = overlap;
    let hop_in = ((hop_out as f64) / factor).round().max(1.0) as usize;
    let frames_out = target.div_ceil(hop_out).saturating_add(1);
    let out_frames = frames_out * hop_out + frame;
    let mut output = vec![0.0f32; out_frames * channels];
    let mut window_sum = vec![0.0f32; out_frames];
    let window: Vec<f32> = (0..frame)
        .map(|n| 0.5 - 0.5 * (TAU * n as f64 / (frame - 1) as f64).cos() as f32)
        .collect();

    for f in 0..frames_out {
        let nominal = (f * hop_in) as isize;
        let offset = if f == 0 {
            0
        } else {
            best_offset(
                input,
                channels,
                &output,
                f * hop_out,
                nominal,
                overlap,
                frames_in,
            )
        };
        let start = (nominal + offset).max(0) as usize;
        let write = f * hop_out;
        for n in 0..frame {
            let at = write + n;
            if at >= out_frames {
                break;
            }
            let w = window[n];
            window_sum[at] += w;
            let source = start + n;
            if source >= frames_in {
                continue;
            }
            for ch in 0..channels {
                output[at * channels + ch] += input[source * channels + ch] * w;
            }
        }
    }

    for (frame_index, sum) in window_sum.iter().enumerate() {
        if *sum > 1e-3 {
            for ch in 0..channels {
                output[frame_index * channels + ch] /= *sum;
            }
        }
    }
    output.truncate(target * channels);
    output
}

/// The offset within ±[`SEARCH`] of `nominal` whose input segment best continues the output
/// already written at `write` (the previous window's second half).
fn best_offset(
    input: &[f32],
    channels: usize,
    output: &[f32],
    write: usize,
    nominal: isize,
    overlap: usize,
    frames_in: usize,
) -> isize {
    let mut best = 0isize;
    let mut best_score = f64::NEG_INFINITY;
    let mut delta = -SEARCH;
    while delta <= SEARCH {
        if let Some(score) = similarity(
            input,
            channels,
            output,
            write,
            nominal + delta,
            overlap,
            frames_in,
        ) {
            if score > best_score {
                best_score = score;
                best = delta;
            }
        }
        delta += SEARCH_STEP;
    }
    let coarse = best;
    for delta in (coarse - SEARCH_STEP + 1)..=(coarse + SEARCH_STEP - 1) {
        if delta % SEARCH_STEP == 0 {
            continue;
        }
        if let Some(score) = similarity(
            input,
            channels,
            output,
            write,
            nominal + delta,
            overlap,
            frames_in,
        ) {
            if score > best_score {
                best_score = score;
                best = delta;
            }
        }
    }
    best
}

/// How well the input segment at `start` continues the output written at `write`.
fn similarity(
    input: &[f32],
    channels: usize,
    output: &[f32],
    write: usize,
    start: isize,
    overlap: usize,
    frames_in: usize,
) -> Option<f64> {
    if start < 0 || start as usize + overlap > frames_in {
        return None;
    }
    let start = start as usize;
    let mut dot = 0f64;
    let mut energy = 1e-12f64;
    for n in 0..overlap {
        let written = write + n;
        for ch in 0..channels {
            let out = output[written * channels + ch] as f64;
            let input = input[(start + n) * channels + ch] as f64;
            dot += out * input;
            energy += input * input;
        }
    }
    Some(dot / energy.sqrt())
}

/// Shifts `input` by `semitones` (positive up, negative down) without changing its length.
///
/// The signal is stretched by the pitch ratio and resampled back, so a +12 shift makes the
/// result an octave higher and the same duration.
pub fn shift_pitch(input: &[f32], channels: usize, semitones: f64) -> Vec<f32> {
    if channels == 0 || input.is_empty() || !semitones.is_finite() {
        return Vec::new();
    }
    let ratio = 2f64.powf(semitones / 12.0);
    if !ratio.is_finite() || ratio <= 0.0 {
        return Vec::new();
    }
    let stretched = stretch(input, channels, ratio);
    if stretched.is_empty() {
        return Vec::new();
    }
    resample(&stretched, channels, ratio)
}

/// Linear-interpolation resampling by `ratio` (> 1 compresses and raises the pitch).
fn resample(input: &[f32], channels: usize, ratio: f64) -> Vec<f32> {
    let frames_in = input.len() / channels;
    if frames_in == 0 || !ratio.is_finite() || ratio <= 0.0 {
        return Vec::new();
    }
    let frames_out = ((frames_in as f64) / ratio).round().max(1.0) as usize;
    let mut output = vec![0.0f32; frames_out * channels];
    for frame in 0..frames_out {
        let source = frame as f64 * ratio;
        let at = source.floor() as usize;
        let frac = (source - at as f64) as f32;
        let next = (at + 1).min(frames_in - 1);
        for ch in 0..channels {
            let a = input[at * channels + ch];
            let b = input[next * channels + ch];
            output[frame * channels + ch] = a + (b - a) * frac;
        }
    }
    output
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_impossible_input() {
        assert!(stretch(&[], 2, 2.0).is_empty());
        assert!(stretch(&[0.0; 16], 0, 2.0).is_empty());
        assert!(stretch(&[0.0; 16], 2, 0.0).is_empty());
        assert!(stretch(&[0.0; 16], 2, f64::NAN).is_empty());
        assert!(shift_pitch(&[0.0; 16], 1, f64::INFINITY).is_empty());
    }

    #[test]
    fn factor_one_reconstructs_the_signal() {
        let rate = 48_000.0;
        let input: Vec<f32> = (0..4_800)
            .map(|i| (TAU * 440.0 * i as f64 / rate).sin() as f32)
            .collect();
        let output = stretch(&input, 1, 1.0);
        assert_eq!(output.len(), input.len());
        let start = 1_000;
        let end = 3_000;
        let (mut dot, mut a, mut b) = (0f64, 0f64, 0f64);
        for i in start..end {
            dot += f64::from(input[i]) * f64::from(output[i]);
            a += f64::from(input[i]).powi(2);
            b += f64::from(output[i]).powi(2);
        }
        let correlation = dot / (a.sqrt() * b.sqrt()).max(1e-12);
        assert!(correlation > 0.9, "correlation {correlation}");
    }
}
