//! Piano Roll (Logic Pro: Piano Roll editor): editor tabs, local toolbar, the left inspector
//! (Time Quantize, Scale Quantize, Velocity), piano keys, ruler with the region strip, and
//! the note grid.
//!
//! Works today: select (click, Shift-click, marquee), move (time and pitch, with Snap),
//! resize from the right edge, create (click), drag-select, delete (Del or right-click), keyboard nudging, Time Quantize
//! with Strength and Swing, Scale Quantize, velocity editing, zoom and scrolling. Edits go to
//! the project and reach the engine when the mouse is released. Score, Step Sequencer and
//! Smart Tempo are not implemented yet and show dimmed.
use crate::i18n::{tr, trf};
use crate::model::{Note, Project, TrackKind};
use crate::note_edit::{self, GRIDS, MODES, ROOTS};
use crate::tracks_area::{snap_step, snap_to, Snap};
use crate::widgets::{
    back_chevron, menu_title, popup_button, slider_h, todo_item, BLUE, BTN_BG, TEXT, TEXT_DIM,
};
use crate::UiState;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Key, Painter, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui,
};

const TAB_H: f32 = 26.0;
const TOOLBAR_H: f32 = 30.0;
const INSPECTOR_W: f32 = 240.0;
const KEYS_W: f32 = 48.0;
const RULER_H: f32 = 22.0;
const REGION_H: f32 = 18.0;
const SB_H: f32 = 12.0;
const VEL_H: f32 = 72.0;

const PANEL_BG: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
const TOOLBAR_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
const ROW_WHITE: Color32 = Color32::from_rgb(26, 26, 26);
const ROW_BLACK: Color32 = Color32::from_rgb(20, 20, 20);
const NOTE_SEL: Color32 = Color32::from_rgb(0xe4, 0xff, 0xb0);
/// Logic's velocity colours for a note: green when soft, yellow, then red when hard.
pub(crate) fn velocity_color(vel: u8) -> Color32 {
    let t = (vel as f32 / 127.0).clamp(0.0, 1.0);
    let green = Color32::from_rgb(60, 185, 70);
    let yellow = Color32::from_rgb(232, 200, 40);
    let red = Color32::from_rgb(216, 43, 40);
    if t < 0.5 {
        green.lerp_to_gamma(yellow, t / 0.5)
    } else {
        yellow.lerp_to_gamma(red, ((t - 0.5) / 0.37).min(1.0))
    }
}

/// The bright bar colour Logic gives a region in the editor (a yellow track: 253, 197, 0).
fn region_bar_color(c: Color32) -> Color32 {
    let max = c.r().max(c.g()).max(c.b()).max(1) as f32;
    let s = |v: u8| v as f32 * 253.0 / max;
    let (r, g, b) = (s(c.r()), s(c.g()), s(c.b()));
    let lum = 0.3 * r + 0.59 * g + 0.11 * b;
    let k = |v: f32| (lum + (v - lum) * 1.3).clamp(0.0, 253.0) as u8;
    Color32::from_rgb(k(r), k(g), k(b))
}

/// Live notes the Piano Roll wants to play; `main` forwards them to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audition {
    On { pitch: u8, vel: u8, track: u16 },
    Off { pitch: u8 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Pointer,
    Pencil,
}

enum Drag {
    None,
    /// Pencil: a note was just created; dragging sets its length.
    Create {
        index: usize,
    },
    Move {
        anchor_beat: f32,
        anchor_pitch: i32,
        orig: Vec<(usize, f32, u8)>,
    },
    Resize {
        anchor_beat: f32,
        orig: Vec<(usize, f32)>,
    },
    Marquee {
        from: Pos2,
    },
}

pub struct PianoView {
    tool: Tool,
    /// Pitch currently sounding because of the mouse (key press or note edit).
    held: Option<u8>,
    /// Fit the region to the width of the editor on the next frame.
    fit: bool,
    /// Note whose velocity stem is being dragged in the velocity lane.
    vel_drag: Option<usize>,
    /// Keep `held` sounding until this time even if the mouse is up (click-created notes).
    release_at: Option<f64>,
    pub beat_px: f32,
    pub key_h: f32,
    scroll_x: f32,
    scroll_y: f32,
    pub selected: Vec<usize>,
    grid: usize,
    strength: f32,
    swing: f32,
    scale_root: Option<u8>,
    scale_mode: usize,
    new_velocity: u8,
    drag: Drag,
    centered_for: Option<(usize, usize)>,
    /// Edits waiting for the mouse to be released before the engine song is rebuilt.
    dirty: bool,
}

impl Default for PianoView {
    fn default() -> Self {
        Self {
            tool: Tool::Pointer,
            held: None,
            fit: true,
            vel_drag: None,
            release_at: None,
            beat_px: 64.0,
            key_h: 12.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            selected: Vec::new(),
            grid: 3,
            strength: 1.0,
            swing: 0.0,
            scale_root: None,
            scale_mode: 0,
            new_velocity: 100,
            drag: Drag::None,
            centered_for: None,
            dirty: false,
        }
    }
}

/// Starts, switches or stops the audition note; `pitch = None` releases.
fn audition(
    out: &mut Vec<Audition>,
    held: &mut Option<u8>,
    pitch: Option<u8>,
    vel: u8,
    track: u16,
) {
    if *held == pitch {
        return;
    }
    if let Some(old) = *held {
        out.push(Audition::Off { pitch: old });
    }
    if let Some(pitch) = pitch {
        out.push(Audition::On { pitch, vel, track });
    }
    *held = pitch;
}

/// The bars that start between `from` and `to` (beats from the region's start `origin`), as
/// (beats from `origin`, 0-based bar number).
fn bar_starts(p: &Project, origin: f64, from: f32, to: f32) -> Vec<(f64, u32)> {
    p.bars_in(origin + from as f64, origin + to as f64)
        .into_iter()
        .map(|b| (b.start - origin, b.bar0))
        .filter(|(rel, _)| *rel >= from as f64 - 1e-9)
        .collect()
}

/// Snap step in beats for the Piano Roll. Smart follows the zoom, like Logic's; when zoomed
/// out it falls back to the signature's beat (an eighth in 6/8).
fn piano_step(snap: Snap, beat_px: f32, beats_per_bar: f64, beat_unit: f64) -> f32 {
    match snap {
        Snap::Smart => match beat_px {
            x if x >= 160.0 => 0.0625,
            x if x >= 80.0 => 0.125,
            x if x >= 24.0 => 0.25,
            _ => beat_unit as f32,
        },
        other => snap_step(other, beat_px, beats_per_bar, beat_unit) as f32,
    }
}

fn id(key: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    egui::Id::unique(("piano_roll", key))
}

fn is_black(pitch: i32) -> bool {
    matches!(pitch.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// Logic octave naming: pitch 60 is C3.
fn c_label(pitch: i32) -> String {
    format!("C{}", pitch / 12 - 2)
}

/// The region the editor shows: the selected MIDI region, else the first one of the track.
pub(crate) fn target_region(p: &Project, s: &UiState) -> Option<(usize, usize)> {
    if let Some((t, r)) = s.tracks.selected {
        if p.tracks
            .get(t)
            .is_some_and(|tr| tr.kind != TrackKind::Audio && r < tr.regions.len())
        {
            return Some((t, r));
        }
    }
    let t = p.selected_track.min(p.tracks.len().checked_sub(1)?);
    let tr = &p.tracks[t];
    (tr.kind != TrackKind::Audio && !tr.regions.is_empty()).then_some((t, 0))
}

/// The editor tabs; the one for the region on show is lit, the rest are not there yet.
/// Returns the tab clicked (the Piano Roll and the Score switch when `switch` allows).
fn tabs(ui: &Ui, g: &Painter, bar: Rect, active: usize, switch: bool) -> Option<usize> {
    let mut clicked = None;
    g.rect_filled(bar, 0.0, PANEL_BG);
    let tabs = [
        tr("Piano Roll"),
        tr("Score"),
        tr("Step Sequencer"),
        tr("Drummer Editor"),
        tr("Smart Tempo"),
    ];
    let font = FontId::proportional(12.5);
    let widths: Vec<f32> = tabs
        .iter()
        .map(|t| g.layout_no_wrap((*t).into(), font.clone(), TEXT).size().x + 28.0)
        .collect();
    let total: f32 = widths.iter().sum();
    let mut x = bar.center().x - total / 2.0;
    for (i, (t, w)) in tabs.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, bar.top() + 3.0), vec2(*w, TAB_H - 6.0));
        if switch && i < 2 {
            let resp = ui.interact(r, egui::Id::unique(("editor_tab", i)), Sense::click());
            if resp.hovered() && i != active {
                g.rect_filled(r, 4.0, Color32::from_white_alpha(14));
            }
            if resp.clicked() {
                clicked = Some(i);
            }
        }
        if i == active {
            g.rect_filled(r, 4.0, BLUE);
            g.text(
                r.center(),
                Align2::CENTER_CENTER,
                *t,
                font.clone(),
                Color32::WHITE,
            );
        } else {
            // The editors that are not there yet are dimmed and inert.
            let live = switch && i < 2;
            g.text(
                r.center(),
                Align2::CENTER_CENTER,
                *t,
                font.clone(),
                if live { TEXT } else { Color32::from_gray(0x7e) },
            );
        }
        x += *w;
    }
    clicked
}

struct Actions {
    delete: bool,
    select_all: bool,
    quantize: bool,
    scale: bool,
    /// Open the MIDI Transform window, with this preset (or as it was left).
    transform: Option<Option<crate::transform::Preset>>,
    /// Whether there is something to undo / redo, and the one picked in the Edit menu.
    can_undo: bool,
    can_redo: bool,
    undo: Option<crate::undo::UndoCmd>,
}

fn toolbar(
    ui: &Ui,
    bar: Rect,
    v: &mut PianoView,
    has_sel: bool,
    snap: &mut Snap,
    act: &mut Actions,
) {
    const NS: &str = "piano_toolbar";
    let g = ui.painter_at(bar);
    g.rect_filled(bar, 0.0, TOOLBAR_BG);
    g.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let cy = bar.center().y;
    back_chevron(&g, pos2(bar.left() + 16.0, cy));
    let mut x = bar.left() + 32.0;
    let uid = |k: &str| egui::Id::unique((NS, k.to_string()));

    let (edit, w) = menu_title(ui, &g, x, cy, tr("Edit"), uid("Edit"));
    x += w + 2.0;
    egui::Popup::menu(&edit).show(|ui| {
        ui.set_min_width(230.0);
        crate::widgets::undo_items(ui, act.can_undo, act.can_redo, &mut act.undo);
        ui.separator();
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(tr("Delete")).shortcut_text(tr("Del")),
            )
            .clicked()
        {
            act.delete = true;
            ui.close();
        }
        let sc = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND,
            Key::A,
        ));
        if ui
            .add(egui::Button::new(tr("Select All")).shortcut_text(sc))
            .clicked()
        {
            act.select_all = true;
            ui.close();
        }
        ui.separator();
        if ui
            .add(egui::Button::new(tr("Quantize")).shortcut_text("Q"))
            .clicked()
        {
            act.quantize = true;
            ui.close();
        }
    });
    let (func, w) = menu_title(ui, &g, x, cy, tr("Functions"), uid("Functions"));
    x += w + 2.0;
    egui::Popup::menu(&func).show(|ui| {
        ui.set_min_width(230.0);
        if ui
            .add_enabled(
                v.scale_root.is_some(),
                egui::Button::new(tr("Scale Quantize")),
            )
            .clicked()
        {
            act.scale = true;
            ui.close();
        }
        ui.separator();
        use crate::transform::Preset;
        for (label, preset) in [
            (tr("MIDI Transform…"), None),
            (tr("Transpose…"), Some(Preset::Transpose)),
            (tr("Legato"), Some(Preset::Legato)),
            (tr("Humanize…"), Some(Preset::Humanize)),
            (tr("Fixed Velocity…"), Some(Preset::FixedVelocity)),
        ] {
            if ui.button(label).clicked() {
                act.transform = Some(preset);
                ui.close();
            }
        }
        todo_item(ui, tr("Note Repeat…"));
    });
    let (view, w) = menu_title(ui, &g, x, cy, tr("View"), uid("View"));
    x += w + 14.0;
    egui::Popup::menu(&view).show(|ui| {
        ui.set_min_width(230.0);
        if ui.button(tr("Zoom In")).clicked() {
            v.beat_px = (v.beat_px * 1.25).min(400.0);
            ui.close();
        }
        if ui.button(tr("Zoom Out")).clicked() {
            v.beat_px = (v.beat_px / 1.25).max(12.0);
            ui.close();
        }
        ui.separator();
        if ui.button(tr("Zoom to Fit")).clicked() {
            v.fit = true;
            ui.close();
        }
        todo_item(ui, tr("Note Colors"));
    });

    // Tool buttons: Pointer / Pencil (shortcuts: Esc and P).
    for (i, tool) in [Tool::Pointer, Tool::Pencil].into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(x + i as f32 * 30.0, cy - 10.0), vec2(26.0, 20.0));
        let resp = ui.interact(
            r,
            uid(if i == 0 {
                "tool_pointer"
            } else {
                "tool_pencil"
            }),
            Sense::click(),
        );
        let on = v.tool == tool;
        if on {
            g.rect_filled(r, 4.0, BLUE);
        } else if resp.hovered() {
            g.rect_filled(r, 4.0, Color32::from_white_alpha(14));
        }
        let col = if on { Color32::WHITE } else { TEXT };
        let c = r.center();
        if i == 0 {
            g.add(Shape::line(
                vec![
                    c + vec2(-3.0, -6.0),
                    c + vec2(5.0, 2.0),
                    c + vec2(1.0, 2.5),
                    c + vec2(-1.0, 7.0),
                    c + vec2(-3.0, -6.0),
                ],
                Stroke::new(1.3, col),
            ));
        } else {
            g.line_segment(
                [c + vec2(5.0, -6.0), c + vec2(-4.0, 3.0)],
                Stroke::new(2.4, col),
            );
            g.add(Shape::convex_polygon(
                vec![
                    c + vec2(-4.0, 3.0),
                    c + vec2(-6.0, 7.0),
                    c + vec2(-2.0, 5.0),
                ],
                col,
                Stroke::NONE,
            ));
        }
        if resp.clicked() {
            v.tool = tool;
        }
    }

    // Right side: zoom sliders, then Snap.
    let (lo, hi) = (12f32.ln(), 400f32.ln());
    let hz = Rect::from_min_size(pos2(bar.right() - 76.0, cy - 6.0), vec2(64.0, 12.0));
    let t = ((v.beat_px.ln() - lo) / (hi - lo)).clamp(0.0, 1.0);
    if let Some(n) = slider_h(ui, &g, hz, uid("hz"), t, true) {
        v.beat_px = (lo + n * (hi - lo)).exp();
    }
    let vz = Rect::from_min_size(hz.min - vec2(86.0, 0.0), vec2(54.0, 12.0));
    let t = (v.key_h - 6.0) / 18.0;
    if let Some(n) = slider_h(ui, &g, vz, uid("vz"), t, true) {
        v.key_h = 6.0 + n * 18.0;
    }
    let snap_r = Rect::from_min_size(pos2(vz.left() - 190.0, cy - 10.0), vec2(150.0, 20.0));
    let resp = popup_button(ui, &g, snap_r, uid("snap"), snap.label());
    egui::Popup::menu(&resp).show(|ui| {
        for m in Snap::ALL {
            if ui.selectable_label(*snap == m, m.label()).clicked() {
                *snap = m;
                ui.close();
            }
        }
    });
    g.text(
        pos2(snap_r.left() - 6.0, cy),
        Align2::RIGHT_CENTER,
        tr("Snap:"),
        FontId::proportional(12.5),
        TEXT_DIM,
    );
}

fn small_q(ui: &Ui, g: &Painter, r: Rect, key: &str, enabled: bool) -> bool {
    let resp = enabled.then(|| ui.interact(r, id(("q", key)), Sense::click()));
    let hovered = resp.as_ref().is_some_and(|r| r.hovered());
    g.rect_filled(
        r,
        4.0,
        if hovered {
            Color32::from_gray(0x62)
        } else {
            BTN_BG
        },
    );
    g.text(
        r.center(),
        Align2::CENTER_CENTER,
        "Q",
        FontId::proportional(12.0),
        if enabled {
            TEXT
        } else {
            Color32::from_gray(0x78)
        },
    );
    resp.is_some_and(|r| r.clicked())
}

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState) {
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let g = ui.painter_at(area);
    g.rect_filled(area, 0.0, PANEL_BG);
    let tab_bar = Rect::from_min_size(area.min, vec2(area.width(), TAB_H));
    let region = target_region(p, s);
    // A Pattern or Drummer region has its own editor.
    let generated = region.and_then(|(t, r)| {
        p.tracks[t].regions[r]
            .generated
            .as_ref()
            .map(|g| matches!(g.source, crate::model::Generator::Drummer(_)))
    });
    // Ordinary MIDI regions can be shown as a score as well.
    let ordinary =
        region.is_some_and(|(t, r)| generated.is_none() && p.tracks[t].regions[r].folder.is_none());
    let active = match generated {
        None if ordinary && s.score_tab => 1,
        None => 0,
        Some(false) => 2,
        Some(true) => 3,
    };
    if let Some(i) = tabs(ui, &g, tab_bar, active, ordinary) {
        s.score_tab = i == 1;
    }
    if let (Some((ti, ri)), true) = (region, active == 1) {
        s.piano_focus = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|pt| area.contains(pt));
        let body = Rect::from_min_max(pos2(area.left(), tab_bar.bottom()), area.max);
        crate::score::show(ui, p, s, ti, ri, body);
        return;
    }
    if let (Some((ti, ri)), Some(_)) = (region, generated) {
        s.piano_focus = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|pt| area.contains(pt));
        let body = Rect::from_min_max(pos2(area.left(), tab_bar.bottom()), area.max);
        crate::step_editor::show(ui, p, s, ti, ri, body);
        return;
    }
    let bar = Rect::from_min_size(
        pos2(area.left(), tab_bar.bottom()),
        vec2(area.width(), TOOLBAR_H),
    );
    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), area.max);
    let hovered = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| area.contains(pt));
    s.piano_focus = hovered;

    let has_sel = !s.piano.selected.is_empty();
    let mut act = Actions {
        delete: false,
        select_all: false,
        quantize: false,
        scale: false,
        transform: None,
        can_undo: s.can_undo,
        can_redo: s.can_redo,
        undo: None,
    };
    let mut snap = s.tracks.snap;
    toolbar(ui, bar, &mut s.piano, has_sel, &mut snap, &mut act);
    s.tracks.snap = snap;
    if let Some(preset) = act.transform {
        s.transform.open_with(preset);
    }
    if act.undo.is_some() {
        s.undo_cmd = act.undo;
    }

    let Some((ti, ri)) = region else {
        g.text(
            body.center(),
            Align2::CENTER_CENTER,
            tr("Select a MIDI region to edit its notes"),
            FontId::proportional(13.0),
            TEXT_DIM,
        );
        return;
    };
    // A take folder's notes are its comp: they are edited after it is flattened.
    if p.tracks[ti].regions[ri].folder.is_some() {
        g.text(
            body.center(),
            Align2::CENTER_CENTER,
            tr("This is a take folder: flatten it to edit its notes"),
            FontId::proportional(13.0),
            TEXT_DIM,
        );
        return;
    }
    let origin = p.region_start_beats(&p.tracks[ti].regions[ri]);
    // The bar and beat length where the region starts (the grid draws every bar on its own).
    let bpb = p.bar_len_at(origin);
    let unit = p.beat_unit_at(origin);
    let (region_name, start_bar, len_beats) = {
        let r = &p.tracks[ti].regions[ri];
        (r.name.clone(), r.start_bar, p.region_len_beats(r) as f32)
    };
    let track_name = p.tracks[ti].name.clone();
    let region_color = region_bar_color(p.tracks[ti].color);
    let position = p.position_beats - origin;

    // Layout.
    let insp = Rect::from_min_size(body.min, vec2(INSPECTOR_W, body.height()));
    let editor = Rect::from_min_max(pos2(insp.right(), body.top()), body.max);
    let lower = editor.bottom() - SB_H - VEL_H - 4.0;
    let keys = Rect::from_min_max(
        pos2(editor.left(), editor.top() + RULER_H + REGION_H),
        pos2(editor.left() + KEYS_W, lower),
    );
    let grid = Rect::from_min_max(
        pos2(keys.right(), keys.top()),
        pos2(editor.right() - SB_H, lower),
    );
    let hbar = Rect::from_min_max(
        pos2(grid.left(), grid.bottom()),
        pos2(grid.right(), grid.bottom() + SB_H),
    );
    let vbar = Rect::from_min_max(
        pos2(grid.right(), grid.top()),
        pos2(editor.right(), grid.bottom()),
    );
    let vel_rect = Rect::from_min_max(
        pos2(grid.left(), hbar.bottom() + 4.0),
        pos2(grid.right(), editor.bottom()),
    );
    let vel_label = Rect::from_min_max(
        pos2(keys.left(), vel_rect.top()),
        pos2(keys.right(), vel_rect.bottom()),
    );
    let ruler = Rect::from_min_max(
        pos2(grid.left(), editor.top()),
        pos2(editor.right(), editor.top() + RULER_H),
    );
    let region_bar = Rect::from_min_max(
        pos2(grid.left(), ruler.bottom()),
        pos2(editor.right(), grid.top()),
    );

    let v = &mut s.piano;
    let (down, now) = ui.input(|i| (i.pointer.primary_down(), i.time));
    if v.held.is_some() && !down {
        match v.release_at {
            Some(t) if now < t => ui
                .ctx()
                .request_repaint_after(std::time::Duration::from_millis(30)),
            _ => {
                audition(&mut s.audition, &mut v.held, None, 0, 0);
                v.release_at = None;
            }
        }
    }
    let track_u8 = ti.min(u16::MAX as usize) as u16;
    let notes_len = p.tracks[ti].regions[ri].notes.len();
    v.selected.retain(|i| *i < notes_len);

    // Centre on the notes when a different region is opened.
    if v.centered_for != Some((ti, ri)) {
        v.centered_for = Some((ti, ri));
        v.fit = true;
        let notes = &p.tracks[ti].regions[ri].notes;
        let (lo, hi) = notes
            .iter()
            .fold((127u8, 0u8), |(a, b), n| (a.min(n.pitch), b.max(n.pitch)));
        let mid = if notes.is_empty() {
            60.0
        } else {
            (lo as f32 + hi as f32) / 2.0
        };
        v.scroll_y = (127.0 - mid) * v.key_h - grid.height() / 2.0;
        v.scroll_x = 0.0;
        v.selected.clear();
    }

    if v.fit {
        v.fit = false;
        v.beat_px = ((grid.width() - 16.0) / len_beats.max(1.0)).clamp(12.0, 400.0);
        v.scroll_x = 0.0;
    }

    // Wheel: vertical scroll; Shift = horizontal; Ctrl/Cmd = horizontal zoom.
    if ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| editor.contains(pt))
    {
        let (d, m) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers));
        if m.command && d.y != 0.0 {
            v.beat_px = (v.beat_px * (1.0 + d.y * 0.003)).clamp(12.0, 400.0);
        } else if m.shift {
            v.scroll_x -= d.y + d.x;
        } else {
            v.scroll_x -= d.x;
            v.scroll_y -= d.y;
        }
    }
    v.scroll_y = v
        .scroll_y
        .clamp(0.0, (128.0 * v.key_h - grid.height()).max(0.0));
    let content_w = ((len_beats + 2.0) * v.beat_px).max(grid.width());
    let content_h = 128.0 * v.key_h;
    {
        let gs = ui.painter_at(Rect::from_min_max(hbar.min, vbar.max));
        gs.rect_filled(hbar, 0.0, Color32::from_gray(0x2a));
        gs.rect_filled(vbar, 0.0, Color32::from_gray(0x2a));
        let tw = hbar.width();
        let thumb_w = (tw * grid.width() / content_w).clamp(24.0, tw);
        let max_x = (content_w - grid.width()).max(1.0);
        let thumb_x = hbar.left() + (tw - thumb_w) * (v.scroll_x / max_x).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(
            pos2(thumb_x, hbar.top() + 2.0),
            vec2(thumb_w, hbar.height() - 4.0),
        );
        let r = ui.interact(thumb, id("hthumb"), Sense::drag());
        if r.dragged() {
            v.scroll_x += r.drag_delta().x * max_x / (tw - thumb_w).max(1.0);
        }
        gs.rect_filled(
            thumb,
            4.0,
            Color32::from_gray(if r.hovered() || r.dragged() {
                0x88
            } else {
                0x66
            }),
        );

        let th = vbar.height();
        let thumb_h = (th * grid.height() / content_h).clamp(24.0, th);
        let max_y = (content_h - grid.height()).max(1.0);
        let thumb_y = vbar.top() + (th - thumb_h) * (v.scroll_y / max_y).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(
            pos2(vbar.left() + 2.0, thumb_y),
            vec2(vbar.width() - 4.0, thumb_h),
        );
        let r = ui.interact(thumb, id("vthumb"), Sense::drag());
        if r.dragged() {
            v.scroll_y += r.drag_delta().y * max_y / (th - thumb_h).max(1.0);
        }
        gs.rect_filled(
            thumb,
            4.0,
            Color32::from_gray(if r.hovered() || r.dragged() {
                0x88
            } else {
                0x66
            }),
        );
    }
    v.scroll_y = v.scroll_y.clamp(0.0, (content_h - grid.height()).max(0.0));
    v.scroll_x = v.scroll_x.clamp(0.0, (content_w - grid.width()).max(0.0));
    let (beat_px, key_h, scroll_x, scroll_y) = (v.beat_px, v.key_h, v.scroll_x, v.scroll_y);
    let x_of = |beat: f32| grid.left() + beat * beat_px - scroll_x;
    let y_of = |pitch: i32| grid.top() + (127 - pitch) as f32 * key_h - scroll_y;
    let beat_at = |x: f32| (x - grid.left() + scroll_x) / beat_px;
    let pitch_at =
        |y: f32| (127 - ((y - grid.top() + scroll_y) / key_h).floor() as i32).clamp(0, 127);
    let step = piano_step(s.tracks.snap, beat_px, bpb, unit);
    let bars = bar_starts(
        p,
        origin,
        scroll_x / beat_px,
        (scroll_x + grid.width()) / beat_px,
    );

    // ---- Grid background
    let gg = ui.painter_at(grid);
    gg.rect_filled(grid, 0.0, ROW_WHITE);
    let top_pitch = pitch_at(grid.top());
    let bottom_pitch = pitch_at(grid.bottom());
    for pitch in bottom_pitch..=top_pitch {
        let y = y_of(pitch);
        let row = Rect::from_min_size(pos2(grid.left(), y), vec2(grid.width(), key_h));
        gg.rect_filled(
            row,
            0.0,
            if is_black(pitch) {
                ROW_BLACK
            } else {
                ROW_WHITE
            },
        );
        if pitch % 12 == 0 {
            gg.line_segment(
                [row.left_bottom(), row.right_bottom()],
                Stroke::new(1.0, Color32::from_gray(0x44)),
            );
        }
    }
    let first = (scroll_x / beat_px).floor() as i32;
    let last = ((scroll_x + grid.width()) / beat_px).ceil() as i32;
    let sub = (step < 1.0 && step * beat_px >= 8.0).then_some(step);
    for b in first..=last {
        let bx = x_of(b as f32);
        gg.line_segment(
            [pos2(bx, grid.top()), pos2(bx, grid.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x3a)),
        );
        if let Some(st) = sub {
            let mut k = st;
            while k < 1.0 {
                let sx = x_of(b as f32 + k);
                gg.line_segment(
                    [pos2(sx, grid.top()), pos2(sx, grid.bottom())],
                    Stroke::new(1.0, Color32::from_gray(0x33)),
                );
                k += st;
            }
        }
    }
    for &(rel, _) in &bars {
        let bx = x_of(rel as f32);
        gg.line_segment(
            [pos2(bx, grid.top()), pos2(bx, grid.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x58)),
        );
    }
    // Outside the region: dim.
    let (rx0, rx1) = (x_of(0.0), x_of(len_beats));
    if rx0 > grid.left() {
        gg.rect_filled(
            Rect::from_min_max(grid.min, pos2(rx0.min(grid.right()), grid.bottom())),
            0.0,
            Color32::from_black_alpha(70),
        );
    }
    if rx1 < grid.right() {
        gg.rect_filled(
            Rect::from_min_max(pos2(rx1.max(grid.left()), grid.top()), grid.max),
            0.0,
            Color32::from_black_alpha(70),
        );
    }

    // ---- Piano keys: press to play, drag to glissando
    let kresp = ui.interact(keys, id("keys"), Sense::click_and_drag());
    if kresp.is_pointer_button_down_on() {
        if let Some(pt) = kresp.interact_pointer_pos() {
            audition(
                &mut s.audition,
                &mut v.held,
                Some(pitch_at(pt.y) as u8),
                100,
                track_u8,
            );
            v.release_at = None;
        }
    }

    // ---- Interaction
    let resp = ui.interact(grid, id("grid"), Sense::click_and_drag());
    let shift = ui.input(|i| i.modifiers.shift);
    let edge_beats = 10.0 / beat_px;
    let notes = &mut p.tracks[ti].regions[ri].notes;
    // Right-click deletes the note under the pointer.
    let (secondary, hover, down_now) = ui.input(|i| {
        (
            i.pointer.button_pressed(egui::PointerButton::Secondary),
            i.pointer.hover_pos(),
            i.pointer.primary_down(),
        )
    });
    if let Some(pt) = hover.filter(|pt| grid.contains(*pt) && resp.hovered()) {
        let (beat, pitch) = (beat_at(pt.x), pitch_at(pt.y));
        let over_note = note_edit::hit_note(notes, beat, pitch as u8, edge_beats);
        if over_note.is_some_and(|(_, on_edge)| on_edge) || matches!(v.drag, Drag::Resize { .. }) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if secondary {
            if let Some((i, _)) = over_note {
                notes.remove(i);
                v.selected.clear();
                v.dirty = true;
            }
        }
    }
    if let Drag::Create { index } = v.drag {
        if down_now {
            if let (Some(n), Some(pt)) = (notes.get_mut(index), hover) {
                let len = snap_to((beat_at(pt.x) - n.start) as f64, step as f64) as f32;
                n.len = len.max(step.max(0.0625));
            }
        } else {
            v.drag = Drag::None;
            v.dirty = true;
        }
    }
    if resp.drag_started() && !matches!(v.drag, Drag::Create { .. }) {
        let origin = ui.input(|i| i.pointer.press_origin());
        if let Some(pt) = origin.or(resp.interact_pointer_pos()) {
            let (beat, pitch) = (beat_at(pt.x), pitch_at(pt.y));
            match note_edit::hit_note(notes, beat, pitch as u8, edge_beats) {
                Some((i, on_edge)) => {
                    audition(
                        &mut s.audition,
                        &mut v.held,
                        Some(notes[i].pitch),
                        notes[i].vel,
                        track_u8,
                    );
                    v.release_at = None;
                    if !v.selected.contains(&i) {
                        if shift {
                            v.selected.push(i);
                        } else {
                            v.selected = vec![i];
                        }
                    }
                    v.drag = if on_edge {
                        Drag::Resize {
                            anchor_beat: beat,
                            orig: v.selected.iter().map(|&k| (k, notes[k].len)).collect(),
                        }
                    } else {
                        Drag::Move {
                            anchor_beat: beat,
                            anchor_pitch: pitch,
                            orig: v
                                .selected
                                .iter()
                                .map(|&k| (k, notes[k].start, notes[k].pitch))
                                .collect(),
                        }
                    };
                }
                None if v.tool == Tool::Pencil && beat >= 0.0 => {
                    let start = (beat / step).floor() * step;
                    notes.push(Note {
                        start,
                        len: step.max(0.0625),
                        pitch: pitch as u8,
                        vel: v.new_velocity,
                    });
                    let index = notes.len() - 1;
                    v.selected = vec![index];
                    v.drag = Drag::Create { index };
                    audition(
                        &mut s.audition,
                        &mut v.held,
                        Some(pitch as u8),
                        v.new_velocity,
                        track_u8,
                    );
                    v.release_at = None;
                    v.dirty = true;
                }
                None => {
                    if !shift {
                        v.selected.clear();
                    }
                    v.drag = Drag::Marquee { from: pt };
                }
            }
        }
    }
    if resp.dragged() {
        if let Some(pt) = resp.interact_pointer_pos() {
            let (beat, pitch) = (beat_at(pt.x), pitch_at(pt.y));
            match &v.drag {
                Drag::Move {
                    anchor_beat,
                    anchor_pitch,
                    orig,
                } => {
                    let delta = snap_to((beat - anchor_beat) as f64, step as f64) as f32;
                    let dp = pitch - anchor_pitch;
                    for &(i, start, pc) in orig {
                        notes[i].start = (start + delta).max(0.0);
                        notes[i].pitch = (pc as i32 + dp).clamp(0, 127) as u8;
                    }
                }
                Drag::Resize { anchor_beat, orig } => {
                    let delta = snap_to((beat - anchor_beat) as f64, step as f64) as f32;
                    for &(i, len) in orig {
                        notes[i].len = (len + delta).max(0.0625);
                    }
                }
                Drag::Marquee { from } => {
                    let (b0, p0) = (beat_at(from.x), pitch_at(from.y));
                    v.selected = note_edit::marquee(notes, b0, beat, p0 as u8, pitch as u8);
                }
                Drag::Create { index } => {
                    if let Some(n) = notes.get_mut(*index) {
                        n.len = (snap_to((beat - n.start) as f64, step as f64) as f32)
                            .max(step.max(0.0625));
                    }
                }
                Drag::None => {}
            }
            if let Drag::Move {
                orig, anchor_pitch, ..
            } = &v.drag
            {
                let first = orig.first().map(|o| o.2 as i32).unwrap_or(0);
                let now_pitch = (first + pitch - anchor_pitch).clamp(0, 127) as u8;
                audition(&mut s.audition, &mut v.held, Some(now_pitch), 100, track_u8);
            }
        }
    }
    if resp.drag_stopped() {
        if matches!(
            v.drag,
            Drag::Move { .. } | Drag::Resize { .. } | Drag::Create { .. }
        ) {
            v.dirty = true;
        }
        v.drag = Drag::None;
    }
    if resp.clicked() {
        if let Some(pt) = resp.interact_pointer_pos() {
            match note_edit::hit_note(notes, beat_at(pt.x), pitch_at(pt.y) as u8, 0.0) {
                Some((i, _)) if shift => {
                    if let Some(k) = v.selected.iter().position(|x| *x == i) {
                        v.selected.remove(k);
                    } else {
                        v.selected.push(i);
                    }
                }
                Some((i, _)) => {
                    v.selected = vec![i];
                    audition(
                        &mut s.audition,
                        &mut v.held,
                        Some(notes[i].pitch),
                        notes[i].vel,
                        track_u8,
                    );
                    v.release_at = Some(now + 0.25);
                }
                // A click (press and release without dragging) on empty space creates a note.
                None if !shift => {
                    let (beat, pitch) = (beat_at(pt.x), pitch_at(pt.y));
                    if beat >= 0.0 {
                        let start = (beat / step).floor() * step;
                        notes.push(Note {
                            start,
                            len: step.max(0.0625),
                            pitch: pitch as u8,
                            vel: v.new_velocity,
                        });
                        v.selected = vec![notes.len() - 1];
                        audition(
                            &mut s.audition,
                            &mut v.held,
                            Some(pitch as u8),
                            v.new_velocity,
                            track_u8,
                        );
                        v.release_at = Some(now + 0.25);
                        v.dirty = true;
                    }
                }
                None => {}
            }
        }
    }
    // ---- Velocity lane (FL Studio style): drag a stem up or down to set that note's velocity.
    let vresp = ui.interact(vel_rect, id("vel_lane"), Sense::click_and_drag());
    if vresp.drag_started() || vresp.clicked() {
        let origin = ui.input(|i| i.pointer.press_origin());
        if let Some(pt) = origin.or(vresp.interact_pointer_pos()) {
            let near = |n: &Note| (x_of(n.start) - pt.x).abs();
            let best = notes
                .iter()
                .enumerate()
                .filter(|(_, n)| near(n) <= 7.0)
                .min_by(|a, b| near(a.1).total_cmp(&near(b.1)))
                .map(|(i, _)| i);
            v.vel_drag = best;
            if let Some(i) = best {
                if !v.selected.contains(&i) {
                    v.selected = vec![i];
                }
            }
        }
    }
    if let Some(i) = v.vel_drag {
        match (down_now, hover) {
            (true, Some(pt)) => {
                let nv = (((vel_rect.bottom() - pt.y) / vel_rect.height()) * 127.0)
                    .round()
                    .clamp(1.0, 127.0) as i32;
                if let Some(cur) = notes.get(i).map(|n| n.vel as i32) {
                    let delta = nv - cur;
                    let targets = if v.selected.contains(&i) {
                        v.selected.clone()
                    } else {
                        vec![i]
                    };
                    for k in targets {
                        if let Some(n) = notes.get_mut(k) {
                            n.vel = (n.vel as i32 + delta).clamp(1, 127) as u8;
                        }
                    }
                    v.dirty = true;
                }
            }
            (true, None) => {}
            (false, _) => v.vel_drag = None,
        }
    }

    // ---- Keyboard (only while the pointer is over the editor)
    if hovered && !ui.ctx().egui_wants_keyboard_input() {
        let (del, all, q, up, down, left, right, big) = ui.input(|i| {
            (
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.modifiers.command && i.key_pressed(Key::A),
                !i.modifiers.command && i.key_pressed(Key::Q),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.modifiers.shift,
            )
        });
        if ui.input(|i| i.key_pressed(Key::P) && !i.modifiers.command) {
            v.tool = Tool::Pencil;
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            v.tool = Tool::Pointer;
        }
        act.delete |= del;
        act.select_all |= all;
        act.quantize |= q;
        let dp = (up as i32 - down as i32) * if big { 12 } else { 1 };
        let dt = (right as i32 - left as i32) as f32 * step;
        if (dp != 0 || dt != 0.0) && !v.selected.is_empty() {
            for &i in &v.selected {
                notes[i].pitch = (notes[i].pitch as i32 + dp).clamp(0, 127) as u8;
                notes[i].start = (notes[i].start + dt).max(0.0);
            }
            v.dirty = true;
        }
    }
    if act.select_all {
        v.selected = (0..notes.len()).collect();
    }

    // ---- Inspector (left): quantize, scale quantize, velocity
    let gi = ui.painter_at(insp);
    gi.rect_filled(insp, 0.0, PANEL_BG);
    gi.line_segment(
        [insp.right_top(), insp.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let ix = insp.left() + 12.0;
    let iw = insp.width() - 24.0;
    let mut y = insp.top() + 10.0;
    let n_sel = v.selected.len();
    // Region header: icon, name and track, like Logic's MIDI region inspector.
    let icon = Rect::from_min_size(pos2(ix, y - 2.0), vec2(26.0, 22.0));
    crate::track_icons::draw_icon(&gi, icon, crate::track_icons::IconKind::Note);
    let font = FontId::proportional(12.0);
    let name = crate::mixer::elide(&gi, &region_name, &font, iw - 70.0);
    gi.text(
        pos2(insp.center().x, y + 2.0),
        Align2::CENTER_CENTER,
        name,
        font,
        TEXT,
    );
    let font = FontId::proportional(10.0);
    let sub = crate::mixer::elide(&gi, &trf("on Track {}", &[&track_name]), &font, iw - 70.0);
    gi.text(
        pos2(insp.center().x, y + 16.0),
        Align2::CENTER_CENTER,
        sub,
        font,
        TEXT_DIM,
    );
    let box_r = Rect::from_min_size(pos2(ix + iw - 24.0, y), vec2(24.0, 18.0));
    gi.rect_filled(box_r, 3.0, Color32::from_gray(0x4a));
    gi.rect_stroke(
        Rect::from_center_size(box_r.center(), vec2(10.0, 8.0)),
        1.5,
        Stroke::new(1.0, Color32::from_gray(0xc0)),
        StrokeKind::Inside,
    );
    y += 46.0;

    gi.text(
        pos2(ix, y),
        Align2::LEFT_TOP,
        tr("Time Quantize (classic)"),
        FontId::proportional(11.5),
        TEXT_DIM,
    );
    y += 18.0;
    let grid_r = Rect::from_min_size(pos2(ix, y), vec2(iw - 38.0, 20.0));
    let gresp = popup_button(ui, &gi, grid_r, id("grid_popup"), tr(GRIDS[v.grid].0));
    egui::Popup::menu(&gresp).show(|ui| {
        for (i, (label, _)) in GRIDS.iter().enumerate() {
            if ui.selectable_label(v.grid == i, tr(label)).clicked() {
                v.grid = i;
                ui.close();
            }
        }
    });
    if small_q(
        ui,
        &gi,
        Rect::from_min_size(pos2(ix + iw - 30.0, y), vec2(30.0, 20.0)),
        "time",
        true,
    ) {
        act.quantize = true;
    }
    y += 34.0;
    for (label, key, value) in [
        (tr("Strength"), "strength", &mut v.strength),
        (tr("Swing"), "swing", &mut v.swing),
    ] {
        gi.text(
            pos2(ix, y),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(11.5),
            TEXT_DIM,
        );
        gi.text(
            pos2(ix + iw, y),
            Align2::RIGHT_TOP,
            format!("{}", (*value * 100.0).round() as i32),
            FontId::proportional(11.5),
            TEXT,
        );
        y += 18.0;
        let r = Rect::from_min_size(pos2(ix, y), vec2(iw, 14.0));
        if let Some(n) = slider_h(ui, &gi, r, id(key), *value, true) {
            *value = n;
        }
        y += 28.0;
    }
    gi.line_segment(
        [
            pos2(insp.left() + 6.0, y - 8.0),
            pos2(insp.right() - 6.0, y - 8.0),
        ],
        Stroke::new(1.0, Color32::from_gray(0x2c)),
    );
    y += 4.0;

    gi.text(
        pos2(ix, y),
        Align2::LEFT_TOP,
        tr("Scale Quantize"),
        FontId::proportional(11.5),
        TEXT_DIM,
    );
    y += 18.0;
    let root_r = Rect::from_min_size(pos2(ix, y), vec2(70.0, 20.0));
    let root_text = v.scale_root.map_or(tr("Off"), |r| ROOTS[r as usize]);
    let rresp = popup_button(ui, &gi, root_r, id("root_popup"), root_text);
    egui::Popup::menu(&rresp).show(|ui| {
        if ui
            .selectable_label(v.scale_root.is_none(), tr("Off"))
            .clicked()
        {
            v.scale_root = None;
            ui.close();
        }
        for (i, r) in ROOTS.iter().enumerate() {
            if ui
                .selectable_label(v.scale_root == Some(i as u8), *r)
                .clicked()
            {
                v.scale_root = Some(i as u8);
                ui.close();
            }
        }
    });
    let mode_r = Rect::from_min_size(pos2(ix + 76.0, y), vec2(iw - 76.0 - 38.0, 20.0));
    let mresp = popup_button(ui, &gi, mode_r, id("mode_popup"), tr(MODES[v.scale_mode].0));
    egui::Popup::menu(&mresp).show(|ui| {
        for (i, (label, _)) in MODES.iter().enumerate() {
            if ui.selectable_label(v.scale_mode == i, tr(label)).clicked() {
                v.scale_mode = i;
                ui.close();
            }
        }
    });
    if small_q(
        ui,
        &gi,
        Rect::from_min_size(pos2(ix + iw - 30.0, y), vec2(30.0, 20.0)),
        "scale",
        v.scale_root.is_some(),
    ) {
        act.scale = true;
    }
    y += 38.0;
    gi.line_segment(
        [
            pos2(insp.left() + 6.0, y - 10.0),
            pos2(insp.right() - 6.0, y - 10.0),
        ],
        Stroke::new(1.0, Color32::from_gray(0x2c)),
    );

    // Velocity of the selection (average), or the velocity for new notes.
    let vel = if n_sel == 0 {
        v.new_velocity as f32
    } else {
        v.selected.iter().map(|&i| notes[i].vel as f32).sum::<f32>() / n_sel as f32
    };
    gi.text(
        pos2(ix, y),
        Align2::LEFT_TOP,
        tr("Velocity"),
        FontId::proportional(11.5),
        TEXT_DIM,
    );
    gi.text(
        pos2(ix + iw, y),
        Align2::RIGHT_TOP,
        format!("{}", vel.round() as i32),
        FontId::proportional(11.5),
        TEXT,
    );
    y += 18.0;
    if let Some(n) = slider_h(
        ui,
        &gi,
        Rect::from_min_size(pos2(ix, y), vec2(iw, 14.0)),
        id("velocity"),
        vel / 127.0,
        true,
    ) {
        let new = (n * 127.0).round().clamp(1.0, 127.0) as u8;
        if n_sel == 0 {
            v.new_velocity = new;
        } else {
            let d = new as i32 - vel.round() as i32;
            for &i in &v.selected {
                notes[i].vel = (notes[i].vel as i32 + d).clamp(1, 127) as u8;
            }
            v.dirty = true;
        }
    }

    // Automation / MIDI: what the lane under the notes shows (Note Velocity).
    let ay = insp.bottom() - 112.0;
    if ay > y + 24.0 {
        gi.text(
            pos2(ix, ay),
            Align2::LEFT_TOP,
            tr("Automation/MIDI"),
            FontId::proportional(11.5),
            TEXT_DIM,
        );
        let power = Rect::from_min_size(pos2(ix, ay + 18.0), vec2(26.0, 20.0));
        gi.rect_filled(power, 3.0, Color32::from_rgb(0x2a, 0x5a, 0xb0));
        gi.circle_stroke(power.center(), 5.0, Stroke::new(1.3, Color32::WHITE));
        let region_btn =
            Rect::from_min_size(pos2(ix + 28.0, ay + 18.0), vec2(iw - 28.0 - 34.0, 20.0));
        gi.rect_filled(region_btn, 0.0, BLUE);
        gi.text(
            region_btn.center(),
            Align2::CENTER_CENTER,
            tr("Region"),
            FontId::proportional(11.5),
            Color32::WHITE,
        );
        let curve = Rect::from_min_size(pos2(ix + iw - 30.0, ay + 18.0), vec2(30.0, 20.0));
        gi.rect_filled(curve, 3.0, Color32::from_gray(0x4a));
        let box_r = Rect::from_min_size(pos2(ix, ay + 42.0), vec2(iw, 44.0));
        gi.rect_filled(box_r, 3.0, Color32::from_gray(0x30));
        gi.text(
            pos2(box_r.center().x, box_r.top() + 10.0),
            Align2::CENTER_CENTER,
            tr("Note Velocity"),
            FontId::proportional(11.5),
            Color32::from_rgb(0x4c, 0xe0, 0x4c),
        );
    }

    let hint = tr("Click: add · Drag: select · Del/Right-click: delete");
    let hint_font = FontId::proportional(10.5);
    let hint_text = crate::mixer::elide(&gi, hint, &hint_font, iw);
    gi.text(
        pos2(ix, insp.bottom() - 18.0),
        Align2::LEFT_TOP,
        hint_text,
        hint_font,
        Color32::from_gray(0x88),
    );

    // ---- Apply toolbar/inspector actions
    let targets: Vec<usize> = if v.selected.is_empty() {
        (0..notes.len()).collect()
    } else {
        v.selected.clone()
    };
    if act.quantize {
        note_edit::quantize(notes, &targets, GRIDS[v.grid].1, v.strength, v.swing);
        v.dirty = true;
    }
    if act.scale {
        if let Some(root) = v.scale_root {
            note_edit::scale_quantize(notes, &targets, root, MODES[v.scale_mode].1);
            v.dirty = true;
        }
    }
    if act.delete && !v.selected.is_empty() {
        let mut sel = std::mem::take(&mut v.selected);
        sel.sort_unstable_by(|a, b| b.cmp(a));
        for i in sel {
            if i < notes.len() {
                notes.remove(i);
            }
        }
        v.dirty = true;
    }

    // ---- Draw keys, notes, ruler, playhead (after edits so they show immediately)
    let notes = &p.tracks[ti].regions[ri].notes;
    let gk = ui.painter_at(keys);
    gk.rect_filled(keys, 0.0, Color32::from_gray(0x20));
    for pitch in bottom_pitch..=top_pitch {
        let y = y_of(pitch);
        let row = Rect::from_min_size(pos2(keys.left(), y), vec2(KEYS_W, key_h));
        if is_black(pitch) {
            gk.rect_filled(
                Rect::from_min_size(row.min, vec2(KEYS_W * 0.62, key_h)),
                0.0,
                Color32::from_gray(0x14),
            );
            gk.rect_filled(
                Rect::from_min_max(pos2(row.left() + KEYS_W * 0.62, row.top()), row.max),
                0.0,
                Color32::from_gray(0xe2),
            );
        } else {
            gk.rect_filled(row, 0.0, Color32::from_gray(0xee));
            gk.line_segment(
                [row.left_bottom(), row.right_bottom()],
                Stroke::new(1.0, Color32::from_gray(0x9a)),
            );
        }
        if v.held == Some(pitch as u8) {
            gk.rect_filled(
                row,
                0.0,
                Color32::from_rgba_unmultiplied(0x2f, 0x6f, 0xd8, 200),
            );
        }
        if pitch % 12 == 0 {
            gk.text(
                pos2(keys.right() - 4.0, y + key_h / 2.0),
                Align2::RIGHT_CENTER,
                c_label(pitch),
                FontId::proportional(9.5),
                Color32::from_gray(0x30),
            );
        }
    }
    // A flag per note, so drawing a big selection does not search the list for every note.
    let is_selected = selection_flags(&v.selected, notes.len());
    for (i, n) in notes.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(x_of(n.start), y_of(n.pitch as i32) + 1.0),
            vec2((n.len * beat_px).max(3.0), key_h - 2.0),
        );
        if r.right() < grid.left()
            || r.left() > grid.right()
            || r.bottom() < grid.top()
            || r.top() > grid.bottom()
        {
            continue;
        }
        let selected = is_selected[i];
        // Logic draws a note with a light edge of its velocity colour around a deeper body.
        let color = velocity_color(n.vel);
        let (fill, edge) = if selected {
            (NOTE_SEL, Color32::WHITE)
        } else {
            (
                color.lerp_to_gamma(Color32::from_gray(0x80), 0.3),
                color.lerp_to_gamma(Color32::WHITE, 0.65),
            )
        };
        gg.rect_filled(r, 1.5, fill);
        gg.rect_stroke(r, 1.5, Stroke::new(1.0, edge), StrokeKind::Inside);
    }
    if let Drag::Marquee { from } = &v.drag {
        if let Some(now) = ui.input(|i| i.pointer.hover_pos()) {
            let r = Rect::from_two_pos(*from, now);
            gg.rect_filled(r, 0.0, Color32::from_white_alpha(22));
            gg.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, Color32::from_white_alpha(150)),
                StrokeKind::Inside,
            );
        }
    }

    // Velocity lane.
    let gv = ui.painter_at(Rect::from_min_max(vel_label.min, vel_rect.max));
    gv.rect_filled(vel_label, 0.0, Color32::from_gray(0x2c));
    gv.text(
        vel_label.center(),
        Align2::CENTER_CENTER,
        tr("Velocity"),
        FontId::proportional(10.5),
        TEXT_DIM,
    );
    gv.rect_filled(vel_rect, 0.0, ROW_WHITE);
    for f in [0.25f32, 0.5, 0.75] {
        let y = vel_rect.bottom() - f * vel_rect.height();
        gv.line_segment(
            [pos2(vel_rect.left(), y), pos2(vel_rect.right(), y)],
            Stroke::new(1.0, Color32::from_gray(0x36)),
        );
    }
    for &(rel, _) in &bars {
        let bx = x_of(rel as f32);
        gv.line_segment(
            [pos2(bx, vel_rect.top()), pos2(bx, vel_rect.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x44)),
        );
    }
    let vel_selected = selection_flags(&v.selected, p.tracks[ti].regions[ri].notes.len());
    for (i, n) in p.tracks[ti].regions[ri].notes.iter().enumerate() {
        let x = x_of(n.start);
        if x < vel_rect.left() - 4.0 || x > vel_rect.right() + 4.0 {
            continue;
        }
        let top = vel_rect.bottom() - n.vel as f32 / 127.0 * (vel_rect.height() - 6.0);
        let col = if vel_selected[i] {
            NOTE_SEL
        } else {
            Color32::from_rgb(115, 197, 113)
        };
        // Logic shows each note's velocity as a dot with a line along the note.
        let end = x_of(n.start + n.len);
        gv.line_segment(
            [pos2(x, top), pos2(end.max(x + 2.0), top)],
            Stroke::new(1.5, col),
        );
        gv.circle_filled(pos2(x, top), 2.5, Color32::from_rgb(131, 218, 127));
    }

    // Ruler (absolute bar numbers) and region strip.
    let gr = ui.painter_at(Rect::from_min_max(
        pos2(keys.left(), editor.top()),
        editor.max,
    ));
    gr.rect_filled(
        Rect::from_min_max(
            pos2(keys.left(), editor.top()),
            pos2(editor.right(), grid.top()),
        ),
        0.0,
        Color32::from_gray(0x1f),
    );
    // The part of the ruler over the region is gold, as in Logic.
    let ruler_gold = Color32::from_rgb(
        (region_color.r() as f32 * 0.66) as u8,
        (region_color.g() as f32 * 0.64) as u8,
        (region_color.b() as f32 * 0.64) as u8,
    );
    let gold = Rect::from_min_max(
        pos2(rx0.max(grid.left()), ruler.top()),
        pos2(rx1.min(grid.right()), ruler.bottom()),
    );
    if gold.width() > 0.0 {
        gr.rect_filled(gold, 0.0, ruler_gold);
    }
    let label_every = [1i32, 2, 4, 8, 16]
        .into_iter()
        .find(|n| *n as f32 * bpb as f32 * beat_px >= 36.0)
        .unwrap_or(32);
    for &(rel, bar0) in &bars {
        let x = x_of(rel as f32);
        gr.line_segment(
            [pos2(x, ruler.top() + 3.0), pos2(x, ruler.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x78)),
        );
        if (bar0 as i32 - (start_bar as i32 - 1)) % label_every == 0 {
            gr.text(
                pos2(x + 3.0, ruler.top() + 4.0),
                Align2::LEFT_TOP,
                format!("{}", bar0 + 1),
                FontId::proportional(11.0),
                TEXT,
            );
        }
    }
    let strip = Rect::from_min_max(
        pos2(rx0.max(region_bar.left()), region_bar.top() + 1.0),
        pos2(rx1.min(region_bar.right()), region_bar.bottom()),
    );
    if strip.width() > 0.0 {
        gr.rect_filled(strip, 0.0, region_color);
        gr.rect_stroke(
            strip,
            0.0,
            Stroke::new(1.0, Color32::from_rgb(27, 21, 14)),
            StrokeKind::Inside,
        );
        gr.text(
            pos2(strip.left() + 6.0, strip.center().y),
            Align2::LEFT_CENTER,
            &region_name,
            FontId::proportional(10.5),
            Color32::from_gray(0x20),
        );
    }
    gr.line_segment(
        [
            pos2(keys.left(), grid.top()),
            pos2(editor.right(), grid.top()),
        ],
        Stroke::new(1.0, Color32::from_gray(0x20)),
    );
    let px = x_of(position as f32);
    if px >= grid.left() && px <= grid.right() {
        gg.line_segment(
            [pos2(px, grid.top()), pos2(px, grid.bottom())],
            Stroke::new(1.0, crate::tracks_area::playhead_colors(p.recording).0),
        );
        gr.add(Shape::convex_polygon(
            vec![
                pos2(px - 5.0, ruler.top() + 2.0),
                pos2(px + 5.0, ruler.top() + 2.0),
                pos2(px, ruler.top() + 10.0),
            ],
            crate::tracks_area::playhead_colors(p.recording).1,
            Stroke::NONE,
        ));
    }

    // Rebuild the engine song once per gesture, not on every frame of a drag.
    if v.dirty && !ui.input(|i| i.pointer.any_down()) {
        v.dirty = false;
        p.song_rev += 1;
    }
}

/// One flag per note of a region of `len` notes: whether its index is in `selected`. Indexes
/// past the end are ignored.
fn selection_flags(selected: &[usize], len: usize) -> Vec<bool> {
    let mut flags = vec![false; len];
    for &i in selected {
        if let Some(f) = flags.get_mut(i) {
            *f = true;
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection_flags_mark_the_selected_notes_and_ignore_stale_indexes() {
        assert_eq!(selection_flags(&[0, 2, 9], 4), [true, false, true, false]);
        assert_eq!(selection_flags(&[], 2), [false, false]);
    }

    use super::*;

    #[test]
    fn zoomed_out_smart_snap_uses_the_signature_beat() {
        assert_eq!(piano_step(Snap::Smart, 10.0, 4.0, 1.0), 1.0);
        assert_eq!(piano_step(Snap::Smart, 10.0, 3.0, 0.5), 0.5);
        assert_eq!(piano_step(Snap::Smart, 100.0, 3.0, 0.5), 0.125);
        assert_eq!(piano_step(Snap::Beat, 100.0, 3.0, 0.5), 0.5);
    }
}
