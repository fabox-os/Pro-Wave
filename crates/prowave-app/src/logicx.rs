//! Importing a Logic Pro project (`.logicx`): its channel strips become tracks, its Audio Units
//! become hosted plug-ins with the settings the project saved, and its MIDI and audio regions
//! are placed where they were.
//!
//! File > Import > Other Projects… adds the project's tracks to the open project, and File >
//! Open or a dropped bundle makes a new project from it. The reading is done by
//! `prowave-logicx-import`; this module only maps what the reader gives onto the app's model.
//! Whatever the reader cannot decode yet is an empty field, so it is left out here and picked up
//! as soon as the reader fills it in.
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

/// Ticks per quarter note in the reader's MIDI records (`prowave-logicx-import` gives its
/// positions at 960 pulses per quarter note).
const TICKS_PER_BEAT: f64 = 960.0;

/// The shortest note or region, in quarter notes, so that a zero-length one still shows.
const MIN_BEATS: f64 = 1.0 / TICKS_PER_BEAT;

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
    if !full.tracks.iter().any(importable) {
        return Err(tr("the Logic Pro project has no track Pro Wave can import").to_string());
    }
    Ok(full)
}

/// The `Media/Audio Files` folder of the bundle `path` names (the bundle itself, or the one
/// bundle inside a folder). `None` when there is no bundle to look in.
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
    Some(bundle.join("Media").join("Audio Files"))
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
/// plug-ins and regions, appended to `p`.
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
        if let Some(db) = strip.volume_db.filter(|v| v.is_finite()) {
            track.volume_db = db.clamp(-60.0, 6.0);
        }
        if let Some(pan) = strip.pan.filter(|v| v.is_finite()) {
            track.pan = pan.clamp(-1.0, 1.0);
        }
        track.mute = strip.mute.unwrap_or(false);
        track.solo = strip.solo.unwrap_or(false);
        track.inserts = inserts_of(strip, &mut report, lookup);
        if !strip.instrument_files.is_empty() {
            report.notes.push(trf(
                "{}: the sampler's instrument files ({}) are not carried over",
                &[&strip.name, &strip.instrument_files.join(", ")],
            ));
        }
        if kind == TrackKind::Instrument {
            track.regions = midi_regions(p, strip);
        } else if kind == TrackKind::Audio {
            track.regions = audio_regions(p, strip, media, ctx, &mut report);
        }
        p.tracks.push(track);
        report.tracks += 1;
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
fn inserts_of(strip: &logic::Track, report: &mut Report, lookup: Lookup<'_>) -> Vec<Insert> {
    let mut out = Vec::new();
    for slot in &strip.plugins {
        let bypass = slot.bypass.unwrap_or(false);
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
                let state = slot.state.clone().filter(|s| !s.is_empty());
                if state.is_some() {
                    report.with_settings += 1;
                }
                let mut insert = Insert::hosted(plugins::add(&info, state));
                insert.bypass = bypass;
                out.push(insert);
            }
            logic::PluginFormat::Stock => {
                if let Some(kind) = FxKind::from_name(&slot.name) {
                    let mut insert = Insert::new(kind);
                    insert.bypass = bypass;
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

/// The MIDI regions of a strip as Pro Wave regions. A region's start and length are in ticks
/// from the start of the project; a note's start is from the start of its region, as the
/// reader's notes are given.
fn midi_regions(p: &Project, strip: &logic::Track) -> Vec<Region> {
    strip
        .midi_regions
        .iter()
        .map(|region| {
            let (start_bar, len_bars) = bar_span(p, region.start_tick, region.length_ticks);
            let mut notes: Vec<Note> = region
                .notes
                .iter()
                .map(|n| Note {
                    start: (n.start_tick as f64 / TICKS_PER_BEAT) as f32,
                    len: (n.length_ticks as f64 / TICKS_PER_BEAT).max(MIN_BEATS) as f32,
                    pitch: n.pitch.min(127),
                    vel: n.velocity.clamp(1, 127),
                })
                .collect();
            notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
            Region {
                name: region.name.clone().unwrap_or_else(|| strip.name.clone()),
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

/// The audio regions of a strip. A region plays a file from the project's `Media/Audio Files`
/// folder; one whose file is not there is left out, with a note.
fn audio_regions(
    p: &Project,
    strip: &logic::Track,
    media: Option<&Path>,
    ctx: &egui::Context,
    report: &mut Report,
) -> Vec<Region> {
    let mut out = Vec::new();
    for region in &strip.audio_regions {
        let file = media.and_then(|dir| safe_media_file(dir, &region.file));
        let Some(file) = file else {
            report.notes.push(trf(
                "{}: the audio file {} is not in Media/Audio Files, so its region is skipped",
                &[&strip.name, &region.file],
            ));
            continue;
        };
        let audio = match AudioFile::open(&file) {
            Ok(audio) => audio,
            Err(e) => {
                report.notes.push(trf(
                    "{}: cannot read {}: {}",
                    &[&strip.name, &region.file, &e],
                ));
                continue;
            }
        };
        let (start_bar, len_bars) = bar_span(p, region.start_tick, region.length_ticks);
        let name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&strip.name)
            .to_string();
        out.push(Region {
            name,
            start_bar,
            len_bars,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            audio: Some(crate::import::audio_region(audio, ctx)),
            params: RegionParams::default(),
            generated: None,
        });
    }
    out
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

/// The bar position (from 1) and length in bars of a span given in ticks, at the project's
/// tempo and signature.
fn bar_span(p: &Project, start_tick: u64, length_ticks: u64) -> (f32, f32) {
    let start = start_tick as f64 / TICKS_PER_BEAT;
    let end = start + length_ticks as f64 / TICKS_PER_BEAT;
    let first = p.beats_to_bar(start);
    let last = p.beats_to_bar(end);
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

    /// The bytes of an Audio Unit class-info plist, to check they come back unchanged.
    const STATE: &[u8] =
        b"<?xml version=\"1.0\"?><plist><dict><key>data</key><data>AQI=</data></dict></plist>";

    fn strip(name: &str, kind: L) -> logic::Track {
        logic::Track {
            name: name.into(),
            kind,
            active: true,
            volume_db: None,
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
        let report = map_into(&mut p, &full, None, &egui::Context::default(), &nothing);
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
    fn levels_and_flags_are_mapped_when_the_reader_knows_them() {
        let mut known = strip("Audio 1", L::Audio);
        known.volume_db = Some(-6.5);
        known.pan = Some(2.0);
        known.mute = Some(true);
        known.solo = Some(false);
        let unknown = strip("Audio 2", L::Audio);
        let mut p = empty();
        map_into(
            &mut p,
            &project(vec![known, unknown]),
            None,
            &egui::Context::default(),
            &nothing,
        );
        assert_eq!(p.tracks[0].volume_db, -6.5);
        assert_eq!(p.tracks[0].pan, 1.0, "pan is held in -1..1");
        assert!(p.tracks[0].mute);
        assert!(!p.tracks[0].solo);
        assert_eq!((p.tracks[1].volume_db, p.tracks[1].pan), (0.0, 0.0));
        assert!(!p.tracks[1].mute);
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
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
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
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
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
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
        assert!(p.tracks[0].inserts.is_empty());
        assert!(report.missing.is_empty());
        assert_eq!(report.notes.len(), 1);
        assert!(report.notes[0].contains("Alchemy"), "{:?}", report.notes);
    }

    #[test]
    fn a_plug_in_in_an_unknown_format_is_listed_not_guessed() {
        let mut inst = strip("Inst 1", L::Instrument);
        let mut slot = stock("Mystery");
        slot.format = PluginFormat::Unknown;
        inst.plugins.push(slot);
        let mut p = empty();
        let report = map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
        assert!(p.tracks[0].inserts.is_empty());
        assert!(report.notes[0].contains("Mystery"));
    }

    #[test]
    fn ticks_become_bars_and_notes_become_beats() {
        let mut inst = strip("Inst 1", L::Instrument);
        inst.midi_regions.push(MidiRegion {
            name: Some("Lead".into()),
            start_tick: 3840,
            length_ticks: 3840,
            notes: vec![
                MidiNote {
                    pitch: 64,
                    velocity: 90,
                    start_tick: 960,
                    length_ticks: 480,
                },
                MidiNote {
                    pitch: 60,
                    velocity: 100,
                    start_tick: 480,
                    length_ticks: 0,
                },
            ],
        });
        let mut p = empty();
        map_into(
            &mut p,
            &project(vec![inst]),
            None,
            &egui::Context::default(),
            &nothing,
        );
        let region = &p.tracks[0].regions[0];
        assert_eq!(region.name, "Lead");
        assert_eq!(
            region.start_bar, 2.0,
            "4 quarter notes is the start of bar 2 in 4/4"
        );
        assert_eq!(region.len_bars, 1.0, "4 quarter notes is one bar");
        // Notes come out sorted by their start, each one in beats from the region's start.
        assert_eq!(region.notes[0].start, 0.5);
        assert_eq!(region.notes[0].pitch, 60);
        assert!(region.notes[0].len > 0.0, "a zero-length note still shows");
        assert_eq!(region.notes[1].start, 1.0);
        assert_eq!(region.notes[1].len, 0.5);
        assert_eq!(region.notes[1].pitch, 64);
        assert_eq!(region.notes[1].vel, 90);
    }

    #[test]
    fn the_tempo_and_signature_are_taken_by_an_empty_project_only() {
        let mut full = project(vec![strip("Inst 1", L::Instrument)]);
        full.info.tempo = 96.5;
        full.info.time_signature = Some("3/4".into());
        let mut p = empty();
        let report = map_into(&mut p, &full, None, &egui::Context::default(), &nothing);
        assert_eq!(p.tempo, 96.5);
        assert_eq!(p.sig, (3, 4));
        assert!(report.notes.is_empty());

        let mut busy = Project::demo();
        let before = (busy.tempo, busy.sig);
        let report = map_into(&mut busy, &full, None, &egui::Context::default(), &nothing);
        assert_eq!((busy.tempo, busy.sig), before);
        assert_eq!(report.notes.len(), 1);
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
    fn audio_regions_play_files_in_the_media_folder_only() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("Media").join("Audio Files");
        std::fs::create_dir_all(&media).unwrap();
        crate::project_io::tests::write_wav(&media.join("Take 1.wav"));
        std::fs::write(dir.path().join("secret.wav"), b"not audio").unwrap();

        let mut audio = strip("Audio 1", L::Audio);
        audio.audio_regions = vec![
            AudioRegion {
                file: "Take 1.wav".into(),
                start_tick: 1920,
                length_ticks: 3840,
            },
            AudioRegion {
                file: "Missing.wav".into(),
                start_tick: 0,
                length_ticks: 960,
            },
            AudioRegion {
                file: "../secret.wav".into(),
                start_tick: 0,
                length_ticks: 960,
            },
        ];
        let mut p = empty();
        let report = map_into(
            &mut p,
            &project(vec![audio]),
            Some(&media),
            &egui::Context::default(),
            &nothing,
        );
        let regions = &p.tracks[0].regions;
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "Take 1");
        assert!(regions[0].audio.is_some());
        assert_eq!(regions[0].start_bar, 1.5);
        assert_eq!(regions[0].len_bars, 1.0);
        assert_eq!(report.notes.len(), 2, "{:?}", report.notes);
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

    /// The real project the format was checked against: three strips, the first with a Sampler,
    /// Sample Delay and an Audio Unit (FabFilter Pro-Q 4) with its settings. Run it with
    /// `cargo test -p prowave-app -- --ignored`.
    #[test]
    #[ignore = "needs the project at /Users/maximerandazzo/Downloads/Projet.logicx"]
    fn the_real_project_keeps_its_plug_in_and_settings() {
        let path = Path::new("/Users/maximerandazzo/Downloads/Projet.logicx");
        let ctx = egui::Context::default();
        let (p, report) = new_project(path, &ctx).unwrap_or_else(|e| panic!("{e}"));
        // The reader also marks Audio 3 as active, so the check is that these three are there.
        let names: Vec<&str> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        for wanted in ["Inst 1", "Audio 1", "Audio 2"] {
            assert!(names.contains(&wanted), "{wanted} in {names:?}: {report:?}");
        }
        let inst = |p: &Project| {
            p.tracks
                .iter()
                .find(|t| t.name == "Inst 1")
                .map(|t| t.inserts.clone())
                .unwrap_or_default()
        };
        let hosted = inst(&p)
            .into_iter()
            .find(|i| i.plugin != 0)
            .unwrap_or_else(|| panic!("a hosted plug-in on Inst 1: {report:?}"));
        assert_eq!(plugins::name(hosted.plugin), "Pro-Q 4");
        let state = plugins::state(hosted.plugin).unwrap_or_default();
        assert!(!state.is_empty(), "the saved settings");

        let dir = tempfile::tempdir().unwrap();
        let saved = crate::project_io::save_project(&p, &dir.path().join("Projet.prowave"))
            .unwrap_or_else(|e| panic!("{e}"));
        let (again, _) =
            crate::project_io::open_project(&saved, &ctx).unwrap_or_else(|e| panic!("{e}"));
        let kept = inst(&again)
            .into_iter()
            .find(|i| i.plugin != 0)
            .unwrap_or_else(|| panic!("the plug-in after saving"));
        assert_eq!(plugins::name(kept.plugin), "Pro-Q 4");
        assert_eq!(plugins::state(kept.plugin), Some(state));
    }
}
