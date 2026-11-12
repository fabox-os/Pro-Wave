//! REX2 loops (Propellerhead ReCycle, Reason and Logic's `.rx2` / `.rex`), read-only.
//!
//! A REX2 file is an IFF-style container (`CAT REX2`) holding the loop's tempo (`GLOB`,
//! `RECY`), its audio format (`SINF`), its slice table (`CAT SLCL` with one `SLCE` per slice)
//! and one compressed waveform for the whole loop (`SDAT`, coded with DWOP). Slices do not
//! store audio of their own: each one is a range of the decoded waveform, which is what lets a
//! loop follow the project tempo.
//!
//! This crate turns a file into [`RexFile`]: the decoded waveform as `i16`, the tempo, the
//! sample rate, and the slices as frame ranges with their flags. The file is treated as
//! untrusted input: every chunk size and field is checked, decoding stops when the bitstream
//! runs out, and nothing panics on damaged data.
//!
//! ```no_run
//! let bytes = std::fs::read("break.rx2")?;
//! let rex = prowave_rex::RexFile::parse(&bytes)?;
//! println!("{} BPM, {} slices", rex.tempo.unwrap_or(0.0), rex.slices.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The writer ([`write`]) is the inverse: it produces a file the reader decodes back, which is
//! how the DWOP codec and the container are tested (and how a loop can be exported).
#![deny(clippy::unwrap_used, clippy::expect_used)]

mod chunks;
mod dwop;
pub mod write;

/// The rate assumed when `SINF` says zero, as the reference reader does.
pub const FALLBACK_SAMPLE_RATE: u32 = 44_100;
/// The most decoded samples accepted (50 million: about 9 minutes of stereo at 44.1 kHz).
const MAX_DECODED_SAMPLES: usize = 50_000_000;
/// The most slices accepted.
const MAX_SLICES: usize = 65_536;

/// A decoded REX2 loop.
#[derive(Clone, Debug, PartialEq)]
pub struct RexFile {
    /// The preview tempo, in BPM.
    pub tempo: Option<f64>,
    /// The tempo the loop was recorded at, in BPM (`RECY`).
    pub original_tempo: Option<f64>,
    /// The loop's sample rate.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u8,
    /// 8, 16, 24 or 32, from `SINF`'s format code.
    pub bit_depth: u16,
    /// Frames per channel in the decoded waveform.
    pub frames: u32,
    /// The loop start frame (`SINF`), when the file says.
    pub loop_start: u32,
    /// The loop end frame, exclusive (`SINF`), when the file says.
    pub loop_end: u32,
    /// The playable slices, in the order the file stores them.
    pub slices: Vec<RexSlice>,
    /// Transient markers the file keeps but that are not slices, in frames.
    pub markers: Vec<u32>,
    /// Interleaved samples, `channels` per frame, scaled by `1 / 32768` when read.
    samples: Vec<i16>,
    /// Things worth telling the user: a missing sample rate, a truncated stream, a bit depth
    /// that was decoded as 16-bit.
    pub warnings: Vec<String>,
}

/// One slice: a range of the decoded waveform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RexSlice {
    /// The first frame.
    pub start: u32,
    /// How many frames the slice covers.
    pub length: u32,
    /// The slice is muted in the file.
    pub muted: bool,
    /// The slice is locked in the file.
    pub locked: bool,
    /// The slice was selected in the editor that made the file.
    pub selected: bool,
}

impl RexSlice {
    /// The first frame after the slice.
    pub fn end(&self) -> u32 {
        self.start.saturating_add(self.length)
    }
}

impl RexFile {
    /// Reads a REX2 file. The message says what is wrong when the file cannot be used.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let all = chunks::walk(bytes)?;
        let is_rex2 = all
            .iter()
            .any(|c| &c.id == b"CAT " && c.data.starts_with(b"REX2"));
        if !is_rex2 {
            return Err("not a REX2 file (no `CAT REX2` chunk)".to_string());
        }
        let mut warnings = Vec::new();

        let glob = chunks::find(&all, b"GLOB").ok_or("REX2 has no `GLOB` chunk")?;
        let tempo = if glob.data.len() >= 20 {
            let milli =
                u32::from_be_bytes([glob.data[16], glob.data[17], glob.data[18], glob.data[19]]);
            (milli > 0).then_some(f64::from(milli) / 1000.0)
        } else {
            warnings.push("the `GLOB` chunk is short; the tempo is unknown".to_string());
            None
        };

        let original_tempo = chunks::find(&all, b"RECY").and_then(|recy| {
            if recy.data.len() < 12 {
                return None;
            }
            let milli =
                u32::from_be_bytes([recy.data[8], recy.data[9], recy.data[10], recy.data[11]]);
            (milli > 0).then_some(f64::from(milli) / 1000.0)
        });

        let sinf = chunks::find(&all, b"SINF").ok_or("REX2 has no `SINF` chunk")?;
        if sinf.data.len() < 10 {
            return Err("the `SINF` chunk is too short".to_string());
        }
        let channels = match sinf.data[0] {
            1 | 2 => sinf.data[0],
            other => {
                return Err(format!(
                    "REX2 says {other} channels; only 1 and 2 are known"
                ))
            }
        };
        let bit_depth = match sinf.data[1] {
            1 => 8,
            3 => 16,
            5 => 24,
            7 => 32,
            other => {
                return Err(format!(
                    "REX2 format code {other} is not one of 1, 3, 5 or 7"
                ))
            }
        };
        let mut sample_rate =
            u32::from_be_bytes([sinf.data[2], sinf.data[3], sinf.data[4], sinf.data[5]]);
        if sample_rate == 0 {
            sample_rate = FALLBACK_SAMPLE_RATE;
            warnings.push(format!(
                "the file says no sample rate; {FALLBACK_SAMPLE_RATE} Hz is assumed"
            ));
        }
        let declared_frames =
            u32::from_be_bytes([sinf.data[6], sinf.data[7], sinf.data[8], sinf.data[9]]);
        if declared_frames == 0 {
            return Err("REX2 says the waveform has no frames".to_string());
        }
        let loop_start = if sinf.data.len() >= 14 {
            u32::from_be_bytes([sinf.data[10], sinf.data[11], sinf.data[12], sinf.data[13]])
        } else {
            0
        };
        let loop_end = if sinf.data.len() >= 18 {
            u32::from_be_bytes([sinf.data[14], sinf.data[15], sinf.data[16], sinf.data[17]])
        } else {
            declared_frames
        };

        let budget = MAX_DECODED_SAMPLES / usize::from(channels);
        let frames = if declared_frames as usize > budget {
            warnings.push(format!(
                "the file declares {declared_frames} frames; only the first {budget} are decoded"
            ));
            budget as u32
        } else {
            declared_frames
        };

        let mut slices = Vec::new();
        let mut markers = Vec::new();
        for chunk in all.iter().filter(|c| &c.id == b"SLCE") {
            if slices.len() + markers.len() >= MAX_SLICES {
                warnings.push(format!(
                    "more than {MAX_SLICES} slices; the rest are ignored"
                ));
                break;
            }
            if chunk.data.len() < 8 {
                warnings.push("a `SLCE` chunk is too short and was ignored".to_string());
                continue;
            }
            let start =
                u32::from_be_bytes([chunk.data[0], chunk.data[1], chunk.data[2], chunk.data[3]]);
            let length =
                u32::from_be_bytes([chunk.data[4], chunk.data[5], chunk.data[6], chunk.data[7]]);
            if length <= 1 {
                // A transient marker, not a playable slice.
                markers.push(start);
                continue;
            }
            let flags = chunk.data.get(10).copied().unwrap_or(0);
            slices.push(RexSlice {
                start,
                length,
                muted: flags & 0b0000_0001 != 0,
                locked: flags & 0b0000_0010 != 0,
                selected: flags & 0b0000_0100 != 0,
            });
        }
        if slices.is_empty() && !markers.is_empty() {
            warnings.push("the file has markers but no slices; the waveform is used whole".into());
        }

        let sdat = chunks::find(&all, b"SDAT").ok_or("REX2 has no `SDAT` chunk")?;
        if sdat.data.is_empty() {
            return Err("the `SDAT` chunk is empty".to_string());
        }
        // A 24-bit file's stream holds 24-bit samples; dropping the low 8 bits leaves a 16-bit
        // waveform rather than clipping everything above full scale. Other depths are coded
        // as 16-bit samples, as the format notes describe.
        let extra_shift = if bit_depth == 24 { 8 } else { 0 };
        match bit_depth {
            16 => {}
            24 => warnings.push(
                "the file says 24-bit; the decoded waveform is reduced to 16-bit samples"
                    .to_string(),
            ),
            other => warnings.push(format!(
                "the file says {other}-bit; the DWOP codec's 16-bit samples are used"
            )),
        }
        let (samples, ran_out) = dwop::decode(
            sdat.data,
            usize::from(channels),
            frames,
            budget,
            extra_shift,
        );
        if ran_out {
            warnings
                .push("the audio stream ends early; the rest of the loop is silence".to_string());
        }
        for slice in &slices {
            if slice.end() > frames {
                warnings.push(format!(
                    "slice at frame {} ends past the waveform and was shortened",
                    slice.start
                ));
                break;
            }
        }
        Ok(Self {
            tempo,
            original_tempo,
            sample_rate,
            channels,
            bit_depth,
            frames,
            loop_start,
            loop_end,
            slices,
            markers,
            samples,
            warnings,
        })
    }

    /// True when `bytes` starts like a REX2 file, without decoding anything.
    pub fn is_rex(bytes: &[u8]) -> bool {
        bytes.len() >= 12 && &bytes[0..4] == b"CAT " && &bytes[8..12] == b"REX2"
    }

    /// How many frames per channel the decoded waveform holds.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / usize::from(self.channels)
    }

    /// One sample as `f32` in `-1..1`; out-of-range frames and channels are silence.
    pub fn sample(&self, frame: usize, channel: usize) -> f32 {
        if channel >= usize::from(self.channels) || frame >= self.frame_count() {
            return 0.0;
        }
        let index = frame * usize::from(self.channels) + channel;
        self.samples.get(index).copied().unwrap_or(0) as f32 / 32768.0
    }

    /// The whole waveform as interleaved `f32` in `-1..1`.
    pub fn to_f32(&self) -> Vec<f32> {
        self.samples
            .iter()
            .map(|&s| f32::from(s) / 32768.0)
            .collect()
    }

    /// How long one frame lasts, in seconds.
    pub fn seconds_per_frame(&self) -> f64 {
        1.0 / f64::from(self.sample_rate.max(1))
    }

    /// The length of the loop, in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.frame_count() as f64 * self.seconds_per_frame()
    }

    /// A slice's range in frames, shortened to the decoded waveform.
    pub fn slice_range(&self, index: usize) -> Option<(usize, usize)> {
        let slice = self.slices.get(index)?;
        let start = (slice.start as usize).min(self.frame_count());
        let end = (slice.end() as usize).min(self.frame_count()).max(start);
        Some((start, end))
    }

    /// A slice as interleaved `f32`.
    pub fn slice_samples(&self, index: usize) -> Vec<f32> {
        let Some((start, end)) = self.slice_range(index) else {
            return Vec::new();
        };
        let channels = usize::from(self.channels).max(1);
        let mut out = Vec::with_capacity((end - start) * channels);
        for frame in start..end {
            for channel in 0..channels {
                out.push(self.sample(frame, channel));
            }
        }
        out
    }

    /// The slice's position in quarter-note beats from the loop start, for a grid of `ppq`
    /// ticks per 4/4 bar (REX's own grid is 15360). `None` when the file has no loop region.
    pub fn slice_beat(&self, index: usize, ppq: u64) -> Option<f64> {
        let slice = self.slices.get(index)?;
        let start = self.loop_start;
        let end = if self.loop_end > self.loop_start {
            self.loop_end
        } else {
            self.frames
        };
        if end <= start || ppq == 0 {
            return None;
        }
        let relative = slice.start.saturating_sub(start);
        let ticks = f64::from(relative) * ppq as f64 / f64::from(end - start);
        // 15360 ticks is a 4/4 bar, so 3840 ticks is a quarter note; beats are quarter notes.
        Some(ticks / 3840.0)
    }
}
