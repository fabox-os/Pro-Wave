//! Importing a Logic Pro project (`.logicx`): its channel strips become tracks, its Audio Units
//! become hosted plug-ins with the settings the project saved, its MIDI notes are placed as the
//! piano roll showed them, and its audio files come in on tracks of their own.
//!
//! File > Import > Other Projects… adds the project's tracks to the open project, and File >
//! Open or a dropped bundle makes a new project from it. The reading is done by
//! `prowave-logicx-import`; this module only maps what the reader gives onto the app's model.
//! Whatever the reader cannot decode yet is an empty field, so it is left out here and picked up
//! as soon as the reader fills it in. The report lists what was not read.
//!
//! Plug-ins are found by their format and id, as `plugins::find` does for a project file. One
//! that is not installed keeps its slot and its saved state, and passes the sound through until
//! it is installed (see `plugins::missing`). Stock Logic plug-ins are not hosted: a name Pro Wave
//! has an effect for gives that effect with its default settings, and the rest are listed in the
//! report. The report window says what was carried over and what was not.

use crate::i18n::{tr, trf};
use crate::model::{
    Insert, Note, Project, Region, RegionParams, Track, TrackKind, MAX_BPM, MIN_BPM,
};
use crate::plugins::{self, Format, PluginInfo};
use prowave_engine::{AudioFile, FxKind};
use prowave_logicx_import as logic;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

/// The shortest region, in bars.
const MIN_BARS: f64 = 1.0 / 16.0;

/// The Audio Unit types that are instruments or MIDI processors. Pro Wave hosts effects only, so
/// these are not carried over.
const AU_INSTRUMENT: [[u8; 4]; 2] = [*b"aumu", *b"aumi"];

/// What an import did, for the Logic Pro Import window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The tracks made.
    pub tracks: usize,
    /// The Audio Units imported with the settings the project saved.
    pub with_settings: usize,
    /// The Audio Units that are not installed on this computer, as (name, vendor).
    pub missing: Vec<(String, String)>,
    /// The stock Logic plug-ins whose settings are not carried over, by name. A plug-in Pro Wave
    /// has an effect for is listed too: it comes with that effect's default settings.
    pub stock: Vec<String>,
    /// Other things left out or changed, one sentence each.
    pub notes: Vec<String>,
}

/// What the reader did not give, so that the report says so once. Each flag is set from the
/// reader's own fields, so its note goes as soon as the reader fills the field in.
#[derive(Default)]
struct Unread {
    volumes: bool,
    mute_solo_bypass: bool,
    positions: bool,
}

/// Whether `path` is a Logic Pro project bundle, by its extension.
pub fn is_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("logicx"))
}

/// Adds the tracks of the Logic Pro project at `path` to `p` and returns what was done. The
/// project's tempo and time signature are taken when `p` has no regions yet, as a Standard MIDI
/// File import does; otherwise they stay, and the report says so. Errors when the project does
/// not read or has no track Pro Wave can use, and then `p` is not changed.
pub fn import_into(p: &mut Project, path: &Path, ctx: &egui::Context) -> Result<Report, String> {
    let full = read(path)?;
    let media = media_dir(path);
    let mut report = map_into(p, &full, media.as_deref(), ctx, &plugins::find);
    // A plug-in is looked up in the catalog as it is at this moment: while the scan runs, one
    // that is installed may not be in it yet.
    if plugins::scan_status() != plugins::Scan::Done && !report.missing.is_empty() {
        report.notes.push(
            tr("The plug-in scan has not finished, so a plug-in shown as not found may still be found.")
                .to_string(),
        );
    }
    Ok(report)
}

/// A new project made from the Logic Pro project at `path`. It has no file yet, so Save asks
/// where to put it: the bundle itself is never written.
pub fn new_project(path: &Path, ctx: &egui::Context) -> Result<(Project, Report), String> {
    let full = read(path)?;
    let mut p = Project::empty();
    p.tracks.clear();
    p.selected_track = 0;
    p.name = full.info.name.clone();
    let media = media_dir(path);
    let report = map_into(&mut p, &full, media.as_deref(), ctx, &plugins::find);
    Ok((p, report))
}

/// Reads the bundle, and refuses it before anything is changed when it holds no track to import.
fn read(path: &Path) -> Result<logic::FullProject, String> {
    let full = logic::read_full(path).map_err(|e| e.to_string())?;
    check_resolution(full.ticks_per_quarter)?;
    if !full.tracks.iter().any(importable) {
        return Err(tr("the Logic Pro project has no track Pro Wave can import").to_string());
    }
    Ok(full)
}

/// Refuses a project whose note times have no resolution: every position is a tick count
/// divided by the ticks per quarter note, which must not be zero.
fn check_resolution(ticks_per_quarter: u32) -> Result<(), String> {
    if ticks_per_quarter == 0 {
        return Err(tr(
            "the Logic Pro project's note times have no resolution, so its notes cannot be placed",
        )
        .to_string());
    }
    Ok(())
}

/// The `Media` folder of the bundle `path` names (the bundle itself, or the one bundle inside a
/// folder). The reader names audio files relative to it, as `Audio Files/Take 1.wav`. `None` when
/// there is no bundle to look in.
fn media_dir(path: &Path) -> Option<PathBuf> {
    let bundle = if path.join("Alternatives").is_dir() {
        path.to_path_buf()
    } else {
        std::fs::read_dir(path)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| is_bundle(candidate) && candidate.join("Alternatives").is_dir())?
    };
    Some(bundle.join("Media"))
}

/// Whether a channel strip becomes a track: an active instrument, audio or aux strip. Masters,
/// outputs, inputs, busses and the unused default strips do not.
fn importable(track: &logic::Track) -> bool {
    track.active
        && matches!(
            track.kind,
            logic::TrackKind::Instrument | logic::TrackKind::Audio | logic::TrackKind::Aux
        )
}

/// The app's track kind for a strip of the project, if it becomes a track.
fn track_kind(kind: logic::TrackKind) -> Option<TrackKind> {
    match kind {
        logic::TrackKind::Instrument => Some(TrackKind::Instrument),
        logic::TrackKind::Audio => Some(TrackKind::Audio),
        logic::TrackKind::Aux => Some(TrackKind::Aux),
        _ => None,
    }
}

/// Looks up an installed plug-in by format and id (`plugins::find` in the app, a fake catalog
/// in the tests).
type Lookup<'a> = &'a dyn Fn(Format, &str) -> Option<PluginInfo>;

/// The mapping itself: the tempo and signature, then every importable strip as a track with its
/// plug-ins and regions, appended to `p`. An audio region goes on the track that owns it. An
/// audio file that no imported track plays gets a track of its own, once per file.
fn map_into(
    p: &mut Project,
    full: &logic::FullProject,
    media: Option<&Path>,
    ctx: &egui::Context,
    lookup: Lookup<'_>,
) -> Report {
    let mut report = Report::default();
    if p.tracks.iter().any(|t| !t.regions.is_empty()) {
        report.notes.push(
            tr("The project keeps its tempo and time signature, as it already has regions.")
                .to_string(),
        );
    } else {
        apply_timing(p, &full.info);
    }
    let tpq = full.ticks_per_quarter;
    let mut unread = Unread::default();
    let mut regions_made = false;
    for record in full.audio_regions.iter().filter(|r| r.file.is_none()) {
        report.notes.push(trf(
            "The audio region {} names no file in the project, so it is skipped",
            &[&record.name],
        ));
    }
    // The audio regions that an imported track owns, and the files those tracks play.
    let mut owned: Vec<&logic::AudioRegion> = Vec::new();
    let mut played: Vec<String> = Vec::new();
    for strip in full.tracks.iter().filter(|t| importable(t)) {
        let Some(kind) = track_kind(strip.kind) else {
            continue;
        };
        if p.tracks.len() >= prowave_engine::MAX_TRACKS {
            report.notes.push(trf(
                "Only {} tracks fit in a Pro Wave project: the others are not imported",
                &[&prowave_engine::MAX_TRACKS],
            ));
            break;
        }
        let mut track = Track::new(strip.name.clone(), kind);
        match strip.volume_db.filter(|v| v.is_finite()) {
            Some(db) => track.volume_db = db.clamp(-60.0, 6.0),
            None => unread.volumes = true,
        }
        if let Some(pan) = strip.pan.filter(|v| v.is_finite()) {
            // Logic's pan runs from -64 (hard left) to +63 (hard right), Pro Wave's from -1 to 1.
            track.pan = (pan / 64.0).clamp(-1.0, 1.0);
        }
        match strip.mute {
            Some(mute) => track.mute = mute,
            None => unread.mute_solo_bypass = true,
        }
        match strip.solo {
            Some(solo) => track.solo = solo,
            None => unread.mute_solo_bypass = true,
        }
        track.inserts = inserts_of(strip, &mut report, lookup, &mut unread);
        if !strip.instrument_files.is_empty() {
            report.notes.push(trf(
                "{}: the sampler's instrument files ({}) are not carried over",
                &[&strip.name, &strip.instrument_files.join(", ")],
            ));
        }
        if kind == TrackKind::Instrument {
            track.regions = midi_regions(p, strip, tpq, &mut unread);
        }
        for record in &strip.audio_regions {
            owned.push(record);
            let Some(rel) = record.file.as_deref() else {
                continue;
            };
            let Some(audio) = open_audio(rel, media, &mut report) else {
                continue;
            };
            track
                .regions
                .push(audio_region_of(p, record, audio, tpq, ctx, &mut unread));
            played.push(rel.to_string());
        }
        regions_made |= !track.regions.is_empty();
        p.tracks.push(track);
        report.tracks += 1;
    }
    // The audio regions whose owner is not an imported track: placed ones get a track per file,
    // and an unplaced one is left out, because the project does not play it.
    for record in full.audio_regions.iter().filter(|r| r.file.is_some()) {
        if owned.contains(&record) {
            continue;
        }
        let Some(file) = record.file.as_deref() else {
            continue;
        };
        if record.start_tick.is_none() {
            report.notes.push(trf(
                "The audio region {} is not placed in the project, so it is skipped",
                &[&record.name],
            ));
            continue;
        }
        if played.iter().any(|f| f == file) {
            continue;
        }
        if p.tracks.len() >= prowave_engine::MAX_TRACKS {
            break;
        }
        if let Some(track) = audio_track(p, record, file, tpq, media, ctx, &mut report) {
            played.push(file.to_string());
            regions_made = true;
            p.tracks.push(track);
            report.tracks += 1;
        }
    }
    if unread.volumes {
        report.notes.push(
            tr("The volumes were not read from the Logic Pro project, so the tracks stay at 0 dB")
                .to_string(),
        );
    }
    if unread.mute_solo_bypass {
        report.notes.push(
            tr("Mute, solo and plug-in bypass were not read from the Logic Pro project, so they are all off")
                .to_string(),
        );
    }
    if regions_made && unread.positions {
        report.notes.push(
            tr("The positions of the regions were not read from the Logic Pro project, so each one starts at bar 1")
                .to_string(),
        );
    }
    if report.tracks > 0 {
        p.selected_track = p.tracks.len() - 1;
        p.song_rev += 1;
    }
    report
}

/// Takes the tempo and time signature of the project, as Logic saved them. The tempo changes of
/// the project are not read yet, so the tempo is the one at its start.
fn apply_timing(p: &mut Project, info: &logic::Project) {
    if info.tempo.is_finite() {
        p.tempo = (info.tempo as f32).clamp(MIN_BPM, MAX_BPM);
    }
    p.tempo_changes.clear();
    p.tempo_ramp = false;
    p.sig_changes.clear();
    if let Some(sig) = info.time_signature.as_deref().and_then(parse_signature) {
        p.sig = sig;
    }
}

/// A signature such as `3/4`, when its numerator and denominator are ones Pro Wave has.
fn parse_signature(text: &str) -> Option<(u8, u8)> {
    let (num, den) = text.trim().split_once('/')?;
    let num: u8 = num.trim().parse().ok()?;
    let den: u8 = den.trim().parse().ok()?;
    ((1..=99).contains(&num) && matches!(den, 1 | 2 | 4 | 8 | 16 | 32)).then_some((num, den))
}

/// The inserts of a strip, in slot order. Audio Units are hosted with their saved state; stock
/// plug-ins become the effect of the same name, with default settings, when Pro Wave has one.
/// Instruments are not hosted, so the track keeps Pro Wave's own instrument.
fn inserts_of(
    strip: &logic::Track,
    report: &mut Report,
    lookup: Lookup<'_>,
    unread: &mut Unread,
) -> Vec<Insert> {
    let mut out = Vec::new();
    for slot in &strip.plugins {
        if slot.bypass.is_none() {
            unread.mute_solo_bypass = true;
        }
        match slot.format {
            logic::PluginFormat::AudioUnit => {
                let Some(id) = au_id(slot) else {
                    report.notes.push(trf(
                        "{}: the plug-in {} has no codes, so it is not carried over",
                        &[&strip.name, &slot.name],
                    ));
                    continue;
                };
                if slot.au_type.is_some_and(|t| AU_INSTRUMENT.contains(&t)) {
                    report.notes.push(trf(
                        "{}: the instrument {} is not hosted yet, so the track keeps Pro Wave's default instrument",
                        &[&strip.name, &slot.name],
                    ));
                    continue;
                }
                let found = lookup(Format::AudioUnit, &id);
                let vendor = slot.vendor.clone().unwrap_or_else(|| manufacturer(slot));
                if found.is_none() {
                    report.missing.push((slot.name.clone(), vendor.clone()));
                }
                let info = found.unwrap_or_else(|| PluginInfo {
                    format: Format::AudioUnit,
                    id,
                    name: slot.name.clone(),
                    vendor,
                    path: PathBuf::new(),
                });
                if slot.state.as_ref().is_some_and(|s| !s.is_empty()) {
                    report.with_settings += 1;
                }
                // The state goes to the plug-in exactly as the project stores it.
                let mut insert = Insert::hosted(plugins::add(&info, slot.state.clone()));
                if let Some(bypass) = slot.bypass {
                    insert.bypass = bypass;
                }
                out.push(insert);
            }
            logic::PluginFormat::Stock => {
                if let Some(kind) = FxKind::from_name(&slot.name) {
                    let mut insert = Insert::new(kind);
                    if let Some(bypass) = slot.bypass {
                        insert.bypass = bypass;
                    }
                    out.push(insert);
                }
                report.stock.push(stock_label(&slot.name));
            }
            logic::PluginFormat::Unknown => report.notes.push(trf(
                "{}: the plug-in {} is in a format Pro Wave does not know, so it is not carried over",
                &[&strip.name, &slot.name],
            )),
        }
    }
    out.truncate(prowave_engine::MAX_INSERTS);
    out
}

/// The Audio Unit id of a slot: `type:subtype:manufacturer`, each code as eight lowercase hex
/// digits in reading order, as the scan writes it. `None` when the slot has no codes.
fn au_id(slot: &logic::PluginSlot) -> Option<String> {
    Some(format!(
        "{}:{}:{}",
        hex(slot.au_type?),
        hex(slot.au_subtype?),
        hex(slot.au_manufacturer?)
    ))
}

/// The name a stock plug-in is listed by. The reader gives Sample Delay the eight-letter name
/// that Logic stores it under.
fn stock_label(name: &str) -> String {
    match name {
        "SmpleDly" => "Sample Delay".to_string(),
        other => other.to_string(),
    }
}

fn hex(code: [u8; 4]) -> String {
    code.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The vendor to show for a plug-in whose vendor the reader does not know: its manufacturer code
/// as written in the project (for example `FabF`).
fn manufacturer(slot: &logic::PluginSlot) -> String {
    slot.au_manufacturer
        .map(|code| String::from_utf8_lossy(&code).into_owned())
        .unwrap_or_default()
}

/// The MIDI regions of a strip as Pro Wave regions. A note's start is in ticks from the start of
/// its region, as the reader gives it, so it lands where the piano roll showed it. A region the
/// reader did not place starts at bar 1 and covers its notes, whole bars.
fn midi_regions(p: &Project, strip: &logic::Track, tpq: u32, unread: &mut Unread) -> Vec<Region> {
    strip
        .midi_regions
        .iter()
        .map(|region| {
            if region.start_tick.is_none() {
                unread.positions = true;
            }
            let mut notes: Vec<Note> = region
                .notes
                .iter()
                .map(|n| Note {
                    start: beats_of(n.start_tick, tpq) as f32,
                    // A zero-length note is one tick long, so that it still shows.
                    len: beats_of(n.length_ticks, tpq).max(1.0 / f64::from(tpq)) as f32,
                    pitch: n.pitch.min(127),
                    vel: n.velocity.clamp(1, 127),
                })
                .collect();
            notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
            let notes_end = region
                .notes
                .iter()
                .map(|n| beats_of(n.start_tick + n.length_ticks, tpq))
                .fold(0.0, f64::max);
            let bars = p.beats_to_bar(notes_end).ceil().max(1.0);
            let start = region.start_tick.map_or(0.0, |t| beats_of(t, tpq));
            let length = region
                .length_ticks
                .map_or(p.bar_to_beats(bars), |t| beats_of(t, tpq));
            let (start_bar, len_bars) = region_bars(p, start, length);
            Region {
                name: region
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| strip.name.clone()),
                start_bar,
                len_bars,
                notes,
                controls: Vec::new(),
                folder: None,
                audio: None,
                params: RegionParams::default(),
                generated: None,
            }
        })
        .collect()
}

/// Opens the audio file `rel` (relative to the bundle's `Media` folder). A file that is missing,
/// outside `Media` or unreadable is left out, with a note.
fn open_audio(rel: &str, media: Option<&Path>, report: &mut Report) -> Option<AudioFile> {
    let Some(file) = media.and_then(|dir| safe_media_file(dir, rel)) else {
        report.notes.push(trf(
            "The audio file {} is not in Media, so it is skipped",
            &[&rel],
        ));
        return None;
    };
    match AudioFile::open(&file) {
        Ok(audio) => Some(audio),
        Err(e) => {
            report.notes.push(trf("cannot read {}: {}", &[&rel, &e]));
            None
        }
    }
}

/// The audio region of `record` on the track that owns it. Its start is the reader's start, or
/// bar 1 when the reader did not place it. Its length is the reader's length in ticks, or the
/// length in frames of the record at the file's sample rate, or the whole file.
fn audio_region_of(
    p: &Project,
    record: &logic::AudioRegion,
    audio: AudioFile,
    tpq: u32,
    ctx: &egui::Context,
    unread: &mut Unread,
) -> Region {
    if record.start_tick.is_none() {
        unread.positions = true;
    }
    let start = record.start_tick.map_or(0.0, |t| beats_of(t, tpq));
    let length = match record.length_ticks {
        Some(ticks) => beats_of(ticks, tpq),
        None => {
            let seconds = record.length_frames.map_or_else(
                || audio.duration_secs(),
                |frames| f64::from(frames) / f64::from(audio.sample_rate.max(1)),
            );
            p.tempo_map().advance(start, seconds) - start
        }
    };
    let (start_bar, len_bars) = region_bars(p, start, length);
    Region {
        name: record.name.clone(),
        start_bar,
        len_bars,
        notes: Vec::new(),
        controls: Vec::new(),
        folder: None,
        audio: Some(crate::import::audio_region(audio, ctx)),
        params: RegionParams::default(),
        generated: None,
    }
}

/// The track for an audio file that no imported track plays: a new audio track named after the
/// file, with the region the reader placed. The report says why it is on a track of its own.
fn audio_track(
    p: &Project,
    record: &logic::AudioRegion,
    file: &str,
    tpq: u32,
    media: Option<&Path>,
    ctx: &egui::Context,
    report: &mut Report,
) -> Option<Track> {
    let audio = open_audio(file, media, report)?;
    report.notes.push(trf(
        "The audio file {} plays on no track that is imported, so it is on a track of its own",
        &[&file],
    ));
    let name = file_stem(file);
    let mut track = Track::new(name, TrackKind::Audio);
    // Only a record the reader placed gets here (see `map_into`), so no position is unread.
    let mut placed = Unread::default();
    track.regions = vec![audio_region_of(p, record, audio, tpq, ctx, &mut placed)];
    Some(track)
}

/// The name of an audio file without its folder and extension (`Audio Files/Inst 1_bip.wav`
/// is `Inst 1_bip`).
fn file_stem(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
        .to_string()
}

/// The file `rel` names inside `dir`, when it is a plain relative path that stays inside `dir`
/// (a project from elsewhere must not point the import at other files).
fn safe_media_file(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let root = std::fs::canonicalize(dir).ok()?;
    let target = std::fs::canonicalize(dir.join(rel)).ok()?;
    target.starts_with(&root).then_some(target)
}

/// Quarter notes for a time in ticks, at the reader's ticks per quarter note.
fn beats_of(ticks: u64, ticks_per_quarter: u32) -> f64 {
    ticks as f64 / f64::from(ticks_per_quarter)
}

/// The bar (from 1) and length in bars of a region that starts `start` quarter notes into the
/// project and lasts `length` quarter notes. A region is at least one sixteenth of a bar long.
fn region_bars(p: &Project, start: f64, length: f64) -> (f32, f32) {
    let first = p.beats_to_bar(start);
    let last = p.beats_to_bar(start + length);
    ((first + 1.0) as f32, (last - first).max(MIN_BARS) as f32)
}

/// Shows the Logic Pro Import window with `report` until it is closed.
pub fn show_report(ctx: &egui::Context, report: &mut Option<Report>) {
    let Some(shown) = report.clone() else {
        return;
    };
    let mut open = true;
    let mut close = false;
    egui::Window::new(tr("Logic Pro Import"))
        .id(egui::Id::unique("logicx_import_report"))
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label(trf(
                "{} tracks imported, {} plug-ins with their settings, {} plug-ins not found on this computer, {} stock plug-ins not carried over",
                &[&shown.tracks, &shown.with_settings, &shown.missing.len(), &shown.stock.len()],
            ));
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    if !shown.missing.is_empty() {
                        ui.add_space(8.0);
                        ui.strong(tr("Not found on this computer"));
                        for (name, vendor) in &shown.missing {
                            ui.label(format!("{name} ({vendor})"));
                        }
                    }
                    if !shown.stock.is_empty() {
                        ui.add_space(8.0);
                        ui.strong(tr("Stock plug-ins not carried over"));
                        for name in &shown.stock {
                            ui.label(name);
                        }
                    }
                    if !shown.notes.is_empty() {
                        ui.add_space(8.0);
                        ui.strong(tr("Other things not carried over"));
                        for note in &shown.notes {
                            ui.label(note);
                        }
                    }
                });
            ui.add_space(8.0);
            if ui.button(tr("OK")).clicked() {
                close = true;
            }
        });
    if close || !open {
        *report = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use logic::{AudioRegion, MidiNote, MidiRegion, PluginFormat, PluginSlot, TrackKind as L};

    /// The ticks per quarter note of the test projects: the reader's resolution.
    const TPQ: u32 = 960;

    /// The bytes of an Audio Unit class-info plist, to check they come back unchanged.
    const STATE: &[u8] =
        b"<?xml version=\"1.0\"?><plist><dict><key>data</key><data>AQI=</data></dict></plist>";

    fn strip(name: &str, kind: L) -> logic::Track {
        logic::Track {
            name: name.into(),
            kind,
            active: true,
            volume_db: None,
            fader_raw: None,
            pan: None,
            mute: None,
            solo: None,
            plugins: Vec::new(),
            midi_regions: Vec::new(),
            audio_regions: Vec::new(),
            instrument_files: Vec::new(),
        }
    }

    fn au(
        name: &str,
        kind: &[u8; 4],
        sub: &[u8; 4],
        manu: &[u8; 4],
        state: Option<&[u8]>,
    ) -> PluginSlot {
        PluginSlot {
            slot: 0,
            name: name.into(),
            vendor: None,
            format: PluginFormat::AudioUnit,
            au_type: Some(*kind),
            au_subtype: Some(*sub),
            au_manufacturer: Some(*manu),
            bypass: Some(false),
            state: state.map(<[u8]>::to_vec),
        }
    }

    fn stock(name: &str) -> PluginSlot {
        PluginSlot {
            slot: 0,
            name: name.into(),
            vendor: None,
            format: PluginFormat::Stock,
            au_type: None,
            au_subtype: None,
            au_manufacturer: None,
            bypass: None,
            state: None,
        }
    }

    fn midi_region(
        name: Option<&str>,
        start_tick: Option<u64>,
        length_ticks: Option<u64>,
        notes: Vec<MidiNote>,
    ) -> MidiRegion {
        MidiRegion {
            name: name.map(str::to_string),
            start_tick,
            length_ticks,
            notes,
        }
    }

    fn note(pitch: u8, velocity: u8, start_tick: u64, length_ticks: u64) -> MidiNote {
        MidiNote {
            pitch,
            velocity,
            start_tick,
            length_ticks,
        }
    }

    /// An audio record: `start` is its tick when the reader placed it, `frames` its length in
    /// frames when the reader read it.
    fn audio_record(
        name: &str,
        file: Option<&str>,
        start: Option<u64>,
        frames: Option<u32>,
    ) -> AudioRegion {
        AudioRegion {
            name: name.into(),
            file: file.map(str::to_string),
            start_tick: start,
            length_frames: frames,
            length_ticks: None,
        }
    }

    fn project(tracks: Vec<logic::Track>) -> logic::FullProject {
        logic::FullProject {
            info: logic::Project {
                name: "Song".into(),
                tempo: 120.0,
                key_signature: None,
                time_signature: Some("4/4".into()),
                sample_rate: None,
                channels: None,
                alternative: 0,
                logic_version: None,
                project_data_bytes: 0,
            },
            tracks,
            unassigned_plugins: Vec::new(),
            audio_regions: Vec::new(),
            audio_files: Vec::new(),
            ticks_per_quarter: TPQ,
        }
    }

    /// An empty project: no tracks, so the tempo and signature are taken.
    fn empty() -> Project {
        let mut p = Project::empty();
        p.tracks.clear();
        p.selected_track = 0;
        p
    }

    const PRO_Q: (&[u8; 4], &[u8; 4], &[u8; 4]) = (b"aumf", b"FQ4p", b"FabF");
    const PRO_Q_ID: &str = "61756d66:46513470:46616246";

    fn nothing(_: Format, _: &str) -> Option<PluginInfo> {
        None
    }

    fn installed(format: Format, id: &str) -> Option<PluginInfo> {
        (format == Format::AudioUnit && id == PRO_Q_ID).then(|| PluginInfo {
            format,
            id: id.into(),
            name: "Pro-Q 4".into(),
            vendor: "FabFilter".into(),
            path: PathBuf::new(),
        })
    }

    /// Maps `full` onto `p` with no plug-ins installed and no media folder.
    fn map(p: &mut Project, full: &logic::FullProject) -> Report {
        map_into(p, full, None, &egui::Context::default(), &nothing)
    }

    #[test]
    fn only_active_instrument_audio_and_aux_strips_become_tracks() {
        let mut inactive = strip("Inst 9", L::Instrument);
        inactive.active = false;
        let full = project(vec![
            strip("Master", L::Master),
            strip("Output 1-2", L::Output),
            strip("Input 1", L::Input),
            strip("Bus 1", L::Bus),
            strip("Bus 12", L::Unknown(3)),
            inactive,
            strip("Inst 1", L::Instrument),
            strip("Audio 1", L::Audio),
            strip("Aux 1", L::Aux),
        ]);
        let mut p = empty();
        let report = map(&mut p, &full);
        let names: Vec<(&str, TrackKind)> =
            p.tracks.iter().map(|t| (t.name.as_str(), t.kind)).collect();
        assert_eq!(
            names,
            [
                ("Inst 1", TrackKind::Instrument),
                ("Audio 1", TrackKind::Audio),
                ("Aux 1", TrackKind::Aux),
            ]
        );
        assert_eq!(report.tracks, 3);
        assert_eq!(p.selected_track, 2);
    }

    #[test]
    fn pan_is_mapped_from_logics_scale_and_flags_are_taken_when_read() {
        let mut known = strip("Audio 1", L::Audio);
        known.volume_db = Some(-6.5);
        known.mute = Some(true);
        known.solo = Some(false);
        let mut left = strip("Audio 2", L::Audio);
        left.pan = Some(-64.0);
        let mut right = strip("Audio 3", L::Audio);
        right.pan = Some(63.0);
        let unknown = strip("Audio 4", L::Audio);
        let mut p = empty();
        map(&mut p, &project(vec![known, left, right, unknown]));
        assert_eq!(p.tracks[0].volume_db, -6.5);
        assert!(p.tracks[0].mute);
        assert!(!p.tracks[0].solo);
        assert_eq!(p.tracks[0].pan, 0.0, "no pan read: centre");
        assert_eq!(p.tracks[1].pan, -1.0, "-64 is hard left");
        assert_eq!(
            p.tracks[2].pan,
            63.0 / 64.0,
            "+63 is just short of hard right"
        );
        assert_eq!((p.tracks[3].volume_db, p.tracks[3].pan), (0.0, 0.0));
        assert!(!p.tracks[3].mute);
    }

    #[test]
    fn what_the_reader_does_not_read_is_reported_once() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut slot = au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, None);
        slot.bypass = None;
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map(
            &mut p,
            &project(vec![
                inst,
                strip("Audio 1", L::Audio),
                strip("Aux 1", L::Aux),
            ]),
        );
        let count = |word: &str| report.notes.iter().filter(|n| n.contains(word)).count();
        assert_eq!(count("volumes"), 1, "{:?}", report.notes);
        assert_eq!(count("bypass"), 1, "{:?}", report.notes);
        assert_eq!(count("positions"), 0, "no region was made");
        assert_eq!(p.tracks[1].volume_db, 0.0, "the volume stays at 0 dB");
    }

    #[test]
    fn a_field_the_reader_gives_is_not_reported() {
        let mut audio = strip("Audio 1", L::Audio);
        audio.volume_db = Some(0.0);
        audio.mute = Some(false);
        audio.solo = Some(false);
        let mut inst = strip("Inst 1", L::Instrument);
        inst.volume_db = Some(-3.0);
        inst.mute = Some(false);
        inst.solo = Some(false);
        let mut slot = au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, Some(STATE));
        slot.bypass = Some(false);
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map(&mut p, &project(vec![audio, inst]));
        assert!(report.notes.is_empty(), "{:?}", report.notes);
    }

    #[test]
    fn an_installed_audio_unit_uses_its_catalog_entry_and_keeps_its_state() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut slot = au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, Some(STATE));
        slot.bypass = Some(true);
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &installed,
        );
        let insert = p.tracks[0].inserts[0];
        assert_ne!(insert.plugin, 0);
        assert!(insert.bypass);
        assert_eq!(
            plugins::info(insert.plugin),
            installed(Format::AudioUnit, PRO_Q_ID)
        );
        assert_eq!(plugins::state(insert.plugin), Some(STATE.to_vec()));
        assert!(report.missing.is_empty());
        assert_eq!(report.with_settings, 1);
    }

    #[test]
    fn a_bypass_the_reader_gives_is_kept_and_an_unread_one_is_off() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut on = au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, None);
        on.bypass = Some(true);
        let mut unread = stock("Compressor");
        unread.bypass = None;
        inst.plugins = vec![on, unread];
        let mut p = empty();
        map(&mut p, &project(vec![inst]));
        let kinds: Vec<bool> = p.tracks[0].inserts.iter().map(|i| i.bypass).collect();
        assert_eq!(kinds, [true, false]);
    }

    #[test]
    fn an_audio_unit_not_installed_keeps_its_slot_and_is_reported() {
        let mut inst = strip("Inst 1", L::Instrument);
        inst.plugins
            .push(au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, Some(STATE)));
        let mut p = empty();
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
        let insert = p.tracks[0].inserts[0];
        let info = plugins::info(insert.plugin).unwrap();
        assert_eq!(info.format, Format::AudioUnit);
        assert_eq!(info.id, PRO_Q_ID);
        assert_eq!(info.name, "Pro-Q 4");
        assert_eq!(
            info.vendor, "FabF",
            "the manufacturer code, until the reader knows the vendor"
        );
        assert!(info.path.as_os_str().is_empty());
        assert_eq!(plugins::state(insert.plugin), Some(STATE.to_vec()));
        assert_eq!(
            report.missing,
            [("Pro-Q 4".to_string(), "FabF".to_string())]
        );
        assert_eq!(report.with_settings, 1);
    }

    #[test]
    fn a_vendor_the_reader_knows_is_used_for_a_missing_plug_in() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut slot = au("Pro-Q 4", PRO_Q.0, PRO_Q.1, PRO_Q.2, None);
        slot.vendor = Some("FabFilter".into());
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map(&mut p, &project(vec![inst]));
        assert_eq!(
            report.missing,
            [("Pro-Q 4".to_string(), "FabFilter".to_string())]
        );
        assert_eq!(report.with_settings, 0, "no state, no settings counted");
    }

    #[test]
    fn stock_plug_ins_map_by_name_and_the_rest_are_listed() {
        let mut inst = strip("Inst 1", L::Instrument);
        inst.plugins = vec![
            stock("Sampler"),
            stock("SmpleDly"),
            stock("Compressor"),
            stock("Channel EQ"),
        ];
        let mut p = empty();
        let report = map(&mut p, &project(vec![inst]));
        let kinds: Vec<FxKind> = p.tracks[0].inserts.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [FxKind::Compressor, FxKind::ChannelEq]);
        assert_eq!(p.tracks[0].inserts[0].params, FxKind::Compressor.defaults());
        assert_eq!(
            report.stock,
            ["Sampler", "Sample Delay", "Compressor", "Channel EQ"]
        );
        assert_eq!(
            p.tracks[0].inserts.len(),
            2,
            "Sampler and Sample Delay have no effect here"
        );
        assert_eq!(report.missing.len(), 0);
    }

    #[test]
    fn an_instrument_audio_unit_is_not_hosted() {
        let mut inst = strip("Inst 1", L::Instrument);
        inst.plugins
            .push(au("Alchemy", b"aumu", b"Alch", b"Appl", None));
        let mut p = empty();
        let report = map(&mut p, &project(vec![inst]));
        assert!(p.tracks[0].inserts.is_empty());
        assert!(report.missing.is_empty());
        let alchemy: Vec<&String> = report
            .notes
            .iter()
            .filter(|n| n.contains("Alchemy"))
            .collect();
        assert_eq!(alchemy.len(), 1, "{:?}", report.notes);
    }

    #[test]
    fn a_plug_in_in_an_unknown_format_is_listed_not_guessed() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut slot = stock("Mystery");
        slot.format = PluginFormat::Unknown;
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map(&mut p, &project(vec![inst]));
        assert!(p.tracks[0].inserts.is_empty());
        assert!(report.notes[0].contains("Mystery"));
    }

    #[test]
    fn notes_are_beats_from_the_region_start_at_the_readers_resolution() {
        let mut inst = strip("Inst 1", L::Instrument);
        // The region starts at beat 4 (the start of bar 2 in 4/4) and lasts one bar.
        inst.midi_regions.push(midi_region(
            Some("Lead"),
            Some(4 * u64::from(TPQ)),
            Some(4 * u64::from(TPQ)),
            vec![note(64, 90, 480, 240), note(60, 100, 0, 0)],
        ));
        let mut p = empty();
        map(&mut p, &project(vec![inst]));
        let region = &p.tracks[0].regions[0];
        assert_eq!(region.name, "Lead");
        assert_eq!(
            region.start_bar, 2.0,
            "4 quarter notes is the start of bar 2 in 4/4"
        );
        assert_eq!(region.len_bars, 1.0, "4 quarter notes is one bar");
        // Notes come out sorted by their start, each one in beats from the region's start: a
        // note at 480 ticks is half a beat in, and 240 ticks is a quarter of a beat.
        assert_eq!(region.notes[0].start, 0.0);
        assert_eq!(region.notes[0].pitch, 60);
        assert!(region.notes[0].len > 0.0, "a zero-length note still shows");
        assert_eq!(region.notes[1].start, 0.5);
        assert_eq!(region.notes[1].len, 0.25);
        assert_eq!(region.notes[1].pitch, 64);
        assert_eq!(region.notes[1].vel, 90);
    }

    #[test]
    fn a_region_whose_position_is_not_read_starts_at_bar_one_and_covers_its_notes() {
        let mut inst = strip("Inst 1", L::Instrument);
        // The notes end 12 beats after the region start, which is three bars.
        inst.midi_regions.push(midi_region(
            None,
            None,
            None,
            vec![
                note(59, 64, 960, 960),
                note(64, 64, 4 * u64::from(TPQ), 8 * u64::from(TPQ)),
            ],
        ));
        let mut p = empty();
        let report = map(&mut p, &project(vec![inst]));
        let region = &p.tracks[0].regions[0];
        assert_eq!(region.start_bar, 1.0);
        assert_eq!(region.len_bars, 3.0);
        assert_eq!(
            report
                .notes
                .iter()
                .filter(|n| n.contains("positions"))
                .count(),
            1,
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn a_region_is_placed_where_the_reader_says_or_at_bar_one() {
        let p = empty();
        // Four quarter notes in is bar 2 of 4/4, and a bar lasts four quarter notes.
        assert_eq!(region_bars(&p, 4.0, 4.0), (2.0, 1.0));
        assert_eq!(region_bars(&p, 0.0, 4.0), (1.0, 1.0));
        assert_eq!(region_bars(&p, 0.0, 0.0), (1.0, MIN_BARS as f32));
    }

    #[test]
    fn a_project_with_no_tick_resolution_is_refused() {
        assert!(check_resolution(0).is_err());
        assert!(check_resolution(TPQ).is_ok());
    }

    #[test]
    fn the_tempo_and_signature_are_taken_by_an_empty_project_only() {
        let mut full = project(vec![strip("Inst 1", L::Instrument)]);
        full.info.tempo = 96.5;
        full.info.time_signature = Some("3/4".into());
        let mut p = empty();
        let report = map(&mut p, &full);
        assert_eq!(p.tempo, 96.5);
        assert_eq!(p.sig, (3, 4));
        assert!(
            report.notes.iter().all(|n| !n.contains("tempo")),
            "{:?}",
            report.notes
        );

        let mut busy = Project::demo();
        let before = (busy.tempo, busy.sig);
        let report = map(&mut busy, &full);
        assert_eq!((busy.tempo, busy.sig), before);
        assert!(report.notes.iter().any(|n| n.contains("tempo")));
    }

    #[test]
    fn signatures_must_be_ones_pro_wave_has() {
        assert_eq!(parse_signature("7/8"), Some((7, 8)));
        assert_eq!(parse_signature(" 4 / 4 "), Some((4, 4)));
        assert_eq!(parse_signature("4/5"), None);
        assert_eq!(parse_signature("0/4"), None);
        assert_eq!(parse_signature("four"), None);
    }

    #[test]
    fn audio_regions_go_on_their_owner_track_and_unowned_files_on_one_track_each() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("Media");
        std::fs::create_dir_all(media.join("Audio Files")).unwrap();
        crate::project_io::tests::write_wav(&media.join("Audio Files").join("Take 1.wav"));
        crate::project_io::tests::write_wav(&media.join("Audio Files").join("Take 2.wav"));
        std::fs::write(dir.path().join("secret.wav"), b"not audio").unwrap();
        let t = u64::from(TPQ);
        // The wav holds 22 050 frames at 44 100 Hz: half a second, one beat at 120 BPM.
        let frames = Some(22_050);

        let mut audio_3 = strip("Audio 3", L::Audio);
        audio_3.audio_regions = vec![
            // Placed on bar 2 (four beats in), on its own track.
            audio_record(
                "Take 1_bip",
                Some("Audio Files/Take 1.wav"),
                Some(4 * t),
                frames,
            ),
            audio_record(
                "Missing_bip",
                Some("Audio Files/Missing.wav"),
                Some(0),
                frames,
            ),
            audio_record("Escape_bip", Some("../secret.wav"), Some(0), frames),
        ];
        let mut audio_4 = strip("Audio 4", L::Audio);
        // Not placed by the reader: it goes to the start of the project.
        audio_4.audio_regions = vec![audio_record(
            "Take 1_bip.2",
            Some("Audio Files/Take 1.wav"),
            None,
            frames,
        )];
        let mut full = project(vec![strip("Inst 1", L::Instrument), audio_3, audio_4]);
        full.audio_regions = vec![
            audio_record(
                "Take 1_bip",
                Some("Audio Files/Take 1.wav"),
                Some(4 * t),
                frames,
            ),
            audio_record("Take 1_bip.2", Some("Audio Files/Take 1.wav"), None, frames),
            audio_record(
                "Missing_bip",
                Some("Audio Files/Missing.wav"),
                Some(0),
                frames,
            ),
            audio_record("Escape_bip", Some("../secret.wav"), Some(0), frames),
            audio_record("Nameless_bip", None, Some(0), frames),
            // A file that no imported track plays gets a track of its own, once.
            audio_record(
                "Take 2_bip",
                Some("Audio Files/Take 2.wav"),
                Some(0),
                frames,
            ),
            audio_record(
                "Take 2_bip.1",
                Some("Audio Files/Take 2.wav"),
                Some(t),
                frames,
            ),
            // Not placed and not on a track: left out.
            audio_record("Take 2_bip.2", Some("Audio Files/Take 2.wav"), None, frames),
        ];
        let mut p = empty();
        let report = map_into(
            &mut p,
            &full,
            Some(&media),
            &egui::Context::default(),
            &nothing,
        );
        let placed: Vec<(&str, &str, f32)> = p
            .tracks
            .iter()
            .flat_map(|track| {
                track
                    .regions
                    .iter()
                    .map(move |r| (track.name.as_str(), r.name.as_str(), r.start_bar))
            })
            .collect();
        assert_eq!(
            placed,
            [
                ("Audio 3", "Take 1_bip", 2.0),
                ("Audio 4", "Take 1_bip.2", 1.0),
                ("Take 2", "Take 2_bip", 1.0),
            ],
            "{:?}",
            report.notes
        );
        assert_eq!(report.tracks, 4);
        let audio_3_region = &p.tracks[1].regions[0];
        assert!(
            (audio_3_region.len_bars - 0.25).abs() < 1e-3,
            "{}",
            audio_3_region.len_bars
        );
        let count = |word: &str| report.notes.iter().filter(|n| n.contains(word)).count();
        assert_eq!(count("not in Media"), 2, "{:?}", report.notes);
        assert_eq!(count("names no file"), 1, "{:?}", report.notes);
        assert_eq!(count("is not placed"), 1, "{:?}", report.notes);
        assert_eq!(count("plays on no track"), 1, "{:?}", report.notes);
        assert_eq!(
            count("positions"),
            1,
            "only the unplaced Audio 4 region: {:?}",
            report.notes
        );
    }

    #[test]
    fn a_relative_path_inside_the_media_folder_is_the_only_one_taken() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.wav"), b"x").unwrap();
        assert!(safe_media_file(dir.path(), "a.wav").is_some());
        assert!(safe_media_file(dir.path(), "../a.wav").is_none());
        assert!(safe_media_file(dir.path(), "/etc/hosts").is_none());
        assert!(safe_media_file(dir.path(), "").is_none());
        assert!(safe_media_file(dir.path(), "nope.wav").is_none());
    }

    #[test]
    fn a_project_of_masters_and_busses_has_nothing_to_import() {
        let full = project(vec![strip("Master", L::Master), strip("Bus 1", L::Bus)]);
        assert!(!full.tracks.iter().any(importable));
    }

    #[test]
    fn only_a_logicx_extension_takes_the_logic_pro_route() {
        assert!(!is_bundle(Path::new("Song.band")));
        assert!(is_bundle(Path::new("/Music/Song.LOGICX")));
    }

    #[test]
    #[ignore = "needs the project at /Users/maximerandazzo/Downloads/Projet.logicx"]
    fn the_real_project_keeps_its_notes_audio_plug_in_and_settings() {
        let path = Path::new("/Users/maximerandazzo/Downloads/Projet.logicx");
        let ctx = egui::Context::default();
        let (p, report) = new_project(path, &ctx).unwrap_or_else(|e| panic!("{e}"));
        // Checks the imported tracks and returns the plug-in settings of Inst 1.
        let check = |p: &Project| -> Vec<u8> {
            let inst = p
                .tracks
                .iter()
                .find(|t| t.name == "Inst 1")
                .unwrap_or_else(|| panic!("Inst 1 is imported: {report:?}"));
            assert_eq!(inst.regions.len(), 1, "one MIDI region");
            assert_eq!(inst.regions[0].start_bar, 1.0, "the region starts at bar 1");
            let notes: Vec<(u8, u8, f32, f32)> = inst.regions[0]
                .notes
                .iter()
                .map(|n| (n.pitch, n.vel, n.start, n.len))
                .collect();
            assert_eq!(
                notes,
                [
                    (59, 80, 0.25, 0.25),
                    (62, 80, 0.5, 0.25),
                    (64, 63, 1.0, 2.0),
                ],
                "(pitch, velocity, start and length in beats)"
            );
            let hosted = inst
                .inserts
                .iter()
                .find(|i| i.plugin != 0)
                .unwrap_or_else(|| panic!("a hosted plug-in on Inst 1: {report:?}"));
            assert_eq!(plugins::name(hosted.plugin), "Pro-Q 4");
            let state = plugins::state(hosted.plugin).unwrap_or_default();
            assert!(!state.is_empty(), "the saved settings");
            let audio_3 = p
                .tracks
                .iter()
                .find(|t| t.name == "Audio 3")
                .unwrap_or_else(|| panic!("Audio 3 is imported: {report:?}"));
            let placed: Vec<(&str, f32)> = audio_3
                .regions
                .iter()
                .map(|r| (r.name.as_str(), r.start_bar))
                .collect();
            assert_eq!(placed, [("Inst 1_bip", 2.0)], "the bounce, one bar in");
            state
        };
        let state = check(&p);

        let dir = tempfile::tempdir().unwrap();
        let saved = crate::project_io::save_project(&p, &dir.path().join("Projet.prowave"))
            .unwrap_or_else(|e| panic!("{e}"));
        let (again, _) =
            crate::project_io::open_project(&saved, &ctx).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(check(&again), state, "the same after saving and reloading");
    }
}
