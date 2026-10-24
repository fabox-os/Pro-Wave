//! Audio analysis for Smart Tempo and Flex Time: where the notes and hits start in a recording
//! (onsets) and the tempo they imply. Plain DSP on a mono mix, run off the audio thread.
//!
//! The method: the energy of the sound's first difference (which leaves out the low hum and
//! keeps the attacks) in blocks of 2.5 ms, as a logarithm; the rises of it are the onset
//! strength. Onsets are its peaks over a moving threshold. The tempo is the lag at which the
//! onset strength repeats (its autocorrelation, with the lags of two and four beats helping),
//! between 60 and 200 beats per minute, nudged towards the usual speeds and refined to a
//! fraction of a beat per minute.

/// Blocks of the onset strength per second.
const RATE: f64 = 400.0;
/// The slowest and fastest tempo looked for.
pub const MIN_BPM: f64 = 60.0;
pub const MAX_BPM: f64 = 200.0;
/// The smallest rise of the log energy that is an onset.
const MIN_RISE: f32 = 0.3;
/// No onset closer than this to the one before it, in seconds.
const MIN_GAP: f64 = 0.05;
/// Analysis stops here: ten minutes of a recording say what its tempo is.
pub const MAX_SECONDS: f64 = 600.0;
/// The shortest recording a tempo is guessed for, in seconds (about four beats at 120).
const MIN_SECONDS: f64 = 2.0;

/// A tempo guessed from audio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoEstimate {
    pub bpm: f64,
    /// 0 to 1: how much of the onset strength repeats at this tempo.
    pub confidence: f64,
}

/// What the analysis found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    /// Where hits and notes start, in seconds from the start of the file.
    pub onsets: Vec<f64>,
    pub tempo: Option<TempoEstimate>,
}

/// Feeds mono samples in; [`Analyzer::finish`] gives the result.
pub struct Analyzer {
    block: usize,
    filled: usize,
    energy: f64,
    last: f32,
    envelope: Vec<f32>,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Self {
            block: ((rate / RATE).round() as usize).max(1),
            filled: 0,
            energy: 0.0,
            last: 0.0,
            envelope: Vec::new(),
        }
    }

    /// Whether enough has been read ([`MAX_SECONDS`]).
    pub fn is_full(&self) -> bool {
        self.envelope.len() as f64 / RATE >= MAX_SECONDS
    }

    pub fn push(&mut self, sample: f32) {
        // Not-a-number and infinite samples count as silence, as in playback.
        let x = if sample.is_finite() { sample } else { 0.0 };
        let d = f64::from(x - self.last);
        self.last = x;
        self.energy += d * d;
        self.filled += 1;
        if self.filled == self.block {
            let rms = (self.energy / self.block as f64).sqrt();
            self.envelope.push((1.0 + 1000.0 * rms).ln() as f32);
            self.filled = 0;
            self.energy = 0.0;
        }
    }

    pub fn finish(self) -> Analysis {
        analyze_envelope(&self.envelope)
    }
}

/// The result for an onset-strength curve of [`RATE`] blocks per second.
fn analyze_envelope(envelope: &[f32]) -> Analysis {
    let flux: Vec<f32> = envelope
        .windows(2)
        .map(|w| (w[1] - w[0]).max(0.0))
        .collect();
    Analysis {
        onsets: pick_onsets(&flux),
        tempo: estimate_tempo(&flux),
    }
}

/// Peaks of the onset strength over a threshold that follows its surroundings.
fn pick_onsets(flux: &[f32]) -> Vec<f64> {
    let half = (0.15 * RATE) as usize;
    let gap = (MIN_GAP * RATE) as usize;
    let max = flux.iter().copied().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return Vec::new();
    }
    // A running sum gives each block's surrounding mean in constant time.
    let mut sums = Vec::with_capacity(flux.len() + 1);
    sums.push(0.0f64);
    for f in flux {
        sums.push(sums[sums.len() - 1] + f64::from(*f));
    }
    let mut out: Vec<f64> = Vec::new();
    let mut last: Option<usize> = None;
    for i in 1..flux.len().saturating_sub(1) {
        let f = flux[i];
        if f < flux[i - 1] || f <= flux[i + 1] {
            continue;
        }
        let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(flux.len()));
        let mean = (sums[hi] - sums[lo]) / (hi - lo) as f64;
        // Above the surroundings, and above a floor so that hiss does not count.
        // (a rise of the log energy of 0.3 is about a third more sound.)
        if f64::from(f) < mean * 1.6 + f64::from(max) * 0.08 || f < MIN_RISE {
            continue;
        }
        if last.is_some_and(|l| i - l < gap) {
            continue;
        }
        last = Some(i);
        // The rise happened during the block before the peak.
        out.push(i as f64 / RATE);
    }
    out
}

/// The tempo at which the onset strength repeats.
fn estimate_tempo(flux: &[f32]) -> Option<TempoEstimate> {
    if (flux.len() as f64) < MIN_SECONDS * RATE {
        return None;
    }
    let mean = flux.iter().map(|f| f64::from(*f)).sum::<f64>() / flux.len() as f64;
    let x: Vec<f64> = flux.iter().map(|f| f64::from(*f) - mean).collect();
    let zero: f64 = x.iter().map(|v| v * v).sum();
    if zero <= 1e-9 {
        return None;
    }
    let acf = |lag: usize| -> f64 {
        if lag >= x.len() {
            return 0.0;
        }
        // Normalised by the overlap, so a short recording does not favour short lags.
        let n = x.len() - lag;
        x[..n]
            .iter()
            .zip(&x[lag..])
            .map(|(a, b)| a * b)
            .sum::<f64>()
            / n as f64
            / (zero / x.len() as f64)
    };
    let lag_of = |bpm: f64| (60.0 * RATE / bpm).round() as usize;
    let (min_lag, max_lag) = (lag_of(MAX_BPM), lag_of(MIN_BPM));
    // Score of a beat length: it, and two and four of them, should all repeat.
    let score = |lag: usize| -> f64 {
        let bpm = 60.0 * RATE / lag as f64;
        // A mild liking for the usual speeds (a bump around 120 on a log scale).
        let pref = (-0.5 * ((bpm / 120.0).ln() / 0.9).powi(2)).exp();
        (acf(lag) + 0.5 * acf(2 * lag) + 0.25 * acf(4 * lag)) * (0.6 + 0.4 * pref)
    };
    let scores: Vec<f64> = (min_lag..=max_lag).map(score).collect();
    let (best, best_score) = scores
        .iter()
        .copied()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if best_score <= 0.0 {
        return None;
    }
    // Refine between blocks with a parabola through the peak and its neighbours.
    let mut lag = (min_lag + best) as f64;
    if best > 0 && best + 1 < scores.len() {
        let (a, b, c) = (scores[best - 1], scores[best], scores[best + 1]);
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            lag += (0.5 * (a - c) / denom).clamp(-0.5, 0.5);
        }
    }
    // A beat length also scores well for two beats at half of it (a 150 would be read as 75):
    // when the half repeats nearly as strongly, it is the beat.
    let mut at = min_lag + best;
    while at / 2 >= min_lag {
        let half = at / 2;
        let (local, strength) = (half.saturating_sub(2)..=half + 2)
            .map(|l| (l, acf(l)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((half, 0.0));
        if strength < 0.8 * acf(at) {
            break;
        }
        lag = local as f64;
        at = local;
    }
    let bpm = 60.0 * RATE / lag;
    let confidence = (acf(at).clamp(0.0, 1.0)).sqrt();
    (bpm.is_finite() && confidence > 0.05).then_some(TempoEstimate { bpm, confidence })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// A click track: a short burst of noise at every beat of `bpm`, with a little hiss.
    fn clicks(bpm: f64, seconds: f64, offset: f64) -> Vec<f32> {
        let n = (seconds * f64::from(SR)) as usize;
        let mut s = vec![0.0f32; n];
        let mut seed = 0x1234_5678u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        for v in s.iter_mut() {
            *v = noise() * 0.002;
        }
        let beat = 60.0 / bpm;
        let mut t = offset;
        while t < seconds {
            let at = (t * f64::from(SR)) as usize;
            for k in 0..(0.012 * f64::from(SR)) as usize {
                if at + k < n {
                    let decay = (-(k as f32) / 250.0).exp();
                    s[at + k] += noise() * 0.6 * decay;
                }
            }
            t += beat;
        }
        s
    }

    fn analyze(samples: &[f32], sr: u32) -> Analysis {
        let mut a = Analyzer::new(sr);
        for s in samples {
            a.push(*s);
        }
        a.finish()
    }

    #[test]
    fn the_tempo_of_a_click_track_is_found() {
        for bpm in [64.0, 75.0, 90.0, 100.0, 120.0, 128.0, 140.0, 150.0, 174.0] {
            let found = analyze(&clicks(bpm, 20.0, 0.3), SR);
            let t = found.tempo.unwrap_or_else(|| panic!("no tempo for {bpm}"));
            assert!((t.bpm - bpm).abs() < 1.0, "{bpm}: found {}", t.bpm);
            assert!(t.confidence > 0.3, "{bpm}: {}", t.confidence);
        }
    }

    #[test]
    fn onsets_are_where_the_clicks_are() {
        let found = analyze(&clicks(120.0, 10.0, 0.25), SR);
        // 10 s at two beats a second, the first at 0.25 s.
        assert!(
            (19..=21).contains(&found.onsets.len()),
            "{}",
            found.onsets.len()
        );
        for (i, t) in found.onsets.iter().enumerate() {
            let expected = 0.25 + i as f64 * 0.5;
            assert!((t - expected).abs() < 0.03, "{i}: {t} vs {expected}");
        }
    }

    #[test]
    fn other_sample_rates_give_the_same_answer() {
        for sr in [22_050u32, 48_000, 96_000] {
            let n = (12.0 * f64::from(sr)) as usize;
            let mut s = vec![0.0f32; n];
            let mut seed = 99u32;
            let beat = 60.0 / 110.0;
            let mut t = 0.1;
            while t < 12.0 {
                let at = (t * f64::from(sr)) as usize;
                for k in 0..(0.012 * f64::from(sr)) as usize {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    if at + k < n {
                        s[at + k] = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 1.2;
                    }
                }
                t += beat;
            }
            let t = analyze(&s, sr).tempo.expect("a tempo");
            assert!((t.bpm - 110.0).abs() < 1.0, "{sr}: {}", t.bpm);
        }
    }

    #[test]
    fn silence_noise_and_short_clips_have_no_tempo() {
        assert_eq!(analyze(&vec![0.0; 44_100 * 10], SR), Analysis::default());
        assert!(analyze(&[], SR).tempo.is_none());
        assert!(
            analyze(&clicks(120.0, 1.0, 0.1), SR).tempo.is_none(),
            "too short"
        );
        // Steady noise has no beat: no onsets worth the name, and at most a weak tempo.
        let mut seed = 7u32;
        let noise: Vec<f32> = (0..44_100 * 10)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.5
            })
            .collect();
        let found = analyze(&noise, SR);
        assert!(found.onsets.len() < 20, "{}", found.onsets.len());
        assert!(found.tempo.is_none_or(|t| t.confidence < 0.5));
    }

    #[test]
    fn odd_samples_and_extreme_rates_do_not_break_it() {
        let mut s = clicks(120.0, 5.0, 0.2);
        s[1000] = f32::NAN;
        s[2000] = f32::INFINITY;
        s[3000] = f32::NEG_INFINITY;
        assert!(analyze(&s, SR).tempo.is_some());
        // A silly sample rate is clamped rather than dividing by zero.
        for sr in [0u32, 1, 8, 384_000] {
            let _ = analyze(&clicks(120.0, 0.5, 0.0)[..2000], sr);
        }
        let mut full = Analyzer::new(8000);
        assert!(!full.is_full());
        for _ in 0..(8000.0 * MAX_SECONDS) as usize + 8000 {
            full.push(0.0);
        }
        assert!(full.is_full());
    }

    #[test]
    fn the_work_is_bounded() {
        // Ten minutes of audio analyse in a few seconds even in a debug build.
        let began = std::time::Instant::now();
        let mut a = Analyzer::new(SR);
        let beat = (SR as f32 * 0.5) as usize;
        for i in 0..(f64::from(SR) * MAX_SECONDS) as usize {
            let level = if i % beat < 400 { 0.5 } else { 0.0 };
            a.push(if i % 2 == 0 { level } else { -level });
        }
        let found = a.finish();
        assert!(found.tempo.is_some());
        assert!(began.elapsed().as_secs() < 40, "{:?}", began.elapsed());
    }
}
