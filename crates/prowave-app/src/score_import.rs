//! Import of score and drum-machine files into Pro Wave's project model.
//!
//! The readers live in their own crates (`prowave-musicxml`, `prowave-mxl`,
//! `prowave-guitarpro` and `prowave-hydrogen-import`) and treat their input as untrusted.
//! This module only converts the notes they return into tracks and regions, using the same
//! coordinate rules as [`crate::midi_file`]: positions are quarter notes, a region starts on a
//! 1-based bar, and a note's `start` is relative to its region.

use std::collections::HashMap;
use std::path::Path;

use crate::model::{Note, Project, Region, RegionParams, TrackKind};

/// Past this bar an imported region would be outside the arrangement.
const MAX_BAR: f64 = 9999.0;
/// The shortest note that can be placed on the grid.
const MIN_LEN: f64 = 1.0 / 960.0;
/// A track's notes are capped as well, so a generous file cannot exhaust memory.
const MAX_NOTES_PER_TRACK: usize = 250_000;
/// Hydrogen's pattern `<size>` and note positions are in ticks, 48 to a quarter note.
const H2_TICKS_PER_QUARTER: f64 = 48.0;
/// The `<size>` Hydrogen gives a pattern that does not say (four quarter notes).
const DEFAULT_PATTERN_TICKS: u32 = 192;
/// The longest pattern imported, in quarter notes, so a bad `<size>` cannot make one huge.
const MAX_PATTERN_BEATS: f64 = 1024.0;

/// A Hydrogen pattern's length in quarter notes, from its `<size>` in ticks.
fn pattern_beats(size_ticks: u32) -> f64 {
    (f64::from(size_ticks.max(1)) / H2_TICKS_PER_QUARTER).min(MAX_PATTERN_BEATS)
}

/// File > Import entries handled by the format-specific readers rather than the MIDI or audio
/// importers. The string labels are the menu IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportCmd {
    MusicXml,
    GuitarPro,
    Hydrogen,
    Rex,
    LogicX,
}

#[derive(Clone, Copy, Debug)]
struct ImportedNote {
    start: f64, // quarter notes from the song start
    len: f64,   // quarter notes
    pitch: u8,
    vel: u8,
}

#[derive(Debug)]
struct ImportedTrack {
    name: String,
    kind: TrackKind,
    notes: Vec<ImportedNote>,
}

/// Whether a dropped or command-line file is one of the formats this module reads.
pub fn is_supported_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("musicxml" | "xml" | "mxl" | "gp" | "h2song")
    )
}

/// Reads a MusicXML score, compressed `.mxl` bundles included. Returns the track count.
pub fn import_musicxml(p: &mut Project, path: &Path) -> Result<usize, String> {
    let compressed = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mxl"));
    let xml = if compressed {
        prowave_mxl::read_path(path).map_err(|e| e.to_string())?
    } else {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    };
    let score = prowave_musicxml::parse(&xml).map_err(|e| e.to_string())?;
    let mut tempo = None;
    let mut project_sig = None;
    let mut tracks = Vec::new();
    for part in &score.parts {
        let mut divisions = 1u32;
        let mut cursor = 0.0f64;
        let mut notes = Vec::new();
        let mut sig = project_sig.unwrap_or((4, 4));
        let mut chord_start;
        for measure in &part.measures {
            if let Some(a) = measure.attributes {
                if let Some(d) = a.divisions {
                    divisions = d.max(1) as u32;
                }
                if let (Some(beats), Some(beat_type)) = (a.beats, a.beat_type) {
                    if beats > 0 && beat_type > 0 {
                        sig = (beats, beat_type);
                        if project_sig.is_none() {
                            project_sig = Some(sig);
                        }
                    }
                }
            }
            if tempo.is_none() {
                tempo = measure.tempo;
            }
            let start = cursor;
            chord_start = start;
            for note in &measure.notes {
                let beats = (note.duration as f64 / f64::from(divisions)).max(MIN_LEN);
                let at = if note.chord { chord_start } else { cursor };
                if !note.chord {
                    chord_start = at;
                    cursor += beats;
                }
                if note.rest || note.pitch.is_none() || notes.len() >= MAX_NOTES_PER_TRACK {
                    continue;
                }
                // MusicXML has no velocity; Logic's default for an imported score is mezzo.
                notes.push(ImportedNote {
                    start: at,
                    len: beats,
                    pitch: note.pitch.map_or(60, |p| p.midi()),
                    vel: 100,
                });
            }
            // An empty measure still takes the space its time signature says.
            let bar_len = f64::from(sig.0) * 4.0 / f64::from(sig.1.max(1));
            cursor = start + bar_len.max(cursor - start);
        }
        if !notes.is_empty() {
            tracks.push(ImportedTrack {
                name: if part.name.is_empty() {
                    score.title.clone().unwrap_or_else(|| "MusicXML".into())
                } else {
                    part.name.clone()
                },
                kind: TrackKind::Instrument,
                notes,
            });
        }
    }
    Ok(commit(p, tracks, tempo, project_sig))
}

/// Reads a Guitar Pro 7/8 `.gp` score. Returns the track count.
pub fn import_guitarpro(p: &mut Project, path: &Path) -> Result<usize, String> {
    let score = prowave_guitarpro::read_path(path).map_err(|e| e.to_string())?;
    let mut bar_starts = Vec::with_capacity(score.master_bars.len() + 1);
    let mut at = 0.0f64;
    for bar in &score.master_bars {
        bar_starts.push(at);
        let len = f64::from(bar.time_signature.beats.max(1)) * 4.0
            / f64::from(bar.time_signature.beat_type.max(1));
        at += len;
    }
    bar_starts.push(at);
    let sig = score
        .master_bars
        .first()
        .map(|b| (b.time_signature.beats, b.time_signature.beat_type));
    let mut tracks = Vec::new();
    for track in &score.tracks {
        let mut notes = Vec::new();
        for (bar_index, bar) in track.bars.iter().enumerate() {
            let base = bar_starts.get(bar_index).copied().unwrap_or(at);
            for beat in &bar.beats {
                let start = base + beat.position.max(0.0);
                for note in &beat.notes {
                    if notes.len() >= MAX_NOTES_PER_TRACK {
                        break;
                    }
                    notes.push(ImportedNote {
                        start,
                        len: beat.duration.max(MIN_LEN),
                        pitch: note.midi.min(127),
                        vel: 100,
                    });
                }
            }
        }
        if !notes.is_empty() {
            let name = if track.name.is_empty() {
                track.instrument.clone()
            } else {
                track.name.clone()
            };
            tracks.push(ImportedTrack {
                name,
                kind: TrackKind::Instrument,
                notes,
            });
        }
    }
    Ok(commit(p, tracks, Some(score.tempo), sig))
}

/// Reads a Hydrogen `.h2song` session's patterns into a Drummer track. Returns the track count.
pub fn import_hydrogen(p: &mut Project, path: &Path) -> Result<usize, String> {
    let song = prowave_hydrogen_import::read_path(path).map_err(|e| e.to_string())?;
    // Use each instrument's MIDI out note when it has one, otherwise the General MIDI drum
    // range from C1, so a kit without note numbers still gets distinct sounds.
    let mut pitches = HashMap::new();
    for (i, instrument) in song.instruments.iter().enumerate() {
        if !instrument.muted {
            let fallback = (36 + i % 60) as u8;
            pitches.insert(
                instrument.id,
                instrument.midi_note.unwrap_or(fallback).min(127),
            );
        }
    }
    let mut notes = Vec::new();
    // Each sequence step plays one pattern, one after another. A pattern's length comes from
    // its `<size>` in ticks; `denominator` only names the time signature's note value and does
    // not change the length.
    let mut base = 0.0f64;
    for step in 0..song.sequence.len().min(4096) {
        let pattern = song.pattern_at(step);
        // A missing pattern plays nothing but still takes a default pattern's length.
        let beats = pattern.map_or_else(
            || pattern_beats(DEFAULT_PATTERN_TICKS),
            |p| pattern_beats(p.size),
        );
        for note in pattern.into_iter().flat_map(|p| p.notes.iter()) {
            // The cap is for the whole song: a short pattern can be played many times over.
            if notes.len() >= MAX_NOTES_PER_TRACK {
                break;
            }
            if note.note_off || note.probability <= 0.0 {
                continue;
            }
            let Some(&pitch) = pitches.get(&note.instrument) else {
                continue;
            };
            let start = base + (f64::from(note.position) / H2_TICKS_PER_QUARTER).clamp(0.0, beats);
            let len = if note.length < 0 {
                0.25
            } else {
                (f64::from(note.length) / H2_TICKS_PER_QUARTER).clamp(MIN_LEN, beats)
            };
            notes.push(ImportedNote {
                start,
                len,
                pitch,
                vel: (f64::from(note.velocity.clamp(0.0, 1.0)) * 127.0)
                    .round()
                    .max(1.0) as u8,
            });
        }
        base += beats;
    }
    let name = if song.name.is_empty() {
        "Hydrogen".into()
    } else {
        song.name.clone()
    };
    let tracks = if notes.is_empty() {
        Vec::new()
    } else {
        vec![ImportedTrack {
            name,
            kind: TrackKind::Drummer,
            notes,
        }]
    };
    Ok(commit(p, tracks, Some(song.tempo), None))
}

/// Puts the converted notes in the project as tracks and regions, as `midi_file::import` does.
fn commit(
    p: &mut Project,
    tracks: Vec<ImportedTrack>,
    tempo: Option<f64>,
    sig: Option<(u8, u8)>,
) -> usize {
    // Nothing to add, or no room for it: leave the project alone, tempo and signature included.
    let has_notes = tracks.iter().any(|t| !t.notes.is_empty());
    if !has_notes || p.tracks.len() >= prowave_engine::MAX_TRACKS {
        return 0;
    }
    if p.tracks.iter().all(|t| t.regions.is_empty()) {
        if let Some(bpm) = tempo {
            if bpm.is_finite() {
                p.tempo = bpm.clamp(
                    f64::from(crate::model::MIN_BPM),
                    f64::from(crate::model::MAX_BPM),
                ) as f32;
                p.tempo_changes.clear();
            }
        }
        if let Some((beats, beat_type)) = sig {
            if beats > 0 && beat_type > 0 {
                p.sig = (beats, beat_type);
                p.sig_changes.clear();
            }
        }
    }
    let mut made = 0;
    for track in tracks {
        if track.notes.is_empty() || p.tracks.len() >= prowave_engine::MAX_TRACKS {
            continue;
        }
        let first = track
            .notes
            .iter()
            .map(|n| n.start)
            .fold(f64::INFINITY, f64::min);
        let last = track
            .notes
            .iter()
            .map(|n| n.start + n.len)
            .fold(f64::NEG_INFINITY, f64::max);
        if !first.is_finite() || !last.is_finite() {
            continue;
        }
        let start_bar0 = p
            .beats_to_bar(first + 1e-9)
            .floor()
            .clamp(0.0, MAX_BAR - 1.0);
        let origin = p.bar_to_beats(start_bar0);
        let len_bars = (p.beats_to_bar(last - 1e-9) - start_bar0)
            .ceil()
            .clamp(1.0, MAX_BAR - start_bar0);
        let notes: Vec<Note> = track
            .notes
            .iter()
            .map(|n| Note {
                start: (n.start - origin).max(0.0) as f32,
                len: n.len.max(MIN_LEN) as f32,
                pitch: n.pitch.min(127),
                vel: n.vel.clamp(1, 127),
            })
            .collect();
        let ti = p.add_track(track.kind);
        p.tracks[ti].name = track.name;
        p.tracks[ti].regions.push(Region {
            name: "Imported".into(),
            start_bar: start_bar0 as f32 + 1.0,
            len_bars: len_bars as f32,
            notes,
            controls: Vec::new(),
            folder: None,
            audio: None,
            params: RegionParams::default(),
            generated: None,
        });
        made += 1;
    }
    if made > 0 {
        p.song_rev += 1;
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musicxml_notes_become_tracks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <score-partwise version="4.0">
          <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
          <part id="P1">
            <measure number="1">
              <attributes><divisions>2</divisions><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
              <direction><sound tempo="96"/></direction>
              <note><pitch><step>C</step><octave>4</octave></pitch><duration>2</duration></note>
              <note><chord/><pitch><step>E</step><octave>4</octave></pitch><duration>2</duration></note>
              <note><rest/><duration>2</duration></note>
            </measure>
          </part>
        </score-partwise>"#;
        let score = prowave_musicxml::parse(xml).expect("the test score parses");
        let mut p = Project::empty();
        // Read through the same path as the menu, using a temporary file.
        let dir = std::env::temp_dir().join(format!("prowave-score-import-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("score.musicxml");
        std::fs::write(&path, xml).expect("write the test score");
        let made = import_musicxml(&mut p, &path).expect("import the test score");
        let _ = std::fs::remove_file(&path);
        assert_eq!(made, 1);
        assert_eq!(p.tempo, 96.0);
        assert_eq!(p.sig, (4, 4));
        assert_eq!(score.note_count(), 3);
        let track = p.tracks.last().expect("the imported track is appended");
        let notes = &track.regions[0].notes;
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].pitch, notes[0].vel), (60, 100));
        assert_eq!((notes[1].pitch, notes[1].start), (64, 0.0));
        assert!(notes[0].len > 0.0);
    }

    #[test]
    fn hydrogen_notes_become_a_drummer_track() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <song>
          <version>1.2.0</version><bpm>115</bpm><name>Demo</name>
          <instrumentList>
            <instrument><id>0</id><name>Kick</name><midiOutNote>36</midiOutNote></instrument>
          </instrumentList>
          <patternList>
            <pattern><name>P</name><size>192</size><denominator>4</denominator>
              <noteList><note><position>0</position><velocity>0.8</velocity><length>-1</length><instrument>0</instrument></note></noteList>
            </pattern>
          </patternList>
          <patternSequence><group><patternID>P</patternID></group></patternSequence>
        </song>"#;
        let dir = std::env::temp_dir().join(format!("prowave-score-import-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("song.h2song");
        std::fs::write(&path, xml).expect("write the test song");
        let mut p = Project::empty();
        let made = import_hydrogen(&mut p, &path).expect("import the test song");
        let _ = std::fs::remove_file(&path);
        assert_eq!(made, 1);
        assert_eq!(p.tempo, 115.0);
        let track = p.tracks.last().expect("the imported track is appended");
        assert_eq!(track.kind, TrackKind::Drummer);
        let notes = &track.regions[0].notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].pitch, 36);
        assert_eq!(notes[0].start, 0.0);
        assert!(notes[0].vel > 1);
    }

    /// A one-instrument Hydrogen song: `patterns` is (size, denominator, note position) per
    /// pattern, and the sequence plays them in the order given.
    fn hydrogen_song(name: &str, patterns: &[(u32, u32, u32)]) -> String {
        let mut list = String::new();
        let mut sequence = String::new();
        for (i, (size, denom, position)) in patterns.iter().enumerate() {
            list.push_str(&format!(
                "<pattern><name>P{i}</name><size>{size}</size><denominator>{denom}</denominator>\
                 <noteList><note><position>{position}</position><velocity>0.8</velocity>\
                 <length>-1</length><instrument>0</instrument></note></noteList></pattern>"
            ));
            sequence.push_str(&format!("<group><patternID>P{i}</patternID></group>"));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <song><version>1.2.0</version><bpm>120</bpm><name>{name}</name>
              <instrumentList><instrument><id>0</id><name>Kick</name><midiOutNote>36</midiOutNote></instrument></instrumentList>
              <patternList>{list}</patternList>
              <patternSequence>{sequence}</patternSequence>
            </song>"#
        )
    }

    fn hydrogen_starts(name: &str, patterns: &[(u32, u32, u32)]) -> Vec<f32> {
        let dir = std::env::temp_dir().join(format!("prowave-h2-timing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.h2song"));
        std::fs::write(&path, hydrogen_song(name, patterns)).expect("write the test song");
        let mut p = Project::empty();
        let made = import_hydrogen(&mut p, &path).expect("import the test song");
        let _ = std::fs::remove_file(&path);
        assert_eq!(made, 1);
        let track = p.tracks.last().expect("the imported track is appended");
        let mut starts: Vec<f32> = track.regions[0].notes.iter().map(|n| n.start).collect();
        starts.sort_by(f32::total_cmp);
        starts
    }

    #[test]
    fn hydrogen_pattern_positions_follow_pattern_size() {
        // A 4/4 bar (192 ticks): tick 48 is beat 1, and the next pattern starts at beat 4.
        assert_eq!(
            hydrogen_starts("four_four", &[(192, 4, 48), (192, 4, 48)]),
            [1.0, 5.0]
        );
        // A 2/4 bar (96 ticks): tick 48 is beat 1, not beat 2, and the next bar starts at beat 2.
        assert_eq!(
            hydrogen_starts("two_four", &[(96, 4, 48), (96, 4, 48)]),
            [1.0, 3.0]
        );
        // A 3/4 bar (144 ticks): the second bar starts at beat 3, not beat 4.
        assert_eq!(
            hydrogen_starts("three_four", &[(144, 4, 0), (144, 4, 0)]),
            [0.0, 3.0]
        );
    }

    #[test]
    fn hydrogen_denominator_does_not_change_pattern_length() {
        // 6/8 in 144 ticks is three quarter notes long whatever the denominator says.
        assert_eq!(
            hydrogen_starts("six_eight", &[(144, 8, 0), (144, 8, 0)]),
            [0.0, 3.0]
        );
    }

    #[test]
    fn hydrogen_note_cap_applies_to_the_whole_song() {
        // One 1000-note pattern played 300 times is 300 000 notes, past the cap. A small file
        // must not be able to expand past it.
        let mut notes = String::new();
        for i in 0..1000u32 {
            notes.push_str(&format!(
                "<note><position>{}</position><velocity>0.8</velocity><length>-1</length>\
                 <instrument>0</instrument></note>",
                i % 192
            ));
        }
        let mut sequence = String::new();
        for _ in 0..300 {
            sequence.push_str("<group><patternID>P</patternID></group>");
        }
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <song><version>1.2.0</version><bpm>120</bpm><name>Cap</name>
              <instrumentList><instrument><id>0</id><name>Kick</name><midiOutNote>36</midiOutNote></instrument></instrumentList>
              <patternList><pattern><name>P</name><size>192</size><denominator>4</denominator>
                <noteList>{notes}</noteList></pattern></patternList>
              <patternSequence>{sequence}</patternSequence>
            </song>"#
        );
        let dir = std::env::temp_dir().join(format!("prowave-h2-cap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cap.h2song");
        std::fs::write(&path, xml).expect("write the test song");
        let mut p = Project::empty();
        let made = import_hydrogen(&mut p, &path).expect("import the test song");
        let _ = std::fs::remove_file(&path);
        assert_eq!(made, 1);
        let track = p.tracks.last().expect("the imported track is appended");
        assert_eq!(track.regions[0].notes.len(), MAX_NOTES_PER_TRACK);
    }

    #[test]
    fn a_score_with_no_notes_leaves_the_project_alone() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <score-partwise version="4.0">
          <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
          <part id="P1">
            <measure number="1">
              <attributes><divisions>1</divisions><time><beats>3</beats><beat-type>4</beat-type></time></attributes>
              <direction><sound tempo="96"/></direction>
              <note><rest/><duration>3</duration></note>
            </measure>
          </part>
        </score-partwise>"#;
        let dir = std::env::temp_dir().join(format!("prowave-score-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rests.musicxml");
        std::fs::write(&path, xml).expect("write the test score");
        let mut p = Project::empty();
        let tempo = p.tempo;
        let sig = p.sig;
        let tracks = p.tracks.len();
        let made = import_musicxml(&mut p, &path).expect("the score parses");
        let _ = std::fs::remove_file(&path);
        assert_eq!(made, 0);
        assert_eq!(p.tempo, tempo, "no note means no tempo change");
        assert_eq!(p.sig, sig, "no note means no signature change");
        assert_eq!(p.tracks.len(), tracks);
    }

    #[test]
    fn only_known_extensions_are_supported() {
        assert!(is_supported_path(Path::new("a/Score.MUSICXML")));
        assert!(is_supported_path(Path::new("a/score.mxl")));
        assert!(is_supported_path(Path::new("a/tab.gp")));
        assert!(is_supported_path(Path::new("a/song.h2song")));
        assert!(!is_supported_path(Path::new("a/song.mid")));
        assert!(!is_supported_path(Path::new("a/song.wav")));
    }
}
