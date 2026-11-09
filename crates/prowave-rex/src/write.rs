//! Writing a REX2 file: the inverse of [`crate::RexFile::parse`].
//!
//! This exists so the container and the DWOP codec can be tested against each other with
//! files that never leave the process, and so a loop Pro Wave has in memory can be handed to
//! another program that reads REX2. It writes the chunks a reader needs (`HEAD`, `GLOB`,
//! `RECY`, `CAT SLCL`, `SINF`, `SDAT`) and nothing else: no creator information, no device
//! chunks and no transient stretch, which REX players generate at render time.

use crate::dwop;

/// The magic at the start of `HEAD`.
const HEAD_MAGIC: u32 = 0x490c_f18d;
/// The most slices one file may hold.
const MAX_WRITE_SLICES: usize = 65_536;

/// What to write.
#[derive(Clone, Debug, PartialEq)]
pub struct RexData {
    pub sample_rate: u32,
    /// 1 (mono) or 2 (stereo).
    pub channels: u8,
    /// The preview tempo in BPM.
    pub tempo: f64,
    /// The recording tempo in BPM; zero writes the preview tempo.
    pub original_tempo: f64,
    /// Interleaved samples, `channels` per frame.
    pub samples: Vec<i16>,
    /// Slice ranges as `(start frame, length in frames)`.
    pub slices: Vec<(u32, u32)>,
}

impl RexData {
    /// A mono or stereo loop with no slices yet.
    pub fn new(sample_rate: u32, channels: u8, tempo: f64, samples: Vec<i16>) -> Self {
        Self {
            sample_rate,
            channels,
            tempo,
            original_tempo: tempo,
            samples,
            slices: Vec::new(),
        }
    }
}

/// Writes `data` as a REX2 file.
pub fn write(data: &RexData) -> Result<Vec<u8>, String> {
    if !(1..=2).contains(&data.channels) {
        return Err(format!("{} channels: REX2 holds 1 or 2", data.channels));
    }
    if data.sample_rate == 0 || data.sample_rate > 768_000 {
        return Err(format!("{} Hz is not a sample rate", data.sample_rate));
    }
    if data.samples.is_empty()
        || !data
            .samples
            .len()
            .is_multiple_of(usize::from(data.channels))
    {
        return Err("the samples are not a whole number of frames".to_string());
    }
    if !data.tempo.is_finite() || data.tempo <= 0.0 {
        return Err("the tempo must be a positive number".to_string());
    }
    if data.slices.len() > MAX_WRITE_SLICES {
        return Err(format!("at most {MAX_WRITE_SLICES} slices can be written"));
    }
    let frames = u32::try_from(data.samples.len() / usize::from(data.channels))
        .map_err(|_| "the waveform is longer than a REX2 file can address".to_string())?;

    for (start, length) in &data.slices {
        if start.saturating_add(*length) > frames || *length == 0 {
            return Err(format!(
                "slice at frame {start} with length {length} is outside the {frames}-frame waveform"
            ));
        }
    }

    let mut children = Vec::new();
    children.extend_from_slice(&head());
    children.extend_from_slice(&glob(data));
    children.extend_from_slice(&recy(data));
    if !data.slices.is_empty() {
        let mut slice_chunks = Vec::new();
        for (start, length) in &data.slices {
            let mut payload = Vec::with_capacity(11);
            payload.extend_from_slice(&start.to_be_bytes());
            payload.extend_from_slice(&length.to_be_bytes());
            payload.extend_from_slice(&0x7fffu16.to_be_bytes());
            payload.push(0);
            slice_chunks.extend_from_slice(&chunk(b"SLCE", &payload));
        }
        children.extend_from_slice(&cat(b"SLCL", &slice_chunks));
    }
    children.extend_from_slice(&sinf(data, frames));
    let audio = dwop::encode(&data.samples, usize::from(data.channels))?;
    children.extend_from_slice(&chunk(b"SDAT", &audio));

    let mut file = cat(b"REX2", &children);
    if file.len() % 2 == 1 {
        file.push(0);
    }
    Ok(file)
}

fn head() -> Vec<u8> {
    let mut payload = Vec::with_capacity(29);
    payload.extend_from_slice(&HEAD_MAGIC.to_be_bytes());
    // Version record `bc 01`, the oldest one readers accept.
    payload.extend_from_slice(&[0xbc, 0x01]);
    payload.extend_from_slice(&[0; 7]);
    payload.extend_from_slice(&[0; 16]);
    chunk(b"HEAD", &payload)
}

fn glob(data: &RexData) -> Vec<u8> {
    let mut payload = Vec::with_capacity(22);
    payload.extend_from_slice(&(data.slices.len() as u32).to_be_bytes());
    // One bar of four beats is enough: players derive the length from the waveform.
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.push(0); // beats
    payload.push(4); // time-signature numerator
    payload.push(4); // time-signature denominator
    payload.push(0x4e); // analysis sensitivity, as the reference writer uses
    payload.extend_from_slice(&0u16.to_be_bytes()); // gate sensitivity
    payload.extend_from_slice(&1000u16.to_be_bytes()); // processing gain, unity
    payload.extend_from_slice(&1u16.to_be_bytes()); // pitch
    payload.extend_from_slice(&milli_bpm(data.tempo).to_be_bytes());
    payload.push(1); // transmit as slices
    payload.push(0); // silence selected
    chunk(b"GLOB", &payload)
}

fn recy(data: &RexData) -> Vec<u8> {
    let mut payload = Vec::with_capacity(15);
    payload.extend_from_slice(&[0xbc, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
    let original = if data.original_tempo.is_finite() && data.original_tempo > 0.0 {
        data.original_tempo
    } else {
        data.tempo
    };
    payload.extend_from_slice(&milli_bpm(original).to_be_bytes());
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.push(8); // unit byte, as the reference writer uses
    chunk(b"RECY", &payload)
}

fn sinf(data: &RexData, frames: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(18);
    payload.push(data.channels);
    payload.push(3); // 16-bit integer
    payload.extend_from_slice(&data.sample_rate.to_be_bytes());
    payload.extend_from_slice(&frames.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes()); // loop start
    payload.extend_from_slice(&frames.to_be_bytes()); // loop end, exclusive
    chunk(b"SINF", &payload)
}

fn milli_bpm(bpm: f64) -> u32 {
    (bpm * 1000.0).round().clamp(1.0, f64::from(u32::MAX)) as u32
}

/// One chunk: id, big-endian size, payload, and the pad byte an odd size needs.
fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len() + 1);
    out.extend_from_slice(id);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A container chunk: the type, then the child chunks.
fn cat(typ: &[u8; 4], children: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + children.len());
    payload.extend_from_slice(typ);
    payload.extend_from_slice(children);
    chunk(b"CAT ", &payload)
}
