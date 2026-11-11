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
//!
//! # Cost
//!
//! For every output window (one per 512 frames per channel) the similarity search tries about
//! 135 candidate offsets, each comparing 512 frames on every channel: roughly 140k
//! multiply-adds per channel per window. A three-minute stereo clip at 48 kHz stretched by 1.2
//! (about 14k windows) took 2.7 s on an Apple M-series laptop in a release build
//! (`times_a_few_minutes_of_stereo`, run with `--release --ignored --nocapture`).
//!
//! # Limits
//!
//! A result would take more than [`MAX_OUTPUT_SAMPLES`] samples (about 1 GiB of `f32`) is
//! refused with [`Error::TooLarge`] before anything is allocated. [`shift_pitch`] accepts up to
//! [`MAX_SEMITONES`] semitones; further shifts are refused with [`Error::ShiftRange`] because
//! the stretch-then-resample pair would ask for an absurd intermediate length.

use std::f64::consts::TAU;

/// The longest analysis frame, in frames per channel. Short inputs use a smaller window.
const FRAME: usize = 1024;
/// How far (in frames) the similarity search may move the analysis position.
const SEARCH: isize = 256;
/// The most samples (all channels) a stretched result may hold: about 1 GiB of `f32`.
const MAX_OUTPUT_SAMPLES: usize = 1 << 28;
/// Coarse step of the similarity search; refined to single frames around the best candidate.
const SEARCH_STEP: isize = 4;
/// The furthest [`shift_pitch`] moves a signal: four octaves either way.
pub const MAX_SEMITONES: f64 = 48.0;

/// Why a stretch or a pitch shift was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The channel count is zero.
    NoChannels,
    /// There is no sample to work on.
    EmptyInput,
    /// The factor is zero, negative, or not a finite number.
    BadFactor,
    /// The result would be longer than [`MAX_OUTPUT_SAMPLES`] samples.
    TooLarge,
    /// The shift is not a finite number.
    BadShift,
    /// The shift is further than [`MAX_SEMITONES`] semitones.
    ShiftRange,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoChannels => f.write_str("a stretch needs at least one channel"),
            Error::EmptyInput => f.write_str("there is no audio to stretch"),
            Error::BadFactor => f.write_str("the stretch factor must be finite and above zero"),
            Error::TooLarge => f.write_str("the stretched result would be larger than 1 GiB"),
            Error::BadShift => f.write_str("the pitch shift must be a finite number"),
            Error::ShiftRange => write!(
                f,
                "the pitch shift must be within {MAX_SEMITONES} semitones"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Stretches `input` by `factor` (> 1 longer, < 1 shorter) without changing its pitch.
///
/// Returns interleaved samples; the result is about `input`'s frame count × `factor`.
pub fn stretch(input: &[f32], channels: usize, factor: f64) -> Result<Vec<f32>, Error> {
    if channels == 0 {
        return Err(Error::NoChannels);
    }
    if input.is_empty() || input.len() / channels == 0 {
        return Err(Error::EmptyInput);
    }
    if !factor.is_finite() || factor <= 0.0 {
        return Err(Error::BadFactor);
    }
    let frames_in = input.len() / channels;
    let target_f = (frames_in as f64) * factor;
    // The check is in `f64` before the cast, so a huge factor cannot saturate into a length
    // that slips past the comparison. The frame of padding the windows write is included.
    if !target_f.is_finite()
        || (target_f.round().max(1.0) + FRAME as f64) * channels as f64 > MAX_OUTPUT_SAMPLES as f64
    {
        return Err(Error::TooLarge);
    }
    let target = target_f.round().max(1.0) as usize;

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
        for (n, w) in window.iter().copied().enumerate() {
            let at = write + n;
            if at >= out_frames {
                break;
            }
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
    Ok(output)
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
    let mut input_energy = 1e-12f64;
    let mut output_energy = 1e-12f64;
    for n in 0..overlap {
        let written = write + n;
        for ch in 0..channels {
            let out = output[written * channels + ch] as f64;
            let input = input[(start + n) * channels + ch] as f64;
            dot += out * input;
            input_energy += input * input;
            output_energy += out * out;
        }
    }
    // Normalized cross-correlation: loudness of either side does not change the choice.
    Some(dot / (input_energy * output_energy).sqrt())
}

/// Shifts `input` by `semitones` (positive up, negative down) without changing its length.
///
/// The signal is stretched by the pitch ratio and resampled back, so a +12 shift makes the
/// result an octave higher and the same duration. Shifts further than [`MAX_SEMITONES`] are
/// refused rather than asking for an absurd intermediate length.
pub fn shift_pitch(input: &[f32], channels: usize, semitones: f64) -> Result<Vec<f32>, Error> {
    if channels == 0 {
        return Err(Error::NoChannels);
    }
    if input.is_empty() || input.len() / channels == 0 {
        return Err(Error::EmptyInput);
    }
    if !semitones.is_finite() {
        return Err(Error::BadShift);
    }
    if semitones.abs() > MAX_SEMITONES {
        return Err(Error::ShiftRange);
    }
    let ratio = 2f64.powf(semitones / 12.0);
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(Error::BadShift);
    }
    let stretched = stretch(input, channels, ratio)?;
    resample(&stretched, channels, ratio)
}

/// Linear-interpolation resampling by `ratio` (> 1 compresses and raises the pitch).
fn resample(input: &[f32], channels: usize, ratio: f64) -> Result<Vec<f32>, Error> {
    let frames_in = input.len() / channels;
    if frames_in == 0 {
        return Err(Error::EmptyInput);
    }
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(Error::BadFactor);
    }
    let frames_out_f = ((frames_in as f64) / ratio).round();
    if !frames_out_f.is_finite()
        || frames_out_f.max(1.0) * channels as f64 > MAX_OUTPUT_SAMPLES as f64
    {
        return Err(Error::TooLarge);
    }
    let frames_out = frames_out_f.max(1.0) as usize;
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
    Ok(output)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_impossible_input() {
        assert_eq!(stretch(&[], 2, 2.0), Err(Error::EmptyInput));
        assert_eq!(stretch(&[0.0; 16], 0, 2.0), Err(Error::NoChannels));
        assert_eq!(stretch(&[0.0; 16], 2, 0.0), Err(Error::BadFactor));
        assert_eq!(stretch(&[0.0; 16], 2, f64::NAN), Err(Error::BadFactor));
        assert_eq!(
            shift_pitch(&[0.0; 16], 1, f64::INFINITY),
            Err(Error::BadShift)
        );
    }

    #[test]
    fn extreme_pitch_shifts_are_refused_instead_of_overflowing() {
        let input = vec![0.1f32; 12_000 * 2];
        // Far outside the range: no stretch is attempted.
        assert_eq!(shift_pitch(&input, 2, -1200.0), Err(Error::ShiftRange));
        assert_eq!(shift_pitch(&input, 2, 49.0), Err(Error::ShiftRange));
        // The extremes of the range still give the input's length back.
        for semitones in [MAX_SEMITONES, -MAX_SEMITONES] {
            let output = shift_pitch(&input, 2, semitones).expect("inside the range");
            assert_eq!(output.len(), input.len(), "{semitones} semitones");
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn an_oversized_result_is_refused_before_it_is_allocated() {
        assert_eq!(stretch(&[0.25; 8], 1, 1.0e300), Err(Error::TooLarge));
    }

    #[test]
    fn factor_one_reconstructs_the_signal() {
        let rate = 48_000.0;
        let input: Vec<f32> = (0..4_800)
            .map(|i| (TAU * 440.0 * i as f64 / rate).sin() as f32)
            .collect();
        let output = stretch(&input, 1, 1.0).expect("a valid stretch");
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

    /// The timing note in the module docs; not part of the normal test run.
    #[test]
    #[ignore = "timing: run with --release --ignored --nocapture"]
    fn times_a_few_minutes_of_stereo() {
        let rate = 48_000.0;
        let frames = 180 * 48_000;
        let input: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let t = TAU * 440.0 * i as f64 / rate;
                [t.sin() as f32, (t * 1.5).sin() as f32]
            })
            .collect();
        let started = std::time::Instant::now();
        let output = stretch(&input, 2, 1.2).expect("three minutes of stereo");
        println!(
            "3 min stereo at 48 kHz, factor 1.2: {:?} ({} frames out)",
            started.elapsed(),
            output.len() / 2
        );
        assert_eq!(
            output.len() / 2,
            (frames as f64 * 1.2).round() as usize,
            "the whole clip is stretched"
        );
    }
}
