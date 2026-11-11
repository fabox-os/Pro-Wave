//! Sine tones through the stretcher: length, pitch and stereo channels.

use prowave_stretch::{shift_pitch, stretch};
use std::f64::consts::TAU;

const RATE: f64 = 48_000.0;
const FRAMES: usize = 12_000; // 0.25 s

fn mono(hz: f64) -> Vec<f32> {
    (0..FRAMES)
        .map(|i| (TAU * hz * i as f64 / RATE).sin() as f32)
        .collect()
}

fn stereo(left: f64, right: f64) -> Vec<f32> {
    (0..FRAMES)
        .flat_map(|i| {
            [
                (TAU * left * i as f64 / RATE).sin() as f32,
                (TAU * right * i as f64 / RATE).sin() as f32,
            ]
        })
        .collect()
}

/// The Goertzel magnitude at `hz` in one channel.
fn magnitude(samples: &[f32], channels: usize, channel: usize, hz: f64) -> f64 {
    if samples.len() / channels == 0 {
        return 0.0;
    }
    let coeff = 2.0 * (TAU * hz / RATE).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for frame in samples.chunks_exact(channels) {
        let s0 = f64::from(frame[channel]) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

fn assert_finite(samples: &[f32]) {
    assert!(
        samples.iter().all(|sample| sample.is_finite()),
        "the result must contain no NaN or infinity"
    );
}

#[test]
fn stretching_doubles_the_length_and_keeps_the_pitch() {
    let input = mono(440.0);
    let output = stretch(&input, 1, 2.0).expect("a valid stretch");
    assert_finite(&output);
    let expected = 2 * FRAMES;
    assert!(
        output.len().abs_diff(expected) <= 2,
        "{} frames, expected about {expected}",
        output.len()
    );
    let target = magnitude(&output, 1, 0, 440.0);
    let other = magnitude(&output, 1, 0, 300.0) + magnitude(&output, 1, 0, 660.0);
    assert!(target > other * 3.0, "440 Hz {target} vs other {other}");
}

#[test]
fn an_octave_shift_raises_pitch_and_keeps_length() {
    let input = mono(440.0);
    let output = shift_pitch(&input, 1, 12.0).expect("a valid shift");
    assert_finite(&output);
    assert!(
        output.len().abs_diff(FRAMES) <= 2,
        "{} frames, expected about {FRAMES}",
        output.len()
    );
    let target = magnitude(&output, 1, 0, 880.0);
    let other = magnitude(&output, 1, 0, 440.0) + magnitude(&output, 1, 0, 660.0);
    assert!(target > other * 3.0, "880 Hz {target} vs other {other}");
}

#[test]
fn stereo_channels_stay_in_step() {
    let input = stereo(440.0, 880.0);
    let output = stretch(&input, 2, 1.5).expect("a valid stretch");
    assert_finite(&output);
    assert_eq!(output.len() % 2, 0);
    let frames = output.len() / 2;
    assert!(frames.abs_diff((FRAMES as f64 * 1.5) as usize) <= 2);
    let left = magnitude(&output, 2, 0, 440.0);
    let left_wrong = magnitude(&output, 2, 0, 880.0);
    let right = magnitude(&output, 2, 1, 880.0);
    let right_wrong = magnitude(&output, 2, 1, 440.0);
    assert!(
        left > left_wrong * 3.0,
        "left: 440 {left} vs 880 {left_wrong}"
    );
    assert!(
        right > right_wrong * 3.0,
        "right: 880 {right} vs 440 {right_wrong}"
    );
}

#[test]
fn output_stays_bounded_and_never_panics() {
    let input = mono(220.0);
    for factor in [0.5, 0.75, 1.25, 3.0] {
        let output = stretch(&input, 1, factor).expect("a valid stretch");
        assert_finite(&output);
        let peak = output
            .iter()
            .fold(0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak <= 2.0, "factor {factor} peaked at {peak}");
    }
    assert!(stretch(&[0.25; 1], 1, 2.0)
        .expect("a one-frame stretch")
        .iter()
        .all(|s| s.is_finite()));
}

#[test]
fn an_oversized_request_is_refused() {
    let input = mono(440.0);
    assert_eq!(
        stretch(&input, 1, 1.0e9),
        Err(prowave_stretch::Error::TooLarge)
    );
}

#[test]
fn a_quiet_input_is_stretched_like_a_loud_one() {
    let loud = mono(440.0);
    let quiet: Vec<f32> = loud.iter().map(|s| s * 0.01).collect();
    let a = stretch(&loud, 1, 1.5).expect("a valid stretch");
    let b = stretch(&quiet, 1, 1.5).expect("a valid stretch");
    assert_eq!(a.len(), b.len());
    // Same choices of offset, so the quiet result is the loud one scaled.
    for (x, y) in a.iter().zip(&b).skip(64).take(4_000) {
        assert!((x * 0.01 - y).abs() < 1e-3, "{x} {y}");
    }
}

#[test]
fn the_end_of_a_stretched_clip_is_not_silent() {
    let input = mono(440.0);
    let output = stretch(&input, 1, 2.0).expect("a valid stretch");
    let tail = &output[output.len() - output.len() / 20..];
    let rms = (tail.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
    assert!(rms > 0.05, "the last 5% of the clip is silent (RMS {rms})");
}
