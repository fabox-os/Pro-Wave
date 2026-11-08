//! MusicXML export of the score (File > Export > Score as MusicXML…): every track's MIDI notes
//! as measures, with the project's time and key signatures and tempo, so another program can
//! print or edit them.
//!
//! One `<part>` per track that has notes. Notes are split at bar lines and tied across them,
//! rests fill the gaps, and notes that start together are written as chords. Durations are exact
//! in ticks (`divisions` per quarter note); the display-only `<type>` is left out.

use crate::model::{Project, Track};
use std::fmt::Write as _;
use std::path::Path;

/// Ticks per quarter note in the file.
const DIVISIONS: i64 = 480;

/// Writes the score of `p` to `path`. Returns the number of notes written.
pub fn export(p: &Project, path: &Path) -> Result<usize, String> {
    let (xml, notes) = score_xml(p)?;
    std::fs::write(path, xml).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(notes)
}

/// The MusicXML document for `p`, and how many notes it holds.
pub fn score_xml(p: &Project) -> Result<(String, usize), String> {
    let tracks = tracks_with_notes(p);
    let parts: Vec<Vec<(i64, i64, u8)>> = tracks.iter().map(|t| ticks(t, p)).collect();
    let notes: usize = parts.iter().map(|notes| notes.len()).sum();
    if notes == 0 {
        return Err("there are no notes to export".to_string());
    }
    let measures = measures(p, &parts);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 4.0 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n");
    out.push_str("<score-partwise version=\"4.0\">\n");
    let _ = writeln!(
        out,
        "  <work><work-title>{}</work-title></work>",
        xml_text(&p.name)
    );
    out.push_str("  <part-list>\n");
    for (i, track) in tracks.iter().enumerate() {
        let _ = writeln!(
            out,
            "    <score-part id=\"P{}\"><part-name>{}</part-name></score-part>",
            i + 1,
            xml_text(&track.name)
        );
    }
    out.push_str("  </part-list>\n");
    for (i, notes) in parts.iter().enumerate() {
        let _ = writeln!(out, "  <part id=\"P{}\">", i + 1);
        write_part(&mut out, p, notes, &measures);
        out.push_str("  </part>\n");
    }
    out.push_str("</score-partwise>\n");
    Ok((out, notes))
}

/// The tracks that have notes, in the order their parts are written.
fn tracks_with_notes(p: &Project) -> Vec<&Track> {
    p.tracks
        .iter()
        .filter(|t| t.regions.iter().any(|r| !r.notes.is_empty()))
        .collect()
}

/// A track's notes in project ticks: (start, end, pitch).
fn ticks(track: &Track, p: &Project) -> Vec<(i64, i64, u8)> {
    let mut notes: Vec<(i64, i64, u8)> = Vec::new();
    for region in &track.regions {
        let origin = p.region_start_beats(region);
        for note in &region.notes {
            let start = origin + f64::from(note.start);
            let end = start + f64::from(note.len);
            if !(start.is_finite() && end.is_finite()) || end <= start {
                continue;
            }
            notes.push((
                (start * DIVISIONS as f64).round() as i64,
                (end * DIVISIONS as f64).round() as i64,
                note.pitch,
            ));
        }
    }
    notes.sort_by_key(|n| (n.0, n.2));
    notes
}

/// One bar of the score.
struct Measure {
    start: i64,
    end: i64,
    num: u8,
    den: u8,
}

/// The bars covered by `parts`, from the first to the one the last note ends in.
fn measures(p: &Project, parts: &[Vec<(i64, i64, u8)>]) -> Vec<Measure> {
    let end = parts
        .iter()
        .flat_map(|notes| notes.iter().map(|n| n.1))
        .max()
        .unwrap_or(0);
    let last_bar = p
        .beats_to_bar(end as f64 / DIVISIONS as f64)
        .ceil()
        .max(1.0) as u32;
    let mut out = Vec::new();
    for bar in 0..last_bar {
        let (num, den) = p.signature_of_bar(bar);
        let start = (p.bar_to_beats(bar as f64) * DIVISIONS as f64).round() as i64;
        let len = f64::from(num) * 4.0 / f64::from(den.max(1));
        let end = start + (len * DIVISIONS as f64).round() as i64;
        out.push(Measure {
            start,
            end,
            num,
            den,
        });
    }
    out
}

/// One part: its measures, their attributes, rests, notes, chords and ties.
fn write_part(out: &mut String, p: &Project, notes: &[(i64, i64, u8)], measures: &[Measure]) {
    let mut previous_signature = (0u8, 0u8);
    for (i, measure) in measures.iter().enumerate() {
        let _ = writeln!(out, "    <measure number=\"{}\">", i + 1);
        if i == 0 {
            let _ = writeln!(
                out,
                "      <attributes><divisions>{DIVISIONS}</divisions><key><fifths>{}</fifths></key>{}</attributes>",
                crate::score::fifths(p.key),
                time_signature(measure)
            );
            let tempo = p.tempo_at(0.0);
            let _ = writeln!(
                out,
                "      <direction placement=\"above\"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>{:.0}</per-minute></metronome></direction-type><sound tempo=\"{:.2}\"/></direction>",
                tempo, tempo
            );
        } else if (measure.num, measure.den) != previous_signature {
            let _ = writeln!(
                out,
                "      <attributes>{}</attributes>",
                time_signature(measure)
            );
        }
        previous_signature = (measure.num, measure.den);
        write_measure(out, notes, measure);
        out.push_str("    </measure>\n");
    }
}

fn time_signature(measure: &Measure) -> String {
    format!(
        "<time><beats>{}</beats><beat-type>{}</beat-type></time>",
        measure.num, measure.den
    )
}

/// A measure's contents: the notes that sound in it, split at its ends and tied, with rests
/// filling what is left.
fn write_measure(out: &mut String, notes: &[(i64, i64, u8)], measure: &Measure) {
    let first = notes.partition_point(|n| n.1 <= measure.start);
    let last = notes.partition_point(|n| n.0 < measure.end);
    // (start in the measure, end in the measure, the note's own start and end, pitch)
    let mut segments: Vec<(i64, i64, i64, i64, u8)> = notes[first..last]
        .iter()
        .map(|&(start, end, pitch)| {
            (
                start.max(measure.start) - measure.start,
                end.min(measure.end) - measure.start,
                start,
                end,
                pitch,
            )
        })
        .collect();
    // Chords: notes that start together. The last note of a chord is the one that moves time
    // on in MusicXML, so the shortest go first.
    segments.sort_by_key(|s| (s.0, s.1, s.4));
    let mut at = 0i64;
    let mut k = 0usize;
    while k < segments.len() {
        let onset = segments[k].0;
        let mut group = Vec::new();
        while k < segments.len() && segments[k].0 == onset {
            group.push(segments[k]);
            k += 1;
        }
        if onset < at {
            // It starts before the note that sounds now: it is dropped rather than summed.
            continue;
        }
        if onset > at {
            write_rest(out, onset - at);
            at = onset;
        }
        // A chord lasts as long as its longest note.
        let longest = group.iter().map(|g| g.1).max().unwrap_or(onset);
        let length = (longest - onset).max(1);
        for (i, &(_, end, start, abs_end, pitch)) in group.iter().enumerate() {
            let duration = end.max(onset + 1) - onset;
            write_note(
                out,
                pitch,
                duration,
                i > 0,
                start < measure.start,
                abs_end > measure.end,
            );
        }
        at += length;
    }
    let left = (measure.end - measure.start) - at;
    if left > 0 {
        write_rest(out, left);
    }
}

fn write_rest(out: &mut String, duration: i64) {
    let _ = writeln!(
        out,
        "      <note><rest/><duration>{duration}</duration><voice>1</voice></note>"
    );
}

fn write_note(
    out: &mut String,
    pitch: u8,
    duration: i64,
    chord: bool,
    tie_stop: bool,
    tie_start: bool,
) {
    let (step, alter, octave) = pitch_name(pitch);
    let mut ties = String::new();
    if tie_stop {
        ties.push_str("<tie type=\"stop\"/>");
    }
    if tie_start {
        ties.push_str("<tie type=\"start\"/>");
    }
    let mut notations = String::new();
    if tie_stop || tie_start {
        notations.push_str("<notations>");
        if tie_stop {
            notations.push_str("<tied type=\"stop\"/>");
        }
        if tie_start {
            notations.push_str("<tied type=\"start\"/>");
        }
        notations.push_str("</notations>");
    }
    let alter = if alter == 0 {
        String::new()
    } else {
        format!("<alter>{alter}</alter>")
    };
    let _ = writeln!(
        out,
        "      <note>{chord}<pitch><step>{step}</step>{alter}<octave>{octave}</octave></pitch><duration>{duration}</duration><voice>1</voice>{ties}{notations}</note>",
        chord = if chord { "<chord/>" } else { "" },
    );
}

/// A MIDI note as a MusicXML pitch: step, alteration and octave.
fn pitch_name(pitch: u8) -> (&'static str, i8, i32) {
    const STEPS: [&str; 12] = ["C", "C", "D", "D", "E", "F", "F", "G", "G", "A", "A", "B"];
    const ALTERS: [i8; 12] = [0, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 0];
    let pc = usize::from(pitch % 12);
    (STEPS[pc], ALTERS[pc], i32::from(pitch / 12) - 1)
}

/// Text for an XML document: the five characters that must be escaped.
fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Note, Region, RegionParams, TrackKind};

    fn note(start: f32, len: f32, pitch: u8) -> Note {
        Note {
            start,
            len,
            pitch,
            vel: 100,
        }
    }

    /// One instrument track named Piano with a one-bar-scale region holding `notes`.
    fn with_notes(notes: Vec<Note>) -> Project {
        let mut project = Project::empty();
        project.name = "Test Song".to_string();
        project.tracks[0].name = "Piano".to_string();
        project.tracks[0].kind = TrackKind::Instrument;
        project.tracks[0].regions.push(Region {
            name: "Chorus".to_string(),
            start_bar: 1.0,
            len_bars: 4.0,
            notes,
            controls: Vec::new(),
            folder: None,
            generated: None,
            audio: None,
            params: RegionParams::default(),
        });
        project
    }

    fn score(project: &Project) -> String {
        score_xml(project).unwrap_or_else(|e| panic!("{e}")).0
    }

    #[test]
    fn the_score_is_musicxml_with_measures_notes_and_rests() {
        let project = with_notes(vec![note(0.0, 1.0, 60), note(1.0, 2.0, 64)]);
        let (xml, count) = score_xml(&project).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(count, 2);
        assert!(xml.starts_with("<?xml"), "{xml}");
        assert!(xml.contains("<score-partwise version=\"4.0\">"));
        assert!(xml.contains("<part-name>Piano</part-name>"));
        assert!(xml.contains("<fifths>0</fifths>"));
        assert!(xml.contains("<beats>4</beats><beat-type>4</beat-type>"));
        assert!(xml.contains("<step>C</step><octave>4</octave>"), "C4");
        assert!(xml.contains("<step>E</step><octave>4</octave>"), "E4");
        // A quarter note and a half note in 4/4 leave one beat, written as a rest.
        assert!(xml.contains("<rest/><duration>480</duration>"), "{xml}");
        assert!(xml.contains("<measure number=\"1\">"));
    }

    #[test]
    fn notes_across_a_bar_line_are_tied() {
        let project = with_notes(vec![note(3.0, 2.0, 60)]);
        let xml = score(&project);
        assert!(xml.contains("<tie type=\"start\"/>"), "{xml}");
        assert!(xml.contains("<tie type=\"stop\"/>"), "{xml}");
        assert!(xml.contains("<measure number=\"2\">"), "{xml}");
    }

    #[test]
    fn the_project_key_is_written_and_an_empty_score_is_an_error() {
        let mut project = with_notes(vec![note(0.0, 1.0, 61)]);
        project.key = "Ebmin";
        let xml = score(&project);
        assert!(xml.contains("<fifths>-6</fifths>"), "{xml}");
        assert!(xml.contains("<alter>1</alter>"), "C sharp: {xml}");
        assert!(score_xml(&Project::empty()).is_err());
    }

    #[test]
    fn a_three_four_bar_holds_three_beats() {
        let mut project = with_notes(vec![
            note(0.0, 1.0, 60),
            note(1.0, 1.0, 62),
            note(2.0, 1.0, 64),
        ]);
        project.sig = (3, 4);
        let xml = score(&project);
        assert!(
            xml.contains("<beats>3</beats><beat-type>4</beat-type>"),
            "{xml}"
        );
        // No rest: the three quarter notes fill the bar.
        assert!(!xml.contains("<rest/>"), "{xml}");
        assert!(xml.contains("<measure number=\"1\">"));
    }
}
