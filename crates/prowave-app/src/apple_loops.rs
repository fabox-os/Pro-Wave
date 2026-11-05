//! Apple Loops tags: the category, genre, key, beat count and so on that Logic Pro and
//! GarageBand show in their Loop Browsers, read from a loop's `.caf` file.
//!
//! An Apple Loop is a Core Audio Format (CAF) file with a `uuid` chunk that holds
//! null-terminated key/value strings. It has no tempo field: the tempo is the beat count
//! times 60, divided by the loop's duration, so the duration is read too (from the `pakt`
//! chunk of compressed loops, or the size of the `data` chunk of PCM ones). Only chunk headers
//! and the small metadata chunk are read, never the audio.
//!
//! The layout follows the community notes on the format (a census of the loops Apple ships).
//! Loops in the older AIFF-based format are not read.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// The identifier of the Apple Loop metadata `uuid` chunk.
const LOOP_UUID: [u8; 16] = [
    0x29, 0x81, 0x92, 0x73, 0xb5, 0xbf, 0x4a, 0xef, 0xb7, 0x8d, 0x62, 0xd1, 0xef, 0x90, 0xbb, 0x2c,
];
/// The largest metadata chunk read. Real ones are a few hundred bytes.
const MAX_TAG_CHUNK: u64 = 64 * 1024;
/// Key/value pairs read from the metadata chunk.
const MAX_PAIRS: usize = 64;
/// Chunks looked at before giving up on a file.
const MAX_CHUNKS: usize = 64;
/// Tempos outside this range are not shown (a wrong beat count would give one).
const TEMPO_RANGE: std::ops::RangeInclusive<f32> = 20.0..=400.0;

/// What the metadata of an Apple Loop says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoopTags {
    /// Instrument category, such as "Bass", and its subcategory, such as "Electric Bass".
    pub category: String,
    pub subcategory: String,
    pub genre: String,
    pub descriptors: Vec<String>,
    /// The number of beats; a one-shot has none.
    pub beats: Option<u32>,
    pub time_signature: String,
    /// The root note, such as "A" or "F#", and the scale ("major", "minor", "both", ...).
    pub key: String,
    pub key_type: String,
    /// Beats per minute, from the beat count and the duration, when both are known.
    pub tempo: Option<f32>,
}

impl LoopTags {
    /// The key as the browser shows it: "Am" for A minor, "A" for A major or either scale.
    pub fn key_text(&self) -> String {
        if self.key.is_empty() {
            return String::new();
        }
        if self.key_type == "minor" {
            format!("{}m", self.key)
        } else {
            self.key.clone()
        }
    }

    /// One line for the browser: tempo, key and category, for example "128 BPM · Am · Bass".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(tempo) = self.tempo {
            parts.push(format!("{tempo:.0} BPM"));
        }
        let key = self.key_text();
        if !key.is_empty() {
            parts.push(key);
        }
        if !self.category.is_empty() {
            parts.push(self.category.clone());
        }
        parts.join(" · ")
    }

    /// Whether `needle` (lower case) appears in the tags: category, subcategory, genre,
    /// descriptors, key, time signature or tempo.
    pub fn matches(&self, needle: &str) -> bool {
        let text = |s: &str| s.to_lowercase().contains(needle);
        text(&self.category)
            || text(&self.subcategory)
            || text(&self.genre)
            || text(&self.time_signature)
            || text(&self.key_text())
            || self.descriptors.iter().any(|d| text(d))
            || self.tempo.is_some_and(|t| text(&format!("{t:.0} bpm")))
    }
}

/// The tags of the Apple Loop at `path`, if it is a `.caf` file with Apple Loop metadata.
pub fn read_file(path: &Path) -> Option<LoopTags> {
    let is_caf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("caf"));
    if !is_caf {
        return None;
    }
    read_tags(&mut BufReader::new(File::open(path).ok()?))
}

/// The audio format of the loop, as far as the duration needs it.
struct Desc {
    rate: f64,
    format: [u8; 4],
    bytes_per_packet: u32,
    frames_per_packet: u32,
}

fn bytes<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// The Apple Loop tags of a CAF stream: `None` if it is not a CAF file, has no Apple Loop
/// metadata chunk, or is too damaged to read. Never panics, whatever the bytes.
pub fn read_tags<R: Read + Seek>(r: &mut R) -> Option<LoopTags> {
    let len = r.seek(SeekFrom::End(0)).ok()?;
    r.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 8];
    r.read_exact(&mut head).ok()?;
    if &head[..4] != b"caff" || head[4..6] != [0, 1] {
        return None;
    }
    let mut pos = 8u64;
    let mut desc = None;
    let mut valid_frames = None;
    let mut data_bytes = None;
    let mut pairs = None;
    for _ in 0..MAX_CHUNKS {
        if pos.checked_add(12)? > len {
            break;
        }
        r.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 12];
        r.read_exact(&mut h).ok()?;
        let kind: [u8; 4] = bytes(&h, 0)?;
        let raw = i64::from_be_bytes(bytes(&h, 4)?);
        let body = pos + 12;
        // A size of -1 means the chunk runs to the end of the file.
        let size = if raw == -1 {
            len - body
        } else {
            u64::try_from(raw).ok()?
        };
        let end = body.checked_add(size)?;
        if end > len {
            break;
        }
        match &kind {
            b"desc" if size >= 32 => {
                let mut b = [0u8; 32];
                r.read_exact(&mut b).ok()?;
                desc = Some(Desc {
                    rate: f64::from_be_bytes(bytes(&b, 0)?),
                    format: bytes(&b, 8)?,
                    bytes_per_packet: u32::from_be_bytes(bytes(&b, 16)?),
                    frames_per_packet: u32::from_be_bytes(bytes(&b, 20)?),
                });
            }
            b"pakt" if size >= 24 => {
                let mut b = [0u8; 16];
                r.read_exact(&mut b).ok()?;
                valid_frames = Some(i64::from_be_bytes(bytes(&b, 8)?));
            }
            b"data" => data_bytes = Some(size),
            b"uuid" if (20..=MAX_TAG_CHUNK).contains(&size) => {
                let mut b = vec![0u8; size as usize];
                r.read_exact(&mut b).ok()?;
                if b[..16] == LOOP_UUID {
                    pairs = Some(parse_pairs(&b[16..]));
                }
            }
            _ => {}
        }
        pos = end;
    }
    let mut tags = tags_from(pairs?);
    tags.tempo = tags.beats.and_then(|beats| {
        let d = desc.as_ref()?;
        let frames = match (valid_frames, data_bytes) {
            (Some(frames), _) if frames > 0 => frames as f64,
            (_, Some(size))
                if &d.format == b"lpcm" && d.frames_per_packet == 1 && d.bytes_per_packet > 0 =>
            {
                // The data chunk starts with a 4-byte edit count.
                (size.saturating_sub(4) / u64::from(d.bytes_per_packet)) as f64
            }
            _ => return None,
        };
        if !(d.rate.is_finite() && d.rate > 0.0 && frames > 0.0) {
            return None;
        }
        let tempo = (f64::from(beats) * 60.0 * d.rate / frames) as f32;
        TEMPO_RANGE.contains(&tempo).then_some(tempo)
    });
    Some(tags)
}

/// The key/value pairs of the metadata chunk: a big-endian count, then `key\0value\0` pairs.
fn parse_pairs(payload: &[u8]) -> Vec<(String, String)> {
    let Some(count) = bytes(payload, 0).map(u32::from_be_bytes) else {
        return Vec::new();
    };
    let mut strings = payload[4..].split(|&b| b == 0);
    let mut out = Vec::new();
    for _ in 0..(count as usize).min(MAX_PAIRS) {
        let (Some(key), Some(value)) = (strings.next(), strings.next()) else {
            break;
        };
        out.push((
            String::from_utf8_lossy(key).into_owned(),
            String::from_utf8_lossy(value).into_owned(),
        ));
    }
    out
}

fn tags_from(pairs: Vec<(String, String)>) -> LoopTags {
    let mut tags = LoopTags::default();
    for (key, value) in pairs {
        match key.as_str() {
            "category" => tags.category = value,
            "subcategory" => tags.subcategory = value,
            "genre" => tags.genre = value,
            "time signature" => tags.time_signature = value,
            "key signature" => tags.key = value,
            "key type" => tags.key_type = value,
            "beat count" => tags.beats = value.trim().parse().ok().filter(|&b| b > 0),
            "descriptors" => {
                tags.descriptors = value
                    .split(',')
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    tags
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = kind.to_vec();
        v.extend_from_slice(&(body.len() as i64).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn desc(rate: f64, format: &[u8; 4], bytes_per_packet: u32, frames_per_packet: u32) -> Vec<u8> {
        let mut b = rate.to_be_bytes().to_vec();
        b.extend_from_slice(format);
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&bytes_per_packet.to_be_bytes());
        b.extend_from_slice(&frames_per_packet.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&16u32.to_be_bytes());
        chunk(b"desc", &b)
    }

    fn pakt(valid_frames: i64) -> Vec<u8> {
        let mut b = 100i64.to_be_bytes().to_vec();
        b.extend_from_slice(&valid_frames.to_be_bytes());
        b.extend_from_slice(&[0u8; 8]);
        chunk(b"pakt", &b)
    }

    fn meta(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut b = LOOP_UUID.to_vec();
        b.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
        for (k, v) in pairs {
            b.extend_from_slice(k.as_bytes());
            b.push(0);
            b.extend_from_slice(v.as_bytes());
            b.push(0);
        }
        chunk(b"uuid", &b)
    }

    fn caf(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = b"caff".to_vec();
        v.extend_from_slice(&[0, 1, 0, 0]);
        for c in chunks {
            v.extend_from_slice(c);
        }
        v
    }

    fn read(bytes: Vec<u8>) -> Option<LoopTags> {
        read_tags(&mut Cursor::new(bytes))
    }

    const BASS: [(&str, &str); 8] = [
        ("category", "Bass"),
        ("subcategory", "Electric Bass"),
        ("genre", "Funk"),
        ("beat count", "8"),
        ("time signature", "4/4"),
        ("key signature", "A"),
        ("key type", "minor"),
        ("descriptors", "Grooving, Electric,Dry"),
    ];

    #[test]
    fn a_compressed_loop_gets_its_tags_and_a_tempo_from_the_packet_table() {
        // Eight beats in four seconds (176400 frames at 44100 Hz) is 120 BPM.
        let tags = read(caf(&[
            desc(44_100.0, b"aac ", 0, 1024),
            pakt(176_400),
            chunk(b"data", &[0u8; 64]),
            meta(&BASS),
        ]))
        .unwrap();
        assert_eq!(tags.category, "Bass");
        assert_eq!(tags.subcategory, "Electric Bass");
        assert_eq!(tags.genre, "Funk");
        assert_eq!(tags.beats, Some(8));
        assert_eq!(tags.time_signature, "4/4");
        assert_eq!((tags.key.as_str(), tags.key_type.as_str()), ("A", "minor"));
        assert_eq!(tags.descriptors, ["Grooving", "Electric", "Dry"]);
        assert_eq!(tags.tempo.map(|t| t.round()), Some(120.0));
        assert_eq!(tags.summary(), "120 BPM · Am · Bass");
    }

    #[test]
    fn a_pcm_loop_gets_its_tempo_from_the_size_of_its_data() {
        // 16-bit stereo: 4 bytes a frame. Four beats in two seconds is 120 BPM.
        let mut audio = vec![0u8; 4]; // the edit count
        audio.extend(vec![0u8; 4 * 88_200]);
        let tags = read(caf(&[
            desc(44_100.0, b"lpcm", 4, 1),
            chunk(b"data", &audio),
            meta(&[("beat count", "4")]),
        ]))
        .unwrap();
        assert_eq!(tags.tempo.map(|t| t.round()), Some(120.0));
    }

    #[test]
    fn a_one_shot_has_tags_but_no_tempo() {
        let tags = read(caf(&[
            desc(44_100.0, b"aac ", 0, 1024),
            pakt(44_100),
            meta(&[("category", "Drums"), ("subcategory", "Kick")]),
        ]))
        .unwrap();
        assert_eq!(tags.beats, None);
        assert_eq!(tags.tempo, None);
        assert_eq!(tags.summary(), "Drums");
    }

    #[test]
    fn a_tempo_that_makes_no_sense_is_not_shown() {
        // Two beats in a minute is 2 BPM: a wrong beat count, not music.
        let tags = read(caf(&[
            desc(44_100.0, b"aac ", 0, 1024),
            pakt(44_100 * 60),
            meta(&[("beat count", "2")]),
        ]))
        .unwrap();
        assert_eq!(tags.beats, Some(2));
        assert_eq!(tags.tempo, None);
    }

    #[test]
    fn files_that_are_not_apple_loops_give_no_tags() {
        // A plain CAF without the metadata chunk.
        assert_eq!(read(caf(&[desc(44_100.0, b"lpcm", 4, 1)])), None);
        // Another uuid chunk (the beat markers, say) is not the metadata.
        let mut other = [7u8; 16].to_vec();
        other.extend_from_slice(&[0u8; 8]);
        assert_eq!(read(caf(&[chunk(b"uuid", &other)])), None);
        // Other formats, and bytes that are nothing at all.
        assert_eq!(read(b"RIFF....WAVE".to_vec()), None);
        assert_eq!(read(b"FORM\0\0\0\0AIFF".to_vec()), None);
        assert_eq!(read(Vec::new()), None);
        assert_eq!(read(vec![0xff; 200]), None);
        // A CAF version we do not know.
        let mut v = caf(&[meta(&BASS)]);
        v[5] = 9;
        assert_eq!(read(v), None);
    }

    #[test]
    fn damaged_files_never_panic() {
        let whole = caf(&[
            desc(44_100.0, b"aac ", 0, 1024),
            pakt(176_400),
            chunk(b"data", &[0u8; 32]),
            meta(&BASS),
        ]);
        // Every truncation reads without a panic, and the whole file reads.
        for n in 0..whole.len() {
            let _ = read(whole[..n].to_vec());
        }
        assert!(read(whole.clone()).is_some());
        // Every single wrong byte reads without a panic too.
        for i in 0..whole.len() {
            let mut bad = whole.clone();
            bad[i] ^= 0xff;
            let _ = read(bad);
        }
        // A chunk that claims more than the file holds, or a negative size.
        let mut huge = caf(&[meta(&BASS)]);
        huge[8 + 4..8 + 12].copy_from_slice(&i64::MAX.to_be_bytes());
        assert_eq!(read(huge), None);
        let mut negative = caf(&[meta(&BASS)]);
        negative[8 + 4..8 + 12].copy_from_slice(&(-5i64).to_be_bytes());
        assert_eq!(read(negative), None);
        // A pair count far larger than the data, and more pairs than the limit.
        let mut lying = LOOP_UUID.to_vec();
        lying.extend_from_slice(&u32::MAX.to_be_bytes());
        lying.extend_from_slice(b"category\0Bass\0");
        let tags = read(caf(&[chunk(b"uuid", &lying)])).unwrap();
        assert_eq!(tags.category, "Bass");
    }

    #[test]
    fn the_browser_line_and_search_use_the_tags() {
        let tags = LoopTags {
            category: "Bass".into(),
            subcategory: "Electric Bass".into(),
            genre: "Funk".into(),
            descriptors: vec!["Grooving".into()],
            key: "F#".into(),
            key_type: "major".into(),
            tempo: Some(127.6),
            ..LoopTags::default()
        };
        assert_eq!(tags.key_text(), "F#");
        assert_eq!(tags.summary(), "128 BPM · F# · Bass");
        for hit in ["bass", "electric", "funk", "groov", "f#", "128 bpm"] {
            assert!(tags.matches(hit), "{hit}");
        }
        assert!(!tags.matches("drums"));
        assert_eq!(LoopTags::default().summary(), "");
        assert!(!LoopTags::default().matches("x"));
    }

    #[test]
    fn only_caf_files_are_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = caf(&[meta(&BASS)]);
        let loop_file = dir.path().join("Funk Bass.CAF");
        std::fs::write(&loop_file, &bytes).unwrap();
        assert_eq!(
            read_file(&loop_file).map(|t| t.category),
            Some("Bass".into())
        );
        let wav = dir.path().join("x.wav");
        std::fs::write(&wav, &bytes).unwrap();
        assert_eq!(read_file(&wav), None, "the extension decides");
        assert_eq!(read_file(&dir.path().join("missing.caf")), None);
    }
}
