//! Audio preview ("audition") of a file from a browser, played live next to the song.
//!
//! Logic Pro equivalent: clicking a file in the Loop Browser plays it in the project's
//! output while the song keeps running. The file is decoded and resampled to the output
//! rate off the audio thread ([`Preview::from_file`]); the audio thread only reads frames.
use crate::disk::{AudioError, AudioFile};

/// A preview longer than this is cut, so a long file cannot fill the memory.
pub const MAX_PREVIEW_SECONDS: f64 = 120.0;

/// A decoded file ready to be played once through the output.
pub struct Preview {
    /// Interleaved stereo at the output rate.
    frames: Vec<f32>,
    pos: usize,
}

impl Preview {
    /// Decodes `path` and resamples it (linear) to `out_rate`. Run off the audio thread.
    pub fn from_file(path: impl AsRef<std::path::Path>, out_rate: u32) -> Result<Self, AudioError> {
        let file = AudioFile::open(path)?;
        let stereo = file.read_stereo_limited(MAX_PREVIEW_SECONDS)?;
        Ok(Self::from_stereo(&stereo, file.sample_rate, out_rate))
    }

    /// Builds a preview from interleaved stereo at `in_rate`, resampled to `out_rate`.
    pub fn from_stereo(stereo: &[f32], in_rate: u32, out_rate: u32) -> Self {
        let n_in = stereo.len() / 2;
        let (in_rate, out_rate) = (in_rate.max(1), out_rate.max(1));
        if in_rate == out_rate || n_in < 2 {
            return Self {
                frames: stereo[..n_in * 2].to_vec(),
                pos: 0,
            };
        }
        let step = f64::from(in_rate) / f64::from(out_rate);
        let n_out = (n_in as f64 / step) as usize;
        let mut frames = Vec::with_capacity(n_out * 2);
        for i in 0..n_out {
            let x = i as f64 * step;
            let a = (x as usize).min(n_in - 1);
            let b = (a + 1).min(n_in - 1);
            let f = (x - a as f64) as f32;
            for ch in 0..2 {
                let (p, q) = (stereo[a * 2 + ch], stereo[b * 2 + ch]);
                frames.push(p + (q - p) * f);
            }
        }
        Self { frames, pos: 0 }
    }

    /// Length in frames at the output rate.
    pub fn len(&self) -> usize {
        self.frames.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether every frame has been played.
    pub fn finished(&self) -> bool {
        self.pos >= self.frames.len()
    }

    /// The next frame, or silence after the end. Audio thread: no allocation.
    pub fn next_frame(&mut self) -> (f32, f32) {
        match self.frames.get(self.pos..self.pos + 2) {
            Some([l, r]) => {
                self.pos += 2;
                (*l, *r)
            }
            _ => (0.0, 0.0),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_keeps_every_frame_in_order() {
        let mut p = Preview::from_stereo(&[0.1, 0.2, 0.3, 0.4], 48_000, 48_000);
        assert_eq!(p.len(), 2);
        assert_eq!(p.next_frame(), (0.1, 0.2));
        assert_eq!(p.next_frame(), (0.3, 0.4));
        assert!(p.finished());
        assert_eq!(p.next_frame(), (0.0, 0.0));
    }

    #[test]
    fn resampling_scales_the_length_by_the_rate_ratio() {
        let stereo = vec![0.5f32; 2 * 44_100];
        let p = Preview::from_stereo(&stereo, 44_100, 48_000);
        assert!((p.len() as i64 - 48_000).abs() <= 1);
    }

    #[test]
    fn an_empty_input_is_finished_at_once() {
        let p = Preview::from_stereo(&[], 44_100, 48_000);
        assert!(p.is_empty() && p.finished());
    }
}
