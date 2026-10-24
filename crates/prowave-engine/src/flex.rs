//! Flex Time and Flex Pitch, offline: a recording moved in time (its parts sped up or slowed
//! down so that chosen points land where they should) or its notes moved in pitch, written to
//! a new file that the region then plays. Nothing here runs on the audio thread.
//!
//! - Time: a list of [`WarpPoint`]s says where moments of the recording (`src`, seconds into
//!   the file) go (`dst`, seconds into the result). Between points the sound is stretched by
//!   the mode's method: **Slicing** moves each piece as it is (gaps are silent, overlaps
//!   cut), **Speed** plays it faster or slower (the pitch follows, like tape), and the others
//!   keep the pitch with WSOLA (overlap-add of pieces chosen to line up with the sound before
//!   them), with a window that suits the material: short for **Monophonic** and
//!   **Rhythmic**, longer for **Polyphonic**, and long and without the lining up for
//!   **Tempophone**, which then sounds grainy on purpose.
//! - Pitch: [`pitch_track`] finds the pitch (YIN), [`detect_notes`] the notes between onsets,
//!   and [`shift_pitch`] moves stretches of the recording up or down (stretched with WSOLA,
//!   then resampled back to the original length).
use std::io::Write;
use std::path::Path;

/// How a Flex Time edit stretches the sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FlexMode {
    Slicing,
    #[default]
    Rhythmic,
    Monophonic,
    Polyphonic,
    Speed,
    Tempophone,
}

impl FlexMode {
    pub const ALL: [FlexMode; 6] = [
        FlexMode::Slicing,
        FlexMode::Rhythmic,
        FlexMode::Monophonic,
        FlexMode::Polyphonic,
        FlexMode::Speed,
        FlexMode::Tempophone,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FlexMode::Slicing => "Slicing",
            FlexMode::Rhythmic => "Rhythmic",
            FlexMode::Monophonic => "Monophonic",
            FlexMode::Polyphonic => "Polyphonic",
            FlexMode::Speed => "Speed",
            FlexMode::Tempophone => "Tempophone",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The WSOLA window in seconds and how far a piece may move to line up, as a fraction of
    /// the window.
    fn window(self) -> (f64, f64) {
        match self {
            FlexMode::Monophonic => (0.030, 0.25),
            FlexMode::Rhythmic => (0.040, 0.25),
            FlexMode::Polyphonic => (0.080, 0.25),
            FlexMode::Tempophone => (0.120, 0.0),
            FlexMode::Slicing | FlexMode::Speed => (0.040, 0.25),
        }
    }
}

/// Where a moment of the recording goes: `src` seconds into the file lands at `dst` seconds
/// into the result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpPoint {
    pub src: f64,
    pub dst: f64,
}

/// The longest result made, in seconds, so a wild edit cannot fill the memory.
pub const MAX_SECONDS: f64 = 1200.0;

/// The time map: the points in order of `src`, both times rising, starting at (0, 0). After
/// the last point the recording goes on at its own speed.
fn clean_points(points: &[WarpPoint], src_len: f64) -> Vec<WarpPoint> {
    let mut v: Vec<WarpPoint> = points
        .iter()
        .copied()
        .filter(|p| p.src.is_finite() && p.dst.is_finite() && p.src > 0.0 && p.src < src_len)
        .map(|p| WarpPoint {
            src: p.src,
            dst: p.dst.clamp(0.0, MAX_SECONDS),
        })
        .collect();
    v.sort_by(|a, b| a.src.total_cmp(&b.src));
    let mut out = vec![WarpPoint { src: 0.0, dst: 0.0 }];
    for p in v {
        let last = out[out.len() - 1];
        // Both must move forward by at least a millisecond, or the point is left out.
        if p.src > last.src + 0.001 && p.dst > last.dst + 0.001 {
            out.push(p);
        }
    }
    out
}

/// Where `src` lands.
fn forward(points: &[WarpPoint], src: f64) -> f64 {
    let i = points.partition_point(|p| p.src <= src).max(1) - 1;
    let a = points[i];
    match points.get(i + 1) {
        Some(b) => a.dst + (src - a.src) * (b.dst - a.dst) / (b.src - a.src),
        None => a.dst + (src - a.src),
    }
}

/// Which moment of the recording is heard at `dst`.
fn backward(points: &[WarpPoint], dst: f64) -> f64 {
    let i = points.partition_point(|p| p.dst <= dst).max(1) - 1;
    let a = points[i];
    match points.get(i + 1) {
        Some(b) => a.src + (dst - a.dst) * (b.src - a.src) / (b.dst - a.dst),
        None => a.src + (dst - a.dst),
    }
}

/// A stereo recording (interleaved) moved in time as `points` say.
pub fn warp(input: &[f32], sample_rate: u32, points: &[WarpPoint], mode: FlexMode) -> Vec<f32> {
    let sr = f64::from(sample_rate.max(1));
    let frames = input.len() / 2;
    let src_len = frames as f64 / sr;
    let map = clean_points(points, src_len);
    let dst_len = forward(&map, src_len).min(MAX_SECONDS);
    let out_frames = (dst_len * sr).round() as usize;
    if frames == 0 || out_frames == 0 {
        return Vec::new();
    }
    match mode {
        FlexMode::Slicing => slices(input, sr, &map, out_frames),
        FlexMode::Speed => varispeed(input, sr, &map, out_frames),
        _ => stretch_pieces(input, sr, &map, out_frames, mode),
    }
}

fn frame_at(input: &[f32], i: usize) -> (f32, f32) {
    match (input.get(2 * i), input.get(2 * i + 1)) {
        (Some(l), Some(r)) => (*l, *r),
        _ => (0.0, 0.0),
    }
}

/// Linear interpolation between frames at a fractional position.
fn frame_lerp(input: &[f32], pos: f64) -> (f32, f32) {
    if pos < 0.0 {
        return (0.0, 0.0);
    }
    let i = pos.floor() as usize;
    let f = (pos - i as f64) as f32;
    let (a, b) = (frame_at(input, i), frame_at(input, i + 1));
    (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
}

/// Each piece between two points moved as it is.
fn slices(input: &[f32], sr: f64, map: &[WarpPoint], out_frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; out_frames * 2];
    let fade = (0.003 * sr) as usize;
    let src_frames = input.len() / 2;
    for (k, a) in map.iter().enumerate() {
        let src_end = map.get(k + 1).map_or(src_frames, |b| (b.src * sr) as usize);
        let dst_end = map
            .get(k + 1)
            .map_or(out_frames, |b| ((b.dst * sr) as usize).min(out_frames));
        let (s0, d0) = ((a.src * sr) as usize, (a.dst * sr) as usize);
        let len = src_end.saturating_sub(s0).min(dst_end.saturating_sub(d0));
        let cut = src_end.saturating_sub(s0) > len;
        for j in 0..len {
            // A short fade in at a slice's start and a fade out where it is cut off.
            let mut g = 1.0f32;
            if k > 0 && j < fade {
                g = j as f32 / fade as f32;
            }
            if cut && len - j <= fade {
                g = g.min((len - j) as f32 / fade as f32);
            }
            let (l, r) = frame_at(input, s0 + j);
            out[2 * (d0 + j)] = l * g;
            out[2 * (d0 + j) + 1] = r * g;
        }
    }
    out
}

/// Faster or slower, with the pitch, like tape.
fn varispeed(input: &[f32], sr: f64, map: &[WarpPoint], out_frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(out_frames * 2);
    for j in 0..out_frames {
        let src = backward(map, j as f64 / sr) * sr;
        let (l, r) = frame_lerp(input, src);
        out.push(l);
        out.push(r);
    }
    out
}

/// Each piece between two points stretched on its own with WSOLA, starting exactly at its
/// point: the points are usually on the hits, and a hit then lands where it was asked to go
/// instead of being lost between two windows.
fn stretch_pieces(
    input: &[f32],
    sr: f64,
    map: &[WarpPoint],
    out_frames: usize,
    mode: FlexMode,
) -> Vec<f32> {
    let src_frames = input.len() / 2;
    let mut out = vec![0.0f32; out_frames * 2];
    for (k, a) in map.iter().enumerate() {
        let (s0, d0) = ((a.src * sr) as usize, (a.dst * sr) as usize);
        let (s1, d1) = match map.get(k + 1) {
            Some(b) => (
                (b.src * sr) as usize,
                ((b.dst * sr) as usize).min(out_frames),
            ),
            None => (src_frames, out_frames),
        };
        if d1 <= d0 || s1 <= s0 || s0 >= src_frames {
            continue;
        }
        let ratio = (s1 - s0) as f64 / (d1 - d0) as f64;
        let piece = wsola(&input[2 * s0..], sr, d1 - d0, mode, |t| t * ratio);
        out[2 * d0..2 * d1].copy_from_slice(&piece[..2 * (d1 - d0)]);
    }
    out
}

/// The pitch kept, the time moved: WSOLA. `src_at(dst seconds)` is the moment of the
/// recording wanted at each moment of the result.
fn wsola(
    input: &[f32],
    sr: f64,
    out_frames: usize,
    mode: FlexMode,
    src_at: impl Fn(f64) -> f64,
) -> Vec<f32> {
    let (window_s, search) = mode.window();
    let w = ((window_s * sr) as usize).max(16) & !1;
    let hop = w / 2;
    let delta = (w as f64 * search) as isize;
    let src_frames = input.len() / 2;
    // Hann window: two of them half a window apart add up to one.
    let hann: Vec<f32> = (0..w)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / w as f32).cos())
        .collect();
    // A mono copy, every other sample, for lining the pieces up quickly.
    let mono = |i: isize| -> f32 {
        if i < 0 {
            return 0.0;
        }
        let (l, r) = frame_at(input, i as usize);
        l + r
    };
    let mut out = vec![0.0f32; (out_frames + w) * 2];
    let mut norm = vec![0.0f32; out_frames + w];
    // Where the previous piece came from; its natural continuation is a hop later.
    let mut prev: Option<isize> = None;
    let mut o = 0usize;
    while o < out_frames {
        let centre = o as f64 + hop as f64;
        // The first piece starts exactly at the start (often a hit); the others are placed by
        // their middles.
        let want = if o == 0 {
            (src_at(0.0) * sr) as isize
        } else {
            (src_at(centre / sr) * sr) as isize - hop as isize
        };
        let pos = match prev {
            Some(p) if delta > 0 => {
                // The piece near `want` that best continues the sound so far.
                let natural = p + hop as isize;
                let mut best = (f32::NEG_INFINITY, want);
                let mut k = -delta;
                while k <= delta {
                    let cand = want + k;
                    let mut c = 0.0f32;
                    let mut i = 0;
                    while i < hop {
                        c += mono(cand + i as isize) * mono(natural + i as isize);
                        i += 4;
                    }
                    if c > best.0 {
                        best = (c, cand);
                    }
                    k += 2;
                }
                best.1
            }
            _ => want,
        };
        for i in 0..w {
            let s = pos + i as isize;
            if s < 0 || s as usize >= src_frames {
                continue;
            }
            let (l, r) = frame_at(input, s as usize);
            let g = hann[i];
            out[2 * (o + i)] += l * g;
            out[2 * (o + i) + 1] += r * g;
            norm[o + i] += g;
        }
        prev = Some(pos);
        o += hop;
    }
    // Where the windows do not add up to one (the very start), even it out.
    for (i, n) in norm.iter().enumerate().take(out_frames) {
        if *n > 1e-3 && (*n - 1.0).abs() > 1e-3 {
            out[2 * i] /= n;
            out[2 * i + 1] /= n;
        }
    }
    out.truncate(out_frames * 2);
    out
}

/// A stretch of the recording (seconds) moved by some semitones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchShift {
    pub start: f64,
    pub end: f64,
    pub semitones: f32,
}

/// The recording with each stretch in `shifts` moved in pitch, the same length as before. The
/// stretches fade in and out over 5 ms against the rest.
pub fn shift_pitch(
    input: &[f32],
    sample_rate: u32,
    shifts: &[PitchShift],
    mode: FlexMode,
) -> Vec<f32> {
    let sr = f64::from(sample_rate.max(1));
    let frames = input.len() / 2;
    let mut out = input.to_vec();
    let fade = (0.005 * sr) as usize;
    for s in shifts {
        if !(s.semitones.is_finite() && s.semitones.abs() > 0.001 && s.end > s.start) {
            continue;
        }
        let semis = s.semitones.clamp(-24.0, 24.0);
        let a = ((s.start.max(0.0) * sr) as usize).min(frames);
        let b = ((s.end * sr) as usize).min(frames);
        if b <= a + 16 {
            continue;
        }
        // A little either side, so the fades have sound to work with.
        let (pa, pb) = (a.saturating_sub(fade), (b + fade).min(frames));
        let piece = &input[2 * pa..2 * pb];
        let factor = 2f64.powf(f64::from(semis) / 12.0);
        // Stretch by the factor keeping the pitch, then play it back by the factor faster:
        // the original length at the new pitch.
        let n = pb - pa;
        let stretched_frames = (n as f64 * factor).round() as usize;
        let stretched = wsola(piece, sr, stretched_frames, mode, |t| t / factor);
        for j in 0..n {
            let (l, r) = frame_lerp(&stretched, j as f64 * factor);
            let at = pa + j;
            // 0 outside the stretch, 1 inside, fading over `fade` at each end.
            let g = if at < a {
                1.0 - (a - at) as f32 / fade.max(1) as f32
            } else if at >= b {
                1.0 - (at - b + 1) as f32 / fade.max(1) as f32
            } else {
                1.0
            }
            .clamp(0.0, 1.0);
            out[2 * at] = out[2 * at] * (1.0 - g) + l * g;
            out[2 * at + 1] = out[2 * at + 1] * (1.0 - g) + r * g;
        }
    }
    out
}

// ------------------------------------------------------------------ pitch detection

/// Pitch frames per second of [`pitch_track`].
pub const PITCH_RATE: f64 = 100.0;
/// The lowest and highest pitch looked for, in Hz.
const MIN_HZ: f64 = 55.0;
const MAX_HZ: f64 = 1400.0;

/// The pitch every 10 ms of a mono recording, in Hz, or `None` where there is no clear pitch
/// (silence, noise, drums). YIN on a copy at about 11 kHz.
pub fn pitch_track(mono: &[f32], sample_rate: u32) -> Vec<Option<f32>> {
    let sr = f64::from(sample_rate.max(1));
    // Average groups of samples down to about 11 kHz (also a gentle low-pass).
    let step = ((sr / 11_025.0).round() as usize).max(1);
    let rate = sr / step as f64;
    let low: Vec<f32> = mono
        .chunks(step)
        .map(|c| {
            c.iter()
                .map(|x| if x.is_finite() { *x } else { 0.0 })
                .sum::<f32>()
                / c.len() as f32
        })
        .collect();
    let max_tau = (rate / MIN_HZ).ceil() as usize;
    let min_tau = ((rate / MAX_HZ).floor() as usize).max(2);
    let w = max_tau + 16;
    let hop = (rate / PITCH_RATE).round().max(1.0) as usize;
    let mut out = Vec::new();
    let mut start = 0;
    let mut d = vec![0.0f32; max_tau + 2];
    while start + w + max_tau + 1 < low.len() {
        let frame = &low[start..start + w + max_tau + 1];
        let energy: f32 = frame[..w].iter().map(|x| x * x).sum::<f32>() / w as f32;
        if energy < 1e-6 {
            out.push(None);
            start += hop;
            continue;
        }
        // Difference function, then its cumulative mean normalised form.
        let mut running = 0.0f32;
        d[0] = 1.0;
        for tau in 1..=max_tau {
            let mut s = 0.0f32;
            for i in 0..w {
                let e = frame[i] - frame[i + tau];
                s += e * e;
            }
            running += s;
            d[tau] = if running > 0.0 {
                s * tau as f32 / running
            } else {
                1.0
            };
        }
        // The first dip under the threshold, followed to its bottom.
        let mut found = None;
        let mut tau = min_tau;
        while tau < max_tau {
            if d[tau] < 0.15 {
                while tau + 1 < max_tau && d[tau + 1] < d[tau] {
                    tau += 1;
                }
                found = Some(tau);
                break;
            }
            tau += 1;
        }
        out.push(found.map(|t| {
            // A parabola through the dip gives a fraction of a sample.
            let (a, b, c) = (d[t - 1], d[t], d[t + 1]);
            let den = a - 2.0 * b + c;
            let shift = if den.abs() > 1e-9 {
                0.5 * (a - c) / den
            } else {
                0.0
            };
            (rate / (t as f64 + f64::from(shift.clamp(-0.5, 0.5)))) as f32
        }));
        start += hop;
    }
    out
}

/// The MIDI note number (with a fraction) of a frequency.
pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// A note found in a recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedNote {
    /// Seconds from the start of the file.
    pub start: f64,
    pub end: f64,
    /// The MIDI note number it is sung or played at, with the fraction (a pitch that is off).
    pub pitch: f32,
    /// How loud it is, 0 to 1 (its peak).
    pub level: f32,
}

/// The notes of a mono recording: the stretches between `onsets` (and where the pitch breaks)
/// that have a steady enough pitch, at the median of their pitch.
pub fn detect_notes(mono: &[f32], sample_rate: u32, onsets: &[f64]) -> Vec<DetectedNote> {
    let sr = f64::from(sample_rate.max(1));
    let track = pitch_track(mono, sample_rate);
    let len = mono.len() as f64 / sr;
    let mut cuts: Vec<f64> = onsets
        .iter()
        .copied()
        .filter(|t| *t > 0.0 && *t < len)
        .collect();
    cuts.insert(0, 0.0);
    cuts.push(len);
    cuts.dedup();
    let mut notes = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (fa, fb) = (
            (a * PITCH_RATE) as usize,
            ((b * PITCH_RATE) as usize).min(track.len()),
        );
        // Runs of frames with a pitch inside this stretch, each its own note when the pitch
        // jumps by more than a semitone.
        let mut run: Vec<(usize, f32)> = Vec::new();
        let flush = |run: &mut Vec<(usize, f32)>, notes: &mut Vec<DetectedNote>| {
            if run.len() >= 5 {
                let mut p: Vec<f32> = run.iter().map(|r| r.1).collect();
                p.sort_by(f32::total_cmp);
                let pitch = p[p.len() / 2];
                let start = run[0].0 as f64 / PITCH_RATE;
                let end = (run[run.len() - 1].0 + 1) as f64 / PITCH_RATE;
                let (sa, sb) = ((start * sr) as usize, ((end * sr) as usize).min(mono.len()));
                let level = mono[sa.min(sb)..sb].iter().fold(0.0f32, |m, x| {
                    if x.is_finite() {
                        m.max(x.abs())
                    } else {
                        m
                    }
                });
                notes.push(DetectedNote {
                    start,
                    end,
                    pitch,
                    level: level.min(1.0),
                });
            }
            run.clear();
        };
        for (f, frame) in track.iter().enumerate().take(fb).skip(fa) {
            match *frame {
                Some(hz) => {
                    let m = hz_to_midi(hz);
                    if run.last().is_some_and(|r| (r.1 - m).abs() > 1.0) {
                        flush(&mut run, &mut notes);
                    }
                    run.push((f, m));
                }
                None => flush(&mut run, &mut notes),
            }
        }
        flush(&mut run, &mut notes);
    }
    notes
}

// ------------------------------------------------------------------ writing

/// Writes interleaved stereo as a 32-bit float WAV.
pub fn write_wav(path: &Path, stereo: &[f32], sample_rate: u32) -> std::io::Result<()> {
    let frames = stereo.len() / 2;
    let data_len =
        u32::try_from(frames * 8).map_err(|_| std::io::Error::other("too long for a WAV file"))?;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    b.extend_from_slice(&(sample_rate * 8).to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in &stereo[..frames * 2] {
        let v = if s.is_finite() { *s } else { 0.0 };
        b.extend_from_slice(&v.to_le_bytes());
    }
    // Written next to its final name first, so a reader never sees half a file.
    let tmp = path.with_extension("wav.part");
    std::fs::File::create(&tmp)?.write_all(&b)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    fn sine(hz: f64, seconds: f64) -> Vec<f32> {
        let n = (seconds * f64::from(SR)) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (std::f64::consts::TAU * hz * i as f64 / f64::from(SR)).sin() as f32 * 0.5;
            v.push(s);
            v.push(s);
        }
        v
    }

    fn mono(stereo: &[f32]) -> Vec<f32> {
        stereo.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect()
    }

    /// The pitch in the middle of a stereo signal.
    fn pitch_of(stereo: &[f32]) -> f32 {
        let t = pitch_track(&mono(stereo), SR);
        let mut v: Vec<f32> = t.iter().flatten().copied().collect();
        assert!(!v.is_empty(), "no pitch found");
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    }

    /// A click (a 2 ms burst) at each of `times`, in a silent stereo signal.
    fn clicks_at(times: &[f64], seconds: f64) -> Vec<f32> {
        let n = (seconds * f64::from(SR)) as usize;
        let mut v = vec![0.0f32; n * 2];
        for t in times {
            let a = (t * f64::from(SR)) as usize;
            for k in 0..88 {
                if a + k < n {
                    v[2 * (a + k)] = 0.8;
                    v[2 * (a + k) + 1] = 0.8;
                }
            }
        }
        v
    }

    /// Where the clicks of a signal are (the start of each run of loud samples).
    fn click_times(stereo: &[f32]) -> Vec<f64> {
        let m = mono(stereo);
        let mut out = Vec::new();
        let mut quiet_for = usize::MAX;
        for (i, x) in m.iter().enumerate() {
            if x.abs() > 0.3 {
                if quiet_for > 400 {
                    out.push(i as f64 / f64::from(SR));
                }
                quiet_for = 0;
            } else {
                quiet_for = quiet_for.saturating_add(1);
            }
        }
        out
    }

    #[test]
    fn the_time_map_goes_both_ways_and_ignores_nonsense() {
        let map = clean_points(
            &[
                WarpPoint { src: 2.0, dst: 3.0 },
                WarpPoint { src: 1.0, dst: 1.0 },
                WarpPoint { src: 1.5, dst: 0.5 }, // goes back in time: left out
                WarpPoint {
                    src: f64::NAN,
                    dst: 1.0,
                },
                WarpPoint { src: 9.0, dst: 9.0 }, // past the end
            ],
            5.0,
        );
        assert_eq!(map.len(), 3);
        assert_eq!(forward(&map, 1.5), 2.0);
        assert_eq!(backward(&map, 2.0), 1.5);
        // After the last point, one second is one second.
        assert_eq!(forward(&map, 4.0), 5.0);
        for t in [0.0, 0.3, 1.0, 1.7, 2.0, 3.3] {
            assert!((backward(&map, forward(&map, t)) - t).abs() < 1e-9);
        }
    }

    #[test]
    fn slicing_moves_each_hit_to_its_place() {
        let input = clicks_at(&[0.5, 1.0, 1.6], 2.5);
        // The second hit is late: pull it to 0.9; the third goes to 1.5.
        let points = [
            WarpPoint { src: 1.0, dst: 0.9 },
            WarpPoint { src: 1.6, dst: 1.5 },
        ];
        let out = warp(&input, SR, &points, FlexMode::Slicing);
        let found = click_times(&out);
        let want = [0.5, 0.9, 1.5];
        assert_eq!(found.len(), 3, "{found:?}");
        for (f, w) in found.iter().zip(want) {
            assert!((f - w).abs() < 0.003, "{found:?}");
        }
        assert!((out.len() as f64 / 2.0 / f64::from(SR) - 2.4).abs() < 0.01);
    }

    #[test]
    fn stretching_keeps_the_pitch_and_speed_does_not() {
        let input = sine(440.0, 2.0);
        // Half as fast: two seconds become four.
        let points = [WarpPoint { src: 1.9, dst: 3.8 }];
        for mode in [
            FlexMode::Rhythmic,
            FlexMode::Monophonic,
            FlexMode::Polyphonic,
        ] {
            let out = warp(&input, SR, &points, mode);
            assert!((out.len() as f64 / 2.0 / f64::from(SR) - 3.9).abs() < 0.01);
            let p = pitch_of(&out);
            assert!((p - 440.0).abs() < 5.0, "{mode:?}: {p}");
            // No loud jumps where the pieces join.
            let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(peak < 0.75, "{mode:?}: {peak}");
        }
        let out = warp(&input, SR, &points, FlexMode::Speed);
        let p = pitch_of(&out[..(1.5 * f64::from(SR)) as usize * 2]);
        assert!((p - 220.0).abs() < 4.0, "speed: {p}");
        // Tempophone stretches too, grainy but at the pitch.
        let out = warp(&input, SR, &points, FlexMode::Tempophone);
        assert!((pitch_of(&out) - 440.0).abs() < 10.0);
    }

    #[test]
    fn faster_works_too_and_hits_land_where_asked() {
        let input = clicks_at(&[0.25, 0.75, 1.25, 1.75], 2.0);
        // Everything twice as fast, with a point on every hit (as Flex markers are).
        let points: Vec<WarpPoint> = [0.25, 0.75, 1.25, 1.75]
            .into_iter()
            .map(|t| WarpPoint {
                src: t,
                dst: t / 2.0,
            })
            .collect();
        let out = warp(&input, SR, &points, FlexMode::Rhythmic);
        let found = click_times(&out);
        assert_eq!(found.len(), 4, "{found:?}");
        for (f, w) in found.iter().zip([0.125, 0.375, 0.625, 0.875]) {
            assert!((f - w).abs() < 0.025, "{found:?}");
        }
    }

    #[test]
    fn pitch_is_found_and_moved() {
        for hz in [82.4, 110.0, 220.0, 440.0, 987.8] {
            let p = pitch_of(&sine(hz, 0.5));
            assert!((p - hz as f32).abs() / (hz as f32) < 0.01, "{hz}: {p}");
        }
        assert!(pitch_track(&vec![0.0; 44_100], SR)
            .iter()
            .all(Option::is_none));
        let input = sine(440.0, 1.0);
        for semis in [-12.0f32, -2.0, 3.0, 7.0] {
            let out = shift_pitch(
                &input,
                SR,
                &[PitchShift {
                    start: 0.0,
                    end: 1.0,
                    semitones: semis,
                }],
                FlexMode::Monophonic,
            );
            assert_eq!(out.len(), input.len(), "same length");
            let want = 440.0 * 2f32.powf(semis / 12.0);
            let p = pitch_of(&out[4410..out.len() - 4410]);
            assert!((p - want).abs() / want < 0.015, "{semis}: {p} vs {want}");
        }
        // Only the stretch asked for moves.
        let out = shift_pitch(
            &sine(440.0, 2.0),
            SR,
            &[PitchShift {
                start: 1.0,
                end: 2.0,
                semitones: 12.0,
            }],
            FlexMode::Monophonic,
        );
        let half = out.len() / 2;
        assert!((pitch_of(&out[..half - 4410]) - 440.0).abs() < 5.0);
        assert!((pitch_of(&out[half + 4410..]) - 880.0).abs() < 12.0);
    }

    #[test]
    fn notes_are_found_with_their_pitch() {
        // A4 for half a second, then E5, with an onset between.
        let mut s = sine(440.0, 0.5);
        s.extend(sine(659.25, 0.5));
        let notes = detect_notes(&mono(&s), SR, &[0.5]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!((notes[0].pitch - 69.0).abs() < 0.2, "{notes:?}");
        assert!((notes[1].pitch - 76.0).abs() < 0.2, "{notes:?}");
        assert!(notes[0].start < 0.1 && (notes[1].start - 0.5).abs() < 0.05);
        assert!(notes.iter().all(|n| n.level > 0.3 && n.end > n.start));
        // A slightly flat note reads as flat.
        let flat = detect_notes(&mono(&sine(440.0 * 2f64.powf(-0.3 / 12.0), 0.6)), SR, &[]);
        assert!((flat[0].pitch - 68.7).abs() < 0.1, "{flat:?}");
        // Silence has no notes.
        assert!(detect_notes(&vec![0.0; 44_100], SR, &[0.3]).is_empty());
    }

    #[test]
    fn odd_input_does_not_break_anything() {
        assert!(warp(&[], SR, &[], FlexMode::Rhythmic).is_empty());
        let mut s = sine(440.0, 0.3);
        s[100] = f32::NAN;
        for mode in FlexMode::ALL {
            let _ = warp(
                &s,
                SR,
                &[WarpPoint {
                    src: 0.1,
                    dst: 10.0,
                }],
                mode,
            );
            let _ = warp(
                &s,
                0,
                &[WarpPoint {
                    src: 0.1,
                    dst: 0.0001,
                }],
                mode,
            );
            let _ = shift_pitch(
                &s,
                SR,
                &[PitchShift {
                    start: -1.0,
                    end: 9.0,
                    semitones: 99.0,
                }],
                mode,
            );
        }
        // A wild point cannot make an endless file.
        let out = warp(
            &s,
            SR,
            &[WarpPoint {
                src: 0.2,
                dst: 1.0e12,
            }],
            FlexMode::Slicing,
        );
        assert!(out.len() as f64 / 2.0 / f64::from(SR) <= MAX_SECONDS + 1.0);
        assert!(pitch_track(&[0.5; 10], SR).is_empty());
        for m in FlexMode::ALL {
            assert_eq!(FlexMode::from_name(m.name()), Some(m));
        }
    }

    #[test]
    fn a_written_file_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flex.wav");
        let s = sine(330.0, 0.5);
        write_wav(&path, &s, SR).unwrap();
        let f = crate::AudioFile::open(&path).unwrap();
        assert_eq!((f.sample_rate, f.channels, f.frames), (SR, 2, 22_050));
        let back = f.read_stereo().unwrap();
        assert_eq!(back.len(), s.len());
        assert!(back.iter().zip(&s).all(|(a, b)| (a - b).abs() < 1e-6));
    }
}
