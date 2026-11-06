//! System Exclusive messages in regions. A message is stored as one `Control` per byte, all at one
//! start, the first 0xF0 and the last 0xF7 (see `model::SYSEX_BYTE`). This module reads a message
//! typed in hex, shows one as text, and puts one into a region at the playhead.

use crate::model::{Control, Project, Region, TrackKind, SYSEX_BYTE};

/// The longest message the entry takes, in bytes.
pub const MAX_BYTES: usize = 1024;

/// Why a typed message is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// A word is not one or two hex digits.
    NotHex,
    /// It does not start with F0 and end with F7.
    NotFramed,
    /// Longer than [`MAX_BYTES`].
    TooLong,
}

/// The bytes of a message typed as hex words, separated by spaces or commas (`F0 7E 7F F7`, or
/// `0xF0,0x7E,0xF7`).
pub fn parse(text: &str) -> Result<Vec<u8>, ParseError> {
    let mut bytes = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == ',') {
        if word.is_empty() {
            continue;
        }
        let digits = word
            .strip_prefix("0x")
            .or_else(|| word.strip_prefix("0X"))
            .unwrap_or(word);
        if digits.is_empty() || digits.len() > 2 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ParseError::NotHex);
        }
        bytes.push(u8::from_str_radix(digits, 16).map_err(|_| ParseError::NotHex)?);
        if bytes.len() > MAX_BYTES {
            return Err(ParseError::TooLong);
        }
    }
    if bytes.len() < 2 || bytes.first() != Some(&0xf0) || bytes.last() != Some(&0xf7) {
        return Err(ParseError::NotFramed);
    }
    Ok(bytes)
}

/// The message as hex words: `F0 7E 7F F7`.
pub fn text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The region a new message goes into: the selected region if it can hold one, else the region
/// of the selected track that the playhead is in. `None` when there is neither.
pub fn target(p: &Project, selected: Option<(usize, usize)>) -> Option<(usize, usize)> {
    let can_hold = |t: usize, r: usize| {
        p.tracks.get(t).is_some_and(|tr| {
            tr.kind.has_regions()
                && tr.kind != TrackKind::Audio
                && tr.regions.get(r).is_some_and(plain)
        })
    };
    if let Some((t, r)) = selected {
        if can_hold(t, r) {
            return Some((t, r));
        }
    }
    let t = p.selected_track;
    let at = p.position_beats;
    let tr = p.tracks.get(t)?;
    tr.regions
        .iter()
        .enumerate()
        .find(|(r, reg)| {
            let start = p.region_start_beats(reg);
            plain(reg) && can_hold(t, *r) && at >= start && at < p.region_end_beats(reg)
        })
        .map(|(r, _)| (t, r))
}

/// A region that keeps its own events: not audio, not a take folder, not generated from settings.
fn plain(r: &Region) -> bool {
    r.audio.is_none() && r.folder.is_none() && r.generated.is_none()
}

/// Puts `bytes` into region `r` of track `t` at the playhead (or at the region's start if the
/// playhead is before it). Returns whether it was done.
pub fn insert(p: &mut Project, t: usize, r: usize, bytes: &[u8]) -> bool {
    if bytes.len() < 2 || bytes.len() > MAX_BYTES || bytes.first() != Some(&0xf0) {
        return false;
    }
    let Some(region) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return false;
    };
    if !plain(region) {
        return false;
    }
    let offset = (p.position_beats - p.region_start_beats(region)).max(0.0) as f32;
    let region = &mut p.tracks[t].regions[r];
    region.controls.extend(bytes.iter().map(|&b| Control {
        start: offset,
        num: SYSEX_BYTE,
        val: u16::from(b),
    }));
    p.song_rev += 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_words_are_read_with_or_without_prefixes_and_separators() {
        assert_eq!(parse("F0 7E 7F F7"), Ok(vec![0xf0, 0x7e, 0x7f, 0xf7]));
        assert_eq!(parse("0xf0,0x41,0xF7"), Ok(vec![0xf0, 0x41, 0xf7]));
        assert_eq!(parse("  f0\n41  f7 "), Ok(vec![0xf0, 0x41, 0xf7]));
    }

    #[test]
    fn a_message_must_be_hex_and_framed() {
        assert_eq!(parse("F0 4G F7"), Err(ParseError::NotHex));
        assert_eq!(parse("F0 123 F7"), Err(ParseError::NotHex), "three digits");
        assert_eq!(parse("F0 41"), Err(ParseError::NotFramed), "no F7");
        assert_eq!(parse("41 F7"), Err(ParseError::NotFramed), "no F0");
        assert_eq!(parse(""), Err(ParseError::NotFramed));
        let long = format!("F0 {} F7", "00 ".repeat(MAX_BYTES));
        assert_eq!(parse(&long), Err(ParseError::TooLong));
    }

    #[test]
    fn a_message_shows_as_hex_words() {
        assert_eq!(text(&[0xf0, 0x7e, 0x7f, 0xf7]), "F0 7E 7F F7");
        assert_eq!(text(&[0x0a]), "0A");
    }

    #[test]
    fn a_message_goes_into_the_region_at_the_playhead() {
        let mut p = Project::demo();
        let r = p.tracks[0].regions[0].clone();
        p.tracks[0].regions[0].controls.clear();
        p.position_beats = p.region_start_beats(&r) + 2.0;
        assert!(insert(&mut p, 0, 0, &[0xf0, 0x41, 0xf7]));
        let controls = &p.tracks[0].regions[0].controls;
        assert_eq!(controls.len(), 3);
        assert!(controls
            .iter()
            .all(|c| c.num == SYSEX_BYTE && c.start == 2.0));
        let vals: Vec<u16> = controls.iter().map(|c| c.val).collect();
        assert_eq!(vals, vec![0xf0, 0x41, 0xf7]);
        // Not a message: nothing is added.
        assert!(!insert(&mut p, 0, 0, &[0x41, 0xf7]));
        assert_eq!(p.tracks[0].regions[0].controls.len(), 3);
    }

    #[test]
    fn a_message_is_refused_where_a_region_keeps_no_events_of_its_own() {
        let mut p = Project::demo();
        p.tracks[0].regions[0].audio = Some(crate::model::AudioRegion {
            file: prowave_engine::AudioFile::open(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../prowave-engine/tests/fixtures/tone.caf"),
            )
            .unwrap(),
            peaks: Default::default(),
            flex: None,
        });
        assert!(!insert(&mut p, 0, 0, &[0xf0, 0xf7]));
        assert_ne!(
            target(&p, Some((0, 0))),
            Some((0, 0)),
            "an audio region is never a target"
        );
    }
}
