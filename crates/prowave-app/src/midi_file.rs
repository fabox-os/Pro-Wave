//! Standard MIDI Files (`.mid`): reading one into the project (File > Import > MIDI File…) and
//! writing the project's MIDI (File > Export > Selection as MIDI File…).
//!
//! [`parse`] and [`write`] work on bytes and know nothing of the project. The import makes a
//! track for each track and channel of the file, with one region holding its notes, and
//! takes the tempo and signature changes when the project has no regions yet. The export
//! writes what plays: the notes after region transpose, quantize, mute and MIDI FX, a
//! tempo track with every tempo change and signature, and the sustain pedal.
use crate::model::{
    Control, Note, Project, Region, RegionParams, TrackKind, CHANNEL_PRESSURE, PITCH_BEND,
    SYSEX_BYTE,
};
use std::collections::VecDeque;

/// Ticks to a quarter note in the files written.
pub const EXPORT_PPQ: u16 = 480;
/// Most notes read from one file, and most tracks made from it.
const MAX_NOTES: usize = 2_000_000;
const MAX_TRACKS: usize = 512;
/// Largest file read (the notes of a long piece are well under a megabyte).
const MAX_FILE_BYTES: u64 = 128 << 20;
/// Most tempo and signature changes taken from a file.
const MAX_TEMPO_CHANGES: usize = 4096;
const MAX_SIGNATURE_CHANGES: usize = 1024;
/// The furthest bar a region of an imported file may reach (the project's own limit).
const MAX_BAR: f64 = 9999.0;
/// The most bytes of SysEx a file's tracks may carry in all (a huge message is dropped).
const MAX_SYSEX_BYTES: usize = 65536;
/// The MIDI channel (0-based) drums are on.
pub const DRUM_CHANNEL: u8 = 9;

/// A note of a file: ticks from the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileNote {
    pub start: u64,
    pub len: u64,
    pub pitch: u8,
    pub vel: u8,
}

/// A controller, pitch bend, channel pressure or SysEx byte of a file, at a tick. The kinds are
/// the same `num` values as a region's [`Control`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileControl {
    pub tick: u64,
    pub num: u8,
    pub val: u16,
}

/// The notes of one channel of one track of a file, and what else it plays.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FileTrack {
    pub name: String,
    /// 0-based.
    pub channel: u8,
    pub notes: Vec<FileNote>,
    /// Controllers (the sustain pedal is 64), pitch bend, channel pressure and the bytes of
    /// SysEx messages, in tick order.
    pub controls: Vec<FileControl>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MidiFile {
    /// Ticks to a quarter note (0 for an SMPTE-timed file).
    pub ppq: u16,
    /// Ticks a second for an SMPTE-timed file (0 for a metrical one). Its ticks are absolute
    /// time; [`import`] turns them into beats at the project's tempo.
    pub smpte_ticks_per_second: f64,
    /// (tick, beats per minute), sorted.
    pub tempos: Vec<(u64, f64)>,
    /// (tick, numerator, denominator), sorted.
    pub signatures: Vec<(u64, u8, u8)>,
    pub tracks: Vec<FileTrack>,
}

// ------------------------------------------------------------------------- reading

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self
            .data
            .get(self.at)
            .ok_or("the file ends in the middle of an event")?;
        self.at += 1;
        Ok(b)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("a length is too big")?;
        let s = self
            .data
            .get(self.at..end)
            .ok_or("the file ends in the middle of an event")?;
        self.at = end;
        Ok(s)
    }

    fn vlq(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for _ in 0..4 {
            let b = self.byte()?;
            v = (v << 7) | u64::from(b & 0x7f);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("a number is too long".into())
    }

    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
}

/// Reads a Standard MIDI File (format 0, 1 or 2; times in ticks per quarter note).
pub fn parse(data: &[u8]) -> Result<MidiFile, String> {
    let mut r = Reader { data, at: 0 };
    if r.bytes(4).map_err(|_| "not a MIDI file")? != b"MThd" {
        return Err("not a MIDI file".into());
    }
    let len = u32::from_be_bytes(r.bytes(4)?.try_into().map_err(|_| "bad header")?) as usize;
    if len < 6 {
        return Err("the header is too short".into());
    }
    let head = r.bytes(len)?;
    let format = u16::from_be_bytes([head[0], head[1]]);
    let count = u16::from_be_bytes([head[2], head[3]]) as usize;
    let division = u16::from_be_bytes([head[4], head[5]]);
    if format > 2 {
        return Err(format!("MIDI file format {format} is not known"));
    }
    // A negative division is SMPTE time: the high byte is -frames a second (29 means the
    // 29.97 drop-frame rate) and the low byte the ticks in a frame.
    let (ppq, smpte_ticks_per_second) = if division & 0x8000 != 0 {
        let fps = 256 - u16::from((division >> 8) as u8);
        let ticks_per_frame = u16::from((division & 0xff) as u8);
        if !(24..=30).contains(&fps) || ticks_per_frame == 0 {
            return Err("a bad SMPTE time division".into());
        }
        let fps = if fps == 29 { 29.97 } else { f64::from(fps) };
        (0, f64::from(ticks_per_frame) * fps)
    } else if division == 0 {
        return Err("the time division is zero".into());
    } else {
        (division, 0.0)
    };
    let mut file = MidiFile {
        ppq,
        smpte_ticks_per_second,
        ..MidiFile::default()
    };
    let mut total = 0usize;
    for _ in 0..count {
        if r.done() {
            break;
        }
        let id = r.bytes(4)?;
        let size = u32::from_be_bytes(r.bytes(4)?.try_into().map_err(|_| "bad chunk")?) as usize;
        // A chunk that claims more than is left is read as far as it goes.
        let size = size.min(data.len() - r.at);
        let body = r.bytes(size)?;
        if id != b"MTrk" {
            continue;
        }
        read_track(body, &mut file, &mut total)?;
    }
    file.tempos.sort_by_key(|t| t.0);
    file.signatures.sort_by_key(|t| t.0);
    file.tracks.truncate(MAX_TRACKS);
    Ok(file)
}

/// Adds the notes and the tempo and signature changes of one track chunk to `file`.
fn read_track(body: &[u8], file: &mut MidiFile, total: &mut usize) -> Result<(), String> {
    let mut r = Reader { data: body, at: 0 };
    let (mut tick, mut status) = (0u64, 0u8);
    let mut name = String::new();
    // Notes sounding: per channel and pitch the start tick and velocity.
    let mut open: Vec<VecDeque<(u64, u8)>> = vec![VecDeque::new(); 16 * 128];
    let mut channels: Vec<FileTrack> = Vec::new();
    // SysEx has no channel: its bytes go to the first channel of this track that has notes.
    let mut sysex: Vec<FileControl> = Vec::new();
    let slot = |ch: u8, channels: &mut Vec<FileTrack>| -> usize {
        match channels.iter().position(|t| t.channel == ch) {
            Some(i) => i,
            None => {
                channels.push(FileTrack {
                    channel: ch,
                    ..FileTrack::default()
                });
                channels.len() - 1
            }
        }
    };
    while !r.done() {
        tick = tick.saturating_add(r.vlq()?);
        let first = r.byte()?;
        let event = if first & 0x80 != 0 {
            first
        } else {
            // Running status: the byte read is the first data byte.
            r.at -= 1;
            status
        };
        match event {
            0xff => {
                let kind = r.byte()?;
                let len = r.vlq()? as usize;
                let data = r.bytes(len)?;
                match (kind, data) {
                    (0x03, _) if name.is_empty() => {
                        name = String::from_utf8_lossy(data)
                            .trim()
                            .chars()
                            .take(64)
                            .collect();
                    }
                    (0x51, [a, b, c]) => {
                        let us = u32::from_be_bytes([0, *a, *b, *c]).max(1);
                        file.tempos.push((tick, 60_000_000.0 / f64::from(us)));
                    }
                    (0x58, [num, den, ..]) if *num > 0 && *den < 7 => {
                        file.signatures.push((tick, *num, 1u8 << *den));
                    }
                    (0x2f, _) => break,
                    _ => {}
                }
            }
            0xf0 => {
                // A System Exclusive message: F0, its length, then the rest, usually ending with
                // F7. Kept whole (F0 first; F7 added when the file leaves it out).
                let len = r.vlq()? as usize;
                let rest = r.bytes(len)?;
                let mut message = vec![0xf0];
                message.extend_from_slice(rest);
                if message.last() != Some(&0xf7) {
                    message.push(0xf7);
                }
                if sysex.len() + message.len() <= MAX_SYSEX_BYTES {
                    sysex.extend(message.into_iter().map(|b| FileControl {
                        tick,
                        num: SYSEX_BYTE,
                        val: u16::from(b),
                    }));
                }
                status = 0;
            }
            0xf7 => {
                // An escape (a continuation of a SysEx message, or a special byte): skipped.
                let len = r.vlq()? as usize;
                r.bytes(len)?;
                status = 0;
            }
            0x80..=0xef => {
                status = event;
                let ch = event & 0x0f;
                let one = matches!(event & 0xf0, 0xc0 | 0xd0);
                let a = r.byte()? & 0x7f;
                let b = if one { 0 } else { r.byte()? & 0x7f };
                match event & 0xf0 {
                    0x90 if b > 0 => {
                        open[ch as usize * 128 + a as usize].push_back((tick, b));
                        slot(ch, &mut channels);
                    }
                    0x80 | 0x90 => {
                        let list = &mut open[ch as usize * 128 + a as usize];
                        if let Some((start, vel)) = list.pop_front() {
                            let i = slot(ch, &mut channels);
                            if *total < MAX_NOTES {
                                *total += 1;
                                channels[i].notes.push(FileNote {
                                    start,
                                    len: tick - start,
                                    pitch: a,
                                    vel,
                                });
                            }
                        }
                    }
                    0xb0 => {
                        let i = slot(ch, &mut channels);
                        channels[i].controls.push(FileControl {
                            tick,
                            num: a,
                            val: u16::from(b),
                        });
                    }
                    0xe0 => {
                        // Pitch bend: the least significant 7 bits come first.
                        let i = slot(ch, &mut channels);
                        channels[i].controls.push(FileControl {
                            tick,
                            num: PITCH_BEND,
                            val: u16::from(a) | (u16::from(b) << 7),
                        });
                    }
                    0xd0 => {
                        let i = slot(ch, &mut channels);
                        channels[i].controls.push(FileControl {
                            tick,
                            num: CHANNEL_PRESSURE,
                            val: u16::from(a),
                        });
                    }
                    _ => {}
                }
            }
            _ => return Err("an event that is not MIDI".into()),
        }
    }
    // Notes still sounding at the end of the track end there.
    for (k, list) in open.iter().enumerate() {
        for &(start, vel) in list {
            let (ch, pitch) = ((k / 128) as u8, (k % 128) as u8);
            let i = slot(ch, &mut channels);
            channels[i].notes.push(FileNote {
                start,
                len: tick.saturating_sub(start).max(1),
                pitch,
                vel,
            });
        }
    }
    if let Some(first) = channels.iter_mut().find(|t| !t.notes.is_empty()) {
        first.controls.extend(sysex);
    }
    let many = channels.len() > 1;
    for mut t in channels {
        if t.notes.is_empty() {
            continue;
        }
        t.notes.sort_by_key(|n| (n.start, n.pitch));
        t.controls.sort_by_key(|c| c.tick);
        t.name = if many {
            format!("{name} {}", t.channel + 1).trim().to_string()
        } else {
            name.clone()
        };
        file.tracks.push(t);
    }
    Ok(())
}

// ------------------------------------------------------------------------- writing

/// The variable-length quantity of `v`, as bytes.
fn vlq_bytes(v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    vlq(&mut out, v);
    out
}

fn vlq(out: &mut Vec<u8>, mut v: u64) {
    let mut bytes = vec![(v & 0x7f) as u8];
    v >>= 7;
    while v > 0 {
        bytes.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.extend(bytes.iter().rev());
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

/// Events of one track chunk, collected as (tick, order, bytes) and then written with deltas.
struct Events(Vec<(u64, u8, Vec<u8>)>);

impl Events {
    fn push(&mut self, tick: u64, order: u8, bytes: Vec<u8>) {
        self.0.push((tick, order, bytes));
    }

    fn finish(mut self) -> Vec<u8> {
        // Note Offs before Note Ons at the same tick, so a repeated note is not cut short.
        self.0.sort_by_key(|e| (e.0, e.1));
        let mut out = Vec::new();
        let mut last = 0u64;
        for (tick, _, bytes) in self.0 {
            vlq(&mut out, tick - last);
            out.extend(bytes);
            last = tick;
        }
        vlq(&mut out, 0);
        out.extend_from_slice(&[0xff, 0x2f, 0x00]);
        out
    }
}

fn meta(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![0xff, kind];
    vlq(&mut v, data.len() as u64);
    v.extend_from_slice(data);
    v
}

/// Writes a format 1 file: a tempo track, then one track for each of `file.tracks` (whose
/// notes are in ticks of `file.ppq`).
pub fn write(file: &MidiFile) -> Vec<u8> {
    let mut out = Vec::new();
    let mut head = Vec::new();
    head.extend_from_slice(&1u16.to_be_bytes());
    head.extend_from_slice(&((file.tracks.len() + 1) as u16).to_be_bytes());
    head.extend_from_slice(&file.ppq.to_be_bytes());
    chunk(&mut out, b"MThd", &head);

    let mut tempo = Events(Vec::new());
    for &(tick, num, den) in &file.signatures {
        let power = den.max(1).trailing_zeros() as u8;
        tempo.push(tick, 0, meta(0x58, &[num, power, 24, 8]));
    }
    for &(tick, bpm) in &file.tempos {
        let us = (60_000_000.0 / bpm.clamp(1.0, 1000.0)).round() as u32;
        tempo.push(tick, 0, meta(0x51, &us.to_be_bytes()[1..]));
    }
    chunk(&mut out, b"MTrk", &tempo.finish());

    for t in &file.tracks {
        let mut ev = Events(Vec::new());
        ev.push(0, 0, meta(0x03, t.name.as_bytes()));
        let ch = t.channel & 0x0f;
        for n in &t.notes {
            ev.push(
                n.start,
                2,
                vec![0x90 | ch, n.pitch.min(127), n.vel.clamp(1, 127)],
            );
            ev.push(
                n.start + n.len.max(1),
                1,
                vec![0x80 | ch, n.pitch.min(127), 0],
            );
        }
        let mut sysex: Vec<u8> = Vec::new();
        let mut sysex_tick = 0u64;
        for c in &t.controls {
            match c.num {
                PITCH_BEND => {
                    let v = c.val.min(16383);
                    ev.push(c.tick, 0, vec![0xe0 | ch, (v & 0x7f) as u8, (v >> 7) as u8]);
                }
                CHANNEL_PRESSURE => {
                    ev.push(c.tick, 0, vec![0xd0 | ch, c.val.min(127) as u8]);
                }
                SYSEX_BYTE => {
                    // The bytes of a message are gathered until F7 and written as one message.
                    let b = c.val.min(255) as u8;
                    if b == 0xf0 {
                        sysex.clear();
                        sysex_tick = c.tick;
                        sysex.push(b);
                    } else if !sysex.is_empty() {
                        sysex.push(b);
                    }
                    if b == 0xf7 && sysex.len() > 1 {
                        // F0, the length of the rest, then the rest (which ends with F7).
                        let mut msg = vec![0xf0];
                        msg.extend(vlq_bytes(sysex.len() as u64 - 1));
                        msg.extend_from_slice(&sysex[1..]);
                        ev.push(sysex_tick, 0, msg);
                        sysex.clear();
                    }
                }
                num => ev.push(
                    c.tick,
                    0,
                    vec![0xb0 | ch, num.min(127), c.val.min(127) as u8],
                ),
            }
        }
        chunk(&mut out, b"MTrk", &ev.finish());
    }
    out
}

// ------------------------------------------------------------------------- the project

/// Ticks of a file for `beats` quarter notes.
fn ticks(beats: f64, ppq: u16) -> u64 {
    (beats.max(0.0) * f64::from(ppq)).round() as u64
}

/// What the project's MIDI plays, as a file (see the module description). When `only` names a
/// track and region, just that region.
pub fn export(p: &Project, only: Option<(usize, usize)>) -> MidiFile {
    let ppq = EXPORT_PPQ;
    let mut file = MidiFile {
        ppq,
        ..MidiFile::default()
    };
    // The tempo changes; a curve is written as a step for each quarter note along it.
    let points = p.tempo_points();
    for (i, pt) in points.iter().enumerate() {
        file.tempos.push((ticks(pt.beat, ppq), f64::from(pt.bpm)));
        if let (true, Some(next)) = (pt.ramp, points.get(i + 1)) {
            let mut b = pt.beat + 1.0;
            while b < next.beat - 1e-9 {
                file.tempos.push((ticks(b, ppq), f64::from(p.tempo_at(b))));
                b += 1.0;
            }
        }
    }
    file.signatures.push((0, p.sig.0, p.sig.1));
    for c in &p.sig_changes {
        file.signatures.push((
            ticks(p.bar_to_beats(f64::from(c.bar) - 1.0), ppq),
            c.num,
            c.den,
        ));
    }
    let mut next_channel = 0u8;
    for (ti, t) in p.tracks.iter().enumerate() {
        if !t.kind.has_regions() || t.kind == TrackKind::Audio {
            continue;
        }
        let channel = match t.kind {
            TrackKind::Drummer => DRUM_CHANNEL,
            TrackKind::ExternalMidi => t.midi_channel.clamp(1, 16) - 1,
            _ => {
                if next_channel == DRUM_CHANNEL {
                    next_channel += 1;
                }
                let c = next_channel;
                next_channel = (next_channel + 1) % 16;
                c
            }
        };
        let mut track = FileTrack {
            name: t.name.clone(),
            channel,
            ..FileTrack::default()
        };
        let mut played = Vec::new();
        for (ri, r) in t.regions.iter().enumerate() {
            if only.is_some_and(|o| o != (ti, ri)) || r.params.mute {
                continue;
            }
            let origin = p.region_start_beats(r);
            played.extend(crate::engine_sync::region_notes(t, r, origin, ti as u16));
            for c in &r.controls {
                track.controls.push(FileControl {
                    tick: ticks(origin + f64::from(c.start), ppq),
                    num: c.num,
                    val: c.val,
                });
            }
        }
        // What the track's MIDI FX make of them.
        for n in crate::midi_fx::apply(&t.midi_fx, played) {
            track.notes.push(FileNote {
                start: ticks(n.start_beat, ppq),
                len: ticks(n.end_beat, ppq)
                    .saturating_sub(ticks(n.start_beat, ppq))
                    .max(1),
                pitch: n.pitch,
                vel: n.vel.clamp(1, 127),
            });
        }
        if !track.notes.is_empty() {
            track.notes.sort_by_key(|n| (n.start, n.pitch));
            track.controls.sort_by_key(|c| c.tick);
            file.tracks.push(track);
        }
    }
    file
}

/// Puts the contents of `file` in the project: a new track for each track of the file, with
/// one region of its notes, from the bar of its first note to the end of its last. The
/// tempo and signature changes of the file replace the project's when it has no regions yet.
/// Returns how many tracks were made.
pub fn import(p: &mut Project, file: &MidiFile) -> usize {
    // SMPTE ticks are seconds (ticks a second); at the project's tempo a second is bpm/60
    // quarters, which is what the rest of the importer counts in.
    let ppq = if file.smpte_ticks_per_second > 0.0 {
        file.smpte_ticks_per_second * 60.0 / f64::from(p.tempo.max(1.0))
    } else {
        f64::from(file.ppq.max(1))
    };
    if p.tracks.iter().all(|t| t.regions.is_empty()) {
        apply_timing(p, file);
    }
    let mut made = 0;
    for ft in &file.tracks {
        if ft.notes.is_empty() {
            continue;
        }
        let kind = if ft.channel == DRUM_CHANNEL {
            TrackKind::Drummer
        } else {
            TrackKind::Instrument
        };
        let first = ft.notes.iter().map(|n| n.start).min().unwrap_or(0) as f64 / ppq;
        let last = ft.notes.iter().map(|n| n.start + n.len).max().unwrap_or(0) as f64 / ppq;
        // Notes past the project's last bar are not placed.
        let start_bar0 = p
            .beats_to_bar(first + 1e-9)
            .floor()
            .clamp(0.0, MAX_BAR - 1.0);
        let origin = p.bar_to_beats(start_bar0);
        let len_bars = (p.beats_to_bar(last - 1e-9) - start_bar0)
            .ceil()
            .clamp(1.0, MAX_BAR - start_bar0);
        // A file that wants more tracks than the engine plays gets as many as it can.
        if p.tracks.len() >= prowave_engine::MAX_TRACKS {
            break;
        }
        let ti = p.add_track(kind);
        if !ft.name.is_empty() {
            p.tracks[ti].name = ft.name.clone();
        }
        let notes: Vec<Note> = ft
            .notes
            .iter()
            .map(|n| Note {
                start: (n.start as f64 / ppq - origin) as f32,
                len: (n.len as f64 / ppq).max(1.0 / 960.0) as f32,
                pitch: n.pitch.min(127),
                vel: n.vel.clamp(1, 127),
            })
            .collect();
        let controls: Vec<Control> = ft
            .controls
            .iter()
            .map(|c| Control {
                start: (c.tick as f64 / ppq - origin).max(0.0) as f32,
                num: c.num,
                val: c.val,
            })
            .collect();
        let name = if ft.name.is_empty() {
            "MIDI".to_string()
        } else {
            ft.name.clone()
        };
        p.tracks[ti].regions.push(Region {
            name,
            start_bar: start_bar0 as f32 + 1.0,
            len_bars: len_bars as f32,
            notes,
            controls,
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

/// A MIDI file command picked in the File menu; the app runs it with a file dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiCmd {
    Import,
    Export,
}

/// Whether `path` names a MIDI file.
pub fn is_midi_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mid" | "midi" | "smf"))
}

/// Reads the MIDI file at `path` into the project; returns how many tracks it made.
pub fn import_path(p: &mut Project, path: &std::path::Path) -> Result<usize, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err("the file is too big for a MIDI file".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file = parse(&bytes)?;
    match import(p, &file) {
        0 => Err("there are no notes in it".into()),
        n => Ok(n),
    }
}

/// Writes the project's MIDI (or one region's) to `path`.
pub fn export_path(
    p: &Project,
    only: Option<(usize, usize)>,
    path: &std::path::Path,
) -> Result<(), String> {
    let file = export(p, only);
    if file.tracks.is_empty() {
        return Err("there are no notes to write".into());
    }
    std::fs::write(path, write(&file)).map_err(|e| e.to_string())
}

/// The tempo and signature changes of `file` as the project's.
fn apply_timing(p: &mut Project, file: &MidiFile) {
    let ppq = f64::from(file.ppq.max(1));
    p.tempo_changes.clear();
    p.tempo_ramp = false;
    p.sig_changes.clear();
    if let Some(&(_, bpm)) = file.tempos.first() {
        p.tempo = (bpm as f32).clamp(crate::model::MIN_BPM, crate::model::MAX_BPM);
    }
    for &(tick, bpm) in file.tempos.iter().skip(1).take(MAX_TEMPO_CHANGES) {
        p.add_tempo_change(tick as f64 / ppq, bpm as f32);
    }
    // Signature changes sit on bar lines: count the bars from one change to the next.
    let (mut bar0, mut at_tick, mut sig) = (0.0f64, 0u64, (4u8, 4u8));
    for (i, &(tick, num, den)) in file
        .signatures
        .iter()
        .take(MAX_SIGNATURE_CHANGES)
        .enumerate()
    {
        let bar_ticks = ppq * 4.0 * f64::from(sig.0) / f64::from(sig.1);
        let bar = bar0 + ((tick - at_tick) as f64 / bar_ticks).round();
        if i == 0 && tick == 0 {
            p.sig = (num.clamp(1, 99), den);
        } else {
            p.set_signature_from((bar as u32).saturating_add(1), (num, den));
        }
        (bar0, at_tick, sig) = (bar, tick, (num, den));
    }
    p.tidy_tempo();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> MidiFile {
        MidiFile {
            ppq: 480,
            smpte_ticks_per_second: 0.0,
            tempos: vec![(0, 120.0), (3840, 90.0)],
            signatures: vec![(0, 4, 4), (1920, 3, 4)],
            tracks: vec![
                FileTrack {
                    name: "Piano".into(),
                    channel: 0,
                    notes: vec![
                        FileNote {
                            start: 0,
                            len: 480,
                            pitch: 60,
                            vel: 100,
                        },
                        FileNote {
                            start: 480,
                            len: 240,
                            pitch: 64,
                            vel: 80,
                        },
                        FileNote {
                            start: 480,
                            len: 960,
                            pitch: 67,
                            vel: 90,
                        },
                    ],
                    controls: vec![
                        FileControl {
                            tick: 0,
                            num: 64,
                            val: 127,
                        },
                        FileControl {
                            tick: 960,
                            num: 64,
                            val: 0,
                        },
                    ],
                },
                FileTrack {
                    name: "Drums".into(),
                    channel: 9,
                    notes: vec![FileNote {
                        start: 960,
                        len: 120,
                        pitch: 36,
                        vel: 120,
                    }],
                    controls: vec![],
                },
            ],
        }
    }

    #[test]
    fn what_is_written_is_read_back() {
        let file = song();
        let bytes = write(&file);
        assert_eq!(&bytes[..4], b"MThd");
        let back = parse(&bytes).unwrap();
        assert_eq!(back.ppq, 480);
        // A tempo is stored as microseconds to a quarter note.
        assert_eq!(back.tempos.len(), 2);
        for (a, b) in back.tempos.iter().zip(&file.tempos) {
            assert!(a.0 == b.0 && (a.1 - b.1).abs() < 0.001, "{a:?} {b:?}");
        }
        assert_eq!(back.signatures, file.signatures);
        assert_eq!(back.tracks, file.tracks);
    }

    #[test]
    fn running_status_note_off_by_zero_velocity_and_split_channels_are_read() {
        // Format 0, 96 ppq: a track named "Mix" with channel 1 and channel 10 notes, written
        // with running status, and a Note On of velocity 0 as the Note Off.
        let mut t: Vec<u8> = Vec::new();
        t.extend([0, 0xff, 0x03, 3, b'M', b'i', b'x']);
        t.extend([0, 0x90, 60, 100]);
        t.extend([0, 0x99, 36, 110]);
        t.extend([48, 64, 90]); // running status: channel 10, pitch 64
        t.extend([48, 0x90, 60, 0]);
        t.extend([48, 0x99, 36, 0]);
        t.extend([0, 0x99, 64, 0]);
        t.extend([0, 0xff, 0x2f, 0]);
        let mut f: Vec<u8> = Vec::new();
        f.extend(b"MThd");
        f.extend(6u32.to_be_bytes());
        f.extend([0, 0, 0, 1, 0, 96]);
        f.extend(b"MTrk");
        f.extend((t.len() as u32).to_be_bytes());
        f.extend(&t);
        let m = parse(&f).unwrap();
        assert_eq!(m.ppq, 96);
        assert_eq!(m.tracks.len(), 2, "one track for each channel");
        let names: Vec<&str> = m.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix 1", "Mix 10"]);
        assert_eq!(
            m.tracks[0].notes,
            [FileNote {
                start: 0,
                len: 96,
                pitch: 60,
                vel: 100
            }]
        );
        assert_eq!(m.tracks[1].channel, 9);
        assert_eq!(m.tracks[1].notes.len(), 2);
        assert_eq!(
            m.tracks[1].notes[0],
            FileNote {
                start: 0,
                len: 144,
                pitch: 36,
                vel: 110
            }
        );
        assert_eq!(
            m.tracks[1].notes[1],
            FileNote {
                start: 48,
                len: 96,
                pitch: 64,
                vel: 90
            }
        );
    }

    #[test]
    fn damaged_and_unusual_files_are_refused_or_survived() {
        assert!(parse(b"").is_err());
        assert!(parse(b"RIFF....WAVE").is_err());
        // SMPTE time division: 25 frames a second, 40 ticks a frame.
        let mut f: Vec<u8> = b"MThd".to_vec();
        f.extend(6u32.to_be_bytes());
        f.extend([0, 1, 0, 0, 0xe7, 0x28]);
        let m = parse(&f).unwrap();
        assert_eq!((m.ppq, m.smpte_ticks_per_second), (0, 1_000.0));
        // A track that ends in the middle of an event, and one with notes never released.
        let good = write(&song());
        for cut in [20, 40, good.len() - 3, good.len() / 2] {
            let _ = parse(&good[..cut]);
        }
        let mut t: Vec<u8> = vec![0, 0x90, 60, 100];
        t.extend([0x81, 0x00, 0xff, 0x2f, 0]);
        let mut f: Vec<u8> = b"MThd".to_vec();
        f.extend(6u32.to_be_bytes());
        f.extend([0, 0, 0, 1, 1, 0xe0]);
        f.extend(b"MTrk");
        f.extend((t.len() as u32).to_be_bytes());
        f.extend(&t);
        let m = parse(&f).unwrap();
        assert_eq!(
            m.tracks[0].notes,
            [FileNote {
                start: 0,
                len: 128,
                pitch: 60,
                vel: 100
            }]
        );
        // Random bytes never panic.
        let mut seed = 12345u32;
        for _ in 0..200 {
            let junk: Vec<u8> = (0..300)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            let mut with_head = good[..22].to_vec();
            with_head.extend(junk);
            let _ = parse(&with_head);
        }
    }

    #[test]
    fn importing_makes_tracks_regions_and_the_tempo_map() {
        let mut p = Project::empty();
        p.tracks.clear();
        let made = import(&mut p, &song());
        assert_eq!(made, 2);
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[0].name, "Piano");
        assert_eq!(
            (p.tracks[0].kind, p.tracks[1].kind),
            (TrackKind::Instrument, TrackKind::Drummer)
        );
        assert_eq!(p.tempo, 120.0);
        assert_eq!(p.tempo_at(100.0), 90.0, "the change at beat 8");
        // 3/4 from the start of bar 2 (tick 1920 is beat 4).
        assert_eq!(p.sig_changes.len(), 1);
        assert_eq!((p.sig_changes[0].bar, p.sig_changes[0].num), (2, 3));
        let r = &p.tracks[0].regions[0];
        assert_eq!(r.start_bar, 1.0);
        assert_eq!(r.notes.len(), 3);
        assert_eq!((r.notes[1].start, r.notes[1].len), (1.0, 0.5));
        assert_eq!(r.controls.len(), 2);
        // The drum note is in beat 2 of the first bar, a bar of region is enough.
        assert_eq!(p.tracks[1].regions[0].notes[0].start, 2.0);
        // A project that already has regions keeps its own tempo.
        let mut q = Project::demo();
        let tempo = q.tempo;
        import(&mut q, &song());
        assert_eq!(q.tempo, tempo);
    }

    #[test]
    fn exporting_writes_the_tempo_map_and_the_notes_that_play() {
        let mut p = Project::demo();
        p.tempo = 100.0;
        p.add_tempo_change(16.0, 140.0);
        p.set_tempo_ramp(1, false);
        p.set_signature_from(5, (3, 4));
        p.tracks[0].regions[0].params.transpose = 12;
        let all = export(&p, None);
        let bytes = write(&all);
        let back = parse(&bytes).unwrap();
        assert_eq!(back.tempos[0], (0, 100.0));
        assert!(back
            .tempos
            .iter()
            .any(|t| t.0 == 16 * 480 && (t.1 - 140.0).abs() < 0.01));
        assert!(
            back.signatures.contains(&(0, 4, 4)) && back.signatures.contains(&(16 * 480, 3, 4))
        );
        assert!(!back.tracks.is_empty());
        // Transposed as played.
        let plain = Project::demo();
        let low = plain.tracks[0].regions[0].notes[0].pitch;
        let first = &back.tracks[0];
        assert!(first.notes.iter().any(|n| n.pitch == low + 12));
        // Only a region.
        let one = export(&p, Some((0, 0)));
        assert_eq!(one.tracks.len(), 1);
        // A drum track is on channel 10.
        let d = p
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Drummer)
            .unwrap();
        let drums = export(&p, Some((d, 0)));
        assert_eq!(drums.tracks[0].channel, DRUM_CHANNEL);
        // A curve is written as steps.
        p.set_tempo_ramp(0, true);
        let curved = export(&p, None);
        assert!(curved.tempos.len() > all.tempos.len() + 10);
    }

    #[test]
    fn an_imported_file_exports_the_same_notes() {
        let mut p = Project::empty();
        p.tracks.clear();
        import(&mut p, &song());
        let out = export(&p, None);
        let mut a: Vec<FileNote> = song().tracks[0].notes.clone();
        let mut b: Vec<FileNote> = out.tracks[0].notes.clone();
        a.sort_by_key(|n| (n.start, n.pitch));
        b.sort_by_key(|n| (n.start, n.pitch));
        assert_eq!(a, b);
        assert_eq!(out.tracks[1].channel, DRUM_CHANNEL);
    }

    fn chunk_bytes(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(body);
        v
    }

    /// A file shaped like what sequencers write: a conductor track with name, key and time
    /// signature, several tempos; a track with program change, pitch bend, other controllers,
    /// text, a channel prefix and SysEx between its notes; an unknown chunk in between.
    fn sequencer_file() -> Vec<u8> {
        let mut conductor: Vec<u8> = Vec::new();
        conductor.extend([0, 0xff, 0x03, 4, b'S', b'o', b'n', b'g']);
        conductor.extend([0, 0xff, 0x58, 4, 6, 3, 24, 8]); // 6/8
        conductor.extend([0, 0xff, 0x59, 2, 0xfe, 0]); // key signature: one flat
        conductor.extend([0, 0xff, 0x51, 3, 0x07, 0xa1, 0x20]); // 500000 us: 120 bpm
                                                                // 8 beats later (a two-byte delta: 3840 ticks at 480 ppq), 96 bpm.
        conductor.extend([0x9e, 0x00, 0xff, 0x51, 3, 0x09, 0x89, 0x68]); // 625000 us
        conductor.extend([0, 0xff, 0x2f, 0]);

        let mut lead: Vec<u8> = Vec::new();
        lead.extend([0, 0xff, 0x03, 4, b'L', b'e', b'a', b'd']);
        lead.extend([0, 0xff, 0x20, 1, 0]); // channel prefix
        lead.extend([0, 0xff, 0x01, 5, b'h', b'e', b'l', b'l', b'o']); // text
        lead.extend([0, 0xc0, 5]); // program change (one data byte)
        lead.extend([0, 0xe0, 0x00, 0x50]); // pitch bend
        lead.extend([0, 0xb0, 7, 100]); // volume
        lead.extend([0, 0xb0, 64, 127]); // sustain on
        lead.extend([0, 0x90, 60, 90]);
        lead.extend([0, 0xf0, 4, 0x7e, 0x7f, 0x09, 0xf7]); // sysex
        lead.extend([0x83, 0x60, 0x80, 60, 40]); // 480 ticks later: note off (a real velocity)
        lead.extend([0, 0xb0, 64, 0]); // sustain off
        lead.extend([0, 0xa0, 60, 33]); // polyphonic aftertouch
        lead.extend([0, 0xd0, 22]); // channel pressure
        lead.extend([0x00, 0x90, 62, 80, 0x60, 62, 0]); // running status: note on, then off
        lead.extend([0, 0xff, 0x2f, 0]);

        let mut f = chunk_bytes(b"MThd", &[0, 1, 0, 3, 0x01, 0xe0]);
        f.extend(chunk_bytes(b"MTrk", &conductor));
        f.extend(chunk_bytes(b"XFIH", &[1, 2, 3, 4, 5]));
        f.extend(chunk_bytes(b"MTrk", &lead));
        f
    }

    #[test]
    fn a_file_shaped_like_a_sequencers_is_read_in_full() {
        let m = parse(&sequencer_file()).unwrap();
        assert_eq!(m.ppq, 480);
        assert_eq!(
            m.signatures,
            [(0, 6, 8)],
            "6/8: the denominator is a power of two"
        );
        assert_eq!(m.tempos.len(), 2);
        assert!((m.tempos[0].1 - 120.0).abs() < 1e-9 && m.tempos[0].0 == 0);
        assert!(
            (m.tempos[1].1 - 96.0).abs() < 1e-9 && m.tempos[1].0 == 3840,
            "{:?}",
            m.tempos
        );
        assert_eq!(m.tracks.len(), 1, "the conductor track has no notes");
        let t = &m.tracks[0];
        assert_eq!(t.name, "Lead");
        assert_eq!(t.channel, 0);
        assert_eq!(
            t.notes,
            [
                FileNote {
                    start: 0,
                    len: 480,
                    pitch: 60,
                    vel: 90
                },
                FileNote {
                    start: 480,
                    len: 96,
                    pitch: 62,
                    vel: 80
                },
            ]
        );
        let got: Vec<(u64, u8, u16)> = t.controls.iter().map(|c| (c.tick, c.num, c.val)).collect();
        // Everything the file plays besides its notes is kept, in order: a pitch bend, a volume
        // controller, the sustain pedal, a universal SysEx message, and channel pressure.
        assert_eq!(
            got,
            [
                (0, PITCH_BEND, 10240),
                (0, 7, 100),
                (0, 64, 127),
                (0, SYSEX_BYTE, 0xf0),
                (0, SYSEX_BYTE, 0x7e),
                (0, SYSEX_BYTE, 0x7f),
                (0, SYSEX_BYTE, 0x09),
                (0, SYSEX_BYTE, 0xf7),
                (480, 64, 0),
                (480, CHANNEL_PRESSURE, 22),
            ]
        );
        // Into a project: 6/8 from the start, a tempo change after 8 beats, a region.
        let mut p = Project::empty();
        p.tracks.clear();
        assert_eq!(import(&mut p, &m), 1);
        assert_eq!(p.sig, (6, 8));
        assert_eq!(p.tempo_at(0.0), 120.0);
        assert!((p.tempo_at(8.5) - 96.0).abs() < 1e-3);
        assert_eq!(p.tracks[0].regions[0].notes.len(), 2);
        // And back out as a file that reads the same.
        let out = parse(&write(&export(&p, None))).unwrap();
        assert_eq!(out.tracks[0].notes, t.notes);
        assert_eq!(out.signatures, m.signatures);
    }

    #[test]
    fn a_big_file_is_read_quickly() {
        let mut t: Vec<u8> = Vec::new();
        for i in 0..200_000u32 {
            let pitch = (30 + i % 60) as u8;
            t.extend([0, 0x90, pitch, 100, 10, 0x80, pitch, 0]);
        }
        t.extend([0, 0xff, 0x2f, 0]);
        let mut f = chunk_bytes(b"MThd", &[0, 0, 0, 1, 0x01, 0xe0]);
        f.extend(chunk_bytes(b"MTrk", &t));
        let began = std::time::Instant::now();
        let m = parse(&f).unwrap();
        assert_eq!(m.tracks[0].notes.len(), 200_000);
        let mut p = Project::empty();
        p.tracks.clear();
        import(&mut p, &m);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_file_on_disk_goes_in_and_comes_out_through_the_menu_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.mid");
        std::fs::write(&path, sequencer_file()).unwrap();
        assert!(is_midi_path(&path) && is_midi_path(std::path::Path::new("X.MID")));
        assert!(!is_midi_path(std::path::Path::new("x.wav")));
        let mut p = Project::empty();
        p.tracks.clear();
        assert_eq!(import_path(&mut p, &path), Ok(1));
        let out = dir.path().join("out.mid");
        export_path(&p, None, &out).unwrap();
        assert!(parse(&std::fs::read(&out).unwrap()).is_ok());
        // Things that go wrong say so instead of doing nothing.
        let missing = dir.path().join("nope.mid");
        assert!(import_path(&mut p, &missing).is_err());
        std::fs::write(dir.path().join("junk.mid"), b"not midi").unwrap();
        assert!(import_path(&mut p, &dir.path().join("junk.mid")).is_err());
        let empty = Project::empty();
        let mut none = empty.clone();
        none.tracks.clear();
        assert!(
            export_path(&none, None, &dir.path().join("x.mid")).is_err(),
            "no notes to write"
        );
        assert!(export_path(&p, None, &dir.path().join("no/such/folder/x.mid")).is_err());
    }

    fn one_track_file(ppq: u16, body: &[u8]) -> Vec<u8> {
        let mut f = chunk_bytes(b"MThd", &[0, 0, 0, 1, (ppq >> 8) as u8, ppq as u8]);
        f.extend(chunk_bytes(b"MTrk", body));
        f
    }

    #[test]
    fn hostile_files_cost_little_and_never_panic() {
        let took = |f: &dyn Fn()| {
            let began = std::time::Instant::now();
            f();
            began.elapsed()
        };
        // 200 000 Note Ons of one pitch, then as many Note Offs: the pairing must not be
        // quadratic.
        let mut t: Vec<u8> = Vec::new();
        for _ in 0..200_000 {
            t.extend([0, 0x90, 60, 100]);
        }
        for _ in 0..200_000 {
            t.extend([1, 0x80, 60, 0]);
        }
        t.extend([0, 0xff, 0x2f, 0]);
        let f = one_track_file(480, &t);
        assert!(
            took(&|| assert_eq!(parse(&f).unwrap().tracks[0].notes.len(), 200_000)).as_secs() < 3
        );

        // The furthest times a file can state, with the smallest tick: regions stay inside
        // the project's last bar and nothing overflows.
        let mut t: Vec<u8> = vec![0xff, 0xff, 0xff, 0x7f, 0x90, 60, 100];
        t.extend([0xff, 0xff, 0xff, 0x7f, 0x80, 60, 0]);
        t.extend([0xff, 0xff, 0xff, 0x7f, 0xff, 0x58, 4, 255, 6, 24, 8]);
        t.extend([0xff, 0xff, 0xff, 0x7f, 0xff, 0x51, 3, 0, 0, 0]);
        t.extend([0, 0xff, 0x2f, 0]);
        let m = parse(&one_track_file(1, &t)).unwrap();
        let mut p = Project::empty();
        p.tracks.clear();
        import(&mut p, &m);
        for tr in &p.tracks {
            for r in &tr.regions {
                assert!(
                    r.start_bar >= 1.0 && r.start_bar + r.len_bars <= 10_001.0,
                    "{r:?}",
                    r = (r.start_bar, r.len_bars)
                );
            }
        }
        let _ = write(&export(&p, None));

        // A hundred thousand tempo and signature changes: a few thousand are taken, quickly.
        let mut t: Vec<u8> = Vec::new();
        for i in 0..100_000u32 {
            t.extend([1, 0xff, 0x51, 3, 0x07, 0xa1, (i % 200) as u8]);
            t.extend([0, 0xff, 0x58, 4, 3, 2, 24, 8]);
        }
        t.extend([0, 0x90, 60, 100, 1, 0x80, 60, 0, 0, 0xff, 0x2f, 0]);
        let m = parse(&one_track_file(480, &t)).unwrap();
        let mut p = Project::empty();
        p.tracks.clear();
        assert!(
            took(&|| {
                let mut q = p.clone();
                import(&mut q, &m);
                assert!(q.tempo_changes.len() <= MAX_TEMPO_CHANGES);
                assert!(q.sig_changes.len() <= MAX_SIGNATURE_CHANGES);
            })
            .as_secs()
                < 10
        );
        import(&mut p, &m);

        // More channels than the project can hold tracks for.
        let mut t: Vec<u8> = Vec::new();
        for _ in 0..3 {
            for ch in 0..16u8 {
                t.extend([0, 0x90 | ch, 60, 100, 1, 0x80 | ch, 60, 0]);
            }
        }
        t.extend([0, 0xff, 0x2f, 0]);
        let mut f = chunk_bytes(b"MThd", &[0, 1, 0x00, 0x7f, 1, 0xe0]);
        for _ in 0..127 {
            f.extend(chunk_bytes(b"MTrk", &t));
        }
        let m = parse(&f).unwrap();
        assert!(m.tracks.len() <= MAX_TRACKS);
        let mut p = Project::empty();
        p.tracks.clear();
        import(&mut p, &m);
        assert!(p.tracks.len() <= prowave_engine::MAX_TRACKS);

        // A file too big for a MIDI file is not read at all.
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.mid");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        let mut q = Project::empty();
        assert!(import_path(&mut q, &big).is_err());
    }

    /// Pitch bend, channel pressure and SysEx survive writing and reading.
    #[test]
    fn pitch_bend_pressure_and_sysex_are_written_and_read_back() {
        let sysex = [0xf0u8, 0x41, 0x10, 0x42, 0xf7];
        let mut controls = vec![
            FileControl {
                tick: 0,
                num: PITCH_BEND,
                val: 8192,
            },
            FileControl {
                tick: 100,
                num: PITCH_BEND,
                val: 16383,
            },
            FileControl {
                tick: 200,
                num: CHANNEL_PRESSURE,
                val: 90,
            },
        ];
        controls.extend(sysex.iter().map(|&b| FileControl {
            tick: 300,
            num: SYSEX_BYTE,
            val: u16::from(b),
        }));
        let file = MidiFile {
            ppq: 480,
            smpte_ticks_per_second: 0.0,
            tracks: vec![FileTrack {
                name: "Keys".into(),
                channel: 2,
                notes: vec![FileNote {
                    start: 0,
                    len: 480,
                    pitch: 60,
                    vel: 90,
                }],
                controls: controls.clone(),
            }],
            ..MidiFile::default()
        };
        let back = parse(&write(&file)).expect("the written file reads");
        assert_eq!(back.tracks[0].controls, controls);
    }

    /// A SysEx message the file leaves without its F7 is completed, and the bytes of a bend
    /// come back in the right order (least significant 7 bits first).
    #[test]
    fn a_file_with_bend_pressure_and_an_open_sysex_is_read() {
        let mut body = Vec::new();
        body.extend([0, 0xe0, 0x00, 0x40]); // bend centre
        body.extend([0, 0xd0, 100]); // pressure 100
        body.extend([0, 0xf0, 3, 0x41, 0x10, 0x42]); // SysEx with no F7
        body.extend([0, 0x90, 60, 100]);
        body.extend([96, 0x80, 60, 0]);
        body.extend([0, 0xff, 0x2f, 0]);
        let mut file = Vec::new();
        chunk(&mut file, b"MThd", &[0, 0, 0, 1, 0, 1, 1, 0xe0]);
        chunk(&mut file, b"MTrk", &body);
        let parsed = parse(&file).expect("the file reads");
        let t = &parsed.tracks[0];
        let got: Vec<(u64, u8, u16)> = t.controls.iter().map(|c| (c.tick, c.num, c.val)).collect();
        assert_eq!(
            got,
            vec![
                (0, PITCH_BEND, 8192),
                (0, CHANNEL_PRESSURE, 100),
                (0, SYSEX_BYTE, 0xf0),
                (0, SYSEX_BYTE, 0x41),
                (0, SYSEX_BYTE, 0x10),
                (0, SYSEX_BYTE, 0x42),
                (0, SYSEX_BYTE, 0xf7),
            ]
        );
        // A bend is sent least significant 7 bits first: 0x7f then 0x7f is the top value.
        let mut top = Vec::new();
        top.extend([0, 0x90, 60, 100]);
        top.extend([0, 0xe0, 0x7f, 0x7f]);
        top.extend([96, 0x80, 60, 0]);
        top.extend([0, 0xff, 0x2f, 0]);
        let mut f2 = Vec::new();
        chunk(&mut f2, b"MThd", &[0, 0, 0, 1, 0, 1, 1, 0xe0]);
        chunk(&mut f2, b"MTrk", &top);
        let got = parse(&f2).expect("reads");
        assert_eq!(got.tracks[0].controls[0].val, 16383);
    }

    #[test]
    fn smpte_times_become_beats_at_the_project_tempo() {
        // 25 frames a second and 40 ticks a frame: 1000 ticks a second. A note held for 1000
        // ticks is a second, which is two beats at 120 BPM.
        let mut body = Vec::new();
        body.extend([0, 0x90, 60, 100]);
        body.extend([0x87, 0x68, 0x80, 60, 64]); // delta 1000, note off
        body.extend([0, 0xff, 0x2f, 0]);
        let mut file = Vec::new();
        chunk(&mut file, b"MThd", &[0, 0, 0, 1, 0xe7, 0x28]);
        chunk(&mut file, b"MTrk", &body);
        let parsed = parse(&file).expect("the file reads");
        assert_eq!(parsed.smpte_ticks_per_second, 1_000.0);

        let mut p = Project::empty();
        p.tracks.clear();
        p.tempo = 120.0;
        import(&mut p, &parsed);
        let notes = &p.tracks[0].regions[0].notes;
        assert_eq!(notes.len(), 1);
        assert!(notes[0].start.abs() < 1e-6, "from the start");
        assert!((notes[0].len - 2.0).abs() < 1e-4, "{}", notes[0].len);

        // The same file at 60 BPM covers one beat.
        p.tracks.clear();
        p.tempo = 60.0;
        import(&mut p, &parsed);
        let len = p.tracks[0].regions[0].notes[0].len;
        assert!((len - 1.0).abs() < 1e-4, "{len}");
    }
}
