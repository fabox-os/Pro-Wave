//! A small SoundFont (`.sf2`) writer.
//!
//! Pro Wave reads sample-based instruments that are not SoundFonts (a Hydrogen drum kit is a
//! `drumkit.xml` and a folder of samples) and plays them with the same `rustysynth` player as
//! any `.sf2` file, so nothing on the audio thread changes. This module builds those files in
//! memory.
//!
//! Building allocates and copies the samples; call it off the audio thread.

/// One sample of the font: a name, 16-bit mono PCM and its rate in Hz.
pub struct Sample {
    /// At most 20 bytes are kept, as an `.sf2` name is 20 bytes.
    pub name: String,
    pub pcm: Vec<i16>,
    pub rate: u32,
}

/// One zone of an instrument: the key and velocity range that plays one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    /// Index into the samples given to [`build`].
    pub sample: usize,
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    /// The key at which the sample plays at the pitch it was recorded.
    pub root_key: u8,
    /// Attenuation in centibels (100 = -10 dB); 0 plays the sample as it is.
    pub attenuation: i16,
}

/// One preset of the font, played with one instrument holding its zones.
pub struct Preset<'a> {
    pub name: &'a str,
    pub bank: u16,
    pub program: u8,
    pub zones: &'a [Zone],
}

/// Builds the bytes of a SoundFont with `samples`, and one instrument per preset.
///
/// The file is a RIFF `sfbk` with the INFO, sdta and pdta lists `rustysynth` (and any SoundFont
/// player) reads; every preset's zones go into one instrument of the same index.
pub fn build(name: &str, samples: &[Sample], presets: &[Preset]) -> Vec<u8> {
    let mut smpl = Vec::new();
    let mut starts = Vec::with_capacity(samples.len());
    for sample in samples {
        starts.push((smpl.len() / 2) as u32);
        if sample.pcm.is_empty() {
            smpl.extend_from_slice(&0i16.to_le_bytes());
        } else {
            for v in &sample.pcm {
                smpl.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    // The spec asks for 46 silent frames after the data, for the interpolator.
    smpl.resize(smpl.len() + 46 * 2, 0);

    // Sample headers: name, start, end (both inclusive), loop start and end, rate, original
    // pitch, pitch correction, linked sample and type (1 = mono). The samples do not loop.
    let mut shdr = Vec::new();
    for (i, sample) in samples.iter().enumerate() {
        let start = starts[i];
        let end = start + sample.pcm.len().max(1) as u32 - 1;
        shdr.extend_from_slice(&name20(&sample.name));
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&sample.rate.clamp(1, 192_000).to_le_bytes());
        shdr.push(60); // the original pitch: A4
        shdr.push(0); // pitch correction
        shdr.extend_from_slice(&0u16.to_le_bytes()); // no linked sample
        shdr.extend_from_slice(&1u16.to_le_bytes()); // mono
    }
    shdr.extend_from_slice(&name20("EOS"));
    shdr.resize(shdr.len() + 26, 0);

    // Instruments and their zones.
    let mut igen = Vec::new();
    let mut ibag = Vec::new();
    let mut inst = Vec::new();
    let mut zone_index = 0u16;
    let mut gen_index = 0u16;
    for preset in presets {
        inst.extend_from_slice(&name20(preset.name));
        inst.extend_from_slice(&zone_index.to_le_bytes());
        for zone in preset.zones {
            ibag.extend_from_slice(&gen_index.to_le_bytes());
            ibag.extend_from_slice(&0u16.to_le_bytes());
            igen.extend_from_slice(&gen(43, range(zone.key_lo, zone.key_hi)));
            igen.extend_from_slice(&gen(44, range(zone.vel_lo, zone.vel_hi)));
            igen.extend_from_slice(&gen(58, u16::from(zone.root_key)));
            igen.extend_from_slice(&gen(53, zone.sample as u16));
            igen.extend_from_slice(&gen(48, zone.attenuation as u16));
            gen_index += 5;
            zone_index += 1;
        }
    }
    // The terminal records: one instrument for each preset, and the end of the zone bags and
    // generators.
    inst.extend_from_slice(&name20("EOI"));
    inst.extend_from_slice(&zone_index.to_le_bytes());
    ibag.extend_from_slice(&gen_index.to_le_bytes());
    ibag.extend_from_slice(&0u16.to_le_bytes());
    igen.extend_from_slice(&gen(0, 0));

    // Presets: one zone per preset, which names the instrument of the same index.
    let mut phdr = Vec::new();
    let mut pbag = Vec::new();
    let mut pgen = Vec::new();
    for (i, preset) in presets.iter().enumerate() {
        phdr.extend_from_slice(&name20(preset.name));
        phdr.extend_from_slice(&u16::from(preset.program).to_le_bytes());
        phdr.extend_from_slice(&preset.bank.to_le_bytes());
        phdr.extend_from_slice(&(i as u16).to_le_bytes()); // its first zone
        phdr.extend_from_slice(&[0u8; 12]); // library, genre, morphology
        pbag.extend_from_slice(&(i as u16).to_le_bytes()); // its first generator
        pbag.extend_from_slice(&0u16.to_le_bytes());
        pgen.extend_from_slice(&gen(41, i as u16)); // instrument
    }
    // The terminal records.
    phdr.extend_from_slice(&name20("EOP"));
    phdr.extend_from_slice(&[0u8; 4]); // preset, bank
    phdr.extend_from_slice(&(presets.len() as u16).to_le_bytes());
    phdr.extend_from_slice(&[0u8; 12]);
    pbag.extend_from_slice(&(pgen.len() as u16 / 4).to_le_bytes());
    pbag.extend_from_slice(&0u16.to_le_bytes());
    pgen.extend_from_slice(&gen(0, 0));

    let info = list(
        b"INFO",
        &[
            chunk(b"ifil", &[2, 0, 1, 0]),
            chunk(b"INAM", format!("{name}\0").as_bytes()),
        ],
    );
    let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);
    let pdta = list(
        b"pdta",
        &[
            chunk(b"phdr", &phdr),
            chunk(b"pbag", &pbag),
            chunk(b"pmod", &[0u8; 10]),
            chunk(b"pgen", &pgen),
            chunk(b"inst", &inst),
            chunk(b"ibag", &ibag),
            chunk(b"imod", &[0u8; 10]),
            chunk(b"igen", &igen),
            chunk(b"shdr", &shdr),
        ],
    );

    let mut body = b"sfbk".to_vec();
    body.extend_from_slice(&info);
    body.extend_from_slice(&sdta);
    body.extend_from_slice(&pdta);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// A RIFF chunk: its four-byte id, a little-endian size and the data. Odd-length data gets a
/// zero byte that the size counts, so no reader can fall out of step (rustysynth reads the
/// sizes and does not skip RIFF padding).
fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    let padded = data.len() + data.len() % 2;
    out.extend_from_slice(&(padded as u32).to_le_bytes());
    out.extend_from_slice(data);
    if padded != data.len() {
        out.push(0);
    }
    out
}

/// A RIFF LIST chunk of `kind` holding `parts`.
fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
    let mut body = kind.to_vec();
    for part in parts {
        body.extend_from_slice(part);
    }
    chunk(b"LIST", &body)
}

/// An `.sf2` name: 20 bytes, zero-padded, cut at 20 bytes.
fn name20(name: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    let bytes = name.as_bytes();
    let len = bytes.len().min(20);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

/// A generator record: its operator and its amount.
fn gen(op: u16, amount: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&op.to_le_bytes());
    out[2..].copy_from_slice(&amount.to_le_bytes());
    out
}

/// The amount of a key or velocity range generator: the first value in the low byte.
fn range(lo: u8, hi: u8) -> u16 {
    u16::from(lo) | (u16::from(hi) << 8)
}
