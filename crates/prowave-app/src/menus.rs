//! Top menu bar with the same menus and entries as Logic Pro. Each entry is drawn like a macOS
//! menu item: check-mark column, title, key command on the right; entries that do not apply
//! now, or are not implemented yet, are greyed out (see [`enabled`]). The Smart Controls button in the control bar switches the bar and its
//! menus between the dark look and the light (white) macOS look (`UiState::light_menus`).
use crate::file_commands::FileCmd;
use crate::i18n::{self, tr, trf, Lang};
use crate::model::{Project, TrackKind};
use crate::undo::UndoCmd;
use crate::Bottom;
use egui::{pos2, vec2, Align2, Color32, FontId, Key, KeyboardShortcut, Modifiers, Sense, Stroke};

const FILE: &[&str] = &[
    "New…",
    "New from Template…",
    "Open…",
    "Open Recent",
    "-",
    "Close",
    "Close Project",
    "Save",
    "Save As…",
    "Save A Copy As…",
    "Save as Template…",
    "Revert to",
    "-",
    "Alternatives",
    "Project Management",
    "Project Settings",
    "-",
    "Page Setup…",
    "Print",
    "-",
    "Movie",
    "-",
    "Import",
    "Export",
    "Bounce",
    "Share",
    "-",
    "Show in File Manager",
    "Open in Sound Library Manager",
    "-",
    "Project Alternatives…",
];

/// Entries that open a submenu (▸), as in Logic, and what is in each. "Open Recent" is filled
/// with the remembered projects instead.
const SUBMENUS: &[(&str, &[&str])] = &[
    ("Revert to", &["Revert to Saved"]),
    (
        "Alternatives",
        &[
            "New Alternative…",
            "Rename Alternative…",
            "Delete Alternative",
        ],
    ),
    (
        "Project Management",
        &["Clean Up…", "Consolidate…", "Rename Project…"],
    ),
    (
        "Project Settings",
        &[
            "General…",
            "Synchronization…",
            "Metronome…",
            "Recording…",
            "Tuning…",
            "Audio…",
            "MIDI…",
        ],
    ),
    ("Movie", &["Open Movie…", "Remove Movie"]),
    ("Import", &["Audio File…", "MIDI File…", "Other Projects…"]),
    (
        "Export",
        &[
            "Selection as MIDI File…",
            "Track as Audio File…",
            "All Tracks as Audio Files…",
            "Score as MusicXML…",
        ],
    ),
    (
        "Bounce",
        &[
            "Project or Section…",
            "Regions in Place…",
            "Tracks in Place…",
        ],
    ),
    ("Share", &["Song to File…", "Song to Mobile Device…"]),
    ("Language", LANGUAGE),
];

/// What is in the submenu opened by `label`, if it opens one.
pub(crate) fn submenu(label: &str) -> Option<&'static [&'static str]> {
    SUBMENUS
        .iter()
        .find(|(title, _)| *title == label)
        .map(|(_, items)| *items)
}

/// Every entry that runs something, submenus included (not separators or submenu titles).
#[cfg(test)]
pub(crate) fn all_entries() -> Vec<&'static str> {
    let mut out = Vec::new();
    for (_, items) in MENUS {
        for &it in items {
            match submenu(it) {
                Some(children) => out.extend_from_slice(children),
                None => out.push(it),
            }
        }
    }
    out.retain(|it| *it != "-" && *it != "Open Recent");
    out
}
const EDIT: &[&str] = &[
    "Undo",
    "Redo",
    "Undo History…",
    "Delete Undo History",
    "-",
    "Cut",
    "Copy",
    "Paste",
    "Paste Replace",
    "Delete",
    "-",
    "Select All",
    "Deselect All",
    "-",
    "Split at Playhead",
    "Join",
    "Bounce and Join",
];
const TRACK: &[&str] = &[
    "New",
    "New Aux Track",
    "New VCA Track",
    "New External MIDI Track",
    "New Pattern Region",
    "New Drummer Region",
    "MIDI FX…",
    "Rename Track",
    "Delete Track",
    "-",
    "Create Track Stack…",
    "Release Track Stack",
    "Show Master Track",
    "Show Tempo Track",
    "Show Chord Track",
    "Show Marker Track",
    "Show Signature Track",
    "Show Arrangement Track",
    "-",
    "Assign Track Color…",
    "Assign Track Icon…",
    "Configure Track Header…",
];
const NAVIGATE: &[&str] = &[
    "Go to Beginning",
    "Go to End",
    "Go to Position…",
    "-",
    "Go to Left Locator",
    "Go to Right Locator",
    "-",
    "Go to Next Marker",
    "Go to Previous Marker",
    "Create Marker",
    "Delete Marker",
    "-",
    "Go to Next Region",
    "Go to Previous Region",
    "-",
    "Zoom to Fit",
    "Zoom In",
    "Zoom Out",
];
const RECORD: &[&str] = &[
    "Record",
    "Record / Record Repeat",
    "Record Toggle",
    "Record Enable",
    "-",
    "Capture Recording",
    "-",
    "New Take",
    "Delete Take",
    "Bounce In Place",
    "-",
    "Record Project Settings",
    "Project Settings",
];
const MIX: &[&str] = &[
    "Show Automation",
    "Create Automation",
    "Convert Automation",
    "Delete Automation",
    "-",
    "Group",
    "Edit Groups…",
    "-",
    "Pre-Fader Metering",
    "Mastering",
    "Create Mastering Track",
    "Stem Splitter…",
    "-",
    "Bounce",
    "Export",
    "Import",
];
/// Logic Pro 11's View menu.
const VIEW: &[&str] = &[
    "Hide All Automation",
    "Show Automation",
    "Create Track Automation",
    "Convert Automation",
    "-",
    "Show Region Automation",
    "Show Track Automation",
    "-",
    "Show Advanced Tools",
    "Show Smart Controls",
    "-",
    "Show Piano Roll",
    "Show Score Editor",
    "Show Step Sequencer",
    "Show Session Player",
    "Show Flex Editor",
    "-",
    "Show Mixer",
    "Show Library",
    "Show List Editors",
    "Show Quick Help",
    "Show Inspector",
    "Show Performers",
    "Show Project Notes",
    "Show Movie",
    "Show Media Browser",
    "-",
    "Show Toolbar",
    "Customize Toolbar…",
    "-",
    "Show Track Header",
    "Show Ruler",
    "Show Global Tracks",
    "Show Colors",
    "Show Markers",
    "Show Grid",
    "Show Smart Tempo",
    "-",
    "Bigger Interface",
    "Smaller Interface",
    "Actual Interface Size",
    "-",
    "Enter Full Screen",
];
const WINDOW: &[&str] = &[
    "Minimize",
    "Zoom",
    "-",
    "Score Editor",
    "Step Sequencer",
    "Smart Tempo",
    "Piano Roll",
    "Mixer",
    "Plugin Manager",
    "Video",
    "-",
    "Bring All to Front",
];
const HELP: &[&str] = &[
    "Quick Help",
    "Logic-style Key Commands…",
    "-",
    "Language",
    "-",
    "Check for Updates…",
    "Check for Updates Automatically",
    "-",
    "About Pro Wave",
];

/// Help > Language: every language, each named in its own language.
const LANGUAGE: &[&str] = &[
    "English (Default)",
    "简体中文",
    "日本語",
    "Français",
    "Deutsch",
    "Español",
    "Português (Portugal)",
    "Português (Brasil)",
    "Filipino",
    "Bahasa Indonesia",
    "हिन्दी",
    "Українська",
    "Norsk",
    "Svenska",
    "Suomi",
    "Polski",
    "Lietuvių",
];

/// The menus in bar order, used by the in-window bar and the macOS menu bar.
pub(crate) const MENUS: [(&str, &[&str]); 9] = [
    ("File", FILE),
    ("Edit", EDIT),
    ("Track", TRACK),
    ("Navigate", NAVIGATE),
    ("Record", RECORD),
    ("Mix", MIX),
    ("View", VIEW),
    ("Window", WINDOW),
    ("Help", HELP),
];

/// Entries that can show a check mark (see [`checked`]).
pub(crate) const CHECKABLE: &[&str] = &[
    "Show Tempo Track",
    "Show Marker Track",
    "Show Signature Track",
    "Show Piano Roll",
    "Show Mixer",
    "Show Library",
    "Show Inspector",
    "Show List Editors",
    "Show Quick Help",
    "Show Project Notes",
    "Show Media Browser",
    "Check for Updates Automatically",
    "Show Toolbar",
    "Show Track Header",
    "Show Ruler",
    "Show Colors",
    "Show Grid",
];

/// Whether `label` shows a check mark when it is on: [`CHECKABLE`] and the Language entries.
pub(crate) fn is_checkable(label: &str) -> bool {
    CHECKABLE.contains(&label) || Lang::from_native_name(label).is_some()
}

/// Whether the menu bar is drawn inside the window. On macOS the menus go in the system menu
/// bar at the top of the screen instead (`native_menu`).
pub const IN_WINDOW: bool = !cfg!(target_os = "macos");

/// Colours of one menu look.
struct Look {
    bar: Color32,
    bar_text: Color32,
    menu: Color32,
    text: Color32,
    dim: Color32,
    hover: Color32,
    hover_text: Color32,
    separator: Color32,
}

const DARK: Look = Look {
    bar: Color32::from_rgb(0x3a, 0x3a, 0x3c),
    bar_text: Color32::from_rgb(0xdc, 0xdc, 0xdc),
    menu: Color32::from_rgb(0x4a, 0x4a, 0x4c),
    text: Color32::from_rgb(0xe6, 0xe6, 0xe6),
    dim: Color32::from_rgb(0x8a, 0x8a, 0x8c),
    hover: Color32::from_rgb(0x2f, 0x6f, 0xd8),
    hover_text: Color32::WHITE,
    separator: Color32::from_rgb(0x5e, 0x5e, 0x60),
};

/// macOS light menu bar and menus, as in Logic with the system in light mode.
const LIGHT: Look = Look {
    bar: Color32::from_rgb(0xf5, 0xf5, 0xf5),
    bar_text: Color32::from_rgb(0x1d, 0x1d, 0x1f),
    menu: Color32::from_rgb(0xec, 0xec, 0xec),
    text: Color32::from_rgb(0x1d, 0x1d, 0x1f),
    dim: Color32::from_rgb(0xa8, 0xa8, 0xaa),
    hover: Color32::from_rgb(0x2f, 0x7b, 0xf6),
    hover_text: Color32::WHITE,
    separator: Color32::from_rgb(0xd2, 0xd2, 0xd4),
};

fn look(light: bool) -> &'static Look {
    if light {
        &LIGHT
    } else {
        &DARK
    }
}

/// Fill for the panel behind the menu bar.
pub fn bar_fill(light: bool) -> Color32 {
    look(light).bar
}

/// Logic's default key commands for the entries this app implements.
fn shortcut(label: &str) -> Option<KeyboardShortcut> {
    if let Some(cmd) = FileCmd::from_menu(label) {
        return cmd.shortcut();
    }
    match label {
        "Undo" => return Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
        "Redo" => {
            return Some(KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Z,
            ))
        }
        "Delete" => return Some(KeyboardShortcut::new(Modifiers::NONE, Key::Delete)),
        _ => {}
    }
    let interface_key = match label {
        "Bigger Interface" => Some(Key::Equals),
        "Smaller Interface" => Some(Key::Minus),
        "Actual Interface Size" => Some(Key::Num0),
        _ => None,
    };
    if let Some(key) = interface_key {
        return Some(KeyboardShortcut::new(Modifiers::COMMAND, key));
    }
    let key = match label {
        "Show Library" => Key::Y,
        "Show Inspector" => Key::I,
        "Show List Editors" => Key::D,
        "Show Mixer" => Key::X,
        _ => return None,
    };
    Some(KeyboardShortcut::new(Modifiers::NONE, key))
}

/// Whether `label` can be chosen now. Entries that are not implemented yet are greyed out,
/// so a click never silently does nothing; others are greyed while there is nothing to act on.
pub(crate) fn enabled(label: &str, p: &Project, s: &crate::UiState) -> bool {
    if FileCmd::from_menu(label).is_some() || is_checkable(label) && !CHECKABLE.contains(&label) {
        return true;
    }
    match label {
        "Undo" => s.can_undo,
        "Redo" => s.can_redo,
        "Delete Undo History" => s.can_undo || s.can_redo,
        "Delete" | "Deselect All" => s.tracks.selected.is_some(),
        "Delete Track" => p.tracks.len() > 1,
        "Release Track Stack" => p
            .tracks
            .get(p.selected_track)
            .is_some_and(|t| t.kind == TrackKind::Folder),
        "Go to Next Marker" => p.next_marker(p.position_beats).is_some(),
        "Go to Previous Marker" => p.previous_marker(p.position_beats).is_some(),
        "Delete Marker" => p.marker_at(p.position_beats).is_some(),
        // Pattern and Drummer regions go on a track that plays MIDI.
        "New Pattern Region" | "New Drummer Region" => p
            .tracks
            .get(p.selected_track)
            .is_some_and(|t| t.kind.has_regions() && t.kind != TrackKind::Audio),
        "Go to Next Region" | "Go to Previous Region" => {
            p.tracks.iter().any(|t| !t.regions.is_empty())
        }
        "New from Template…"
        | "New"
        | "New Aux Track"
        | "New VCA Track"
        | "New External MIDI Track"
        | "Create Track Stack…"
        | "Go to Beginning"
        | "Go to End"
        | "Go to Left Locator"
        | "Go to Right Locator"
        | "Zoom to Fit"
        | "Zoom In"
        | "Zoom Out"
        | "Record"
        | "Capture Recording"
        | "Quick Help"
        | "Show Quick Help"
        | "Show Toolbar"
        | "Show Project Notes"
        | "Show Media Browser"
        | "Show Piano Roll"
        | "Show Mixer"
        | "Show Library"
        | "Show Inspector"
        | "Show List Editors"
        | "Check for Updates…"
        | "Check for Updates Automatically"
        | "Show Tempo Track"
        | "Show Marker Track"
        | "Show Signature Track"
        | "MIDI File…"
        | "MIDI FX…"
        | "Selection as MIDI File…"
        | "Create Marker"
        | "Mixer"
        | "Piano Roll"
        | "Bigger Interface"
        | "Smaller Interface"
        | "Actual Interface Size" => true,
        _ => false,
    }
}

/// Start of every region, in beats, sorted (for Go to Next / Previous Region).
fn region_starts(p: &Project) -> Vec<f64> {
    let mut starts: Vec<f64> = p
        .tracks
        .iter()
        .flat_map(|t| t.regions.iter())
        .map(|r| p.region_start_beats(r))
        .collect();
    starts.sort_by(f64::total_cmp);
    starts
}

/// Whether `label` shows a check mark (what it shows is visible).
pub(crate) fn checked(label: &str, s: &crate::UiState) -> bool {
    match label {
        "Show Piano Roll" => s.bottom == Bottom::PianoRoll,
        "Show Mixer" => s.show_mixer && s.bottom == Bottom::Mixer,
        "Show Library" => s.show_library,
        "Show Inspector" => s.show_inspector,
        "Show List Editors" => s.show_lists,
        "Show Quick Help" => s.show_quick_help,
        "Show Toolbar" => s.show_toolbar,
        "Show Project Notes" => s.show_notes,
        "Show Media Browser" => s.browser.open == Some(crate::file_browser::Tab::Files),
        "Check for Updates Automatically" => s.update_auto,
        "Show Tempo Track" => s.tracks.show_tempo,
        "Show Marker Track" => s.tracks.show_markers,
        "Show Signature Track" => s.tracks.show_signatures,
        _ if Lang::from_native_name(label).is_some() => {
            Lang::from_native_name(label) == Some(i18n::current())
        }
        // Always visible in this app.
        "Show Track Header" | "Show Ruler" | "Show Colors" | "Show Grid" => true,
        _ => false,
    }
}

/// Track numbers shifted, so a remembered region or note selection may point at another track.
fn forget_region_selection(s: &mut crate::UiState) {
    s.tracks.selected = None;
    s.piano.selected.clear();
}

/// Runs a menu entry (File entries are queued in `UiState::file_cmd` for `App`).
pub(crate) fn apply(label: &str, p: &mut Project, s: &mut crate::UiState) {
    if let Some(lang) = Lang::from_native_name(label) {
        i18n::choose(lang);
        return;
    }
    match label {
        "Show Mixer" | "Mixer" => {
            s.show_mixer = !(s.show_mixer && s.bottom == Bottom::Mixer);
            s.bottom = Bottom::Mixer;
        }
        "Show Library" => s.show_library = !s.show_library,
        "Show Inspector" => s.show_inspector = !s.show_inspector,
        "Show List Editors" => s.show_lists = !s.show_lists,
        "Show Quick Help" | "Quick Help" => s.show_quick_help = !s.show_quick_help,
        "Show Toolbar" => s.show_toolbar = !s.show_toolbar,
        "Show Project Notes" => s.show_notes = !s.show_notes,
        "Show Media Browser" => s.browser.toggle(crate::file_browser::Tab::Files),
        "Capture Recording" => s.capture_cmd = true,
        "Check for Updates…" => s.update_check = true,
        "Check for Updates Automatically" => {
            s.update_auto = !s.update_auto;
            crate::updater::set_auto_enabled(s.update_auto);
        }
        "Show Tempo Track" => s.tracks.show_tempo = !s.tracks.show_tempo,
        "Show Marker Track" => s.tracks.show_markers = !s.tracks.show_markers,
        "Show Signature Track" => s.tracks.show_signatures = !s.tracks.show_signatures,
        // Logic: Create Marker puts it at the playhead, rounded to the nearest bar.
        "Create Marker" => {
            let bar = p.beats_to_bar(p.position_beats).round();
            p.add_marker(p.bar_to_beats(bar));
            s.tracks.show_markers = true;
        }
        "Delete Marker" => {
            if let Some(i) = p.marker_at(p.position_beats) {
                p.markers.remove(i);
            }
        }
        "Go to Next Marker" => {
            if let Some(b) = p.next_marker(p.position_beats) {
                p.position_beats = b;
            }
        }
        "Go to Previous Marker" => {
            if let Some(b) = p.previous_marker(p.position_beats) {
                p.position_beats = b;
            }
        }
        "Show Piano Roll" | "Piano Roll" => {
            s.bottom = if s.bottom == Bottom::PianoRoll {
                Bottom::Mixer
            } else {
                Bottom::PianoRoll
            }
        }
        "New from Template…" => s.chooser = Some(crate::start_screen::Chooser::templates()),
        // Track > New: a software instrument track, as the + button above the tracks adds.
        "New" => {
            p.add_track(TrackKind::Instrument);
        }
        "New VCA Track" => {
            p.add_track(TrackKind::Vca);
        }
        "New External MIDI Track" => {
            p.add_track(TrackKind::ExternalMidi);
        }
        "MIDI FX…" => s.show_midi_fx = !s.show_midi_fx,
        "MIDI File…" => s.midi_cmd = Some(crate::midi_file::MidiCmd::Import),
        "Selection as MIDI File…" => s.midi_cmd = Some(crate::midi_file::MidiCmd::Export),
        "New Pattern Region" | "New Drummer Region" => {
            // On the selected track, at the bar the playhead is in.
            let bar = p.beats_to_bar(p.position_beats).floor() as f32 + 1.0;
            let track = p.selected_track;
            if let Some(ri) = p.add_generated_region(track, label == "New Drummer Region", bar) {
                s.tracks.selected = Some((track, ri));
            }
        }
        "New Aux Track" => {
            p.add_track(TrackKind::Aux);
        }
        "Undo" => s.undo_cmd = Some(UndoCmd::Undo),
        "Redo" => s.undo_cmd = Some(UndoCmd::Redo),
        "Delete Undo History" => s.undo_cmd = Some(UndoCmd::Clear),
        "Delete" => crate::tracks_area::delete_selected(p, &mut s.tracks),
        "Deselect All" => s.tracks.selected = None,
        "Go to End" => {
            p.position_beats = p
                .tracks
                .iter()
                .flat_map(|t| t.regions.iter())
                .map(|r| p.region_end_beats(r))
                .fold(0.0, f64::max);
        }
        "Go to Left Locator" => p.position_beats = p.cycle_start,
        "Go to Right Locator" => p.position_beats = p.cycle_end,
        "Go to Next Region" => {
            let at = p.position_beats + 1e-6;
            if let Some(b) = region_starts(p).into_iter().find(|b| *b > at) {
                p.position_beats = b;
            }
        }
        "Go to Previous Region" => {
            let at = p.position_beats - 1e-6;
            if let Some(b) = region_starts(p).into_iter().rev().find(|b| *b < at) {
                p.position_beats = b;
            }
        }
        "Zoom In" => s.tracks.bar_w = (s.tracks.bar_w * 1.25).min(600.0),
        "Zoom Out" => s.tracks.bar_w = (s.tracks.bar_w / 1.25).max(6.0),
        "Zoom to Fit" => {
            let w = s.tracks.lanes_w;
            crate::tracks_area::zoom_to_fit(p, &mut s.tracks, w);
        }
        "Create Track Stack…" => {
            p.create_track_stack(p.selected_track);
            forget_region_selection(s);
        }
        "Bigger Interface" | "Smaller Interface" | "Actual Interface Size" => {
            let dir = match label {
                "Bigger Interface" => 1,
                "Smaller Interface" => -1,
                _ => 0,
            };
            s.ui_scale = crate::ui_scale::step(s.ui_scale, dir);
            crate::ui_scale::save(s.ui_scale);
        }
        "Delete Track" => {
            if p.delete_track(p.selected_track) {
                forget_region_selection(s);
            }
        }
        "Release Track Stack" => {
            if p.release_track_stack(p.selected_track) {
                forget_region_selection(s);
            }
        }
        "Go to Beginning" => p.position_beats = 0.0,
        "Record" => p.recording = !p.recording,
        _ => {
            if let Some(cmd) = FileCmd::from_menu(label) {
                s.file_cmd = Some(cmd);
            }
        }
    }
}

/// One menu entry that is always enabled (the Open Recent list).
/// Height of a menu entry and size of its text, in points (macOS menus).
const ROW_H: f32 = 19.0;
const FONT: f32 = 13.0;
/// Left column (check mark) and right padding of an entry.
const CHECK_W: f32 = 22.0;
const RIGHT_PAD: f32 = 10.0;
/// Gap between an entry's title and its key command or arrow.
const KEY_GAP: f32 = 28.0;

/// Width of a menu holding `labels` (and their key commands), so it is as wide as its longest
/// entry, like a macOS menu, instead of stretching across the window.
fn menu_width(ui: &egui::Ui, labels: &[String]) -> f32 {
    let font = FontId::proportional(FONT);
    let width = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    let (mut title, mut key) = (0.0f32, 0.0f32);
    for it in labels.iter().filter(|l| *l != "-") {
        title = title.max(width(tr(it)));
        if let Some(k) = shortcut(it) {
            key = key.max(width(&ui.ctx().format_shortcut(&k)));
        }
        if submenu(it).is_some() || it == "Open Recent" {
            key = key.max(8.0);
        }
    }
    let gap = if key > 0.0 { KEY_GAP } else { 12.0 };
    (CHECK_W + title + gap + key + RIGHT_PAD).max(120.0)
}

/// One entry: check-mark column, title, then the key command or a submenu arrow on the right.
fn row(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    check: bool,
    key: Option<String>,
    arrow: bool,
    look: &Look,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), sense);
    let hot = enabled && resp.hovered();
    let g = ui.painter();
    if hot {
        g.rect_filled(rect.shrink2(vec2(0.0, 1.0)), 4.0, look.hover);
    }
    let color = match (enabled, hot) {
        (false, _) => look.dim,
        (true, true) => look.hover_text,
        (true, false) => look.text,
    };
    let font = FontId::proportional(FONT);
    if check {
        g.text(
            pos2(rect.left() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            "✔",
            FontId::proportional(11.0),
            color,
        );
    }
    g.text(
        pos2(rect.left() + CHECK_W, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        color,
    );
    let right = pos2(rect.right() - RIGHT_PAD, rect.center().y);
    if arrow {
        let c = right + vec2(-3.0, 0.0);
        g.add(egui::Shape::convex_polygon(
            vec![
                c + vec2(-3.0, -4.0),
                c + vec2(3.0, 0.0),
                c + vec2(-3.0, 4.0),
            ],
            color,
            Stroke::NONE,
        ));
    } else if let Some(key) = key {
        let key_color = if hot { look.hover_text } else { look.dim };
        g.text(right, Align2::RIGHT_CENTER, key, font, key_color);
    }
    resp
}

fn separator(ui: &mut egui::Ui, look: &Look) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    let y = rect.center().y;
    ui.painter().line_segment(
        [pos2(rect.left() + 10.0, y), pos2(rect.right() - 10.0, y)],
        Stroke::new(1.0, look.separator),
    );
}

/// How a remembered project is listed: "Night Drive  (Music)", so equal names in different
/// folders can be told apart.
fn recent_label(path: &std::path::Path) -> String {
    let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("?");
    match path
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
    {
        Some(folder) => format!("{name}  ({folder})"),
        None => name.to_string(),
    }
}

/// The Open Recent submenu: the remembered projects, then Clear Menu.
fn recent_items(ui: &mut egui::Ui, s: &mut crate::UiState, look: &Look) {
    let labels: Vec<String> = s.recent.paths().iter().map(|p| recent_label(p)).collect();
    let mut sized = labels.clone();
    sized.push(tr("Clear Menu").into());
    ui.set_width(menu_width(ui, &sized));
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut picked = None;
    for (label, path) in labels.iter().zip(s.recent.paths()) {
        if row(ui, label, true, false, None, false, look).clicked() {
            picked = Some(path.clone());
        }
    }
    if !labels.is_empty() {
        separator(ui, look);
    }
    let clear = row(
        ui,
        tr("Clear Menu"),
        !labels.is_empty(),
        false,
        None,
        false,
        look,
    );
    if let Some(path) = picked {
        s.recent_pick = Some(path);
        s.file_cmd = Some(FileCmd::OpenRecent);
        ui.close();
    } else if clear.clicked() {
        s.recent.clear();
        ui.close();
    }
}

/// The entries of one menu or submenu.
fn entries(ui: &mut egui::Ui, items: &[&str], p: &mut Project, s: &mut crate::UiState) {
    let look = look(s.light_menus);
    let labels: Vec<String> = items.iter().map(|it| it.to_string()).collect();
    ui.set_width(menu_width(ui, &labels));
    ui.spacing_mut().item_spacing.y = 0.0;
    for &it in items {
        if it == "-" {
            separator(ui, look);
            continue;
        }
        if it == "Open Recent" {
            let resp = row(ui, tr(it), true, false, None, true, look);
            egui::containers::menu::SubMenu::new().show(ui, &resp, |ui| recent_items(ui, s, look));
            continue;
        }
        if let Some(children) = submenu(it) {
            let on = children.iter().any(|c| enabled(c, p, s));
            let resp = row(ui, tr(it), on, false, None, true, look);
            if on {
                egui::containers::menu::SubMenu::new()
                    .show(ui, &resp, |ui| entries(ui, children, p, s));
            }
            continue;
        }
        let key = shortcut(it).map(|k| ui.ctx().format_shortcut(&k));
        let (on, check) = (enabled(it, p, s), checked(it, s));
        if row(ui, tr(it), on, check, key, false, look).clicked() {
            apply(it, p, s);
            ui.close();
        }
    }
}

fn menu(ui: &mut egui::Ui, title: &str, items: &[&str], p: &mut Project, s: &mut crate::UiState) {
    let title = egui::RichText::new(tr(title)).color(look(s.light_menus).bar_text);
    ui.menu_button(title, |ui| entries(ui, items, p, s));
}

/// Style of the bar buttons and of the drop-down menus for the current look.
fn menu_style(light: bool) -> impl Fn(&mut egui::Style) + Send + Sync + 'static {
    move |style: &mut egui::Style| {
        egui::containers::menu::menu_style(style);
        let l = look(light);
        let hover_fill = if light {
            Color32::from_rgb(0xdc, 0xdc, 0xdc)
        } else {
            Color32::from_rgb(0x55, 0x55, 0x58)
        };
        let v = &mut style.visuals;
        v.window_fill = l.menu;
        v.window_stroke = Stroke::new(1.0, l.separator);
        v.menu_corner_radius = egui::CornerRadius::same(6);
        v.override_text_color = None;
        v.widgets.noninteractive.fg_stroke.color = l.text;
        v.widgets.inactive.fg_stroke.color = l.bar_text;
        v.widgets.hovered.weak_bg_fill = hover_fill;
        v.widgets.hovered.fg_stroke.color = l.bar_text;
        v.widgets.open.weak_bg_fill = hover_fill;
        v.widgets.open.fg_stroke.color = l.bar_text;
        v.widgets.active.fg_stroke.color = l.bar_text;
    }
}

/// Key commands of the View entries above (Y, I, X), unless a text field has focus.
pub fn handle_keys(ctx: &egui::Context, p: &mut Project, s: &mut crate::UiState) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    for label in ["Show Library", "Show Inspector", "Show Mixer"] {
        if let Some(k) = shortcut(label) {
            if ctx.input_mut(|i| i.consume_shortcut(&k)) {
                apply(label, p, s);
            }
        }
    }
    // Redo first: matching Cmd+Z ignores an extra Shift. Ctrl+Y is Redo on Windows and Linux.
    let redo = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    let redo_y = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
    let undo = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
    if ctx.input_mut(|i| i.consume_shortcut(&redo))
        || (!cfg!(target_os = "macos") && ctx.input_mut(|i| i.consume_shortcut(&redo_y)))
    {
        s.undo_cmd = Some(UndoCmd::Redo);
    } else if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
        s.undo_cmd = Some(UndoCmd::Undo);
    }
    // Cmd/Ctrl + = also comes as Plus on keyboards where = needs no Shift.
    let bigger = KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus);
    if ctx.input_mut(|i| i.consume_shortcut(&bigger)) {
        apply("Bigger Interface", p, s);
    }
    for label in [
        "Bigger Interface",
        "Smaller Interface",
        "Actual Interface Size",
    ] {
        if let Some(k) = shortcut(label) {
            if ctx.input_mut(|i| i.consume_shortcut(&k)) {
                apply(label, p, s);
            }
        }
    }
}

/// The menu bar. Returns where its contents end, so the rest of the row can be used (on Linux
/// it holds the window buttons and moves the window, see `window_chrome`).
pub fn menu_bar(ui: &mut egui::Ui, p: &mut Project, s: &mut crate::UiState) -> f32 {
    let light = s.light_menus;
    let bar = egui::MenuBar::new()
        .style(menu_style(light))
        .config(egui::containers::menu::MenuConfig::new().style(menu_style(light)));
    bar.ui(ui, |ui| {
        ui.label(
            egui::RichText::new("Pro Wave")
                .strong()
                .color(look(light).bar_text),
        );
        for (title, items) in MENUS {
            menu(ui, title, items, p, s);
        }
        status_line(ui, p, s);
        ui.cursor().left()
    })
    .inner
}

/// Colour of the menu bar's text, for things drawn on the bar.
pub fn bar_text(light: bool) -> Color32 {
    look(light).bar_text
}

/// Whether [`status_line`] has anything to say.
pub fn has_status(p: &Project, s: &crate::UiState) -> bool {
    s.status.is_some() || p.tracks.len() > prowave_engine::MAX_TRACKS
}

/// The last error or warning, after the menus (or on its own line on macOS, where the menus
/// are in the system menu bar).
pub fn status_line(ui: &mut egui::Ui, p: &Project, s: &crate::UiState) {
    let warn = if s.light_menus {
        Color32::from_rgb(0xb0, 0x5a, 0x00)
    } else {
        Color32::from_rgb(0xff, 0xb0, 0x40)
    };
    if let Some(msg) = &s.status {
        ui.colored_label(warn, msg);
    }
    // The engine plays only its first MAX_TRACKS tracks; say so instead of failing silently.
    let max = prowave_engine::MAX_TRACKS;
    if p.tracks.len() > max {
        let text = trf(
            "Tracks {}–{} are silent (limit {})",
            &[&(max + 1), &p.tracks.len(), &max],
        );
        ui.colored_label(warn, text).on_hover_text(tr(
            "The audio engine plays up to 1024 tracks. Tracks after that are kept in the project but not played.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_commands::content;

    /// Everything a menu entry can change, to compare before and after.
    fn state(p: &Project, s: &crate::UiState) -> String {
        format!(
            "{:?}",
            (
                content(p) == content(&Project::demo()),
                p.tracks.len(),
                p.position_beats,
                p.recording,
                (s.show_mixer, s.show_library, s.show_inspector, s.show_lists),
                (s.update_check, s.update_auto, s.show_midi_fx),
                (
                    s.capture_cmd,
                    s.show_quick_help,
                    s.show_toolbar,
                    s.show_notes,
                    s.browser.open
                ),
                s.bottom == Bottom::Mixer,
                (s.file_cmd, s.chooser.is_some(), s.undo_cmd, s.midi_cmd),
                (s.ui_scale, s.tracks.bar_w, s.tracks.selected),
                (
                    s.tracks.show_tempo,
                    s.tracks.show_markers,
                    s.tracks.show_signatures
                ),
                (i18n::chosen(), i18n::current()),
            )
        )
    }

    #[test]
    fn view_menu_checks_follow_the_panels() {
        let mut s = crate::UiState::new();
        let mut p = Project::demo();
        assert!(checked("Show Library", &s) && checked("Show Mixer", &s));
        apply("Show Library", &mut p, &mut s);
        assert!(!s.show_library && !checked("Show Library", &s));
        apply("Show Piano Roll", &mut p, &mut s);
        assert!(checked("Show Piano Roll", &s) && !checked("Show Mixer", &s));
        apply("Show Mixer", &mut p, &mut s);
        assert!(checked("Show Mixer", &s));
    }

    #[test]
    fn file_entries_queue_their_command() {
        let mut s = crate::UiState::new();
        apply("Save As…", &mut Project::demo(), &mut s);
        assert_eq!(s.file_cmd, Some(FileCmd::SaveAs));
        assert!(shortcut("Show Library").is_some() && shortcut("Bounce").is_none());

        for (label, cmd) in [
            ("Save A Copy As…", FileCmd::SaveCopy),
            ("Revert to Saved", FileCmd::Revert),
        ] {
            assert!(all_entries().contains(&label), "{label} is in the menus");
            apply(label, &mut Project::demo(), &mut s);
            assert_eq!(s.file_cmd.take(), Some(cmd));
        }
    }

    #[test]
    fn every_enabled_entry_does_something() {
        // An entry that is enabled must be handled by `apply` (or open a file command):
        // a click must never silently do nothing.
        let (p, mut s) = (Project::demo(), crate::UiState::new());
        s.can_undo = true;
        s.can_redo = true;
        s.tracks.selected = Some((0, 0));
        for label in all_entries() {
            if !enabled(label, &p, &s) {
                continue;
            }
            let (mut p2, mut s2) = (Project::demo(), crate::UiState::new());
            s2.tracks.selected = Some((0, 0));
            let before = state(&p2, &s2);
            apply(label, &mut p2, &mut s2);
            // These two may already be where they would take you.
            // These may already be where they would take you.
            let may_stay = [
                "Go to Next Region",
                "Actual Interface Size",
                "English (Default)",
            ]
            .contains(&label);
            assert!(
                before != state(&p2, &s2) || may_stay,
                "{label} is enabled but does nothing"
            );
        }
        // And unimplemented ones are greyed, like the ones from the GitHub report.
        for label in [
            "Cut",
            "Copy",
            "Paste",
            "Close",
            "Export",
            "Stem Splitter…",
            "Join",
        ] {
            assert!(!enabled(label, &p, &s), "{label}");
        }
    }

    #[test]
    fn track_new_adds_an_instrument_track() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        let n = p.tracks.len();
        apply("New", &mut p, &mut s);
        assert_eq!(p.tracks.len(), n + 1);
        assert_eq!(p.tracks[n].kind, TrackKind::Instrument);
    }

    #[test]
    fn undo_entries_follow_the_history_and_queue_their_command() {
        let (p, mut s) = (Project::demo(), crate::UiState::new());
        assert!(!enabled("Undo", &p, &s) && !enabled("Redo", &p, &s));
        s.can_undo = true;
        assert!(enabled("Undo", &p, &s) && enabled("Delete Undo History", &p, &s));
        apply("Undo", &mut Project::demo(), &mut s);
        assert_eq!(s.undo_cmd, Some(UndoCmd::Undo));
    }

    #[test]
    fn region_navigation_jumps_between_region_starts() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        p.position_beats = 0.0;
        apply("Go to End", &mut p, &mut s);
        assert_eq!(
            p.position_beats,
            12.0 * 4.0,
            "the demo's regions end at bar 13"
        );
        apply("Go to Previous Region", &mut p, &mut s);
        assert_eq!(p.position_beats, 0.0);
    }

    #[test]
    fn new_aux_track_adds_an_aux_track() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        assert!(TRACK.contains(&"New Aux Track"));
        let n = p.tracks.len();
        apply("New Aux Track", &mut p, &mut s);
        assert_eq!(p.tracks.len(), n + 1);
        assert_eq!(p.tracks[n].kind, TrackKind::Aux);
        assert_eq!(p.selected_track, n);
    }

    #[test]
    fn track_stack_entries_group_and_release_the_selected_track() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        assert!(TRACK.contains(&"Create Track Stack…") && TRACK.contains(&"Release Track Stack"));
        p.selected_track = 2;
        s.tracks.selected = Some((2, 0));
        let n = p.tracks.len();
        apply("Create Track Stack…", &mut p, &mut s);
        assert_eq!(p.tracks.len(), n + 1);
        assert_eq!(
            (p.tracks[2].kind, p.tracks[3].indent),
            (TrackKind::Folder, 1)
        );
        assert_eq!(
            s.tracks.selected, None,
            "indexes moved, so the selection is dropped"
        );
        apply("Release Track Stack", &mut p, &mut s);
        assert_eq!(p.tracks.len(), n);
        assert!(p
            .tracks
            .iter()
            .all(|t| t.indent == 0 && t.kind != TrackKind::Folder));
    }

    #[test]
    fn recent_projects_are_listed_with_their_folder() {
        let label = recent_label(std::path::Path::new("/home/me/Music/Night Drive.prowave"));
        assert_eq!(label, "Night Drive  (Music)");
        assert_eq!(recent_label(std::path::Path::new("Solo.prowave")), "Solo");
    }

    #[test]
    fn the_file_menu_follows_logic_with_submenus() {
        for title in ["Revert to", "Import", "Export", "Bounce", "Share"] {
            assert!(FILE.contains(&title), "{title}");
            assert!(submenu(title).is_some(), "{title} opens a submenu");
        }
        // Every submenu is reachable from a menu.
        for (title, _) in SUBMENUS {
            assert!(
                MENUS.iter().any(|(_, items)| items.contains(title)),
                "{title}"
            );
        }
        // Revert to > Revert to Saved is enabled, so its submenu opens.
        let (p, s) = (Project::demo(), crate::UiState::new());
        assert!(submenu("Revert to")
            .unwrap()
            .iter()
            .any(|c| enabled(c, &p, &s)));
    }

    #[test]
    fn markers_are_created_at_the_nearest_bar_and_navigated() {
        let (mut p, mut s) = (Project::empty(), crate::UiState::new());
        assert!(!enabled("Go to Next Marker", &p, &s));
        p.position_beats = 5.0;
        apply("Create Marker", &mut p, &mut s);
        assert_eq!(p.markers[0].beat, 4.0, "bar 2 in 4/4");
        assert!(s.tracks.show_markers, "the Marker track opens to show it");
        p.position_beats = 15.0;
        apply("Create Marker", &mut p, &mut s);
        p.position_beats = 0.0;
        apply("Go to Next Marker", &mut p, &mut s);
        apply("Go to Next Marker", &mut p, &mut s);
        assert_eq!(p.position_beats, 16.0);
        apply("Go to Previous Marker", &mut p, &mut s);
        assert_eq!(p.position_beats, 4.0);
        apply("Delete Marker", &mut p, &mut s);
        assert_eq!(p.markers.len(), 1);
        assert_eq!(p.markers[0].beat, 16.0);
    }

    #[test]
    fn help_language_lists_every_language_by_its_own_name() {
        let all: Vec<&str> = Lang::ALL.iter().map(|l| l.native_name()).collect();
        assert_eq!(LANGUAGE, all.as_slice());
        assert!(HELP.contains(&"Language") && submenu("Language").is_some());

        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        assert!(
            checked("English (Default)", &s),
            "English until another is picked"
        );
        apply("Deutsch", &mut p, &mut s);
        assert_eq!(i18n::current(), Lang::De);
        assert_eq!(i18n::chosen(), Some(Lang::De), "saved for the next start");
        assert!(checked("Deutsch", &s) && !checked("English (Default)", &s));
        assert_eq!(tr("Language"), "Sprache", "the menus switch at once");
        apply("English (Default)", &mut p, &mut s);
        assert!(checked("English (Default)", &s));
    }
}
