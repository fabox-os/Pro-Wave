//! Tracks area (Logic Pro: main window "Tracks" area): local toolbar, ruler with cycle area and
//! playhead, track headers and the region lanes.
//!
//! Works today: select/move regions with Snap, Delete and Repeat regions, Set Locators by
//! Regions, drag in the ruler to set the cycle, click to move the playhead, scroll, zoom
//! (sliders and Ctrl+wheel), Catch playhead, add/duplicate tracks, M/S/R/I, volume, pan.
//! Menu entries that are not implemented yet are shown disabled.
use crate::i18n::{tr, trf};
use crate::model::{Project, Region, Track, TrackKind};
use crate::track_icons::{draw_icon, track_icon};
use crate::widgets::{
    context_click, context_menu, menu_title, popup_button, stroke, todo_item, BLUE, BTN_BG, TEXT,
    TEXT_DIM,
};
use crate::{Bottom, UiState};
use egui::{
    pos2, vec2, Align2, Color32, FontId, Key, KeyboardShortcut, Modifiers, Painter, Pos2, Rect,
    Sense, Shape, Stroke, StrokeKind, Ui,
};

const TOOLBAR_H: f32 = 30.0;
const RULER_H: f32 = 26.0;
const HEADER_W: f32 = 340.0;
const NUM_W: f32 = 24.0;
/// How far each folder nesting level moves a track header's contents to the right.
const INDENT_W: f32 = 14.0;

const TOOLBAR_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
const HEADER_BG: Color32 = Color32::from_rgb(0x41, 0x41, 0x41);
const HEADER_SEL: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
const LANE_BG: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
const LANE_SEL: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);
const RULER_BG: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);
const CYCLE: Color32 = Color32::from_rgb(0xd4, 0x9e, 0x2a);
const MUTE_ON: Color32 = Color32::from_rgb(0x3b, 0x8c, 0xe6);
const SOLO_ON: Color32 = Color32::from_rgb(0xe6, 0xc2, 0x2e);
const REC_ON: Color32 = Color32::from_rgb(0xe0, 0x3a, 0x30);
const MON_ON: Color32 = Color32::from_rgb(0xe8, 0x8a, 0x2c);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    Smart,
    Bar,
    Beat,
    Division,
    Ticks,
}

impl Snap {
    pub(crate) const ALL: [Snap; 5] = [
        Snap::Smart,
        Snap::Bar,
        Snap::Beat,
        Snap::Division,
        Snap::Ticks,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Snap::Smart => tr("Smart"),
            Snap::Bar => tr("Bar"),
            Snap::Beat => tr("Beat"),
            Snap::Division => tr("Division"),
            Snap::Ticks => tr("Ticks"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    Overlap,
    NoOverlap,
    XFade,
    ShuffleR,
    ShuffleL,
}

impl DragMode {
    const ALL: [DragMode; 5] = [
        DragMode::Overlap,
        DragMode::NoOverlap,
        DragMode::XFade,
        DragMode::ShuffleR,
        DragMode::ShuffleL,
    ];
    fn label(self) -> &'static str {
        match self {
            DragMode::Overlap => tr("Overlap"),
            DragMode::NoOverlap => tr("No Overlap"),
            DragMode::XFade => tr("X-Fade"),
            DragMode::ShuffleR => tr("Shuffle R"),
            DragMode::ShuffleL => tr("Shuffle L"),
        }
    }
}

/// Where the Fade tool is in [`TOOLS`].
const FADE_TOOL: usize = 9;

pub(crate) const TOOLS: [&str; 14] = [
    "Pointer Tool",
    "Pencil Tool",
    "Eraser Tool",
    "Text Tool",
    "Scissors Tool",
    "Glue Tool",
    "Solo Tool",
    "Mute Tool",
    "Zoom Tool",
    "Fade Tool",
    "Automation Select Tool",
    "Automation Curve Tool",
    "Marquee Tool",
    "Flex Tool",
];

/// A Normalize Region Gain scan running on its own thread: reading a long recording to find
/// its loudest sample takes seconds, which would freeze the window if done on the UI thread.
pub struct NormalizeJob {
    track: usize,
    region: usize,
    path: std::path::PathBuf,
    /// The file's peak, or why it could not be read.
    result: std::sync::mpsc::Receiver<Result<f32, String>>,
}

/// The tool the pointer has in the Tracks area (Logic's left-click tool).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackTool {
    Pointer,
    /// Drag on an audio region to set its fade in (left half) or fade out (right half).
    Fade,
}

pub struct TracksView {
    /// Width of one bar in points (horizontal zoom).
    pub bar_w: f32,
    /// Track height in points (vertical zoom).
    pub row_h: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub snap: Snap,
    pub drag_mode: DragMode,
    pub tool: TrackTool,
    /// A Fade tool drag in progress: `true` for a fade in, `false` for a fade out.
    fade_drag: Option<bool>,
    pub catch: bool,
    /// Global Tempo track above the tracks (Track > Show Tempo Track).
    pub show_tempo: bool,
    /// Global Marker track above the tracks (Track > Show Marker Track).
    pub show_markers: bool,
    /// Global Signature track above the tracks (Track > Show Signature Track).
    pub show_signatures: bool,
    /// (track, region)
    pub selected: Option<(usize, usize)>,
    /// Width of the lanes at the last frame (for Navigate > Zoom to Fit).
    pub lanes_w: f32,
    grab_beats: f64,
    cycle_anchor: Option<f64>,
    /// A swipe on a take row in progress: the comp as it was and where the swipe began.
    swipe: Option<(Vec<crate::model::CompSegment>, f32)>,
    /// Normalize Region Gain scans still running.
    pub normalize_jobs: Vec<NormalizeJob>,
    /// For the layout pictures (`--shots`) only: the right-click menu shown open, of a track
    /// (`(t, None)`) or of a region (`(t, Some(r))`).
    pub shot_menu: Option<(usize, Option<usize>)>,
    /// Smart Tempo analyses still running.
    pub tempo_jobs: Vec<crate::smart_tempo::Job>,
    /// The multitrack Smart Tempo analysis, while it runs.
    pub tempo_all: Option<crate::smart_tempo::AllJob>,
    /// The Smart Tempo Editor window.
    pub tempo_editor: crate::smart_tempo::Editor,
}

impl Default for TracksView {
    fn default() -> Self {
        Self {
            bar_w: 62.0,
            row_h: 56.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            lanes_w: 1000.0,
            snap: Snap::Smart,
            drag_mode: DragMode::NoOverlap,
            tool: TrackTool::Pointer,
            fade_drag: None,
            catch: true,
            show_tempo: false,
            show_markers: false,
            show_signatures: false,
            selected: None,
            grab_beats: 0.0,
            cycle_anchor: None,
            swipe: None,
            normalize_jobs: Vec::new(),
            shot_menu: None,
            tempo_jobs: Vec::new(),
            tempo_all: None,
            tempo_editor: Default::default(),
        }
    }
}

/// True when another track is soloed and `track` is not: it is silent until solo is released
/// (Logic plays only soloed tracks and greys the others' regions).
pub(crate) fn silenced_by_solo(p: &Project, track: usize) -> bool {
    p.tracks.get(track).is_some() && !p.is_soloed(track) && p.any_solo()
}

/// A region while it is being recorded.
const RECORDING_FILL: Color32 = Color32::from_rgb(86, 14, 14);

/// The outline of a selected region.
const SELECTED_EDGE: Color32 = Color32::from_rgb(176, 200, 72);

/// The ruler over a selected region is gold.
const RULER_SELECTION: Color32 = Color32::from_rgb(169, 126, 25);
const RULER_SELECTION_EDGE: Color32 = Color32::from_rgb(95, 69, 20);

/// Logic's playhead is yellow: a pale yellow line and a yellow head.
pub(crate) const PLAYHEAD_LINE: Color32 = Color32::from_rgb(214, 196, 112);
pub(crate) const PLAYHEAD_HEAD: Color32 = Color32::from_rgb(240, 206, 84);
pub(crate) const PLAYHEAD_EDGE: Color32 = Color32::from_rgb(146, 118, 30);
/// While recording the playhead turns red.
const PLAYHEAD_LINE_REC: Color32 = Color32::from_rgb(176, 42, 38);
const PLAYHEAD_HEAD_REC: Color32 = Color32::from_rgb(206, 32, 26);
const PLAYHEAD_EDGE_REC: Color32 = Color32::from_rgb(112, 16, 14);

/// The playhead's line, head and outline colours.
pub(crate) fn playhead_colors(recording: bool) -> (Color32, Color32, Color32) {
    if recording {
        (PLAYHEAD_LINE_REC, PLAYHEAD_HEAD_REC, PLAYHEAD_EDGE_REC)
    } else {
        (PLAYHEAD_LINE, PLAYHEAD_HEAD, PLAYHEAD_EDGE)
    }
}

/// Half the width of the playhead head's grab zone in the ruler.
const PLAYHEAD_GRAB: f32 = 8.0;

/// Beat under screen x `x` while dragging the playhead (never before the start of the song).
fn playhead_beat(x: f32, lanes_left: f32, scroll_x: f32, beat_px: f32) -> f64 {
    ((x - lanes_left + scroll_x) / beat_px.max(1e-3)).max(0.0) as f64
}

/// Half the height of the grab zone on a track's lower edge.
const RESIZE_GRIP: f32 = 3.0;

/// `(top, height)` of every track row in points, from the top of the first track (unscrolled):
/// the global track zoom `row_h` times each track's own height factor.
fn row_layout(p: &Project, row_h: f32) -> Vec<(f32, f32)> {
    let mut top = 0.0;
    p.tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // The tracks of a folded folder take no room (every loop below skips them).
            let h = if p.hidden_by_fold(i) {
                0.0
            } else {
                (row_h * t.height).round().max(1.0) + take_rows_height(t)
            };
            let row = (top, h);
            top += h;
            row
        })
        .collect()
}

/// Index of the track row containing `y` (same origin as [`row_layout`]).
fn row_at(rows: &[(f32, f32)], y: f32) -> Option<usize> {
    rows.iter().position(|&(top, h)| y >= top && y < top + h)
}

fn id(key: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    egui::Id::unique(("tracks_area", key))
}

/// Whether the snap goes to bar lines (which is not a fixed step once signatures change).
fn snaps_to_bars(snap: Snap, beat_px: f32, beat_unit: f64) -> bool {
    match snap {
        Snap::Bar => true,
        Snap::Smart => beat_px * (beat_unit as f32) < 12.0,
        _ => false,
    }
}

/// Fade tool steps per quarter note (1/16 notes, as the Inspector's fades).
const FADE_STEPS_PER_BEAT: f64 = 4.0;

/// The fade (in quarter notes) the Fade tool sets when dragged to `at`, for a region that
/// starts at `start` and is `len` long: from its start for a fade in, from its end for a fade
/// out; in 1/16 notes and never longer than the region.
fn fade_for_drag(at: f64, start: f64, len: f64, fade_in: bool) -> f32 {
    let raw = if fade_in {
        at - start
    } else {
        start + len - at
    };
    ((raw * FADE_STEPS_PER_BEAT).round().max(0.0) / FADE_STEPS_PER_BEAT).min(len) as f32
}

/// Whether region `ri` of track `ti` overlaps another region of the track.
fn overlaps_another(p: &Project, ti: usize, ri: usize) -> bool {
    let track = &p.tracks[ti];
    let (s, e) = (
        p.region_start_beats(&track.regions[ri]),
        p.region_end_beats(&track.regions[ri]),
    );
    track.regions.iter().enumerate().any(|(j, o)| {
        j != ri && p.region_start_beats(o) < e - 1e-9 && p.region_end_beats(o) > s + 1e-9
    })
}

/// Snap step in beats (quarter notes) for the current setting and zoom. `beat_px` is pixels
/// per quarter note; `beat_unit` is the signature's beat in quarter notes (0.5 in 6/8), so
/// Beat snaps to eighths in x/8 as in Logic. Divisions stay 16th notes.
pub(crate) fn snap_step(snap: Snap, beat_px: f32, beats_per_bar: f64, beat_unit: f64) -> f64 {
    match snap {
        Snap::Smart if beat_px >= 48.0 => 0.25,
        Snap::Smart if beat_px * beat_unit as f32 >= 12.0 => beat_unit,
        Snap::Smart => beats_per_bar,
        Snap::Bar => beats_per_bar,
        Snap::Beat => beat_unit,
        Snap::Division => 0.25,
        Snap::Ticks => 1.0 / 960.0,
    }
}

pub(crate) fn snap_to(b: f64, step: f64) -> f64 {
    (b / step).round() * step
}

// ------------------------------------------------------------------ small widgets

fn icon_button(
    ui: &Ui,
    g: &Painter,
    rect: Rect,
    key: &str,
    on: bool,
    draw: impl FnOnce(&Painter, Rect, Color32),
) -> egui::Response {
    let resp = ui.interact(rect, id(("icon", key)), Sense::click());
    if on {
        g.rect_filled(rect, 4.0, BLUE);
    } else if resp.hovered() {
        g.rect_filled(rect, 4.0, Color32::from_white_alpha(14));
    }
    draw(g, rect, if on { Color32::WHITE } else { TEXT });
    resp
}

/// Horizontal mini slider; returns the new 0..1 value.
fn mini_slider(ui: &Ui, g: &Painter, rect: Rect, key: &str, value: f32) -> f32 {
    let resp = ui.interact(
        rect.expand2(vec2(4.0, 6.0)),
        id(("slider", key)),
        Sense::click_and_drag(),
    );
    let mut v = value;
    if let Some(p) = resp.interact_pointer_pos() {
        v = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    }
    let cy = rect.center().y;
    g.line_segment(
        [pos2(rect.left(), cy), pos2(rect.right(), cy)],
        Stroke::new(2.0, Color32::from_gray(0x22)),
    );
    g.circle_filled(
        pos2(rect.left() + rect.width() * v, cy),
        5.5,
        Color32::from_gray(0xc8),
    );
    v
}

fn header_button_bg(selected_row: bool) -> Color32 {
    if selected_row {
        Color32::from_gray(0x72)
    } else {
        Color32::from_gray(0x5a)
    }
}

#[allow(clippy::too_many_arguments)]
fn header_button(
    ui: &Ui,
    g: &Painter,
    rect: Rect,
    key: (usize, &str),
    label: &str,
    on: bool,
    on_col: Color32,
    selected_row: bool,
) -> bool {
    let resp = ui.interact(rect, id(("hdrbtn", key)), Sense::click());
    g.rect_filled(
        rect,
        3.0,
        if on {
            on_col
        } else {
            header_button_bg(selected_row)
        },
    );
    let txt = if on && on_col == SOLO_ON {
        Color32::BLACK
    } else {
        Color32::from_gray(0xe8)
    };
    g.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        txt,
    );
    resp.clicked()
}

// ------------------------------------------------------------------ toolbar icons

fn draw_grid_icon(g: &Painter, r: Rect, c: Color32) {
    for (dx, dy) in [(-3.5, -3.5), (3.5, -3.5), (-3.5, 3.5), (3.5, 3.5)] {
        g.rect_filled(
            Rect::from_center_size(r.center() + vec2(dx, dy), vec2(5.0, 5.0)),
            1.0,
            c,
        );
    }
}

fn draw_list_icon(g: &Painter, r: Rect, c: Color32) {
    for i in -1..=1 {
        let y = r.center().y + i as f32 * 4.0;
        g.rect_filled(
            Rect::from_center_size(pos2(r.center().x, y), vec2(13.0, 2.4)),
            1.0,
            c,
        );
    }
}

fn draw_curve_icon(g: &Painter, r: Rect, c: Color32) {
    let p = r.center();
    let pts: Vec<Pos2> = (0..=12)
        .map(|i| {
            let t = i as f32 / 12.0;
            p + vec2(-7.0 + 14.0 * t, 5.0 - 10.0 * (t * t * (3.0 - 2.0 * t)))
        })
        .collect();
    g.add(Shape::line(pts, stroke(c)));
    g.circle_filled(p + vec2(-7.0, 5.0), 1.8, c);
    g.circle_filled(p + vec2(7.0, -5.0), 1.8, c);
}

fn draw_flex_icon(g: &Painter, r: Rect, c: Color32) {
    // Hourglass on its side (Flex).
    let p = r.center();
    g.add(Shape::line(
        vec![
            p + vec2(-7.0, -5.0),
            p + vec2(7.0, 5.0),
            p + vec2(7.0, -5.0),
            p + vec2(-7.0, 5.0),
            p + vec2(-7.0, -5.0),
        ],
        stroke(c),
    ));
}

fn draw_catch_icon(g: &Painter, r: Rect, c: Color32) {
    // Playhead with a follow arrow.
    let p = r.center();
    g.line_segment([p + vec2(-4.0, -7.0), p + vec2(-4.0, 7.0)], stroke(c));
    g.add(Shape::convex_polygon(
        vec![
            p + vec2(-8.0, -7.0),
            p + vec2(0.0, -7.0),
            p + vec2(-4.0, -3.0),
        ],
        c,
        Stroke::NONE,
    ));
    g.line_segment([p + vec2(-1.0, 2.0), p + vec2(6.0, 2.0)], stroke(c));
    g.add(Shape::line(
        vec![p + vec2(3.5, -0.5), p + vec2(6.5, 2.0), p + vec2(3.5, 4.5)],
        stroke(c),
    ));
}

fn draw_pointer(g: &Painter, at: Pos2, c: Color32) {
    let pts = vec![
        at + vec2(-3.0, -7.0),
        at + vec2(5.0, 1.0),
        at + vec2(1.0, 1.5),
        at + vec2(-1.0, 6.0),
        at + vec2(-3.0, -7.0),
    ];
    g.add(Shape::line(pts, stroke(c)));
}

/// The Fade tool's icon: a ramp down to a baseline.
fn draw_fade_tool(g: &Painter, at: Pos2, c: Color32) {
    g.line_segment([at + vec2(-6.0, 5.0), at + vec2(6.0, 5.0)], stroke(c));
    g.line_segment([at + vec2(-6.0, 5.0), at + vec2(-6.0, -5.0)], stroke(c));
    g.line_segment([at + vec2(-6.0, -5.0), at + vec2(6.0, 5.0)], stroke(c));
}

fn draw_crosshair(g: &Painter, at: Pos2, c: Color32) {
    g.line_segment([at + vec2(-6.0, 0.0), at + vec2(6.0, 0.0)], stroke(c));
    g.line_segment([at + vec2(0.0, -6.0), at + vec2(0.0, 6.0)], stroke(c));
}

fn draw_zoom_icon(g: &Painter, at: Pos2, c: Color32, vertical: bool) {
    let (a, b) = if vertical {
        (vec2(0.0, -6.0), vec2(0.0, 6.0))
    } else {
        (vec2(-6.0, 0.0), vec2(6.0, 0.0))
    };
    g.line_segment([at + a, at + b], stroke(c));
    let n = if vertical {
        vec2(2.5, 0.0)
    } else {
        vec2(0.0, 2.5)
    };
    let d = if vertical {
        vec2(0.0, 2.5)
    } else {
        vec2(2.5, 0.0)
    };
    g.add(Shape::line(
        vec![at + a + n + d, at + a, at + a - n + d],
        stroke(c),
    ));
    g.add(Shape::line(
        vec![at + b + n - d, at + b, at + b - n - d],
        stroke(c),
    ));
}

fn shortcut(ui: &Ui, mods: Modifiers, key: Key) -> String {
    ui.ctx().format_shortcut(&KeyboardShortcut::new(mods, key))
}

// ------------------------------------------------------------------ toolbar

fn toolbar(ui: &Ui, p: &mut Project, s: &mut UiState, bar: Rect, lanes_w: f32) {
    let g = ui.painter_at(bar);
    g.rect_filled(bar, 0.0, TOOLBAR_BG);
    g.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let cy = bar.center().y;
    let (can_undo, can_redo) = (s.can_undo, s.can_redo);
    let mut undo_pick = None;
    let v = &mut s.tracks;

    // Back chevron (dim: no previous view to return to).
    let c = pos2(bar.left() + 16.0, cy);
    g.add(Shape::line(
        vec![c + vec2(3.0, -5.0), c + vec2(-2.0, 0.0), c + vec2(3.0, 5.0)],
        stroke(TEXT_DIM),
    ));

    let mut x = bar.left() + 32.0;
    let sel = v.selected;
    let (edit, w) = menu_title(ui, &g, x, cy, tr("Edit"), id(("menu", "Edit")));
    x += w + 2.0;
    egui::Popup::menu(&edit).show(|ui| {
        ui.set_min_width(260.0);
        crate::widgets::undo_items(ui, can_undo, can_redo, &mut undo_pick);
        ui.separator();
        todo_item(ui, tr("Cut"));
        todo_item(ui, tr("Copy"));
        todo_item(ui, tr("Paste"));
        if ui
            .add_enabled(
                sel.is_some(),
                egui::Button::new(tr("Delete")).shortcut_text(tr("Del")),
            )
            .clicked()
        {
            delete_selected(p, v);
            ui.close();
        }
        ui.separator();
        todo_item(ui, tr("Select All"));
        todo_item(ui, tr("Split Regions at Playhead"));
        todo_item(ui, tr("Join Regions"));
        let rep = shortcut(ui, Modifiers::COMMAND, Key::R);
        if ui
            .add_enabled(
                sel.is_some(),
                egui::Button::new(tr("Repeat Regions")).shortcut_text(rep),
            )
            .clicked()
        {
            repeat_selected(p, v);
            ui.close();
        }
    });
    if undo_pick.is_some() {
        s.undo_cmd = undo_pick;
    }
    let (func, w) = menu_title(ui, &g, x, cy, tr("Functions"), id(("menu", "Functions")));
    x += w + 2.0;
    egui::Popup::menu(&func).show(|ui| {
        ui.set_min_width(260.0);
        if ui
            .add_enabled(
                sel.is_some(),
                egui::Button::new(tr("Set Locators by Regions")),
            )
            .clicked()
        {
            set_locators_by_region(p, v);
            ui.close();
        }
        ui.separator();
        todo_item(ui, tr("Folder"));
        todo_item(ui, tr("Quantize"));
        todo_item(ui, tr("Bounce in Place…"));
        todo_item(ui, tr("Convert to New Sampler Track…"));
        todo_item(ui, tr("Move to Playhead"));
    });
    let (view, w) = menu_title(ui, &g, x, cy, tr("View"), id(("menu", "View")));
    x += w + 10.0;
    egui::Popup::menu(&view).show(|ui| {
        ui.set_min_width(260.0);
        if ui.button(tr("Zoom to Fit")).clicked() {
            zoom_to_fit(p, v, lanes_w);
            ui.close();
        }
        if ui.button(tr("Zoom In")).clicked() {
            v.bar_w = (v.bar_w * 1.25).min(600.0);
            ui.close();
        }
        if ui.button(tr("Zoom Out")).clicked() {
            v.bar_w = (v.bar_w / 1.25).max(6.0);
            ui.close();
        }
        ui.separator();
        if ui.checkbox(&mut v.catch, tr("Catch Playhead")).clicked() {
            ui.close();
        }
        todo_item(ui, tr("Show Global Tracks"));
        todo_item(ui, tr("Configure Track Header…"));
    });

    // View buttons: grid (Live Loops) / list (Tracks, active).
    let r = Rect::from_center_size(pos2(x + 13.0, cy), vec2(26.0, 20.0));
    let _ = icon_button(ui, &g, r, "grid", false, draw_grid_icon);
    let r = Rect::from_center_size(pos2(x + 41.0, cy), vec2(26.0, 20.0));
    let _ = icon_button(ui, &g, r, "list", true, draw_list_icon);
    x += 66.0;
    let r = Rect::from_center_size(pos2(x + 13.0, cy), vec2(26.0, 20.0));
    let _ = icon_button(ui, &g, r, "automation", false, draw_curve_icon);
    let r = Rect::from_center_size(pos2(x + 41.0, cy), vec2(26.0, 20.0));
    let _ = icon_button(ui, &g, r, "flex", false, draw_flex_icon);
    let r = Rect::from_center_size(pos2(x + 69.0, cy), vec2(26.0, 20.0));
    if icon_button(ui, &g, r, "catch", v.catch, draw_catch_icon).clicked() {
        v.catch = !v.catch;
    }
    let left_end = x + 90.0;

    // Right-hand side, laid out from the right edge.
    // Horizontal zoom (log scale, 6..600 points per bar).
    let hz = Rect::from_min_size(pos2(bar.right() - 72.0, cy - 6.0), vec2(64.0, 12.0));
    let (lo, hi) = (6f32.ln(), 600f32.ln());
    let t = ((v.bar_w.ln() - lo) / (hi - lo)).clamp(0.0, 1.0);
    let t2 = mini_slider(ui, &g, hz, "hzoom", t);
    if t2 != t {
        v.bar_w = (lo + t2 * (hi - lo)).exp();
    }
    draw_zoom_icon(&g, pos2(hz.left() - 13.0, cy), TEXT, false);
    // Vertical zoom (track height 36..140).
    let vz = Rect::from_min_size(pos2(hz.left() - 26.0 - 54.0, cy - 6.0), vec2(54.0, 12.0));
    let t = ((v.row_h - 36.0) / (140.0 - 36.0)).clamp(0.0, 1.0);
    let t2 = mini_slider(ui, &g, vz, "vzoom", t);
    if t2 != t {
        v.row_h = 36.0 + t2 * (140.0 - 36.0);
    }
    draw_zoom_icon(&g, pos2(vz.left() - 13.0, cy), TEXT, true);
    // What fits between the two sides; the rest is left out rather than drawn on top.
    let mut rx = vz.left() - 30.0;
    // SNAP_W and DRAG_W allow for the English "Snap:" and "Drag:"; longer translations take
    // the difference from the room left.
    let label_w = |t: &str| {
        g.layout_no_wrap(t.to_owned(), FontId::proportional(12.5), TEXT_DIM)
            .size()
            .x
    };
    let longer = (label_w(tr("Snap:")) - label_w("Snap:")).max(0.0)
        + (label_w(tr("Drag:")) - label_w("Drag:")).max(0.0);
    let show = fit_toolbar(rx - left_end - longer);
    // Waveform zoom / fit buttons (not implemented yet: drawn dim).
    for i in 0..2 {
        if !show.wave {
            break;
        }
        let c = pos2(rx - 10.0 - i as f32 * 26.0, cy);
        g.line_segment(
            [c + vec2(-5.0, -6.0), c + vec2(-5.0, 6.0)],
            stroke(TEXT_DIM),
        );
        g.line_segment([c + vec2(5.0, -6.0), c + vec2(5.0, 6.0)], stroke(TEXT_DIM));
        if i == 0 {
            g.line_segment([c + vec2(-3.0, 0.0), c + vec2(3.0, 0.0)], stroke(TEXT_DIM));
        } else {
            g.line_segment([c + vec2(0.0, -4.0), c + vec2(0.0, 4.0)], stroke(TEXT_DIM));
        }
    }
    if show.wave {
        rx -= 62.0;
    }

    if show.drag {
        rx = drag_menu(ui, &g, v, rx, cy);
    }
    if show.snap {
        rx = snap_menu(ui, &g, v, rx, cy);
    }
    if show.tools {
        tool_menus(ui, &g, v, rx, cy);
    }
}

/// Which optional toolbar items are shown, given the free width between the left-hand group
/// and the zoom sliders. Logic hides toolbar items that do not fit; most useful first: Snap,
/// Drag, the two tool menus, then the waveform zoom buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ToolbarFit {
    snap: bool,
    drag: bool,
    tools: bool,
    wave: bool,
}

/// Widths (with their labels and gaps) of the optional toolbar items.
const SNAP_W: f32 = 204.0;
const DRAG_W: f32 = 160.0;
const TOOLS_W: f32 = 92.0;
const WAVE_W: f32 = 62.0;

fn fit_toolbar(free: f32) -> ToolbarFit {
    let mut left = free;
    let mut take = |w: f32| {
        let fits = left >= w;
        if fits {
            left -= w;
        }
        fits
    };
    let snap = take(SNAP_W);
    let drag = take(DRAG_W);
    let tools = take(TOOLS_W);
    let wave = take(WAVE_W);
    ToolbarFit {
        snap,
        drag,
        tools,
        wave,
    }
}

/// "Drag:" mode menu ending at `rx`; returns where the next item to its left ends.
fn drag_menu(ui: &Ui, g: &Painter, v: &mut TracksView, rx: f32, cy: f32) -> f32 {
    let drag_r = Rect::from_min_size(pos2(rx - 110.0, cy - 10.0), vec2(110.0, 20.0));
    let resp = popup_button(ui, g, drag_r, id(("popup", "drag")), v.drag_mode.label());
    egui::Popup::menu(&resp).show(|ui| {
        for m in DragMode::ALL {
            if ui.selectable_label(v.drag_mode == m, m.label()).clicked() {
                v.drag_mode = m;
                ui.close();
            }
        }
    });
    let label = g.text(
        pos2(drag_r.left() - 6.0, cy),
        Align2::RIGHT_CENTER,
        tr("Drag:"),
        FontId::proportional(12.5),
        TEXT_DIM,
    );
    label.left() - 8.0
}

/// "Snap:" menu ending at `rx`; returns where the next item to its left ends.
fn snap_menu(ui: &Ui, g: &Painter, v: &mut TracksView, rx: f32, cy: f32) -> f32 {
    let snap_r = Rect::from_min_size(pos2(rx - 150.0, cy - 10.0), vec2(150.0, 20.0));
    let resp = popup_button(ui, g, snap_r, id(("popup", "snap")), v.snap.label());
    egui::Popup::menu(&resp).show(|ui| {
        for m in Snap::ALL {
            if ui.selectable_label(v.snap == m, m.label()).clicked() {
                v.snap = m;
                ui.close();
            }
        }
    });
    let label = g.text(
        pos2(snap_r.left() - 6.0, cy),
        Align2::RIGHT_CENTER,
        tr("Snap:"),
        FontId::proportional(12.5),
        TEXT_DIM,
    );
    label.left() - 12.0
}

/// Left-click and Command-click tool menus ending at `rx` (only the Pointer works so far).
fn tool_menus(ui: &Ui, g: &Painter, v: &mut TracksView, rx: f32, cy: f32) {
    for (i, key) in ["tool_l", "tool_r"].iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rx - 40.0 - (1 - i) as f32 * 44.0, cy - 10.0),
            vec2(40.0, 20.0),
        );
        let resp = ui.interact(r, id(*key), Sense::click());
        if resp.hovered() {
            g.rect_filled(r, 4.0, Color32::from_white_alpha(14));
        }
        let icon_at = pos2(r.left() + 13.0, cy);
        if i == 0 && v.tool == TrackTool::Fade {
            draw_fade_tool(g, icon_at, TEXT);
        } else if i == 0 {
            draw_pointer(g, icon_at, TEXT);
        } else {
            draw_crosshair(g, icon_at, TEXT);
        }
        let c = pos2(r.right() - 9.0, cy + 1.0);
        g.add(Shape::line(
            vec![
                c + vec2(-3.0, -1.5),
                c + vec2(0.0, 1.5),
                c + vec2(3.0, -1.5),
            ],
            stroke(TEXT_DIM),
        ));
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(220.0);
            for (n, t) in TOOLS.iter().enumerate() {
                let tool = match n {
                    0 => Some(TrackTool::Pointer),
                    FADE_TOOL => Some(TrackTool::Fade),
                    _ => None,
                };
                match tool {
                    Some(tool) if i == 0 => {
                        if ui.selectable_label(v.tool == tool, tr(t)).clicked() {
                            v.tool = tool;
                            ui.close();
                        }
                    }
                    Some(TrackTool::Pointer) => {
                        let _ = ui.selectable_label(true, tr(t));
                    }
                    _ => todo_item(ui, t),
                }
            }
        });
    }
}

// ------------------------------------------------------------------ right-click menus

/// An entry picked in one of the Tracks area's right-click menus; run once the menus are drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CtxAction {
    NewTrack(TrackKind),
    ZoomToFit,
    OpenPianoRoll,
    /// A Pattern or Drummer region becomes an ordinary MIDI region.
    ConvertToMidi(usize, usize),
    ToggleRegionMute(usize, usize),
    ToggleReverse(usize, usize),
    ToggleTakeFolder(usize, usize),
    FlattenTakeFolder(usize, usize),
    Normalize(usize, usize),
    /// Smart Tempo: say the region's tempo (false) or make it the project's (true).
    DetectTempo(usize, usize, bool),
    /// Smart Tempo of every audio region together (multitrack).
    AdaptTempoToAll,
    SmartTempoEditor(usize, usize),
    FlexMode(usize, usize, prowave_engine::FlexMode),
    Flex(usize, usize, crate::flex_edit::Command),
    FlexRemove(usize, usize),
    RepeatRegion,
    LocatorsByRegion,
    DeleteRegion,
    CreateStack(usize),
    ReleaseStack(usize),
    ToggleMute(usize),
    ToggleSolo(usize),
    ResetHeight(usize),
    DeleteTrack(usize),
    /// Makes the track the Groove Track, or (if it is) no track.
    ToggleGrooveTrack(usize),
    ToggleFollowGroove(usize),
}

/// What the track menu needs to know about the track that was right-clicked.
struct TrackInfo {
    i: usize,
    folder: bool,
    mute: bool,
    solo: bool,
    custom_height: bool,
    last: bool,
    /// The Groove Track, and whether this track follows it (`None`: there is none).
    groove: bool,
    follow_groove: Option<bool>,
}

fn pick(ui: &mut Ui, label: &str, key: &str, action: CtxAction, picked: &mut Option<CtxAction>) {
    if ui
        .add(egui::Button::new(label).shortcut_text(key))
        .clicked()
    {
        *picked = Some(action);
    }
}

fn new_track_items(ui: &mut Ui, picked: &mut Option<CtxAction>) {
    for (label, kind) in [
        (tr("New Software Instrument Track"), TrackKind::Instrument),
        (tr("New Audio Track"), TrackKind::Audio),
        (tr("New Aux Track"), TrackKind::Aux),
        (tr("New VCA Track"), TrackKind::Vca),
        (tr("New External MIDI Track"), TrackKind::ExternalMidi),
    ] {
        pick(ui, label, "", CtxAction::NewTrack(kind), picked);
    }
}

#[allow(clippy::too_many_arguments)]
fn region_items(
    ui: &mut Ui,
    is_audio: bool,
    muted: bool,
    folder: Option<bool>,
    generated: bool,
    ti: usize,
    ri: usize,
    picked: &mut Option<CtxAction>,
) {
    if !is_audio {
        let open = if generated {
            tr("Open Editor")
        } else {
            tr("Open in Piano Roll")
        };
        pick(ui, open, "", CtxAction::OpenPianoRoll, picked);
        if generated {
            pick(
                ui,
                tr("Convert to MIDI Region"),
                "",
                CtxAction::ConvertToMidi(ti, ri),
                picked,
            );
        }
        ui.separator();
    }
    if let Some(open) = folder {
        let toggle = if open {
            tr("Close Take Folder")
        } else {
            tr("Open Take Folder")
        };
        pick(ui, toggle, "", CtxAction::ToggleTakeFolder(ti, ri), picked);
        pick(
            ui,
            tr("Flatten Take Folder"),
            "",
            CtxAction::FlattenTakeFolder(ti, ri),
            picked,
        );
        ui.separator();
    }
    let mute = if muted {
        tr("Unmute Region")
    } else {
        tr("Mute Region")
    };
    pick(ui, mute, "", CtxAction::ToggleRegionMute(ti, ri), picked);
    if is_audio {
        pick(
            ui,
            tr("Normalize Region Gain"),
            "",
            CtxAction::Normalize(ti, ri),
            picked,
        );
        ui.menu_button(tr("Smart Tempo"), |ui| {
            pick(
                ui,
                tr("Smart Tempo Editor…"),
                "",
                CtxAction::SmartTempoEditor(ti, ri),
                picked,
            );
            pick(
                ui,
                tr("Detect Tempo"),
                "",
                CtxAction::DetectTempo(ti, ri, false),
                picked,
            );
            pick(
                ui,
                tr("Adapt Project Tempo to Region"),
                "",
                CtxAction::DetectTempo(ti, ri, true),
                picked,
            );
            pick(
                ui,
                tr("Adapt Project Tempo to All Audio Regions"),
                "",
                CtxAction::AdaptTempoToAll,
                picked,
            );
            pick(
                ui,
                tr("Region Follows Project Tempo"),
                "",
                CtxAction::Flex(ti, ri, crate::flex_edit::Command::FollowProjectTempo),
                picked,
            );
        });
        ui.menu_button(tr("Flex Time"), |ui| {
            ui.menu_button(tr("Flex Mode"), |ui| {
                for mode in prowave_engine::FlexMode::ALL {
                    pick(
                        ui,
                        crate::flex_edit::mode_label(mode),
                        "",
                        CtxAction::FlexMode(ti, ri, mode),
                        picked,
                    );
                }
            });
            pick(
                ui,
                tr("Quantize Audio to 1/16"),
                "",
                CtxAction::Flex(ti, ri, crate::flex_edit::Command::Quantize { grid: 0.25 }),
                picked,
            );
            pick(
                ui,
                tr("Quantize Audio to 1/8"),
                "",
                CtxAction::Flex(ti, ri, crate::flex_edit::Command::Quantize { grid: 0.5 }),
                picked,
            );
            pick(
                ui,
                tr("Remove Flex Edits"),
                "",
                CtxAction::FlexRemove(ti, ri),
                picked,
            );
        });
        ui.menu_button(tr("Flex Pitch"), |ui| {
            pick(
                ui,
                tr("Correct Pitch to Key"),
                "",
                CtxAction::Flex(ti, ri, crate::flex_edit::Command::CorrectPitch),
                picked,
            );
            pick(
                ui,
                tr("Create MIDI Track from Flex Pitch"),
                "",
                CtxAction::Flex(ti, ri, crate::flex_edit::Command::MidiFromPitch),
                picked,
            );
        });
        pick(
            ui,
            tr("Reverse Region"),
            "",
            CtxAction::ToggleReverse(ti, ri),
            picked,
        );
    }
    let repeat_key = ui
        .ctx()
        .format_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::R));
    pick(
        ui,
        tr("Repeat Region"),
        &repeat_key,
        CtxAction::RepeatRegion,
        picked,
    );
    pick(
        ui,
        tr("Set Locators by Region"),
        "",
        CtxAction::LocatorsByRegion,
        picked,
    );
    ui.separator();
    pick(ui, tr("Delete"), "⌫", CtxAction::DeleteRegion, picked);
}

fn track_items(ui: &mut Ui, t: &TrackInfo, picked: &mut Option<CtxAction>) {
    new_track_items(ui, picked);
    ui.separator();
    pick(
        ui,
        tr("Create Track Stack"),
        "",
        CtxAction::CreateStack(t.i),
        picked,
    );
    if t.folder {
        pick(
            ui,
            tr("Release Track Stack"),
            "",
            CtxAction::ReleaseStack(t.i),
            picked,
        );
    }
    ui.separator();
    let mute = if t.mute {
        tr("Unmute Track")
    } else {
        tr("Mute Track")
    };
    pick(ui, mute, "", CtxAction::ToggleMute(t.i), picked);
    let solo = if t.solo {
        tr("Unsolo Track")
    } else {
        tr("Solo Track")
    };
    pick(ui, solo, "", CtxAction::ToggleSolo(t.i), picked);
    if t.custom_height {
        pick(
            ui,
            tr("Reset Track Height"),
            "",
            CtxAction::ResetHeight(t.i),
            picked,
        );
    }
    ui.separator();
    let groove = if t.groove {
        tr("Clear Groove Track")
    } else {
        tr("Set as Groove Track")
    };
    pick(ui, groove, "", CtxAction::ToggleGrooveTrack(t.i), picked);
    if let Some(follows) = t.follow_groove {
        let label = if follows {
            tr("Stop Following Groove Track")
        } else {
            tr("Follow Groove Track")
        };
        pick(ui, label, "", CtxAction::ToggleFollowGroove(t.i), picked);
    }
    ui.separator();
    let delete = ui.add_enabled(!t.last, egui::Button::new(tr("Delete Track")));
    if delete.clicked() {
        *picked = Some(CtxAction::DeleteTrack(t.i));
    }
}

/// Runs a right-click menu entry.
fn apply_ctx(action: CtxAction, p: &mut Project, s: &mut UiState, lanes_w: f32) {
    let v = &mut s.tracks;
    match action {
        CtxAction::NewTrack(kind) => {
            p.add_track(kind);
        }
        CtxAction::ZoomToFit => zoom_to_fit(p, v, lanes_w),
        CtxAction::OpenPianoRoll => s.bottom = Bottom::PianoRoll,
        CtxAction::ConvertToMidi(t, r) => p.convert_to_midi_region(t, r),
        CtxAction::ToggleRegionMute(t, r) => {
            if let Some(region) = p.tracks.get_mut(t).and_then(|t| t.regions.get_mut(r)) {
                region.params.mute = !region.params.mute;
                p.song_rev += 1;
            }
        }
        CtxAction::ToggleTakeFolder(t, r) => {
            if let Some(f) = p
                .tracks
                .get_mut(t)
                .and_then(|t| t.regions.get_mut(r))
                .and_then(|r| r.folder.as_mut())
            {
                f.open = !f.open;
            }
        }
        // The comp as it is becomes an ordinary region: the other takes are let go.
        CtxAction::FlattenTakeFolder(t, r) => {
            if let Some(region) = p.tracks.get_mut(t).and_then(|t| t.regions.get_mut(r)) {
                region.folder = None;
            }
        }
        CtxAction::ToggleReverse(t, r) => {
            if let Some(region) = p.tracks.get_mut(t).and_then(|t| t.regions.get_mut(r)) {
                region.params.reverse = !region.params.reverse;
            }
        }
        CtxAction::Normalize(t, r) => start_normalize(p, s, t, r),
        CtxAction::DetectTempo(t, r, adapt) => crate::smart_tempo::start(p, s, t, r, adapt),
        CtxAction::AdaptTempoToAll => crate::smart_tempo::start_all(p, s),
        CtxAction::ToggleGrooveTrack(i) => {
            let was = p.tracks.get(i).is_some_and(|t| t.params.groove);
            crate::groove::set_groove_track(p, (!was).then_some(i));
        }
        CtxAction::ToggleFollowGroove(i) => {
            if let Some(t) = p.tracks.get_mut(i) {
                t.params.follow_groove = !t.params.follow_groove;
                p.song_rev += 1;
            }
        }
        CtxAction::SmartTempoEditor(t, r) => crate::smart_tempo::open_editor(p, s, t, r),
        CtxAction::FlexMode(t, r, mode) => {
            crate::flex_edit::set_mode(p, t, r, mode);
            p.song_rev += 1;
        }
        CtxAction::Flex(t, r, command) => crate::flex_edit::start(p, s, t, r, command),
        CtxAction::FlexRemove(t, r) => s.flex.remove_later(t, r),
        CtxAction::RepeatRegion => repeat_selected(p, v),
        CtxAction::LocatorsByRegion => set_locators_by_region(p, v),
        CtxAction::DeleteRegion => delete_selected(p, v),
        CtxAction::CreateStack(i) => {
            p.create_track_stack(i);
            forget_track_indexes(s);
        }
        CtxAction::ReleaseStack(i) => {
            if p.release_track_stack(i) {
                forget_track_indexes(s);
            }
        }
        CtxAction::ToggleMute(i) => {
            let mute = !p.tracks[i].mute;
            p.set_folder_flags(i, Some(mute), None);
        }
        CtxAction::ToggleSolo(i) => {
            let solo = !p.tracks[i].solo;
            p.set_folder_flags(i, None, Some(solo));
        }
        CtxAction::ResetHeight(i) => p.tracks[i].height = 1.0,
        CtxAction::DeleteTrack(i) => {
            if p.delete_track(i) {
                forget_track_indexes(s);
            }
        }
    }
}

/// Logic's Normalize Region Gain: finds the region's loudest sample on a background thread,
/// then [`poll_normalize`] sets the gain that brings it to 0 dBFS. The file is not changed.
fn start_normalize(p: &Project, s: &mut UiState, track: usize, region: usize) {
    let Some(audio) = p
        .tracks
        .get(track)
        .and_then(|t| t.regions.get(region))
        .and_then(|r| r.audio.as_ref())
    else {
        s.status = Some(tr("Normalize: this region has no recorded audio").into());
        return;
    };
    let jobs = &mut s.tracks.normalize_jobs;
    if jobs.iter().any(|j| (j.track, j.region) == (track, region)) {
        return;
    }
    let file = audio.file.clone();
    let path = file.path.clone();
    let (tx, result) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("prowave-normalize".into())
        .spawn(move || {
            let peak = prowave_engine::peak_of(&file).map_err(|e| e.to_string());
            let _ = tx.send(peak);
        });
    match spawned {
        Ok(_) => {
            jobs.push(NormalizeJob {
                track,
                region,
                path,
                result,
            });
            s.status = Some(tr("Normalizing…").into());
        }
        Err(e) => s.status = Some(format!("cannot start the normalize scan: {e}")),
    }
}

/// Applies the Normalize Region Gain scans that have finished. Call once per frame.
pub fn poll_normalize(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    use std::sync::mpsc::TryRecvError;
    if s.tracks.normalize_jobs.is_empty() {
        return;
    }
    let mut finished = Vec::new();
    let mut message = None;
    s.tracks
        .normalize_jobs
        .retain(|job| match job.result.try_recv() {
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
            Ok(peak) => {
                finished.push((job.track, job.region, job.path.clone(), peak));
                false
            }
        });
    for (track, region, path, peak) in finished {
        // The project may have changed while the file was being read.
        let Some(r) = p
            .tracks
            .get_mut(track)
            .and_then(|t| t.regions.get_mut(region))
            .filter(|r| r.audio.as_ref().is_some_and(|a| a.file.path == path))
        else {
            continue;
        };
        match peak {
            Ok(peak) => match crate::model::RegionParams::normalize_gain(peak) {
                Some(gain) => r.params.gain_db = gain,
                None => message = Some(tr("Normalize: the region is silent").to_string()),
            },
            Err(e) => {
                message = Some(trf("Normalize: cannot read {}: {}", &[&path.display(), &e]));
            }
        }
    }
    if s.tracks.normalize_jobs.is_empty() {
        // Clears "Normalizing…" (or shows what went wrong).
        s.status = message;
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Track numbers shifted, so a remembered region or note selection may point at another track.
fn forget_track_indexes(s: &mut UiState) {
    s.tracks.selected = None;
    s.piano.selected.clear();
}

// ------------------------------------------------------------------ actions

pub(crate) fn delete_selected(p: &mut Project, v: &mut TracksView) {
    if let Some((t, r)) = v.selected.take() {
        if let Some(track) = p.tracks.get_mut(t) {
            if r < track.regions.len() {
                track.regions.remove(r);
                p.song_rev += 1;
            }
        }
    }
}

fn repeat_selected(p: &mut Project, v: &mut TracksView) {
    let Some((t, r)) = v.selected else { return };
    let Some(track) = p.tracks.get_mut(t) else {
        return;
    };
    let Some(src) = track.regions.get(r).cloned() else {
        return;
    };
    let copy = Region {
        start_bar: src.start_bar + src.len_bars,
        ..src
    };
    track.regions.push(copy);
    v.selected = Some((t, track.regions.len() - 1));
    p.song_rev += 1;
}

fn set_locators_by_region(p: &mut Project, v: &TracksView) {
    let Some((t, r)) = v.selected else { return };
    let Some(reg) = p.tracks.get(t).and_then(|tr| tr.regions.get(r)) else {
        return;
    };
    let (start, end) = (p.region_start_beats(reg), p.region_end_beats(reg));
    p.cycle_start = start;
    p.cycle_end = end;
    p.cycle = true;
}

pub(crate) fn zoom_to_fit(p: &Project, v: &mut TracksView, lanes_w: f32) {
    let end = p
        .tracks
        .iter()
        .flat_map(|t| t.regions.iter())
        .map(|r| r.start_bar - 1.0 + r.len_bars)
        .fold(0.0f32, f32::max);
    if end > 0.0 {
        v.bar_w = ((lanes_w - 20.0) / end).clamp(6.0, 600.0);
        v.scroll_x = 0.0;
    }
}

// ------------------------------------------------------------------ drawing

/// The body colour of a region. Logic draws regions deeper than the track colour; these four
/// were measured from Logic Pro itself (yellow is estimated from the others).
const LOGIC_REGION_COLORS: [([u8; 3], [u8; 3]); 4] = [
    ([0x4e, 0xb0, 0x4a], [43, 150, 53]),
    ([0x3f, 0x5f, 0xc0], [60, 102, 176]),
    ([0xd9, 0xaa, 0x2a], [196, 158, 26]),
    ([0x7a, 0x4f, 0xc0], [97, 63, 175]),
];

/// A selected region keeps its colour but gets deeper and more saturated (Logic: a green
/// region goes from 43, 150, 53 to 15, 136, 29).
fn selected_fill(c: Color32) -> Color32 {
    let (r, g, b) = (c.r() as f32, c.g() as f32, c.b() as f32);
    let lum = 0.3 * r + 0.59 * g + 0.11 * b;
    let k = |v: f32| ((lum + (v - lum) * 1.45) * 0.8).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(k(r), k(g), k(b))
}

fn region_fill(c: Color32) -> Color32 {
    if let Some((_, [r, g, b])) = LOGIC_REGION_COLORS
        .iter()
        .find(|(track, _)| *track == [c.r(), c.g(), c.b()])
    {
        return Color32::from_rgb(*r, *g, *b);
    }
    // Any other colour gets the same kind of darkening.
    let ch = |v: u8| v as f32 * 0.72;
    let (r, g, b) = (ch(c.r()), ch(c.g()), ch(c.b()));
    let lum = 0.3 * r + 0.59 * g + 0.11 * b;
    let sat = |v: f32| (lum + (v - lum) * 1.1).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(sat(r), sat(g), sat(b))
}

// ------------------------------------------------------------------ take folders

/// Height of one take's row in an opened take folder.
const TAKE_ROW_H: f32 = 22.0;
/// The yellow header of a take folder.
const FOLDER_HEAD_H: f32 = 14.0;
/// Logic's take folder header is yellow.
const FOLDER_YELLOW: Color32 = Color32::from_rgb(253, 197, 0);

/// Extra height a track's row needs for its opened take folders (one row per take).
fn take_rows_height(t: &Track) -> f32 {
    t.regions
        .iter()
        .filter_map(|r| r.folder.as_ref())
        .filter(|f| f.open)
        .map(|f| f.takes.len() as f32 * TAKE_ROW_H)
        .fold(0.0, f32::max)
}

/// The triangle that opens and closes a take folder, at the left of its header.
fn folder_toggle_rect(full: Rect) -> Rect {
    Rect::from_min_size(full.min, vec2(16.0, FOLDER_HEAD_H))
}

/// The row of take number `take` (0 = first take) in an opened folder: the latest take is on
/// top, as in Logic. `full` is the folder's own rectangle.
fn take_row_rect(full: Rect, takes: usize, take: usize) -> Rect {
    let k = takes - 1 - take;
    Rect::from_min_size(
        pos2(full.left(), full.bottom() + 1.0 + k as f32 * TAKE_ROW_H),
        vec2(full.width(), TAKE_ROW_H - 2.0),
    )
}

/// Thin note lines (pitch up, time right) of `notes` in `inner`, clipped to `clip`.
#[allow(clippy::too_many_arguments)]
fn draw_note_lines<'a>(
    g: &Painter,
    inner: Rect,
    left: f32,
    beat_px: f32,
    notes: impl Iterator<Item = &'a crate::model::Note>,
    lo: f32,
    hi: f32,
    ink: Color32,
) {
    let step = (2.0f32).min((inner.height() - 4.0) / (hi - lo).max(1.0));
    let mid = (lo + hi) / 2.0;
    for n in notes {
        let nx = left + n.start * beat_px;
        if nx > inner.right() || nx + n.len * beat_px < inner.left() {
            continue;
        }
        let ny = inner.center().y - (n.pitch as f32 - mid) * step;
        let nr = Rect::from_min_max(
            pos2(nx.max(inner.left()), ny - 0.75),
            pos2(
                (nx + (n.len * beat_px).max(2.0)).min(inner.right()),
                ny + 0.75,
            ),
        );
        g.rect_filled(nr, 0.0, ink);
    }
}

/// A take folder as Logic draws it: a yellow header with the name and a triangle, and under
/// it the comp, the stretches of the takes that play, side by side with thin lines between.
/// Opened, a row for each take follows, the stretches the comp uses lit in the region colour.
#[allow(clippy::too_many_arguments)]
fn draw_take_folder(
    g: &Painter,
    full: Rect,
    track: &Track,
    region: &Region,
    folder: &crate::model::TakeFolder,
    beat_px: f32,
    selected: bool,
    silenced: bool,
) {
    let base = if region.params.mute {
        Color32::from_gray(0x6a)
    } else {
        track.color
    };
    let fill = if silenced {
        Color32::from_rgb(0x2e, 0x2e, 0x30)
    } else if selected {
        selected_fill(region_fill(base))
    } else {
        region_fill(base)
    };
    let ink = if silenced {
        Color32::from_gray(0x8c)
    } else {
        fill.lerp_to_gamma(Color32::WHITE, if selected { 0.75 } else { 0.6 })
    };
    let head_h = FOLDER_HEAD_H.min(full.height() * 0.4);
    let round = 4;
    let body = Rect::from_min_max(pos2(full.left(), full.top() + head_h), full.max);
    // The comp: one block per stretch, rounded at the ends of the folder only.
    let last = folder.comp.len().saturating_sub(1);
    for (i, seg) in folder.comp.iter().enumerate() {
        let x0 = (full.left() + seg.start * beat_px).max(full.left());
        let x1 = (full.left() + seg.end * beat_px).min(full.right());
        if x1 <= x0 {
            continue;
        }
        g.rect_filled(
            Rect::from_min_max(pos2(x0, body.top()), pos2(x1, body.bottom())),
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: if i == 0 { round } else { 0 },
                se: if i == last { round } else { 0 },
            },
            fill,
        );
    }
    // The header.
    let head = Rect::from_min_size(full.min, vec2(full.width(), head_h));
    let head_fill = if silenced {
        Color32::from_gray(0x70)
    } else {
        FOLDER_YELLOW
    };
    g.rect_filled(
        head,
        egui::CornerRadius {
            nw: round,
            ne: round,
            sw: 0,
            se: 0,
        },
        head_fill,
    );
    let dark = Color32::from_rgb(40, 30, 0);
    // The triangle: pointing right when closed, down when opened.
    let c = pos2(full.left() + 8.0, head.center().y);
    let tri = if folder.open {
        vec![
            c + vec2(-3.5, -2.0),
            c + vec2(3.5, -2.0),
            c + vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + vec2(-2.0, -3.5),
            c + vec2(-2.0, 3.5),
            c + vec2(3.0, 0.0),
        ]
    };
    g.add(Shape::convex_polygon(tri, dark, Stroke::NONE));
    g.text(
        head.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        &region.name,
        FontId::proportional(10.5),
        dark,
    );
    // Where one stretch gives way to the next: a notch in the header and a line below it.
    for seg in folder.comp.iter().skip(1) {
        let x = full.left() + seg.start * beat_px;
        if x <= full.left() || x >= full.right() {
            continue;
        }
        g.line_segment(
            [pos2(x, head.bottom() - 4.0), pos2(x, head.bottom())],
            Stroke::new(1.0, dark),
        );
        g.line_segment(
            [pos2(x, body.top()), pos2(x, body.bottom())],
            Stroke::new(1.0, Color32::from_black_alpha(150)),
        );
    }
    // What the comp plays.
    let (lo, hi) = pitch_range(&region.notes);
    let inner = Rect::from_min_max(
        pos2(body.left(), body.top() + 2.0),
        body.max - vec2(0.0, 2.0),
    );
    draw_note_lines(
        g,
        inner,
        full.left(),
        beat_px,
        region.notes.iter(),
        lo,
        hi,
        ink,
    );
    let border = if selected {
        SELECTED_EDGE
    } else {
        Color32::from_black_alpha(110)
    };
    g.rect_stroke(
        full,
        egui::CornerRadius::same(round),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );

    // The takes, newest on top.
    if folder.open {
        for (take_idx, take) in folder.takes.iter().enumerate() {
            let row = take_row_rect(full, folder.takes.len(), take_idx);
            g.rect_filled(row, 3.0, Color32::from_gray(0x3a));
            let row_inner =
                Rect::from_min_max(pos2(row.left(), row.top() + 2.0), row.max - vec2(0.0, 2.0));
            let (lo, hi) = pitch_range(&take.notes);
            // The stretches of this take that the comp plays are lit.
            for seg in folder.comp.iter().filter(|s| s.take == take_idx) {
                let x0 = (full.left() + seg.start * beat_px).max(full.left());
                let x1 = (full.left() + seg.end * beat_px).min(full.right());
                if x1 > x0 {
                    g.rect_filled(
                        Rect::from_min_max(pos2(x0, row.top()), pos2(x1, row.bottom())),
                        3.0,
                        fill,
                    );
                }
            }
            let used = |start: f32| {
                folder
                    .comp
                    .iter()
                    .any(|s| s.take == take_idx && start >= s.start && start < s.end)
            };
            draw_note_lines(
                g,
                row_inner,
                full.left(),
                beat_px,
                take.notes.iter().filter(|n| used(n.start)),
                lo,
                hi,
                ink,
            );
            draw_note_lines(
                g,
                row_inner,
                full.left(),
                beat_px,
                take.notes.iter().filter(|n| !used(n.start)),
                lo,
                hi,
                Color32::from_gray(0x86),
            );
            g.text(
                row.left_top() + vec2(5.0, 1.0),
                Align2::LEFT_TOP,
                trf("Take {}", &[&(take_idx + 1)]),
                FontId::proportional(9.5),
                Color32::from_white_alpha(200),
            );
        }
    }
}

/// Lowest and highest pitch of `notes` (a default span when there are none).
fn pitch_range(notes: &[crate::model::Note]) -> (f32, f32) {
    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(48) as f32;
    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(72) as f32;
    (lo, hi)
}

#[allow(clippy::too_many_arguments)]
fn draw_region(
    g: &Painter,
    rr: Rect,
    full: Rect,
    track: &Track,
    region: &Region,
    fades: (f32, f32),
    beat_px: f32,
    selected: bool,
    recording: bool,
    silenced: bool,
    seed: u32,
) {
    // Muted regions are drawn grey, like Logic.
    let base = if region.params.mute {
        Color32::from_gray(0x6a)
    } else {
        track.color
    };
    let body = if recording {
        // The region being recorded is red until the recording stops.
        RECORDING_FILL
    } else if silenced {
        // Another track is soloed: dark grey body, grey content, name in the track colour.
        Color32::from_rgb(0x2e, 0x2e, 0x30)
            .lerp_to_gamma(Color32::WHITE, if selected { 0.06 } else { 0.0 })
    } else if selected {
        selected_fill(region_fill(base))
    } else {
        region_fill(base)
    };
    g.rect_filled(rr, 3.0, body);
    let head_h = 14.0f32.min(rr.height() * 0.4);
    let head = Rect::from_min_size(rr.min, vec2(rr.width(), head_h));
    g.rect_filled(
        head,
        egui::CornerRadius {
            nw: 3,
            ne: 3,
            sw: 0,
            se: 0,
        },
        body.lerp_to_gamma(Color32::WHITE, if silenced { 0.03 } else { 0.0 }),
    );
    let name_color = if silenced {
        base.lerp_to_gamma(Color32::WHITE, 0.15)
    } else {
        body.lerp_to_gamma(Color32::WHITE, if selected { 0.7 } else { 0.85 })
    };
    if let Some(generated) = &region.generated {
        if head.width() > 44.0 {
            draw_generated_mark(
                g,
                head,
                matches!(generated.source, crate::model::Generator::Drummer(_)),
                name_color,
            );
        }
    }
    g.text(
        head.left_center() + vec2(5.0, 0.0),
        Align2::LEFT_CENTER,
        &region.name,
        FontId::proportional(10.5),
        name_color,
    );
    let inner = Rect::from_min_max(
        pos2(rr.left(), head.bottom() + 2.0),
        rr.max - vec2(0.0, 2.0),
    );
    // A Flex edit: its mode in the corner of the name bar and its markers over the waveform.
    if let Some((a, f)) = region
        .audio
        .as_ref()
        .and_then(|a| a.flex.as_ref().map(|f| (a, f)))
    {
        if head.width() > 90.0 {
            let mark = if f.pitch.is_empty() {
                crate::flex_edit::mode_label(f.mode).to_string()
            } else {
                format!("{} ♪", crate::flex_edit::mode_label(f.mode))
            };
            g.text(
                head.right_center() - vec2(5.0, 0.0),
                Align2::RIGHT_CENTER,
                mark,
                FontId::proportional(9.5),
                name_color,
            );
        }
        let secs = a.file.duration_secs().max(1e-3);
        for m in &f.markers {
            let x = full.left() + (m.dst / secs) as f32 * full.width();
            if x > inner.left() && x < inner.right() {
                g.line_segment(
                    [pos2(x, inner.top()), pos2(x, inner.bottom())],
                    Stroke::new(1.0, Color32::from_rgb(0xff, 0xb0, 0x30).gamma_multiply(0.7)),
                );
            }
        }
    }
    let ink = if recording {
        Color32::from_rgb(236, 152, 155)
    } else if silenced {
        Color32::from_gray(0x8c)
    } else {
        // Logic draws the content in a pale tint of the region colour, not pure white.
        body.lerp_to_gamma(Color32::WHITE, if selected { 0.75 } else { 0.6 })
    };
    let edit = region.audio.is_some() || track.kind == TrackKind::Audio;
    if let Some(peaks) = region.audio.as_ref().and_then(|a| a.peaks.get()) {
        let gain = 10f32.powf(region.params.gain_db / 20.0);
        let full_x = (full.left(), full.width());
        draw_peaks(g, inner, full_x, peaks, gain, region.params.reverse, ink);
    } else if region.audio.is_some() {
        g.line_segment(
            [
                pos2(inner.left(), inner.center().y),
                pos2(inner.right(), inner.center().y),
            ],
            Stroke::new(1.0, ink),
        );
    } else if track.kind == TrackKind::Audio {
        fake_waveform(g, inner, full.left(), beat_px, ink, seed);
    } else if !region.notes.is_empty() {
        let lo = region.notes.iter().map(|n| n.pitch).min().unwrap_or(48) as f32;
        let hi = region.notes.iter().map(|n| n.pitch).max().unwrap_or(72) as f32;
        // Logic keeps a fixed, small step per semitone and centres the notes in the region.
        let step = (2.0f32).min((inner.height() - 4.0) / (hi - lo).max(1.0));
        let mid = (lo + hi) / 2.0;
        for n in &region.notes {
            let nx = full.left() + n.start * beat_px;
            if nx > inner.right() || nx + n.len * beat_px < inner.left() {
                continue;
            }
            let ny = inner.center().y - (n.pitch as f32 - mid) * step;
            let nr = Rect::from_min_max(
                pos2(nx.max(inner.left()), ny - 0.75),
                pos2(
                    (nx + (n.len * beat_px).max(2.0)).min(inner.right()),
                    ny + 0.75,
                ),
            );
            g.rect_filled(nr, 0.0, ink);
        }
    }
    if edit {
        draw_fades(g, rr, inner, full, region, fades, beat_px);
    }
    let border = if recording {
        Color32::from_rgb(120, 22, 22)
    } else if selected {
        SELECTED_EDGE
    } else if silenced {
        Color32::from_gray(0x44)
    } else {
        Color32::from_black_alpha(110)
    };
    if selected && !recording {
        // Logic outlines a selected region in yellow-green with a dark line around it.
        g.rect_stroke(
            rr,
            3.0,
            Stroke::new(1.0, Color32::from_black_alpha(220)),
            StrokeKind::Outside,
        );
    }
    g.rect_stroke(rr, 3.0, Stroke::new(1.0, border), StrokeKind::Inside);
}

/// A small mark at the right of a region's header that tells a Drummer region (a drum with
/// its sticks) from a Pattern region (a grid of steps).
fn draw_generated_mark(g: &Painter, head: Rect, drummer: bool, color: Color32) {
    let c = pos2(head.right() - 10.0, head.center().y);
    let stroke = Stroke::new(1.0, color);
    if drummer {
        g.rect_stroke(
            Rect::from_center_size(c + vec2(0.0, 1.5), vec2(8.0, 4.5)),
            1.0,
            stroke,
            StrokeKind::Middle,
        );
        g.line_segment([c + vec2(-4.0, -2.0), c + vec2(-1.0, -4.5)], stroke);
        g.line_segment([c + vec2(4.0, -2.0), c + vec2(1.0, -4.5)], stroke);
    } else {
        for row in 0..2 {
            for col in 0..3 {
                let at = c + vec2(col as f32 * 3.6 - 3.6, row as f32 * 3.6 - 1.8);
                g.rect_filled(Rect::from_center_size(at, vec2(2.6, 2.6)), 0.5, color);
            }
        }
    }
}

/// Height (0.3..1.0) of a demo audio track's pseudo-waveform `beats` into its region. It
/// depends only on the position in the region, so the waveform moves with the region when the
/// view scrolls or zooms.
fn fake_level(seed: u32, beats: f32) -> f32 {
    let step = (beats * 16.0).floor() as u32;
    let mut s = seed ^ step.wrapping_mul(0x9e37_79b9);
    s = s.wrapping_mul(1664525).wrapping_add(1013904223);
    s ^= s >> 15;
    s = s.wrapping_mul(0x2c1b_3c6d);
    s ^= s >> 12;
    ((beats * 1.6).sin().abs() * 0.6 + 0.3) * ((s >> 24) as f32 / 255.0 * 0.6 + 0.4)
}

/// Demo audio tracks without a file get a stable pseudo-waveform. `rect` is the visible part
/// of the region; `x0` is where the whole region starts on screen.
fn fake_waveform(g: &Painter, rect: Rect, x0: f32, beat_px: f32, col: Color32, seed: u32) {
    let mid = rect.center().y;
    // Start on the same 2-px grid as the region's left edge, so columns do not shimmer.
    let mut x = x0 + ((rect.left() - x0) / 2.0).floor().max(0.0) * 2.0;
    while x < rect.right() {
        let h = fake_level(seed, (x - x0) / beat_px.max(1e-3)) * rect.height() * 0.45;
        g.line_segment([pos2(x, mid - h), pos2(x, mid + h)], Stroke::new(1.0, col));
        x += 2.0;
    }
}

/// Real waveform from min/max peaks. `x0`/`full_w` describe the unclipped region.
/// Shades what the region's fades take away: a triangle from the region's start up to where
/// the fade in ends, and one down to the region's end for the fade out, with the curve's line.
fn draw_fades(
    g: &Painter,
    rr: Rect,
    inner: Rect,
    full: Rect,
    region: &Region,
    (fin, fout): (f32, f32),
    beat_px: f32,
) {
    if fin <= 0.0 && fout <= 0.0 {
        return;
    }
    let g = g.with_clip_rect(g.clip_rect().intersect(rr));
    let shade = Color32::from_black_alpha(110);
    let line = Stroke::new(1.2, Color32::from_white_alpha(220));
    let curve = |x: f32| match region.params.fade_curve {
        prowave_engine::FadeCurve::Linear => x,
        prowave_engine::FadeCurve::EqualPower => (x * std::f32::consts::FRAC_PI_2).sin(),
        prowave_engine::FadeCurve::SCurve => x * x * (3.0 - 2.0 * x),
    };
    // `x0` where the fade is silent, `x1` where it reaches full level.
    let fade = |x0: f32, x1: f32| {
        const STEPS: usize = 16;
        let pts: Vec<Pos2> = (0..=STEPS)
            .map(|k| {
                let t = k as f32 / STEPS as f32;
                let y = inner.bottom() - curve(t) * inner.height();
                pos2(x0 + (x1 - x0) * t, y)
            })
            .collect();
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            g.add(Shape::convex_polygon(
                vec![pos2(a.x, inner.top()), pos2(b.x, inner.top()), b, a],
                shade,
                Stroke::NONE,
            ));
        }
        g.add(Shape::line(pts, line));
    };
    if fin > 0.0 {
        fade(full.left(), full.left() + fin * beat_px);
    }
    if fout > 0.0 {
        fade(full.right(), full.right() - fout * beat_px);
    }
}

/// Draws a recorded waveform: `full_x` is the left edge and width of the whole region on
/// screen, `gain` its region gain (linear) and `reverse` plays it back to front.
fn draw_peaks(
    g: &Painter,
    body: Rect,
    (x0, full_w): (f32, f32),
    peaks: &[(f32, f32)],
    gain: f32,
    reverse: bool,
    col: Color32,
) {
    if peaks.is_empty() || full_w <= 0.0 {
        return;
    }
    let mid = body.center().y;
    let half = body.height() * 0.48;
    let mut x = body.left().floor();
    while x < body.right() {
        let a = (((x - x0) / full_w) * peaks.len() as f32).max(0.0) as usize;
        if a >= peaks.len() {
            break;
        }
        let b =
            ((((x + 1.0 - x0) / full_w) * peaks.len() as f32) as usize).clamp(a + 1, peaks.len());
        let (a, b) = if reverse {
            (peaks.len() - b, peaks.len() - a)
        } else {
            (a, b)
        };
        let (lo, hi) = peaks[a..b]
            .iter()
            .fold((0f32, 0f32), |(l, h), p| (l.min(p.0), h.max(p.1)));
        let (lo, hi) = (lo * gain, hi * gain);
        g.line_segment(
            [
                pos2(x, mid - hi.min(1.0) * half),
                pos2(x, mid - lo.max(-1.0) * half),
            ],
            Stroke::new(1.0, col),
        );
        x += 1.0;
    }
}

/// Triangle pointing down (open) or right (folded), drawn as a shape so it needs no font glyph.
fn disclosure_triangle(g: &Painter, c: Pos2, open: bool, col: Color32) {
    let pts = if open {
        vec![
            c + vec2(-4.0, -2.5),
            c + vec2(4.0, -2.5),
            c + vec2(0.0, 3.5),
        ]
    } else {
        vec![
            c + vec2(-2.5, -4.0),
            c + vec2(-2.5, 4.0),
            c + vec2(3.5, 0.0),
        ]
    };
    g.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// A folder's M and S buttons: they set the same state on every track inside the folder.
fn folder_buttons(ui: &Ui, g: &Painter, p: &mut Project, i: usize, x: f32, y: f32, selected: bool) {
    let size = vec2(18.0, 15.0);
    let (mute, solo) = (p.tracks[i].mute, p.tracks[i].solo);
    if header_button(
        ui,
        g,
        Rect::from_min_size(pos2(x, y), size),
        (i, "m"),
        "M",
        mute,
        MUTE_ON,
        selected,
    ) {
        p.set_folder_flags(i, Some(!mute), None);
    }
    if header_button(
        ui,
        g,
        Rect::from_min_size(pos2(x + 21.0, y), size),
        (i, "s"),
        "S",
        solo,
        SOLO_ON,
        selected,
    ) {
        p.set_folder_flags(i, None, Some(!solo));
    }
}

const SLIDER_W: f32 = 58.0;
const SLIDER_H: f32 = 14.0;

/// Where 0 dB sits on a track header's volume slider (Logic puts unity about two thirds up).
const SLIDER_UNITY: f32 = 0.65;

/// Slider position (0..1) for a volume in dB (-60..+6): most of the travel is below unity,
/// the last third is the boost to +6 dB.
fn db_to_slider(db: f32) -> f32 {
    let db = db.clamp(-60.0, 6.0);
    if db >= 0.0 {
        SLIDER_UNITY + (1.0 - SLIDER_UNITY) * db / 6.0
    } else {
        SLIDER_UNITY * ((db + 60.0) / 60.0).sqrt()
    }
}

fn slider_to_db(f: f32) -> f32 {
    let f = f.clamp(0.0, 1.0);
    if f >= SLIDER_UNITY {
        (f - SLIDER_UNITY) / (1.0 - SLIDER_UNITY) * 6.0
    } else {
        -60.0 + 60.0 * (f / SLIDER_UNITY).powi(2)
    }
}

/// Draws track `i`'s header and returns the response of its background (for the right-click
/// menu; the buttons on it handle their own clicks).
fn header(
    ui: &Ui,
    g: &Painter,
    p: &mut Project,
    i: usize,
    hdr: Rect,
    peaks: (f32, f32),
    silenced: bool,
) -> egui::Response {
    let selected = p.selected_track == i;
    g.rect_filled(hdr, 0.0, if selected { HEADER_SEL } else { HEADER_BG });
    g.line_segment(
        [hdr.left_bottom(), hdr.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x2a)),
    );
    let num = Rect::from_min_size(hdr.min, vec2(NUM_W, hdr.height()));
    g.text(
        num.center(),
        Align2::CENTER_CENTER,
        (i + 1).to_string(),
        FontId::proportional(12.0),
        TEXT_DIM,
    );
    let resp = ui.interact(hdr, id(("hdr", i)), Sense::click());
    if resp.clicked() || context_click(&resp) {
        p.selected_track = i;
    }

    let h = hdr.height();
    let icon_size = (h - 14.0).clamp(22.0, 40.0);
    let indent = f32::from(p.tracks[i].indent) * INDENT_W;
    let folder = p.tracks[i].kind == TrackKind::Folder;
    let content_x = hdr.left() + NUM_W + 6.0 + indent;
    if folder {
        // Fold arrow: click to hide or show the tracks inside.
        let arrow = Rect::from_center_size(pos2(content_x + 5.0, hdr.center().y), vec2(14.0, 20.0));
        if ui
            .interact(arrow, id(("fold", i)), Sense::click())
            .clicked()
        {
            p.tracks[i].folded = !p.tracks[i].folded;
        }
        disclosure_triangle(g, arrow.center(), !p.tracks[i].folded, TEXT_DIM);
    }
    let icon = Rect::from_min_size(
        pos2(
            content_x + if folder { 14.0 } else { 0.0 },
            hdr.center().y - icon_size / 2.0,
        ),
        vec2(icon_size, icon_size),
    );
    draw_icon(g, icon, track_icon(&p.tracks[i]));

    let name_x = icon.right() + 10.0;
    let compact = h < 46.0;
    let name_y = if compact {
        hdr.center().y
    } else {
        hdr.center().y - 9.0
    };
    let name_w = hdr.right() - 122.0 - name_x;
    let name_galley = g.layout_no_wrap(p.tracks[i].name.clone(), FontId::proportional(13.0), TEXT);
    let clip = g.with_clip_rect(
        Rect::from_min_size(pos2(name_x, hdr.top()), vec2(name_w.max(20.0), h))
            .intersect(g.clip_rect()),
    );
    clip.galley(
        pos2(name_x, name_y - name_galley.size().y / 2.0),
        name_galley,
        TEXT,
    );

    // A folder only has M and S, which act on everything inside it, and no volume or pan.
    if folder {
        if !compact {
            folder_buttons(ui, g, p, i, name_x, hdr.center().y + 3.0, selected);
        }
        return resp;
    }

    // M S R (I) buttons under the name (compact tracks: hidden, like Logic at small heights).
    let audio = p.tracks[i].kind == TrackKind::Audio;
    let t = &mut p.tracks[i];
    if !compact {
        let by = hdr.center().y + 3.0;
        let mut bx = name_x;
        let size = vec2(18.0, 15.0);
        if header_button(
            ui,
            g,
            Rect::from_min_size(pos2(bx, by), size),
            (i, "m"),
            "M",
            t.mute,
            MUTE_ON,
            selected,
        ) {
            t.mute = !t.mute;
        }
        bx += 21.0;
        if header_button(
            ui,
            g,
            Rect::from_min_size(pos2(bx, by), size),
            (i, "s"),
            "S",
            t.solo,
            SOLO_ON,
            selected,
        ) {
            t.solo = !t.solo;
        }
        bx += 21.0;
        // Aux and Folder tracks hold no regions, so there is nothing to record into.
        if t.kind.has_regions()
            && header_button(
                ui,
                g,
                Rect::from_min_size(pos2(bx, by), size),
                (i, "r"),
                "R",
                t.rec,
                REC_ON,
                selected,
            )
        {
            t.rec = !t.rec;
        }
        if audio {
            bx += 21.0;
            if header_button(
                ui,
                g,
                Rect::from_min_size(pos2(bx, by), size),
                (i, "i"),
                "I",
                t.monitor,
                MON_ON,
                selected,
            ) {
                t.monitor = !t.monitor;
            }
        }
    }

    // Volume slider (dB, -60..+6; double-click resets to 0 dB) and pan knob.
    // Logic's slider: a dark pill with a grey thumb that stays inside it.
    let sl = Rect::from_min_size(
        pos2(hdr.right() - 98.0, hdr.center().y - SLIDER_H / 2.0),
        vec2(SLIDER_W, SLIDER_H),
    );
    let travel = (sl.left() + SLIDER_H / 2.0, sl.right() - SLIDER_H / 2.0);
    let resp = ui.interact(
        sl.expand2(vec2(4.0, 6.0)),
        id(("vol", i)),
        Sense::click_and_drag(),
    );
    if let Some(pt) = resp.interact_pointer_pos() {
        let f = (pt.x - travel.0) / (travel.1 - travel.0);
        t.volume_db = slider_to_db(f.clamp(0.0, 1.0));
    }
    if resp.double_clicked() {
        t.volume_db = 0.0;
    }
    g.rect_filled(sl, SLIDER_H / 2.0, Color32::from_rgb(55, 54, 54));
    // Level meter inside the pill, as in Logic: two thin bars (L over R) that follow the
    // slider's own scale, green, yellow from -6 dB and red above 0 dB.
    let silent = t.mute || silenced;
    for (k, peak) in [peaks.0, peaks.1].into_iter().enumerate() {
        let db = crate::mixer::lin_to_db(peak);
        if db <= -60.0 {
            continue;
        }
        let y = sl.center().y - 3.0 + k as f32 * 3.5;
        let x0 = sl.left() + 3.0;
        let x_at = |db: f32| x0 + (sl.right() - 3.0 - x0) * db_to_slider(db);
        let mut from = -60.0f32;
        for edge in [-6.0f32, 0.0, 6.0] {
            let to = db.min(edge);
            if to > from {
                let r = Rect::from_min_max(pos2(x_at(from), y), pos2(x_at(to), y + 2.5));
                let color = if silent {
                    Color32::from_gray(0x70)
                } else {
                    crate::mixer::meter_colour(to - 0.01)
                };
                g.rect_filled(r, 1.0, color);
            }
            from = edge;
        }
    }
    let knob = pos2(
        travel.0 + (travel.1 - travel.0) * db_to_slider(t.volume_db),
        sl.center().y,
    );
    g.circle_filled(knob, SLIDER_H / 2.0 - 1.0, Color32::from_rgb(136, 134, 135));
    g.circle_stroke(
        knob,
        SLIDER_H / 2.0 - 1.0,
        Stroke::new(1.0, Color32::from_rgb(98, 97, 97)),
    );

    let kc = pos2(hdr.right() - 22.0, hdr.center().y);
    let kresp = ui.interact(
        Rect::from_center_size(kc, vec2(22.0, 22.0)),
        id(("pan", i)),
        Sense::click_and_drag(),
    );
    if kresp.dragged() {
        t.pan = (t.pan - kresp.drag_delta().y * 0.01).clamp(-1.0, 1.0);
    }
    if kresp.double_clicked() {
        t.pan = 0.0;
    }
    crate::widgets::draw_knob(g, kc, 9.0, t.pan);
    resp
}

// ------------------------------------------------------------------ main view

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState, meters: &crate::mixer::Meters) {
    let rec_target = s.rec.target();
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let tb = Rect::from_min_size(area.min, vec2(area.width(), TOOLBAR_H));
    let ruler_y = tb.bottom();
    // Global tracks between the ruler and the tracks: Marker, then Tempo, as in Logic.
    let marker_h = if s.tracks.show_markers {
        crate::marker_track::HEIGHT
    } else {
        0.0
    };
    let tempo_h = if s.tracks.show_tempo {
        crate::tempo_track::HEIGHT
    } else {
        0.0
    };
    let signature_h = if s.tracks.show_signatures {
        crate::signature_track::HEIGHT
    } else {
        0.0
    };
    let tracks_y = ruler_y + RULER_H + marker_h + signature_h + tempo_h;
    let headers = Rect::from_min_max(
        pos2(area.left(), tracks_y),
        pos2(area.left() + HEADER_W, area.bottom()),
    );
    let lanes = Rect::from_min_max(pos2(area.left() + HEADER_W, tracks_y), area.max);
    let ruler = Rect::from_min_max(
        pos2(lanes.left(), ruler_y),
        pos2(area.right(), ruler_y + RULER_H),
    );
    let corner = Rect::from_min_max(
        pos2(area.left(), ruler_y),
        pos2(headers.right(), ruler.bottom()),
    );

    toolbar(ui, p, s, tb, lanes.width());

    // Zoom is counted in pixels per bar of the first bar's length.
    let bpb = p.bar_len(0);
    let v = &mut s.tracks;
    v.lanes_w = lanes.width();

    // ---- wheel: scroll; Shift = horizontal; Ctrl/Cmd = horizontal zoom around the pointer
    let hover = ui.input(|i| i.pointer.hover_pos());
    if hover.is_some_and(|pt| lanes.contains(pt) || headers.contains(pt) || ruler.contains(pt)) {
        let (delta, mods) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers));
        if mods.command && delta.y != 0.0 {
            let pointer_x = hover.map_or(lanes.left(), |p| p.x);
            let beat_at = (pointer_x - lanes.left() + v.scroll_x) / (v.bar_w / bpb as f32);
            v.bar_w = (v.bar_w * (1.0 + delta.y * 0.003)).clamp(6.0, 600.0);
            v.scroll_x = beat_at * (v.bar_w / bpb as f32) - (pointer_x - lanes.left());
        } else if mods.shift {
            v.scroll_x -= delta.y + delta.x;
        } else {
            v.scroll_x -= delta.x;
            v.scroll_y -= delta.y;
        }
    }
    let rows = row_layout(p, v.row_h);
    let content_h = rows.last().map_or(0.0, |(top, h)| top + h);
    v.scroll_y = v
        .scroll_y
        .clamp(0.0, (content_h - lanes.height() + 40.0).max(0.0));
    v.scroll_x = v.scroll_x.clamp(0.0, 1000.0 * v.bar_w);

    let beat_px = v.bar_w / bpb as f32;
    // Catch: page the view when the playhead leaves it.
    if v.catch && p.playing {
        let px = p.position_beats as f32 * beat_px - v.scroll_x;
        if px > lanes.width() - 30.0 || px < 0.0 {
            v.scroll_x = (p.position_beats as f32 * beat_px - lanes.width() * 0.1).max(0.0);
        }
    }
    let (scroll_x, scroll_y) = (v.scroll_x, v.scroll_y);
    let x_of = |beat: f64| lanes.left() + beat as f32 * beat_px - scroll_x;
    let beat_of = |x: f32| ((x - lanes.left() + scroll_x) / beat_px).max(0.0) as f64;

    // ---- lanes background + grid
    let g = ui.painter_at(lanes);
    g.rect_filled(lanes, 0.0, LANE_BG);
    let bars = p.bars_in(beat_of(lanes.left()), beat_of(lanes.right()));
    for (i, &(top, row_h)) in rows.iter().enumerate() {
        if row_h <= 0.0 {
            continue;
        }
        let y = lanes.top() + top - scroll_y;
        let row = Rect::from_min_size(pos2(lanes.left(), y), vec2(lanes.width(), row_h));
        if p.selected_track == i {
            g.rect_filled(row, 0.0, LANE_SEL);
        }
        if p.tracks[i].kind == TrackKind::Folder {
            g.rect_filled(row, 0.0, Color32::from_white_alpha(10));
        }
        g.line_segment(
            [row.left_bottom(), row.right_bottom()],
            Stroke::new(1.0, Color32::from_gray(0x22)),
        );
    }
    for bar in &bars {
        let x = x_of(bar.start);
        g.line_segment(
            [pos2(x, lanes.top()), pos2(x, lanes.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x38)),
        );
        if beat_px * bar.unit as f32 >= 14.0 {
            for k in 1..bar.num {
                let xb = x_of(bar.start + k as f64 * bar.unit);
                g.line_segment(
                    [pos2(xb, lanes.top()), pos2(xb, lanes.bottom())],
                    Stroke::new(1.0, Color32::from_gray(0x30)),
                );
            }
        }
    }

    // Click on an empty lane: select that track, deselect regions.
    let lane_resp = ui.interact(lanes, id("lanes"), Sense::click());
    let mut picked: Option<CtxAction> = None;
    if lane_resp.clicked() || context_click(&lane_resp) {
        if let Some(pt) = lane_resp.interact_pointer_pos() {
            if let Some(row) = row_at(&rows, pt.y - lanes.top() + scroll_y) {
                p.selected_track = row;
            }
        }
        v.selected = None;
    }
    // Double-click on an empty stretch of a Drummer track: a Drummer region there, as in Logic.
    if lane_resp.double_clicked() {
        if let Some(pt) = lane_resp.interact_pointer_pos() {
            let drummer_row = row_at(&rows, pt.y - lanes.top() + scroll_y)
                .filter(|row| p.tracks[*row].kind == TrackKind::Drummer);
            if let Some(row) = drummer_row {
                let bar = p.beats_to_bar(beat_of(pt.x)).floor() as f32 + 1.0;
                if let Some(ri) = p.add_generated_region(row, true, bar) {
                    p.selected_track = row;
                    v.selected = Some((row, ri));
                }
            }
        }
    }
    context_menu(&lane_resp, |ui| {
        new_track_items(ui, &mut picked);
        ui.separator();
        if ui.button(tr("Zoom to Fit")).clicked() {
            picked = Some(CtxAction::ZoomToFit);
        }
    });

    // ---- regions
    let step = snap_step(v.snap, beat_px, bpb, p.beat_unit_at(beat_of(lanes.left())));
    // Snapping to bars follows the signature, so it goes to the nearest bar line.
    let snap_bars = snaps_to_bars(v.snap, beat_px, p.beat_unit_at(beat_of(lanes.left())));
    let sig_owned = p.sigs().to_owned();
    let snap_pos = move |b: f64| -> f64 {
        if snap_bars {
            sig_owned.as_ref().nearest_bar_line(b)
        } else {
            snap_to(b, step)
        }
    };
    let mut moved = false;
    for (ti, &(top, row_h)) in rows.iter().enumerate() {
        if row_h <= 0.0 {
            continue;
        }
        let y = lanes.top() + top - scroll_y;
        if y + row_h < lanes.top() || y > lanes.bottom() {
            continue;
        }
        let base_h = row_h - take_rows_height(&p.tracks[ti]);
        for ri in 0..p.tracks[ti].regions.len() {
            let start_beat = p.region_start_beats(&p.tracks[ti].regions[ri]);
            let len_beats = p.region_len_beats(&p.tracks[ti].regions[ri]);
            let full = Rect::from_min_max(
                pos2(x_of(start_beat), y + 1.0),
                pos2(x_of(start_beat + len_beats), y + base_h - 1.0),
            );
            let rr = full.intersect(lanes);
            if rr.width() <= 0.0 || rr.height() <= 0.0 {
                continue;
            }
            let resp = ui.interact(rr, id(("region", ti, ri)), Sense::click_and_drag());
            if resp.clicked() || resp.drag_started() || context_click(&resp) {
                v.selected = Some((ti, ri));
                p.selected_track = ti;
            }
            if resp.double_clicked() && p.tracks[ti].kind != TrackKind::Audio {
                s.bottom = Bottom::PianoRoll;
            }
            let fade_tool = v.tool == TrackTool::Fade && p.tracks[ti].kind == TrackKind::Audio;
            if fade_tool && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if fade_tool {
                // The Fade tool: the left half of the region gets a fade in, the right half a
                // fade out, as long as the pointer is dragged (in 1/16 notes, as the Inspector).
                if resp.drag_started() {
                    if let Some(pt) = resp.interact_pointer_pos() {
                        v.fade_drag = Some(beat_of(pt.x) < start_beat + len_beats / 2.0);
                    }
                }
                if let (true, Some(fade_in), Some(pt)) =
                    (resp.dragged(), v.fade_drag, resp.interact_pointer_pos())
                {
                    let beats = fade_for_drag(beat_of(pt.x), start_beat, len_beats, fade_in);
                    let params = &mut p.tracks[ti].regions[ri].params;
                    if fade_in {
                        params.fade_in = beats;
                    } else {
                        params.fade_out = beats;
                    }
                }
                if resp.drag_stopped() {
                    v.fade_drag = None;
                    moved = true;
                }
            } else {
                if resp.drag_started() {
                    if let Some(pt) = resp.interact_pointer_pos() {
                        v.grab_beats = beat_of(pt.x) - start_beat;
                    }
                }
                if resp.dragged() {
                    if let Some(pt) = resp.interact_pointer_pos() {
                        let nb = snap_pos((beat_of(pt.x) - v.grab_beats).max(0.0));
                        p.tracks[ti].regions[ri].start_bar = p.beats_to_bar(nb) as f32 + 1.0;
                    }
                }
                if resp.drag_stopped() {
                    moved = true;
                    // Dropped on another audio region: X-Fade crossfades them, the other
                    // modes leave them overlapping.
                    if p.tracks[ti].kind == TrackKind::Audio && overlaps_another(p, ti, ri) {
                        p.tracks[ti].regions[ri].params.crossfade = v.drag_mode == DragMode::XFade;
                    }
                }
            }
            let (is_audio, muted) = (
                p.tracks[ti].kind == TrackKind::Audio,
                p.tracks[ti].regions[ri].params.mute,
            );
            let folder_open = p.tracks[ti].regions[ri].folder.as_ref().map(|f| f.open);
            let generated = p.tracks[ti].regions[ri].generated.is_some();
            let shown = v.shot_menu == Some((ti, Some(ri)));
            crate::widgets::context_menu_shown(&resp, shown, |ui| {
                region_items(
                    ui,
                    is_audio,
                    muted,
                    folder_open,
                    generated,
                    ti,
                    ri,
                    &mut picked,
                );
            });
            let selected = v.selected == Some((ti, ri));
            if folder_open.is_some() {
                // The triangle opens and closes the folder; the rows of an opened one are
                // swiped to pick which take plays where.
                let tri = ui.interact(
                    folder_toggle_rect(full).intersect(lanes),
                    id(("take_toggle", ti, ri)),
                    Sense::click(),
                );
                if tri.clicked() {
                    picked = Some(CtxAction::ToggleTakeFolder(ti, ri));
                    v.selected = Some((ti, ri));
                }
                if folder_open == Some(true) {
                    let n = p.tracks[ti].regions[ri]
                        .folder
                        .as_ref()
                        .map_or(0, |f| f.takes.len());
                    for take in 0..n {
                        let row = take_row_rect(full, n, take).intersect(lanes);
                        if row.width() <= 0.0 || row.height() <= 0.0 {
                            continue;
                        }
                        let resp = ui.interact(
                            row,
                            id(("take_row", ti, ri, take)),
                            Sense::click_and_drag(),
                        );
                        let Some(region) = p.tracks[ti].regions.get_mut(ri) else {
                            continue;
                        };
                        let Some(folder) = region.folder.as_mut() else {
                            continue;
                        };
                        let grab =
                            |pt: egui::Pos2| snap_pos((beat_of(pt.x) - start_beat).max(0.0)) as f32;
                        if resp.drag_started() {
                            if let Some(pt) = resp.interact_pointer_pos() {
                                v.swipe = Some((folder.comp.clone(), grab(pt)));
                            }
                        }
                        if resp.dragged() {
                            if let (Some((orig, anchor)), Some(pt)) =
                                (v.swipe.as_ref(), resp.interact_pointer_pos())
                            {
                                folder.comp = orig.clone();
                                folder.swipe(take, *anchor, grab(pt));
                                folder.apply(&mut region.notes, &mut region.controls);
                            }
                        }
                        if resp.drag_stopped() {
                            v.swipe = None;
                            moved = true;
                        }
                        if resp.clicked() {
                            // A click picks the whole take.
                            let len = folder.len;
                            folder.swipe(take, 0.0, len);
                            folder.apply(&mut region.notes, &mut region.controls);
                            moved = true;
                        }
                    }
                }
                if let Some(folder) = p.tracks[ti].regions[ri].folder.as_ref() {
                    draw_take_folder(
                        &g,
                        full,
                        &p.tracks[ti],
                        &p.tracks[ti].regions[ri],
                        folder,
                        beat_px,
                        selected,
                        silenced_by_solo(p, ti),
                    );
                    continue;
                }
            }
            let tr = &p.tracks[ti];
            draw_region(
                &g,
                rr,
                full,
                tr,
                &tr.regions[ri],
                p.effective_fades(ti, ri),
                beat_px,
                selected,
                rec_target == Some((ti, ri)),
                silenced_by_solo(p, ti),
                ti as u32 * 77 + ri as u32 * 13 + 3,
            );
        }
    }
    if moved {
        p.song_rev += 1;
    }

    if marker_h > 0.0 {
        let top = ruler_y + RULER_H;
        crate::marker_track::show(
            ui,
            p,
            Rect::from_min_max(
                pos2(area.left(), top),
                pos2(headers.right(), top + marker_h),
            ),
            Rect::from_min_max(pos2(lanes.left(), top), pos2(area.right(), top + marker_h)),
            x_of,
            beat_of,
            &snap_pos,
        );
    }
    if signature_h > 0.0 {
        let top = ruler_y + RULER_H + marker_h;
        crate::signature_track::show(
            ui,
            p,
            Rect::from_min_max(
                pos2(area.left(), top),
                pos2(headers.right(), top + signature_h),
            ),
            Rect::from_min_max(
                pos2(lanes.left(), top),
                pos2(area.right(), top + signature_h),
            ),
            x_of,
            beat_of,
        );
    }
    if tempo_h > 0.0 {
        let top = ruler_y + RULER_H + marker_h + signature_h;
        crate::tempo_track::show(
            ui,
            p,
            Rect::from_min_max(pos2(area.left(), top), pos2(headers.right(), tracks_y)),
            Rect::from_min_max(pos2(lanes.left(), top), pos2(area.right(), tracks_y)),
            x_of,
            beat_of,
            &snap_pos,
        );
    }

    // ---- keyboard: Delete, Cmd/Ctrl+R
    if !ui.ctx().egui_wants_keyboard_input() && !s.piano_focus {
        let (del, rep) = ui.input(|i| {
            (
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.modifiers.command && i.key_pressed(Key::R),
            )
        });
        if del {
            delete_selected(p, v);
        }
        if rep {
            repeat_selected(p, v);
        }
    }

    // ---- headers
    let gh = ui.painter_at(headers);
    gh.rect_filled(headers, 0.0, Color32::from_gray(0x3a));
    for (i, &(top, row_h)) in rows.iter().enumerate() {
        if row_h <= 0.0 {
            continue;
        }
        let y = headers.top() + top - scroll_y;
        if y + row_h < headers.top() || y > headers.bottom() {
            continue;
        }
        let hdr = Rect::from_min_size(pos2(headers.left(), y), vec2(HEADER_W, row_h));
        let peaks = meters.tracks.get(i).copied().unwrap_or((0.0, 0.0));
        let silenced = silenced_by_solo(p, i);
        let resp = header(ui, &gh, p, i, hdr, peaks, silenced);
        let t = &p.tracks[i];
        let info = TrackInfo {
            i,
            folder: t.kind == TrackKind::Folder,
            mute: t.mute,
            solo: t.solo,
            custom_height: t.height != 1.0,
            last: p.tracks.len() == 1,
            groove: t.params.groove,
            follow_groove: crate::groove::groove_track(p)
                .filter(|g| *g != i)
                .map(|_| t.params.follow_groove),
        };
        let shown = s.tracks.shot_menu == Some((i, None));
        crate::widgets::context_menu_shown(&resp, shown, |ui| track_items(ui, &info, &mut picked));
    }
    if let Some(action) = picked {
        apply_ctx(action, p, s, lanes.width());
    }
    let v = &mut s.tracks;

    // ---- individual track height: drag the lower edge of a track (header or lane), as in
    // Logic. Registered after headers and regions so the edge wins over them.
    let edges = Rect::from_min_max(pos2(headers.left(), lanes.top()), lanes.right_bottom());
    for (i, &(top, row_h)) in rows.iter().enumerate() {
        if row_h <= 0.0 {
            continue;
        }
        let bottom = lanes.top() + top + row_h - scroll_y;
        if bottom < edges.top() || bottom > edges.bottom() {
            continue;
        }
        let edge = Rect::from_min_max(
            pos2(edges.left(), bottom - RESIZE_GRIP),
            pos2(edges.right(), bottom + RESIZE_GRIP),
        )
        .intersect(edges);
        let resp = ui
            .interact(edge, id(("track_edge", i)), Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if resp.dragged() {
            if let (Some(pt), Some(t)) = (resp.interact_pointer_pos(), p.tracks.get_mut(i)) {
                let wanted = pt.y - (bottom - row_h) - take_rows_height(t);
                t.height = (wanted / v.row_h).clamp(Track::MIN_HEIGHT, Track::MAX_HEIGHT);
            }
        }
        if resp.double_clicked() {
            if let Some(t) = p.tracks.get_mut(i) {
                t.height = 1.0;
            }
        }
    }
    gh.line_segment(
        [headers.right_top(), headers.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x22)),
    );

    // ---- corner above the headers: "+" (new track), duplicate track, global tracks
    let gc = ui.painter_at(corner);
    gc.rect_filled(corner, 0.0, Color32::from_gray(0x3a));
    gc.line_segment(
        [corner.left_bottom(), corner.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let plus = Rect::from_center_size(
        pos2(corner.left() + 18.0, corner.center().y),
        vec2(24.0, 18.0),
    );
    let pr = ui.interact(plus, id("plus"), Sense::click());
    gc.rect_filled(
        plus,
        4.0,
        if pr.hovered() {
            Color32::from_gray(0x66)
        } else {
            BTN_BG
        },
    );
    gc.line_segment(
        [
            plus.center() + vec2(-5.0, 0.0),
            plus.center() + vec2(5.0, 0.0),
        ],
        stroke(TEXT),
    );
    gc.line_segment(
        [
            plus.center() + vec2(0.0, -5.0),
            plus.center() + vec2(0.0, 5.0),
        ],
        stroke(TEXT),
    );
    if pr.clicked() {
        p.add_track(TrackKind::Instrument);
    }
    let dup = Rect::from_center_size(
        pos2(plus.right() + 18.0, corner.center().y),
        vec2(24.0, 18.0),
    );
    let dr = ui.interact(dup, id("dup"), Sense::click());
    let dup_bg = if dr.hovered() {
        Color32::from_gray(0x66)
    } else {
        BTN_BG
    };
    gc.rect_filled(dup, 4.0, dup_bg);
    gc.rect_stroke(
        Rect::from_center_size(dup.center() + vec2(1.5, -1.5), vec2(8.0, 7.0)),
        1.0,
        stroke(TEXT),
        StrokeKind::Middle,
    );
    gc.rect_filled(
        Rect::from_center_size(dup.center() + vec2(-1.5, 1.5), vec2(8.0, 7.0)),
        1.0,
        dup_bg,
    );
    gc.rect_stroke(
        Rect::from_center_size(dup.center() + vec2(-1.5, 1.5), vec2(8.0, 7.0)),
        1.0,
        stroke(TEXT),
        StrokeKind::Middle,
    );
    if dr.clicked() {
        if let Some(t) = p.tracks.get(p.selected_track).cloned() {
            p.tracks.insert(p.selected_track + 1, t);
            p.vca_inserted(p.selected_track + 1);
            // The copy keeps its VCA (the shift above also moved the copy's own entry).
            let copy = p.selected_track + 1;
            let vca = p.tracks[p.selected_track].vca;
            p.tracks[copy].vca = vca;
            p.selected_track += 1;
            p.song_rev += 1;
        }
    }
    let gt = Rect::from_center_size(
        pos2(corner.right() - 18.0, corner.center().y),
        vec2(24.0, 18.0),
    );
    gc.rect_stroke(
        Rect::from_center_size(gt.center(), vec2(14.0, 10.0)),
        1.5,
        stroke(TEXT_DIM),
        StrokeKind::Middle,
    );
    gc.line_segment(
        [
            gt.center() + vec2(-7.0, -1.5),
            gt.center() + vec2(7.0, -1.5),
        ],
        stroke(TEXT_DIM),
    );

    // ---- ruler
    let gr = ui.painter_at(ruler);
    gr.rect_filled(ruler, 0.0, RULER_BG);
    gr.line_segment(
        [ruler.left_bottom(), ruler.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    // The stretch of the ruler over the selected region turns gold, as in Logic.
    let selection = v
        .selected
        .and_then(|(t, r)| p.tracks.get(t)?.regions.get(r))
        .map(|r| (p.region_start_beats(r), p.region_end_beats(r)));
    if let Some((start, end)) = selection {
        let band = Rect::from_min_max(
            pos2(x_of(start).max(ruler.left()), ruler.top()),
            pos2(x_of(end).min(ruler.right()), ruler.bottom()),
        );
        if band.width() > 0.0 {
            gr.rect_filled(band, 0.0, RULER_SELECTION);
            gr.line_segment(
                [band.left_bottom(), band.right_bottom()],
                Stroke::new(1.0, RULER_SELECTION_EDGE),
            );
        }
    }
    if p.cycle {
        let band = Rect::from_min_max(
            pos2(x_of(p.cycle_start), ruler.top() + 2.0),
            pos2(x_of(p.cycle_end), ruler.bottom() - 1.0),
        );
        gr.rect_filled(band, 2.0, CYCLE);
    }
    let label_every = [1i64, 2, 4, 8, 16, 32]
        .into_iter()
        .find(|n| *n as f32 * v.bar_w >= 34.0)
        .unwrap_or(64);
    for bar in &bars {
        let b = bar.bar0 as i64;
        let x = x_of(bar.start);
        let beat = bar.start;
        let in_cycle = p.cycle && beat >= p.cycle_start && beat < p.cycle_end;
        let in_selection = selection.is_some_and(|(s0, s1)| beat >= s0 && beat < s1);
        let col = if in_cycle {
            Color32::from_gray(0x2a)
        } else if in_selection {
            Color32::WHITE
        } else {
            TEXT
        };
        if b % label_every == 0 {
            gr.line_segment(
                [pos2(x, ruler.top() + 4.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, col.gamma_multiply(0.7)),
            );
            gr.text(
                pos2(x + 3.0, ruler.top() + 4.0),
                Align2::LEFT_TOP,
                (b + 1).to_string(),
                FontId::proportional(11.5),
                col,
            );
        } else {
            gr.line_segment(
                [pos2(x, ruler.bottom() - 7.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, col.gamma_multiply(0.6)),
            );
        }
        if beat_px * bar.unit as f32 >= 10.0 {
            for k in 1..bar.num {
                let xb = x_of(bar.start + k as f64 * bar.unit);
                gr.line_segment(
                    [pos2(xb, ruler.bottom() - 4.0), pos2(xb, ruler.bottom())],
                    Stroke::new(1.0, col.gamma_multiply(0.45)),
                );
            }
        }
    }
    // Upper half: click toggles cycle, drag sets it (snapped to bars). Lower half: locate.
    let upper = Rect::from_min_max(ruler.min, pos2(ruler.right(), ruler.center().y));
    let lower = Rect::from_min_max(pos2(ruler.left(), ruler.center().y), ruler.max);
    let up = ui.interact(upper, id("ruler_cycle"), Sense::click_and_drag());
    if up.drag_started() {
        v.cycle_anchor = up
            .interact_pointer_pos()
            .map(|pt| p.sigs().nearest_bar_line(beat_of(pt.x)));
    }
    if let (Some(anchor), true, Some(pt)) =
        (v.cycle_anchor, up.dragged(), up.interact_pointer_pos())
    {
        let other = p.sigs().nearest_bar_line(beat_of(pt.x));
        if (other - anchor).abs() >= p.bar_len_at(anchor.min(other)) - 1e-9 {
            p.cycle_start = anchor.min(other);
            p.cycle_end = anchor.max(other);
            p.cycle = true;
        }
    }
    if up.drag_stopped() {
        v.cycle_anchor = None;
    }
    if up.clicked() {
        p.cycle = !p.cycle;
    }
    let low = ui.interact(lower, id("ruler_locate"), Sense::click_and_drag());
    if let Some(pt) = low.interact_pointer_pos() {
        if low.clicked() || low.dragged() {
            p.position_beats = beat_of(pt.x);
        }
    }

    // ---- playhead
    let px = x_of(p.position_beats);
    // Grab the playhead's head in the ruler and drag it anywhere in the song (back to the
    // start, past the end of the view, ...). Registered after the ruler so the head wins over
    // the cycle area; kept alive while dragging even when the head leaves the view.
    let head_id = id("playhead_head");
    let visible = px >= lanes.left() && px <= lanes.right();
    if visible || ui.ctx().is_being_dragged(head_id) {
        let grab = Rect::from_min_max(
            pos2(px - PLAYHEAD_GRAB, ruler.top()),
            pos2(px + PLAYHEAD_GRAB, ruler.bottom()),
        );
        let resp = ui
            .interact(grab, head_id, Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            if let Some(pt) = resp.interact_pointer_pos() {
                p.position_beats = playhead_beat(pt.x, lanes.left(), scroll_x, beat_px);
            }
        }
    }
    if visible {
        let (line_c, head_c, edge_c) = playhead_colors(p.recording);
        let gp = ui.painter_at(Rect::from_min_max(ruler.min, area.max));
        gp.line_segment(
            [pos2(px, ruler.top() + 6.0), pos2(px, area.bottom())],
            Stroke::new(1.0, line_c),
        );
        let head = vec![
            pos2(px - 6.0, ruler.top() + 2.0),
            pos2(px + 6.0, ruler.top() + 2.0),
            pos2(px + 6.0, ruler.top() + 9.0),
            pos2(px, ruler.top() + 15.0),
            pos2(px - 6.0, ruler.top() + 9.0),
        ];
        gp.add(Shape::convex_polygon(
            head,
            head_c,
            Stroke::new(1.0, edge_c),
        ));
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;

    #[test]
    fn volume_slider_round_trips_and_puts_unity_two_thirds_up() {
        assert_eq!(db_to_slider(0.0), SLIDER_UNITY);
        assert_eq!(db_to_slider(-60.0), 0.0);
        assert_eq!(db_to_slider(6.0), 1.0);
        for db in [-60.0, -30.0, -12.5, -0.1, 0.0, 2.0, 6.0] {
            assert!((slider_to_db(db_to_slider(db)) - db).abs() < 1e-3, "{db}");
        }
    }

    #[test]
    fn the_toolbar_drops_its_least_used_items_first_when_narrow() {
        let all = SNAP_W + DRAG_W + TOOLS_W + WAVE_W;
        let f = fit_toolbar(all);
        assert!(f.snap && f.drag && f.tools && f.wave);
        let f = fit_toolbar(all - 1.0);
        assert!(
            f.snap && f.drag && f.tools && !f.wave,
            "waveform buttons go first"
        );
        let f = fit_toolbar(SNAP_W + DRAG_W);
        assert!(f.snap && f.drag && !f.tools && !f.wave);
        let f = fit_toolbar(SNAP_W);
        assert!(f.snap && !f.drag);
        assert_eq!(
            fit_toolbar(-50.0),
            ToolbarFit {
                snap: false,
                drag: false,
                tools: false,
                wave: false
            }
        );
    }

    #[test]
    fn fake_waveforms_are_tied_to_the_region_not_the_screen() {
        // Same place in the region, same height, however the view is scrolled.
        assert_eq!(fake_level(7, 3.25), fake_level(7, 3.25));
        let levels: Vec<f32> = (0..64).map(|k| fake_level(7, k as f32 / 16.0)).collect();
        assert!(levels.iter().all(|l| (0.0..=1.0).contains(l)));
        assert!(
            levels.windows(2).any(|w| w[0] != w[1]),
            "the waveform varies"
        );
        assert_ne!(
            fake_level(5, 1.0),
            fake_level(6, 1.0),
            "each track has its own"
        );
    }

    #[test]
    fn rows_follow_each_tracks_height() {
        let mut p = Project::demo();
        p.tracks.truncate(3);
        p.tracks[1].height = 2.0;
        p.tracks[2].height = 0.5;
        let rows = row_layout(&p, 56.0);
        assert_eq!(rows, vec![(0.0, 56.0), (56.0, 112.0), (168.0, 28.0)]);
        assert_eq!(row_at(&rows, 0.0), Some(0));
        assert_eq!(row_at(&rows, 167.9), Some(1));
        assert_eq!(row_at(&rows, 168.0), Some(2));
        assert_eq!(row_at(&rows, 196.0), None);
        assert_eq!(row_at(&rows, -1.0), None);
    }

    #[test]
    fn the_tracks_of_a_folded_folder_take_no_room() {
        let mut p = Project::demo();
        p.tracks.truncate(4);
        let folder = p.create_track_stack(1); // wraps track 1: [t0, F, t1, t2, t3]
        assert_eq!(folder, 1);
        let open = row_layout(&p, 56.0);
        assert_eq!(open.len(), 5);
        assert!(open.iter().all(|&(_, h)| h == 56.0));

        p.tracks[1].folded = true;
        let folded = row_layout(&p, 56.0);
        assert_eq!(folded[2].1, 0.0, "the wrapped track is hidden");
        assert_eq!(
            folded[3],
            (112.0, 56.0),
            "the next track moves up under the folder"
        );
        assert_eq!(row_at(&folded, 100.0), Some(1));
        assert_eq!(row_at(&folded, 112.0), Some(3), "hidden rows are never hit");
    }

    #[test]
    fn dragging_the_playhead_follows_the_pointer_and_stops_at_the_start() {
        // Lanes start at x = 100, scrolled by 50 px, 20 px per beat.
        assert_eq!(playhead_beat(150.0, 100.0, 50.0, 20.0), 5.0);
        assert_eq!(playhead_beat(100.0, 100.0, 0.0, 20.0), 0.0);
        // Dragged left of the lanes: pinned to the beginning of the song.
        assert_eq!(playhead_beat(20.0, 100.0, 0.0, 20.0), 0.0);
    }

    #[test]
    fn solo_silences_every_other_track_until_released() {
        let mut p = Project::demo();
        assert!((0..p.tracks.len()).all(|i| !silenced_by_solo(&p, i)));
        p.tracks[2].solo = true;
        assert!(!silenced_by_solo(&p, 2));
        assert!((0..p.tracks.len())
            .filter(|&i| i != 2)
            .all(|i| silenced_by_solo(&p, i)));
        // Soloing a second track adds it to what plays.
        p.tracks[4].solo = true;
        assert!(!silenced_by_solo(&p, 4) && silenced_by_solo(&p, 0));
        p.tracks[2].solo = false;
        p.tracks[4].solo = false;
        assert!((0..p.tracks.len()).all(|i| !silenced_by_solo(&p, i)));
    }

    #[test]
    fn beat_snap_follows_the_signature_beat() {
        // 4/4: a beat is a quarter note, as before.
        assert_eq!(snap_step(Snap::Beat, 20.0, 4.0, 1.0), 1.0);
        assert_eq!(snap_step(Snap::Smart, 20.0, 4.0, 1.0), 1.0);
        assert_eq!(snap_step(Snap::Bar, 20.0, 4.0, 1.0), 4.0);
        // 6/8: a beat is an eighth (0.5 quarter notes), a bar 3 quarter notes.
        assert_eq!(snap_step(Snap::Beat, 20.0, 3.0, 0.5), 0.5);
        assert_eq!(snap_step(Snap::Bar, 20.0, 3.0, 0.5), 3.0);
        // Smart snaps to eighths once an eighth is at least 12 px wide, else to bars.
        assert_eq!(snap_step(Snap::Smart, 24.0, 3.0, 0.5), 0.5);
        assert_eq!(snap_step(Snap::Smart, 20.0, 3.0, 0.5), 3.0);
        // Divisions are 16th notes in any signature.
        assert_eq!(snap_step(Snap::Division, 20.0, 3.0, 0.5), 0.25);
        assert_eq!(snap_step(Snap::Smart, 60.0, 3.0, 0.5), 0.25);
    }

    /// A 0.5 s mono 16-bit 44.1 kHz WAV: a square wave at `amp` (0..1), or silence for 0.
    fn write_square_wav(path: &std::path::Path, amp: f32) {
        let n = 22_050u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&88_200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            let s = if (i / 50) % 2 == 0 { amp } else { -amp };
            b.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// Runs `poll_normalize` until the scan is over (it reads the file on another thread).
    fn wait_for_normalize(p: &mut Project, s: &mut UiState) {
        let ctx = egui::Context::default();
        let began = std::time::Instant::now();
        while !s.tracks.normalize_jobs.is_empty() {
            assert!(began.elapsed().as_secs() < 10, "the scan never finished");
            poll_normalize(&ctx, p, s);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn project_with_wav(amp: f32) -> (Project, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("take.wav");
        write_square_wav(&wav, amp);
        let mut p = Project::empty();
        crate::import::import_audio(&mut p, &wav, &egui::Context::default()).unwrap();
        (p, dir)
    }

    #[test]
    fn normalize_reads_the_file_in_the_background_and_sets_the_gain() {
        let (mut p, _dir) = project_with_wav(0.25);
        let (t, mut s) = (p.tracks.len() - 1, UiState::new());
        start_normalize(&p, &mut s, t, 0);
        assert_eq!(s.tracks.normalize_jobs.len(), 1);
        assert_eq!(s.status.as_deref(), Some("Normalizing…"));
        start_normalize(&p, &mut s, t, 0);
        assert_eq!(s.tracks.normalize_jobs.len(), 1, "one scan per region");
        wait_for_normalize(&mut p, &mut s);
        // A peak of 0.25 needs +12.04 dB to reach full scale.
        let gain = p.tracks[t].regions[0].params.gain_db;
        assert!((gain - 12.04).abs() < 0.05, "gain {gain}");
        assert_eq!(s.status, None, "the message goes away when it is done");
    }

    #[test]
    fn normalize_reports_silence_and_regions_without_audio() {
        let (mut p, _dir) = project_with_wav(0.0);
        let (t, mut s) = (p.tracks.len() - 1, UiState::new());
        start_normalize(&p, &mut s, t, 0);
        wait_for_normalize(&mut p, &mut s);
        assert_eq!(p.tracks[t].regions[0].params.gain_db, 0.0);
        assert_eq!(s.status.as_deref(), Some("Normalize: the region is silent"));
        // A MIDI region has nothing to scan.
        let mut s = UiState::new();
        start_normalize(&Project::demo(), &mut s, 0, 0);
        assert!(s.tracks.normalize_jobs.is_empty());
        assert_eq!(
            s.status.as_deref(),
            Some("Normalize: this region has no recorded audio")
        );
    }

    #[test]
    fn a_scan_for_a_region_that_has_gone_changes_nothing() {
        let (mut p, _dir) = project_with_wav(0.25);
        let (t, mut s) = (p.tracks.len() - 1, UiState::new());
        start_normalize(&p, &mut s, t, 0);
        // The user deletes the region while the file is being read.
        p.tracks[t].regions.clear();
        wait_for_normalize(&mut p, &mut s);
        assert!(p.tracks[t].regions.is_empty());
        assert_eq!(s.status, None);
    }

    #[test]
    fn the_fade_tool_measures_from_the_edge_in_sixteenths() {
        // A region from beat 8, 4 beats long.
        assert_eq!(fade_for_drag(9.0, 8.0, 4.0, true), 1.0);
        assert_eq!(
            fade_for_drag(9.1, 8.0, 4.0, true),
            1.0,
            "snaps to 1/16 notes"
        );
        assert_eq!(fade_for_drag(9.2, 8.0, 4.0, true), 1.25);
        assert_eq!(fade_for_drag(11.0, 8.0, 4.0, false), 1.0, "from the end");
        assert_eq!(
            fade_for_drag(7.0, 8.0, 4.0, true),
            0.0,
            "not before the start"
        );
        assert_eq!(
            fade_for_drag(13.0, 8.0, 4.0, false),
            0.0,
            "not past the end"
        );
        assert_eq!(
            fade_for_drag(30.0, 8.0, 4.0, true),
            4.0,
            "not longer than the region"
        );
        assert_eq!(fade_for_drag(-5.0, 8.0, 4.0, false), 4.0);
    }
}
