//! Editors of Pattern and Drummer regions (Logic Pro: the Step Sequencer and the Drummer
//! Editor). They take the place of the Piano Roll in the bottom panel when the selected region
//! is one of these; the notes are made again from the settings after every change (see
//! `pattern.rs`), and "Convert to MIDI Region" turns the region into an ordinary one.
//!
//! - **Pattern:** a grid with a row for each pitch and a column for each step. Click an empty
//!   step to switch it on (the height of the click is its velocity) and drag to paint more;
//!   click a step that is on to switch it off, or drag up and down in it for its velocity;
//!   right-click or right-drag to clear. Drag a row's name to change its pitch (it sounds
//!   while it is held); right-click it to delete the row.
//! - **Drummer:** the style, the busyness and loudness on a pad (drag the dot), the swing and
//!   how often it fills, the pieces of the kit that play, and a new variation. The lanes show
//!   what it plays.
use crate::i18n::{tr, trf};
use crate::model::{Generator, Project, Track, TrackKind};
use crate::pattern::{
    drums, parts, Direction, DrumStyle, DrummerParams, PatternParams, PatternRow, StepRate,
    DEFAULT_VELOCITY, MAX_ROWS,
};
use crate::piano_roll::{velocity_color, Audition};
use crate::widgets::{context_menu, BLUE, TEXT, TEXT_DIM};
use crate::UiState;
use egui::{
    pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, Sense, Stroke, StrokeKind, Ui,
    UiBuilder,
};

const BG: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
const STRIP_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
const STRIP_H: f32 = 32.0;
const ROW_H: f32 = 24.0;
const LABEL_W: f32 = 124.0;
/// Width of the Drummer's controls.
const PANEL_W: f32 = 330.0;

/// What a click or drag in the pattern grid sets on a step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EditMode {
    /// Whether the step is on, and how hard.
    #[default]
    Velocity,
    /// The chance (percent) it plays.
    Chance,
    /// How many times it repeats within its time (click to count up, right-click to reset).
    Repeat,
    /// Whether it carries on the note before it.
    Tie,
}

impl EditMode {
    const ALL: [EditMode; 4] = [Self::Velocity, Self::Chance, Self::Repeat, Self::Tie];

    fn name(self) -> &'static str {
        match self {
            Self::Velocity => tr("Velocity"),
            Self::Chance => tr("Chance"),
            Self::Repeat => tr("Repeat"),
            Self::Tie => tr("Tie"),
        }
    }
}

/// The chance (percent) the height of the pointer in a step stands for.
fn chance_at(y: f32, cell: Rect) -> u8 {
    (((cell.bottom() - y) / cell.height()).clamp(0.0, 1.0) * 100.0).round() as u8
}

fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::Forward => tr("Forward"),
        Direction::Backward => tr("Backward"),
        Direction::PingPong => tr("Ping-Pong"),
        Direction::Random => tr("Random"),
    }
}

/// What the editors remember between frames.
#[derive(Default)]
pub struct StepView {
    mode: EditMode,
    gesture: Option<Gesture>,
    /// The pitch sounding because a row's name is held.
    held: Option<u8>,
    /// How far a row's name has been dragged towards its next pitch.
    pitch_drag: f32,
    scroll_y: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Gesture {
    /// Switching steps on as the pointer crosses them.
    Paint,
    /// Switching steps off.
    Erase,
    /// Pressed on a step that is on: a drag sets its velocity, a click switches it off.
    Velocity {
        row: usize,
        step: usize,
        from_y: f32,
        moved: bool,
    },
}

/// Whether a track plays a drum kit (so its pattern rows are named after the drums).
pub fn is_drum_track(t: &Track) -> bool {
    t.kind == TrackKind::Drummer
        || t.drums.is_some()
        || t.instrument.is_some_and(|i| i.contains("Drum"))
}

/// The name of a pattern row: a Drum Machine's own pad (a sample pad shows its file), else a
/// General MIDI drum on a drum track, else the note.
fn row_label(pitch: u8, drums: bool, kit: Option<&crate::drum_kit::DrumsUse>) -> String {
    if let Some(pad) = kit.and_then(|_| prowave_engine::drum_machine::pad_of(pitch)) {
        if let Some(kit) = kit {
            return kit.pad_label(pad);
        }
    }
    row_name(pitch, drums)
}

/// Draws the editor for the Pattern or Drummer region `ri` of track `ti` in `body`.
pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState, ti: usize, ri: usize, body: Rect) {
    let g = ui.painter_at(body);
    g.rect_filled(body, 0.0, BG);
    let drummer = matches!(
        p.tracks[ti].regions[ri]
            .generated
            .as_ref()
            .map(|g| &g.source),
        Some(Generator::Drummer(_))
    );
    if drummer {
        show_drummer(ui, p, s, ti, ri, body);
    } else {
        show_pattern(ui, p, s, ti, ri, body);
    }
}

fn strip(ui: &Ui, body: Rect) -> Rect {
    let strip = Rect::from_min_size(body.min, vec2(body.width(), STRIP_H));
    let g = ui.painter_at(strip);
    g.rect_filled(strip, 0.0, STRIP_BG);
    g.line_segment(
        [strip.left_bottom(), strip.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    strip
}

/// A child of `ui` limited to `rect`, laying out left to right.
fn row_ui(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui)) {
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        },
    );
}

// ------------------------------------------------------------------------- names

pub(crate) const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Logic's name for a pitch: 60 is C3.
pub fn note_name(pitch: u8) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[pitch as usize % 12],
        pitch as i32 / 12 - 2
    )
}

/// The name of a General MIDI drum pitch the kit plays.
fn drum_name(pitch: u8) -> Option<String> {
    use drums::*;
    let tom = |n: u32| trf("Tom {}", &[&n]);
    Some(match pitch {
        KICK => tr("Kick").into(),
        SNARE => tr("Snare").into(),
        ELECTRIC_SNARE => tr("Electric Snare").into(),
        CLAP => tr("Clap").into(),
        CLOSED_HAT => tr("Closed Hi-Hat").into(),
        PEDAL_HAT => tr("Pedal Hi-Hat").into(),
        OPEN_HAT => tr("Open Hi-Hat").into(),
        CRASH => tr("Crash").into(),
        RIDE => tr("Ride").into(),
        TAMBOURINE => tr("Tambourine").into(),
        COWBELL => tr("Cowbell").into(),
        TOM_HIGH => tom(1),
        TOM_MID_HIGH => tom(2),
        TOM_MID => tom(3),
        TOM_LOW => tom(4),
        TOM_FLOOR => tom(5),
        TOM_FLOOR_LOW => tom(6),
        _ => return None,
    })
}

fn row_name(pitch: u8, drums: bool) -> String {
    drums
        .then(|| drum_name(pitch))
        .flatten()
        .unwrap_or_else(|| note_name(pitch))
}

fn part_name(part: u8) -> &'static str {
    match part {
        parts::KICK => tr("Kick"),
        parts::SNARE => tr("Snare"),
        parts::HIHAT => tr("Hi-Hat"),
        parts::TOMS => tr("Toms"),
        parts::CYMBALS => tr("Cymbals"),
        _ => tr("Percussion"),
    }
}

fn style_name(style: DrumStyle) -> &'static str {
    match style {
        DrumStyle::Rock => tr("Rock"),
        DrumStyle::Pop => tr("Pop"),
        DrumStyle::Funk => tr("Funk"),
        DrumStyle::Jazz => tr("Jazz"),
        DrumStyle::HipHop => tr("Hip Hop"),
        DrumStyle::Electronic => tr("Electronic"),
    }
}

// ------------------------------------------------------------------------- pattern

/// The pitch a new row gets: the first drum a drum pattern does not have yet, or two
/// semitones below the lowest row.
fn next_row_pitch(pat: &PatternParams, drums: bool) -> u8 {
    use drums::*;
    let used = |p: u8| pat.rows.iter().any(|r| r.pitch == p);
    if drums {
        let order = [
            KICK,
            SNARE,
            CLAP,
            CLOSED_HAT,
            OPEN_HAT,
            TOM_LOW,
            CRASH,
            RIDE,
            ELECTRIC_SNARE,
            TOM_FLOOR_LOW,
            TOM_FLOOR,
            TOM_MID,
            TOM_MID_HIGH,
            TOM_HIGH,
            TAMBOURINE,
            COWBELL,
        ];
        if let Some(p) = order.into_iter().find(|p| !used(*p)) {
            return p;
        }
    }
    let lowest = pat.rows.iter().map(|r| r.pitch).min().unwrap_or(62);
    lowest.saturating_sub(2)
}

/// The velocity the height of the pointer in a step stands for.
fn velocity_at(y: f32, cell: Rect) -> u8 {
    let t = ((cell.bottom() - y) / cell.height()).clamp(0.1, 1.0);
    (t * 127.0).round().max(16.0) as u8
}

fn show_pattern(ui: &mut Ui, p: &mut Project, s: &mut UiState, ti: usize, ri: usize, body: Rect) {
    let Some(Generator::Pattern(mut pat)) = p.tracks[ti].regions[ri]
        .generated
        .as_ref()
        .map(|g| g.source.clone())
    else {
        return;
    };
    let strip = strip(ui, body);
    let (origin, len_beats) = {
        let r = &p.tracks[ti].regions[ri];
        (p.region_start_beats(r), p.region_len_beats(r))
    };
    let bar_len = p.bar_len_at(origin);
    let drums = is_drum_track(&p.tracks[ti]);
    let kit = p.tracks[ti].drums.clone();
    let mut changed = false;
    let mut convert = false;
    let mut mode = s.steps.mode;

    // ---- settings
    row_ui(ui, strip.shrink2(vec2(10.0, 4.0)), |ui| {
        ui.label(tr("Steps:"));
        let mut steps = pat.steps as u32;
        if ui
            .add(egui::DragValue::new(&mut steps).range(1..=64))
            .changed()
        {
            pat.steps = steps as usize;
            changed = true;
        }
        ui.label(tr("Rate:"));
        egui::ComboBox::from_id_salt("pattern_rate")
            .selected_text(pat.rate.name())
            .width(64.0)
            .show_ui(ui, |ui| {
                for r in StepRate::ALL {
                    if ui.selectable_label(pat.rate == r, r.name()).clicked() {
                        pat.rate = r;
                        changed = true;
                    }
                }
            });
        ui.label(tr("Swing:"));
        let mut swing = (pat.swing * 100.0).round() as u32;
        if ui
            .add(egui::DragValue::new(&mut swing).range(0..=50).suffix(" %"))
            .changed()
        {
            pat.swing = swing as f32 / 100.0;
            changed = true;
        }
        ui.label(tr("Gate:"));
        let mut gate = (pat.gate * 100.0).round() as u32;
        if ui
            .add(egui::DragValue::new(&mut gate).range(5..=100).suffix(" %"))
            .changed()
        {
            pat.gate = gate as f32 / 100.0;
            changed = true;
        }
        ui.label(tr("Direction:"));
        egui::ComboBox::from_id_salt("pattern_direction")
            .selected_text(direction_name(pat.direction))
            .width(84.0)
            .show_ui(ui, |ui| {
                for d in Direction::ALL {
                    if ui
                        .selectable_label(pat.direction == d, direction_name(d))
                        .clicked()
                    {
                        pat.direction = d;
                        changed = true;
                    }
                }
            });
        ui.label(tr("Edit:"));
        egui::ComboBox::from_id_salt("pattern_mode")
            .selected_text(mode.name())
            .width(84.0)
            .show_ui(ui, |ui| {
                for m in EditMode::ALL {
                    if ui.selectable_label(mode == m, m.name()).clicked() {
                        mode = m;
                    }
                }
            });
        let random = pat.rows.iter().any(|r| r.chance.iter().any(|c| *c < 100))
            || pat.direction == Direction::Random;
        if random && ui.button(tr("New Variation")).clicked() {
            pat.seed = pat.seed.wrapping_add(1);
            changed = true;
        }
        let room = pat.rows.len() < MAX_ROWS;
        if ui
            .add_enabled(room, egui::Button::new(tr("Add Row")))
            .clicked()
        {
            let pitch = next_row_pitch(&pat, drums);
            pat.rows.push(PatternRow::new(pitch));
            changed = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(tr("Convert to MIDI Region")).clicked() {
                convert = true;
            }
        });
    });

    s.steps.mode = mode;

    // ---- grid
    let area = Rect::from_min_max(pos2(body.left(), strip.bottom()), body.max);
    let grid = Rect::from_min_max(pos2(area.left() + LABEL_W, area.top()), area.max);
    let steps = pat.steps.clamp(1, crate::pattern::MAX_STEPS);
    let cw = ((grid.width() - 12.0) / steps as f32).clamp(10.0, 64.0);
    let v = &mut s.steps;
    let overflow = (pat.rows.len() as f32 * ROW_H - area.height() + 8.0).max(0.0);
    if ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| area.contains(pt))
    {
        v.scroll_y -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    v.scroll_y = v.scroll_y.clamp(0.0, overflow);
    let top = area.top() + 4.0 - v.scroll_y;
    let cell = |r: usize, k: usize| {
        Rect::from_min_size(
            pos2(grid.left() + k as f32 * cw, top + r as f32 * ROW_H),
            vec2(cw, ROW_H),
        )
    };
    let g = ui.painter_at(area);
    let per_beat = pat.rate.per_beat().max(1);
    let step_len = pat.rate.beats();
    let now = p.position_beats - origin;
    let current = (now >= 0.0 && now < len_beats).then(|| pat.step_at(now));
    for (r, row) in pat.rows.iter().enumerate() {
        let y = top + r as f32 * ROW_H;
        if y + ROW_H < area.top() || y > area.bottom() {
            continue;
        }
        let label = Rect::from_min_size(pos2(area.left(), y), vec2(LABEL_W - 4.0, ROW_H - 1.0));
        g.rect_filled(label, 3.0, Color32::from_gray(0x44));
        g.text(
            label.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            row_label(row.pitch, drums, kit.as_ref()),
            FontId::proportional(12.0),
            TEXT,
        );
        for k in 0..steps {
            let c = cell(r, k).shrink(1.0);
            let shade = if (k / per_beat) % 2 == 0 { 0x38 } else { 0x30 };
            g.rect_filled(c, 2.0, Color32::from_gray(shade));
            let vel = row.steps[k];
            if vel > 0 {
                let (level, color) = match mode {
                    EditMode::Chance => (
                        row.chance[k] as f32 / 100.0,
                        Color32::from_rgb(0x3c, 0xa8, 0xd8),
                    ),
                    _ => (vel as f32 / 127.0, velocity_color(vel)),
                };
                let h = (c.height() * level).max(3.0);
                let bar = Rect::from_min_max(pos2(c.left(), c.bottom() - h), c.max);
                g.rect_filled(bar, 2.0, color);
                // What the step also does, in every mode: a bar at the left for a tie, the
                // repeat count, a stripe on top when it does not always play.
                if row.tie[k] {
                    let join = Rect::from_min_max(
                        pos2(c.left() - 2.0, c.top() + 3.0),
                        pos2(c.left() + 3.0, c.bottom() - 3.0),
                    );
                    g.rect_filled(join, 1.0, Color32::WHITE);
                }
                if row.repeat[k] > 1 {
                    g.text(
                        c.right_top() + vec2(-3.0, 1.0),
                        Align2::RIGHT_TOP,
                        format!("×{}", row.repeat[k]),
                        FontId::proportional(10.0),
                        Color32::WHITE,
                    );
                }
                if row.chance[k] < 100 {
                    let w = c.width() * row.chance[k] as f32 / 100.0;
                    g.rect_filled(
                        Rect::from_min_size(c.min, vec2(w, 3.0)),
                        0.0,
                        Color32::from_rgb(0x3c, 0xd8, 0xe8),
                    );
                }
            }
        }
    }
    // Beat lines, stronger at the bars.
    for k in 0..=steps {
        let x = grid.left() + k as f32 * cw;
        let at = k as f64 * step_len / bar_len.max(1e-9);
        let bar_line = (at - at.round()).abs() < 1e-6;
        if bar_line || k % per_beat == 0 {
            let (a, b) = if bar_line { (0xa0, 1.5) } else { (0x58, 1.0) };
            g.line_segment(
                [pos2(x, area.top()), pos2(x, area.bottom())],
                Stroke::new(b, Color32::from_gray(a)),
            );
        }
    }
    if let Some(k) = current {
        let x = grid.left() + k as f32 * cw;
        g.rect_filled(
            Rect::from_min_max(pos2(x, area.top()), pos2(x + cw, area.bottom())),
            0.0,
            Color32::from_white_alpha(22),
        );
    }

    // ---- painting steps
    let resp = ui.interact(
        grid,
        egui::Id::unique(("pattern_grid", ti, ri)),
        Sense::click_and_drag(),
    );
    let (pressed, down, sec_pressed, sec_down) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.secondary_pressed(),
            i.pointer.secondary_down(),
        )
    });
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let row_count = pat.rows.len();
    let cell_at = |pt: egui::Pos2| -> Option<(usize, usize)> {
        let (kx, ry) = (
            ((pt.x - grid.left()) / cw).floor(),
            ((pt.y - top) / ROW_H).floor(),
        );
        (kx >= 0.0 && ry >= 0.0 && (kx as usize) < steps && (ry as usize) < row_count)
            .then_some((ry as usize, kx as usize))
    };
    if let Some(pt) = pointer {
        if let Some(Gesture::Velocity {
            row,
            step,
            from_y,
            moved,
        }) = v.gesture
        {
            if down {
                let moved = moved || (pt.y - from_y).abs() > 3.0;
                if moved {
                    let c = cell(row, step);
                    let r = &mut pat.rows[row];
                    if mode == EditMode::Chance {
                        let chance = chance_at(pt.y, c);
                        if r.chance[step] != chance {
                            r.chance[step] = chance;
                            changed = true;
                        }
                    } else {
                        let vel = velocity_at(pt.y, c);
                        if r.steps[step] != vel {
                            r.steps[step] = vel;
                            changed = true;
                        }
                    }
                }
                v.gesture = Some(Gesture::Velocity {
                    row,
                    step,
                    from_y,
                    moved,
                });
            }
        } else if let Some((r, k)) = cell_at(pt) {
            let now_vel = pat.rows[r].steps[k];
            if mode != EditMode::Velocity {
                // The other properties belong to steps that are on.
                if resp.hovered() && now_vel > 0 {
                    let row = &mut pat.rows[r];
                    match mode {
                        EditMode::Chance if pressed => {
                            v.gesture = Some(Gesture::Velocity {
                                row: r,
                                step: k,
                                from_y: pt.y,
                                moved: false,
                            });
                        }
                        EditMode::Repeat if pressed => {
                            row.repeat[k] = row.repeat[k] % 4 + 1;
                            changed = true;
                        }
                        EditMode::Repeat if sec_pressed && row.repeat[k] != 1 => {
                            row.repeat[k] = 1;
                            changed = true;
                        }
                        EditMode::Tie if pressed => {
                            row.tie[k] = !row.tie[k];
                            changed = true;
                        }
                        _ => {}
                    }
                }
            } else if resp.hovered() && pressed {
                if now_vel == 0 {
                    pat.rows[r].steps[k] = velocity_at(pt.y, cell(r, k));
                    v.gesture = Some(Gesture::Paint);
                    changed = true;
                } else {
                    v.gesture = Some(Gesture::Velocity {
                        row: r,
                        step: k,
                        from_y: pt.y,
                        moved: false,
                    });
                }
            } else if resp.hovered() && sec_pressed {
                v.gesture = Some(Gesture::Erase);
                if now_vel != 0 {
                    pat.rows[r].steps[k] = 0;
                    changed = true;
                }
            } else if v.gesture == Some(Gesture::Paint) && down && now_vel == 0 {
                pat.rows[r].steps[k] = velocity_at(pt.y, cell(r, k));
                changed = true;
            } else if v.gesture == Some(Gesture::Erase) && sec_down && now_vel != 0 {
                pat.rows[r].steps[k] = 0;
                changed = true;
            }
        }
    }
    if !down && !sec_down {
        // Let go: a click on a step that was on, without moving, switches it off.
        if let Some(Gesture::Velocity {
            row,
            step,
            moved: false,
            ..
        }) = v.gesture
        {
            if mode == EditMode::Velocity
                && row < pat.rows.len()
                && step < pat.rows[row].steps.len()
            {
                pat.rows[row].steps[step] = 0;
                changed = true;
            }
        }
        v.gesture = None;
    }

    // ---- the rows' names
    let mut sounding = None;
    let mut delete = None;
    for r in 0..pat.rows.len() {
        let y = top + r as f32 * ROW_H;
        if y + ROW_H < area.top() || y > area.bottom() {
            continue;
        }
        let label = Rect::from_min_size(pos2(area.left(), y), vec2(LABEL_W - 4.0, ROW_H - 1.0))
            .intersect(area);
        let resp = ui
            .interact(
                label,
                egui::Id::unique(("pattern_row", ti, ri, r)),
                Sense::click_and_drag(),
            )
            .on_hover_text(tr(
                "Drag to change the pitch; right-click to delete the row",
            ));
        if resp.drag_started() {
            v.pitch_drag = 0.0;
        }
        if resp.dragged() {
            v.pitch_drag -= resp.drag_delta().y;
            let steps = (v.pitch_drag / 8.0).trunc();
            if steps != 0.0 {
                v.pitch_drag -= steps * 8.0;
                let pitch = (pat.rows[r].pitch as i32 + steps as i32).clamp(0, 127) as u8;
                pat.rows[r].pitch = pitch;
                changed = true;
            }
        }
        if resp.is_pointer_button_down_on() {
            sounding = Some(pat.rows[r].pitch);
        }
        context_menu(&resp, |ui| {
            if ui.button(tr("Delete Row")).clicked() {
                delete = Some(r);
                ui.close();
            }
        });
    }
    if sounding != v.held {
        if let Some(old) = v.held {
            s.audition.push(Audition::Off { pitch: old });
        }
        if let Some(pitch) = sounding {
            s.audition.push(Audition::On {
                pitch,
                vel: DEFAULT_VELOCITY,
                track: ti.min(usize::from(u16::MAX)) as u16,
            });
        }
        v.held = sounding;
    }
    if let Some(r) = delete {
        if r < pat.rows.len() {
            pat.rows.remove(r);
            changed = true;
        }
    }

    if changed {
        pat.tidy();
        if let Some(g) = p.tracks[ti].regions[ri].generated.as_mut() {
            g.source = Generator::Pattern(pat);
        }
        p.regenerate(ti, ri);
    }
    if convert {
        p.convert_to_midi_region(ti, ri);
    }
}

// ------------------------------------------------------------------------- drummer

fn show_drummer(ui: &mut Ui, p: &mut Project, s: &mut UiState, ti: usize, ri: usize, body: Rect) {
    let Some(Generator::Drummer(mut d)) = p.tracks[ti].regions[ri]
        .generated
        .as_ref()
        .map(|g| g.source.clone())
    else {
        return;
    };
    let _ = s;
    let (origin, len_beats) = {
        let r = &p.tracks[ti].regions[ri];
        (p.region_start_beats(r), p.region_len_beats(r))
    };
    let mut changed = false;
    let mut convert = false;
    let g = ui.painter_at(body);

    // ---- the controls on the left
    let panel = Rect::from_min_size(body.min, vec2(PANEL_W, body.height()));
    g.rect_filled(panel, 0.0, STRIP_BG);
    g.line_segment(
        [panel.right_top(), panel.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let pad = Rect::from_min_size(
        pos2(panel.left() + 14.0, panel.top() + 12.0),
        vec2(176.0, 128.0),
    );
    changed |= xy_pad(ui, &g, pad, &mut d, (ti, ri));

    // Style and variation beside the pad.
    let side = Rect::from_min_max(
        pos2(pad.right() + 12.0, pad.top()),
        pos2(panel.right() - 12.0, pad.bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(side), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(tr("Style:"));
        egui::ComboBox::from_id_salt("drummer_style")
            .selected_text(style_name(d.style))
            .width(side.width() - 8.0)
            .show_ui(ui, |ui| {
                for style in DrumStyle::ALL {
                    if ui
                        .selectable_label(d.style == style, style_name(style))
                        .clicked()
                    {
                        d.style = style;
                        // A new style starts with its own swing.
                        d.swing = style.default_swing();
                        changed = true;
                    }
                }
            });
        ui.label(trf("Variation {}", &[&d.seed]));
        if ui.button(tr("New Variation")).clicked() {
            d.seed = d.seed.wrapping_add(1);
            changed = true;
        }
    });

    let lower = Rect::from_min_max(
        pos2(panel.left() + 14.0, pad.bottom() + 28.0),
        pos2(panel.right() - 14.0, panel.bottom() - 6.0),
    );
    ui.scope_builder(UiBuilder::new().max_rect(lower), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.spacing_mut().slider_width = 150.0;
        let mut swing = d.swing as u32;
        if ui
            .add(egui::Slider::new(&mut swing, 0..=100).text(tr("Swing")))
            .changed()
        {
            d.swing = swing as u8;
            changed = true;
        }
        let mut fills = d.fills as u32;
        if ui
            .add(egui::Slider::new(&mut fills, 0..=100).text(tr("Fills")))
            .changed()
        {
            d.fills = fills as u8;
            changed = true;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for part in parts::LIST {
                let on = d.parts & part != 0;
                if ui.selectable_label(on, part_name(part)).clicked() {
                    d.parts ^= part;
                    changed = true;
                }
            }
        });
        if ui.button(tr("Convert to MIDI Region")).clicked() {
            convert = true;
        }
    });

    // ---- what it plays, one lane for each piece of the kit
    let lanes = Rect::from_min_max(pos2(panel.right() + 8.0, body.top() + 8.0), body.max);
    let lane_h = ((lanes.height() - 12.0) / parts::LIST.len() as f32).clamp(18.0, 40.0);
    let grid = Rect::from_min_max(pos2(lanes.left() + 92.0, lanes.top()), lanes.max);
    let beat_px = ((grid.width() - 12.0) / len_beats.max(1.0) as f32).max(1.0);
    let lg = ui.painter_at(lanes);
    for (i, part) in parts::LIST.into_iter().enumerate() {
        let lane = Rect::from_min_size(
            pos2(lanes.left(), lanes.top() + i as f32 * lane_h),
            vec2(lanes.width() - 8.0, lane_h - 2.0),
        );
        lg.rect_filled(lane, 3.0, Color32::from_gray(0x33));
        let on = d.parts & part != 0;
        lg.text(
            lane.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            part_name(part),
            FontId::proportional(12.0),
            if on { TEXT } else { TEXT_DIM },
        );
    }
    let lg = lg.with_clip_rect(grid.intersect(lanes));
    for bar in p.bars_in(origin, origin + len_beats) {
        let x = grid.left() + ((bar.start - origin) as f32) * beat_px;
        lg.line_segment(
            [pos2(x, lanes.top()), pos2(x, lanes.top() + lane_h * 6.0)],
            Stroke::new(1.0, Color32::from_gray(0x5a)),
        );
    }
    for n in &p.tracks[ti].regions[ri].notes {
        let Some(i) = parts::LIST
            .into_iter()
            .position(|part| part == crate::pattern::part_of(n.pitch))
        else {
            continue;
        };
        let lane_top = lanes.top() + i as f32 * lane_h;
        let h = ((lane_h - 6.0) * n.vel as f32 / 127.0).max(3.0);
        let x = grid.left() + n.start * beat_px;
        if x > lanes.right() || x < grid.left() - 8.0 {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(x, lane_top + lane_h - 4.0 - h),
            pos2(
                x + (n.len * beat_px).clamp(2.0, 6.0),
                lane_top + lane_h - 4.0,
            ),
        );
        lg.rect_filled(r, 1.0, velocity_color(n.vel));
    }
    let now = p.position_beats - origin;
    if now >= 0.0 && now < len_beats {
        let x = grid.left() + now as f32 * beat_px;
        lg.line_segment(
            [pos2(x, lanes.top()), pos2(x, lanes.top() + lane_h * 6.0)],
            Stroke::new(1.0, Color32::WHITE),
        );
    }

    if changed {
        d.tidy();
        if let Some(g) = p.tracks[ti].regions[ri].generated.as_mut() {
            g.source = Generator::Drummer(d);
        }
        p.regenerate(ti, ri);
    }
    if convert {
        p.convert_to_midi_region(ti, ri);
    }
}

/// The Drummer's pad: busyness across, loudness up. Returns whether a drag changed them.
fn xy_pad(
    ui: &mut Ui,
    g: &egui::Painter,
    pad: Rect,
    d: &mut DrummerParams,
    (ti, ri): (usize, usize),
) -> bool {
    g.rect_filled(pad, 4.0, Color32::from_gray(0x22));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        let line = Stroke::new(1.0, Color32::from_gray(0x34));
        g.line_segment(
            [
                pos2(pad.left() + pad.width() * t, pad.top()),
                pos2(pad.left() + pad.width() * t, pad.bottom()),
            ],
            line,
        );
        g.line_segment(
            [
                pos2(pad.left(), pad.top() + pad.height() * t),
                pos2(pad.right(), pad.top() + pad.height() * t),
            ],
            line,
        );
    }
    g.rect_stroke(
        pad,
        4.0,
        Stroke::new(1.0, Color32::from_gray(0x50)),
        StrokeKind::Inside,
    );
    let small = FontId::proportional(10.5);
    g.text(
        pad.left_top() + vec2(5.0, 3.0),
        Align2::LEFT_TOP,
        tr("Loud"),
        small.clone(),
        TEXT_DIM,
    );
    g.text(
        pad.left_bottom() + vec2(5.0, -3.0),
        Align2::LEFT_BOTTOM,
        tr("Soft"),
        small.clone(),
        TEXT_DIM,
    );
    g.text(
        pad.left_bottom() + vec2(0.0, 3.0),
        Align2::LEFT_TOP,
        tr("Simple"),
        small.clone(),
        TEXT_DIM,
    );
    g.text(
        pad.right_bottom() + vec2(0.0, 3.0),
        Align2::RIGHT_TOP,
        tr("Complex"),
        small,
        TEXT_DIM,
    );
    let resp = ui.interact(
        pad,
        egui::Id::unique(("drummer_pad", ti, ri)),
        Sense::click_and_drag(),
    );
    let mut changed = false;
    if let (true, Some(pt)) = (
        resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on(),
        resp.interact_pointer_pos(),
    ) {
        let x = ((pt.x - pad.left()) / pad.width()).clamp(0.0, 1.0);
        let y = 1.0 - ((pt.y - pad.top()) / pad.height()).clamp(0.0, 1.0);
        let (c, l) = ((x * 100.0).round() as u8, (y * 100.0).round() as u8);
        if (c, l) != (d.complexity, d.loudness) {
            (d.complexity, d.loudness) = (c, l);
            changed = true;
        }
    }
    let dot = pos2(
        pad.left() + pad.width() * d.complexity.min(100) as f32 / 100.0,
        pad.bottom() - pad.height() * d.loudness.min(100) as f32 / 100.0,
    );
    g.circle_filled(dot, 7.0, BLUE);
    g.circle_stroke(dot, 7.0, Stroke::new(1.5, Color32::WHITE));
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_named_after_drums_or_notes() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(61), "C#3");
        assert_eq!(note_name(0), "C-2");
        assert_eq!(row_name(36, true), "Kick");
        assert_eq!(row_name(36, false), "C1");
        assert_eq!(row_name(37, true), "C#1", "no drum there: its note");
        // Every pitch the Drummer plays has a name.
        for pitch in [
            36, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 54, 56,
        ] {
            assert!(drum_name(pitch).is_some(), "{pitch}");
        }
    }

    #[test]
    fn a_new_row_takes_the_next_drum_or_a_lower_pitch() {
        let mut p = PatternParams::drums();
        // The drum pattern already has a kick, snare, clap, hats, tom and crash.
        assert_eq!(next_row_pitch(&p, true), drums::RIDE);
        assert_eq!(next_row_pitch(&p, false), 34);
        p.rows.clear();
        assert_eq!(next_row_pitch(&p, true), drums::KICK);
        assert_eq!(next_row_pitch(&p, false), 60);
        let m = PatternParams::melodic();
        assert_eq!(next_row_pitch(&m, false), 53);
    }

    #[test]
    fn the_height_of_a_click_is_its_velocity() {
        let cell = Rect::from_min_size(pos2(0.0, 100.0), vec2(30.0, 24.0));
        assert_eq!(velocity_at(100.0, cell), 127);
        assert_eq!(
            velocity_at(124.0, cell),
            16,
            "the bottom is soft, not silent"
        );
        assert!(velocity_at(112.0, cell) > 55 && velocity_at(112.0, cell) < 75);
        assert_eq!(velocity_at(0.0, cell), 127);
    }

    #[test]
    fn drum_tracks_are_the_drummer_and_kit_instruments() {
        let drummer = Track::new("D", TrackKind::Drummer);
        assert!(is_drum_track(&drummer));
        let mut synth = Track::new("S", TrackKind::Instrument);
        assert!(!is_drum_track(&synth));
        synth.instrument = Some("Drum Machine");
        assert!(is_drum_track(&synth));
    }

    // ---- the editors with a pointer, in a headless egui (see ui_test.rs)

    use crate::ui_test::Harness;
    use egui::PointerButton;

    const W: f32 = 1000.0;
    const H: f32 = 420.0;

    fn body() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(W, H))
    }

    /// Where a step of the pattern editor is, in a `body()` scrolled to the top.
    fn pattern_cell(steps: usize, row: usize, step: usize) -> Rect {
        let grid_left = LABEL_W;
        let cw = ((W - grid_left - 12.0) / steps as f32).clamp(10.0, 64.0);
        Rect::from_min_size(
            pos2(
                grid_left + step as f32 * cw,
                STRIP_H + 4.0 + row as f32 * ROW_H,
            ),
            vec2(cw, ROW_H),
        )
    }

    struct Rig {
        p: Project,
        s: UiState,
        h: Harness,
        t: usize,
        r: usize,
    }

    impl Rig {
        fn pattern() -> Self {
            let mut p = Project::empty();
            let t = p.add_track(TrackKind::Instrument);
            let r = p.add_generated_region(t, false, 1.0).unwrap();
            Self::with(p, t, r)
        }

        fn drummer() -> Self {
            let mut p = Project::empty();
            let t = p.add_track(TrackKind::Drummer);
            let r = p.add_generated_region(t, true, 1.0).unwrap();
            Self::with(p, t, r)
        }

        fn with(p: Project, t: usize, r: usize) -> Self {
            let mut rig = Self {
                p,
                s: UiState::new(),
                h: Harness::new(W, H),
                t,
                r,
            };
            rig.settle();
            rig
        }

        fn settle(&mut self) {
            let (p, s, t, r) = (&mut self.p, &mut self.s, self.t, self.r);
            self.h.settle(|ui| show(ui, p, s, t, r, body()));
        }

        fn click(&mut self, pos: egui::Pos2) {
            let (p, s, t, r) = (&mut self.p, &mut self.s, self.t, self.r);
            self.h.click(pos, |ui| show(ui, p, s, t, r, body()));
        }

        fn right_click(&mut self, pos: egui::Pos2) {
            let (p, s, t, r) = (&mut self.p, &mut self.s, self.t, self.r);
            self.h.right_click(pos, |ui| show(ui, p, s, t, r, body()));
        }

        fn drag(&mut self, path: &[egui::Pos2], button: PointerButton) {
            let (p, s, t, r) = (&mut self.p, &mut self.s, self.t, self.r);
            self.h.drag(path, button, |ui| show(ui, p, s, t, r, body()));
        }

        fn pattern_params(&self) -> PatternParams {
            match &self.p.tracks[self.t].regions[self.r]
                .generated
                .as_ref()
                .unwrap()
                .source
            {
                Generator::Pattern(p) => p.clone(),
                other => panic!("{other:?}"),
            }
        }

        fn drummer_params(&self) -> DrummerParams {
            match &self.p.tracks[self.t].regions[self.r]
                .generated
                .as_ref()
                .unwrap()
                .source
            {
                Generator::Drummer(d) => d.clone(),
                other => panic!("{other:?}"),
            }
        }

        fn notes(&self) -> usize {
            self.p.tracks[self.t].regions[self.r].notes.len()
        }
    }

    #[test]
    fn clicking_an_empty_step_switches_it_on_and_the_notes_follow() {
        let mut rig = Rig::pattern();
        assert_eq!(rig.notes(), 0);
        // Near the top of the cell: a hard step.
        let c = pattern_cell(16, 2, 4);
        rig.click(pos2(c.center().x, c.top() + 2.0));
        let pat = rig.pattern_params();
        assert!(pat.rows[2].steps[4] >= 110, "{}", pat.rows[2].steps[4]);
        assert_eq!(pat.rows[2].steps.iter().filter(|v| **v > 0).count(), 1);
        // Four bars of 4/4 loop the one bar of 16 steps four times.
        let notes = &rig.p.tracks[rig.t].regions[rig.r].notes;
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[0].start, 1.0);
        assert_eq!(notes[0].pitch, pat.rows[2].pitch);
        // Near the bottom: soft.
        let c = pattern_cell(16, 3, 9);
        rig.click(pos2(c.center().x, c.bottom() - 2.0));
        assert!(rig.pattern_params().rows[3].steps[9] < 40);
    }

    #[test]
    fn clicking_a_step_that_is_on_switches_it_off_and_dragging_in_it_sets_its_velocity() {
        let mut rig = Rig::pattern();
        let c = pattern_cell(16, 0, 1);
        rig.click(c.center());
        assert!(rig.pattern_params().rows[0].steps[1] > 0);
        rig.click(c.center());
        assert_eq!(
            rig.pattern_params().rows[0].steps[1],
            0,
            "a click switched it off"
        );
        assert_eq!(rig.notes(), 0);
        // On again; dragging down inside it makes it softer instead of switching it off.
        rig.click(pos2(c.center().x, c.top() + 2.0));
        let loud = rig.pattern_params().rows[0].steps[1];
        rig.drag(
            &[
                pos2(c.center().x, c.top() + 3.0),
                pos2(c.center().x, c.top() + 12.0),
                pos2(c.center().x, c.bottom() - 1.0),
            ],
            PointerButton::Primary,
        );
        let soft = rig.pattern_params().rows[0].steps[1];
        assert!(soft > 0 && soft < loud / 2, "{loud} -> {soft}");
    }

    #[test]
    fn dragging_across_steps_paints_them_and_a_right_drag_clears_them() {
        let mut rig = Rig::pattern();
        let y = pattern_cell(16, 1, 0).center().y;
        let x = |k: usize| pattern_cell(16, 1, k).center().x;
        rig.drag(
            &[pos2(x(3), y), pos2(x(4), y), pos2(x(5), y), pos2(x(6), y)],
            PointerButton::Primary,
        );
        let on: Vec<usize> = (0..16)
            .filter(|k| rig.pattern_params().rows[1].steps[*k] > 0)
            .collect();
        assert_eq!(on, [3, 4, 5, 6]);
        rig.drag(&[pos2(x(4), y), pos2(x(5), y)], PointerButton::Secondary);
        let on: Vec<usize> = (0..16)
            .filter(|k| rig.pattern_params().rows[1].steps[*k] > 0)
            .collect();
        assert_eq!(on, [3, 6]);
        rig.right_click(pos2(x(3), y));
        assert_eq!(rig.pattern_params().rows[1].steps[3], 0);
    }

    #[test]
    fn the_edit_modes_set_chance_repeat_and_tie_on_steps_that_are_on() {
        let mut rig = Rig::pattern();
        for k in [2, 3] {
            rig.click(pattern_cell(16, 0, k).center());
        }
        // Chance: a drag to the bottom of the step lowers it; an empty step is left alone.
        rig.s.steps.mode = EditMode::Chance;
        let c = pattern_cell(16, 0, 2);
        rig.drag(
            &[
                pos2(c.center().x, c.top() + 3.0),
                pos2(c.center().x, c.top() + 10.0),
                pos2(c.center().x, c.bottom() - 4.0),
            ],
            PointerButton::Primary,
        );
        let chance = rig.pattern_params().rows[0].chance[2];
        assert!(chance < 30, "{chance}");
        rig.click(pattern_cell(16, 0, 7).center());
        assert_eq!(
            rig.pattern_params().rows[0].steps[7],
            0,
            "no step made in this mode"
        );
        assert_eq!(rig.pattern_params().rows[0].chance[7], 100);
        // Repeat: each click counts up to 4 and round to 1; a right-click resets.
        rig.s.steps.mode = EditMode::Repeat;
        let c = pattern_cell(16, 0, 3).center();
        let seen: Vec<u8> = (0..5)
            .map(|_| {
                rig.click(c);
                rig.pattern_params().rows[0].repeat[3]
            })
            .collect();
        assert_eq!(seen, [2, 3, 4, 1, 2]);
        rig.right_click(c);
        assert_eq!(rig.pattern_params().rows[0].repeat[3], 1);
        // Tie: a click ties the step to the one before, another unties it.
        rig.s.steps.mode = EditMode::Tie;
        rig.click(c);
        assert!(rig.pattern_params().rows[0].tie[3]);
        // (The chance of step 2 is put back, so every bar plays.)
        if let Some(g) = rig.p.tracks[rig.t].regions[rig.r].generated.as_mut() {
            if let Generator::Pattern(pat) = &mut g.source {
                pat.rows[0].chance[2] = 100;
            }
        }
        rig.p.regenerate(rig.t, rig.r);
        // Steps 2 and 3 are tied: one note a bit longer than two steps' worth, repeated in
        // each of the four bars.
        let notes = &rig.p.tracks[rig.t].regions[rig.r].notes;
        assert_eq!(notes.len(), 4);
        assert!(
            notes[0].len > 0.25 && notes[0].len < 0.6,
            "{}",
            notes[0].len
        );
        rig.click(c);
        assert!(!rig.pattern_params().rows[0].tie[3]);
        assert_eq!(rig.notes(), 8);
    }

    #[test]
    fn dragging_a_rows_name_changes_its_pitch_and_it_sounds_while_held() {
        let mut rig = Rig::pattern();
        let before = rig.pattern_params().rows[1].pitch;
        let y = STRIP_H + 4.0 + 1.0 * ROW_H + ROW_H / 2.0;
        let x = 40.0;
        let (p, s, t, r) = (&mut rig.p, &mut rig.s, rig.t, rig.r);
        rig.h.press(pos2(x, y), PointerButton::Primary, |ui| {
            show(ui, p, s, t, r, body())
        });
        assert!(
            rig.s
                .audition
                .iter()
                .any(|a| matches!(a, Audition::On { pitch, .. } if *pitch == before)),
            "it sounds"
        );
        let (p, s, t, r) = (&mut rig.p, &mut rig.s, rig.t, rig.r);
        for dy in [-6.0, -12.0, -20.0] {
            rig.h
                .frame(vec![egui::Event::PointerMoved(pos2(x, y + dy))], |ui| {
                    show(ui, p, s, t, r, body())
                });
        }
        rig.h
            .release(pos2(x, y - 20.0), PointerButton::Primary, |ui| {
                show(ui, p, s, t, r, body())
            });
        rig.h.frame(vec![], |ui| show(ui, p, s, t, r, body()));
        // egui starts a drag after a few points of movement, so 20 points up is one or two
        // semitones.
        let moved = rig.pattern_params().rows[1].pitch as i32 - before as i32;
        assert!((1..=2).contains(&moved), "{moved}");
        assert!(
            rig.s
                .audition
                .iter()
                .any(|a| matches!(a, Audition::Off { .. })),
            "and it stops"
        );
    }

    #[test]
    fn the_drummer_pad_sets_complexity_and_loudness_and_the_notes_follow() {
        let mut rig = Rig::drummer();
        let pad = Rect::from_min_size(pos2(14.0, 12.0), vec2(176.0, 128.0));
        rig.click(pad.center());
        let d = rig.drummer_params();
        assert!(
            (48..=52).contains(&d.complexity) && (48..=52).contains(&d.loudness),
            "{d:?}"
        );
        rig.drag(
            &[
                pad.center(),
                pad.right_top() + vec2(-10.0, 10.0),
                pad.right_top() + vec2(30.0, -30.0),
            ],
            PointerButton::Primary,
        );
        let d = rig.drummer_params();
        assert_eq!(
            (d.complexity, d.loudness),
            (100, 100),
            "dragged past the corner"
        );
        let busy = rig.notes();
        rig.drag(
            &[
                pad.center(),
                pad.left_bottom() + vec2(5.0, -5.0),
                pad.left_bottom() + vec2(-30.0, 30.0),
            ],
            PointerButton::Primary,
        );
        let d = rig.drummer_params();
        assert_eq!((d.complexity, d.loudness), (0, 0));
        assert!(
            rig.notes() < busy,
            "a simple, soft drummer plays less ({} vs {busy})",
            rig.notes()
        );
    }

    #[test]
    fn both_editors_draw_at_small_sizes_without_trouble() {
        for (w, h) in [(1000.0, 420.0), (640.0, 200.0), (300.0, 120.0)] {
            for drummer in [false, true] {
                let mut rig = if drummer {
                    Rig::drummer()
                } else {
                    Rig::pattern()
                };
                rig.h = Harness::new(w, h);
                let body = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
                let (p, s, t, r) = (&mut rig.p, &mut rig.s, rig.t, rig.r);
                for _ in 0..3 {
                    rig.h.frame(vec![], |ui| show(ui, p, s, t, r, body));
                }
            }
        }
    }

    #[test]
    fn a_drum_machine_track_is_a_drum_track_and_its_rows_are_named_after_its_pads() {
        use crate::drum_kit::DrumsUse;
        use prowave_engine::drum_machine::PadKind;
        let mut t = Track::new("Beats", TrackKind::Instrument);
        assert!(!is_drum_track(&t));
        t.drums = Some(DrumsUse::new());
        assert!(is_drum_track(&t));
        let mut kit = DrumsUse::new();
        assert_eq!(row_label(36, true, Some(&kit)), "Kick");
        assert_eq!(row_label(46, true, Some(&kit)), "Open Hi-Hat");
        kit.pads[1].settings = PadKind::Sample.settings();
        kit.pads[1].sample = Some("/kits/snap.wav".into());
        assert_eq!(row_label(37, true, Some(&kit)), "snap.wav");
        // Outside the pads it falls back to the General MIDI name, then the note.
        assert_eq!(row_label(60, true, Some(&kit)), "C3");
        assert_eq!(row_label(36, true, None), "Kick");
    }
}
