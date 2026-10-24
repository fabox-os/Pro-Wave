//! Automation (Logic: the Automation menu of the channel strip and the automation lanes).
//!
//! The fader, the pan knob and every parameter of the effects of a strip can be written while
//! the project plays (Touch, Latch, Write) and are played back by the engine (Read). This
//! module records them ([`tick`] and the helpers the Mixer and the effect windows call when a
//! control is moved) and has the Automation editor window, where the points of a lane are
//! added, moved and removed and the stretches between them are bent.
use crate::i18n::{tr, trf};
use crate::mixer::{db_to_frac, frac_to_db};
use crate::model::{AutoMode, AutoPoint, AutoTarget, Automation, Project, Track};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke, StrokeKind, Ui};
use prowave_engine::curve_shape;

/// What is being dragged in the editor.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    /// A point.
    Point(usize),
    /// The handle in the middle of the stretch that starts at a point (it bends it).
    Curve(usize),
}

/// The editor window's state.
#[derive(Default)]
pub struct AutoEditor {
    /// The track whose lanes are shown, when the window is open.
    pub track: Option<usize>,
    /// The first beat shown and how many beats are shown (0: not set yet).
    pub from: f64,
    pub span: f64,
    drag: Option<Drag>,
}

/// The value of a control in its lane's units: dB, pan, or for an effect parameter its
/// position 0..=1 along its range.
fn current(t: &Track, target: AutoTarget) -> f32 {
    match target {
        AutoTarget::Volume => t.volume_db,
        AutoTarget::Pan => t.pan,
        AutoTarget::Fx { slot, param } => t
            .inserts
            .get(slot as usize)
            .and_then(|i| {
                let info = i.kind.params().get(param as usize)?;
                Some(info.to_norm(i.params[param as usize]))
            })
            .unwrap_or(0.0),
    }
}

/// A lane value in the control's own units (an effect parameter's position 0..=1 becomes
/// its value in dB, Hz, ms...).
pub fn natural(t: &Track, target: AutoTarget, v: f32) -> f32 {
    match target {
        AutoTarget::Fx { slot, param } => t
            .inserts
            .get(slot as usize)
            .and_then(|i| i.kind.params().get(param as usize))
            .map_or(v, |info| info.from_norm(v)),
        _ => v,
    }
}

/// Once a frame, after the playhead was read from the engine: writes what the strips that
/// record (Write, and Touch or Latch with a control held) have now.
pub fn tick(p: &mut Project) {
    let (beat, playing) = (p.position_beats, p.playing);
    for t in p.tracks.iter_mut() {
        if !playing {
            // Latch stops writing when playback stops; the next pass starts afresh.
            if t.auto.mode == AutoMode::Latch {
                t.auto.held = None;
            }
            t.auto.end_pass();
            continue;
        }
        let target = match t.auto.mode {
            AutoMode::Write => Some(t.auto.shown),
            AutoMode::Touch | AutoMode::Latch => t.auto.held,
            AutoMode::Off | AutoMode::Read => None,
        };
        if let Some(target) = target {
            let value = current(t, target);
            t.auto.write(target, beat, value, false);
        }
    }
}

/// Whether the user can move the control: not while the strip plays its lane in Read mode.
pub fn editable(t: &Track, target: AutoTarget) -> bool {
    !(t.auto.mode == AutoMode::Read && !t.auto.lane(target).is_empty())
}

/// The value the control shows, in its lane's units: the lane's at `beat` when the strip
/// plays it, else its own.
pub fn shown_value(t: &Track, target: AutoTarget, beat: f64) -> f32 {
    t.auto
        .played(target, beat)
        .unwrap_or_else(|| current(t, target))
}

/// The user moved a control of the strip to `value`: dB for the fader, -1..1 for the pan,
/// the parameter's own value for an effect. In Touch and Latch the control is now held: the
/// strip stops playing its lane and writes.
pub fn moved(t: &mut Track, target: AutoTarget, value: f32) {
    if matches!(t.auto.mode, AutoMode::Touch | AutoMode::Latch) && t.auto.held != Some(target) {
        t.auto.held = Some(target);
    }
    set_control(t, target, value);
}

/// Sets a control without any automation state changing.
fn set_control(t: &mut Track, target: AutoTarget, value: f32) {
    match target {
        AutoTarget::Volume => t.volume_db = value.clamp(-60.0, 6.0),
        AutoTarget::Pan => t.pan = value.clamp(-1.0, 1.0),
        AutoTarget::Fx { slot, param } => {
            if let Some(ins) = t.inserts.get_mut(slot as usize) {
                if let Some(info) = ins.kind.params().get(param as usize) {
                    if value.is_finite() {
                        ins.params[param as usize] = value.clamp(info.min, info.max);
                    }
                }
            }
        }
    }
}

/// The user let go of a control. Touch returns to the lane (after writing the last value);
/// Latch keeps writing until playback stops.
pub fn released(t: &mut Track, target: AutoTarget, beat: f64, playing: bool) {
    if t.auto.mode == AutoMode::Touch && t.auto.held == Some(target) {
        if playing {
            let value = current(t, target);
            t.auto.write(target, beat, value, true);
        }
        t.auto.end_pass();
        t.auto.held = None;
    }
}

/// Changes the strip's automation mode. Leaving a mode that plays the lanes, the controls
/// keep the value the lanes had at `beat`, as in Logic.
pub fn set_mode(t: &mut Track, mode: AutoMode, beat: f64) {
    if t.auto.mode == mode {
        return;
    }
    let targets: Vec<AutoTarget> = t.auto.lanes().map(|(target, _)| target).collect();
    for target in targets {
        if let Some(v) = t.auto.played(target, beat) {
            let own = natural(t, target, v);
            set_control(t, target, own);
        }
    }
    t.auto.mode = mode;
    t.auto.held = None;
    t.auto.end_pass();
}

/// The name of a lane: "Volume", "Pan", or the effect and its parameter ("?" when the effect
/// is not there any more).
pub fn lane_label(t: &Track, target: AutoTarget) -> String {
    match target {
        AutoTarget::Volume => tr("Volume").to_string(),
        AutoTarget::Pan => tr("Pan").to_string(),
        AutoTarget::Fx { slot, param } => {
            match t
                .inserts
                .get(slot as usize)
                .and_then(|i| Some((i, i.kind.params().get(param as usize)?)))
            {
                Some((i, info)) => format!("{} · {}", i.name(), info.name),
                None => "?".to_string(),
            }
        }
    }
}

/// The text of a lane value: "-3.2 dB", "L 40" / "C" / "R 40" for the pan, an effect
/// parameter in its own units.
pub fn value_text(t: &Track, target: AutoTarget, v: f32) -> String {
    match target {
        AutoTarget::Volume => format!("{v:.1} dB"),
        AutoTarget::Pan => {
            let n = (v * 64.0).round() as i32;
            match n {
                0 => "C".into(),
                n if n < 0 => format!("L {}", -n),
                n => format!("R {n}"),
            }
        }
        AutoTarget::Fx { slot, param } => {
            let Some(info) = t
                .inserts
                .get(slot as usize)
                .and_then(|i| i.kind.params().get(param as usize))
            else {
                return format!("{v:.2}");
            };
            let x = info.from_norm(v);
            if info.unit.is_empty() {
                if info.max <= 1.0 {
                    format!("{:.0} %", x * 100.0)
                } else {
                    format!("{x:.2}")
                }
            } else if x.abs() >= 100.0 {
                format!("{x:.0} {}", info.unit)
            } else {
                format!("{x:.1} {}", info.unit)
            }
        }
    }
}

/// 0 (bottom) to 1 (top) for a lane value.
fn frac_of(target: AutoTarget, v: f32) -> f32 {
    match target {
        AutoTarget::Volume => db_to_frac(v),
        AutoTarget::Pan => (v.clamp(-1.0, 1.0) + 1.0) / 2.0,
        AutoTarget::Fx { .. } => v.clamp(0.0, 1.0),
    }
}

fn value_of(target: AutoTarget, f: f32) -> f32 {
    let (lo, hi) = Automation::range(target);
    match target {
        AutoTarget::Volume => frac_to_db(f).clamp(lo, hi),
        AutoTarget::Pan => (f.clamp(0.0, 1.0) * 2.0 - 1.0).clamp(lo, hi),
        AutoTarget::Fx { .. } => f.clamp(lo, hi),
    }
}

/// The bend that puts the value at the middle of a stretch at `f` of the way from the first
/// value to the second (the inverse of what [`curve_shape`] does at 0.5).
fn curve_from_fraction(f: f32) -> f32 {
    let f = f.clamp(0.02, 0.98);
    let ln_half = 0.5f32.ln();
    if (f - 0.5).abs() < 1e-3 {
        0.0
    } else if f < 0.5 {
        ((f.ln() / ln_half - 1.0) / 4.0).clamp(0.0, 1.0)
    } else {
        (-((1.0 - f).ln() / ln_half - 1.0) / 4.0).clamp(-1.0, 0.0)
    }
}

/// How far a click can be from a point and still be on it, in pixels.
const GRAB: f32 = 8.0;
/// The editor's drawing area height.
const CANVAS_H: f32 = 190.0;

/// Where a point is on `canvas` for the beats `from..from + span`.
fn point_pos(
    canvas: Rect,
    from: f64,
    span: f64,
    target: AutoTarget,
    beat: f64,
    value: f32,
) -> egui::Pos2 {
    pos2(
        canvas.left() + ((beat - from) / span) as f32 * canvas.width(),
        canvas.bottom() - frac_of(target, value) * canvas.height(),
    )
}

/// The beat and value under `pos`.
fn beat_value_at(
    canvas: Rect,
    from: f64,
    span: f64,
    target: AutoTarget,
    pos: egui::Pos2,
) -> (f64, f32) {
    let beat = from + ((pos.x - canvas.left()) / canvas.width()).clamp(0.0, 1.0) as f64 * span;
    let f = (canvas.bottom() - pos.y) / canvas.height();
    (beat, value_of(target, f))
}

/// The Automation editor: a window with the lanes of one track.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    let Some(ti) = s.mixer.auto_editor.track else {
        return;
    };
    if ti >= p.tracks.len() {
        s.mixer.auto_editor.track = None;
        return;
    }
    let ed = &mut s.mixer.auto_editor;
    if ed.span <= 0.0 {
        let end = p
            .tracks
            .iter()
            .flat_map(|t| &t.regions)
            .map(|r| p.region_end_beats(r))
            .fold(16.0f64, f64::max);
        ed.from = 0.0;
        ed.span = end.max(8.0);
    }
    let mut open = true;
    let title = trf("Automation: {}", &[&p.tracks[ti].name]);
    let beat_now = p.position_beats;
    egui::Window::new(title)
        .id(egui::Id::unique("automation_editor"))
        .open(&mut open)
        .default_size([680.0, 330.0])
        .show(ctx, |ui| {
            let mut mode = p.tracks[ti].auto.mode;
            let mut target = p.tracks[ti].auto.shown;
            // A lane of an effect that is not there any more shows the fader's.
            if lane_label(&p.tracks[ti], target) == "?" {
                target = AutoTarget::Volume;
            }
            ui.horizontal(|ui| {
                ui.label(tr("Mode"));
                egui::ComboBox::from_id_salt("auto_mode")
                    .selected_text(tr(mode.label()))
                    .show_ui(ui, |ui| {
                        for m in AutoMode::ALL {
                            ui.selectable_value(&mut mode, m, tr(m.label()));
                        }
                    });
                ui.label(tr("Parameter"));
                let t = &p.tracks[ti];
                egui::ComboBox::from_id_salt("auto_param")
                    .selected_text(lane_label(t, target))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        let mut choices = vec![AutoTarget::Volume, AutoTarget::Pan];
                        for (slot, ins) in t.inserts.iter().enumerate() {
                            for k in 0..ins.kind.params().len() {
                                choices.push(AutoTarget::Fx {
                                    slot: slot as u8,
                                    param: k as u8,
                                });
                            }
                        }
                        for c in choices {
                            let dot = if t.auto.lane(c).is_empty() {
                                "  "
                            } else {
                                "• "
                            };
                            ui.selectable_value(
                                &mut target,
                                c,
                                format!("{dot}{}", lane_label(t, c)),
                            );
                        }
                    });
                ui.separator();
                let ed = &mut s.mixer.auto_editor;
                if ui.button("−").on_hover_text(tr("Zoom Out")).clicked() {
                    ed.span = (ed.span * 2.0).min(4096.0);
                }
                if ui.button("+").on_hover_text(tr("Zoom In")).clicked() {
                    ed.span = (ed.span / 2.0).max(1.0);
                }
                if ui.button("◀").clicked() {
                    ed.from = (ed.from - ed.span / 2.0).max(0.0);
                }
                if ui.button("▶").clicked() {
                    ed.from += ed.span / 2.0;
                }
                ui.separator();
                if ui.button(tr("Clear Lane")).clicked() {
                    p.tracks[ti].auto.lane_mut(target).clear();
                    p.tracks[ti].auto.prune();
                }
            });
            if mode != p.tracks[ti].auto.mode {
                set_mode(&mut p.tracks[ti], mode, beat_now);
            }
            p.tracks[ti].auto.shown = target;
            canvas(ui, p, &mut s.mixer.auto_editor, ti, target);
            let n = p.tracks[ti].auto.lane(target).len();
            ui.label(trf("{} points", &[&n]));
        });
    if !open {
        s.mixer.auto_editor.track = None;
        s.mixer.auto_editor.drag = None;
    }
}

/// The lane's drawing area and its mouse editing.
fn canvas(ui: &mut Ui, p: &mut Project, ed: &mut AutoEditor, ti: usize, target: AutoTarget) {
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, CANVAS_H), Sense::click_and_drag());
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 3.0, Color32::from_gray(0x24));
    let (from, span) = (ed.from, ed.span.max(1.0));
    // Bar lines.
    for bar in p.bars_in(from, from + span) {
        let x = rect.left() + ((bar.start - from) / span) as f32 * rect.width();
        if x < rect.left() || x > rect.right() {
            continue;
        }
        g.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x36)),
        );
        if span <= 128.0 || bar.bar0 % 4 == 0 {
            g.text(
                pos2(x + 3.0, rect.top() + 2.0),
                Align2::LEFT_TOP,
                format!("{}", bar.bar0 + 1),
                FontId::proportional(10.0),
                Color32::from_gray(0x80),
            );
        }
    }
    // Value lines: 0 dB, or the centre.
    let marks: &[f32] = match target {
        AutoTarget::Volume => &[6.0, 0.0, -6.0, -20.0, -40.0],
        AutoTarget::Pan => &[-1.0, 0.0, 1.0],
        AutoTarget::Fx { .. } => &[0.0, 0.25, 0.5, 0.75, 1.0],
    };
    for m in marks {
        let y = rect.bottom() - frac_of(target, *m) * rect.height();
        g.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, Color32::from_gray(if *m == 0.0 { 0x4a } else { 0x30 })),
        );
        g.text(
            pos2(rect.right() - 4.0, y - 1.0),
            Align2::RIGHT_BOTTOM,
            value_text(&p.tracks[ti], target, *m),
            FontId::proportional(9.0),
            Color32::from_gray(0x78),
        );
    }
    let colour = match target {
        AutoTarget::Volume => Color32::from_rgb(0x6c, 0xd0, 0x5a),
        AutoTarget::Pan => Color32::from_rgb(0x5a, 0xa8, 0xf0),
        AutoTarget::Fx { .. } => Color32::from_rgb(0xe6, 0x9a, 0x2e),
    };
    let points: Vec<AutoPoint> = p.tracks[ti].auto.lane(target).to_vec();
    let to_pos = |beat: f64, value: f32| point_pos(rect, from, span, target, beat, value);
    // Where the handle that bends the stretch after point `i` is.
    let handle_pos = |i: usize| {
        let (a, b) = (points[i], points[i + 1]);
        let v = a.value + (b.value - a.value) * curve_shape(0.5, a.curve);
        to_pos((a.beat + b.beat) / 2.0, v)
    };
    if !points.is_empty() {
        let mut line: Vec<egui::Pos2> = Vec::with_capacity(points.len() * 8 + 2);
        line.push(pos2(rect.left(), to_pos(points[0].beat, points[0].value).y));
        for (i, a) in points.iter().enumerate() {
            line.push(to_pos(a.beat, a.value));
            if let Some(b) = points.get(i + 1) {
                if a.curve != 0.0 {
                    for k in 1..16 {
                        let t = k as f32 / 16.0;
                        let v = a.value + (b.value - a.value) * curve_shape(t, a.curve);
                        line.push(to_pos(a.beat + (b.beat - a.beat) * t as f64, v));
                    }
                }
            }
        }
        let last = points[points.len() - 1];
        line.push(pos2(rect.right(), to_pos(last.beat, last.value).y));
        g.add(Shape::line(line, Stroke::new(1.6, colour)));
        for i in 0..points.len().saturating_sub(1) {
            if points[i].value == points[i + 1].value {
                continue;
            }
            let c = handle_pos(i);
            if !rect.expand(4.0).contains(c) {
                continue;
            }
            let r = 3.5;
            let diamond = vec![
                c + vec2(0.0, -r),
                c + vec2(r, 0.0),
                c + vec2(0.0, r),
                c + vec2(-r, 0.0),
            ];
            let bent = points[i].curve != 0.0;
            g.add(Shape::convex_polygon(
                diamond,
                if bent || ed.drag == Some(Drag::Curve(i)) {
                    colour
                } else {
                    Color32::from_gray(0x24)
                },
                Stroke::new(1.0, colour),
            ));
        }
        for (i, a) in points.iter().enumerate() {
            let c = to_pos(a.beat, a.value);
            if !rect.expand(4.0).contains(c) {
                continue;
            }
            g.circle_filled(
                c,
                4.0,
                if ed.drag == Some(Drag::Point(i)) {
                    Color32::WHITE
                } else {
                    colour
                },
            );
            g.circle_stroke(c, 4.0, Stroke::new(1.0, Color32::from_gray(0x10)));
        }
    } else {
        g.text(
            rect.center(),
            Align2::CENTER_CENTER,
            tr("Click to add a point"),
            FontId::proportional(12.0),
            Color32::from_gray(0x78),
        );
    }
    // The playhead.
    let x = rect.left() + ((p.position_beats - from) / span) as f32 * rect.width();
    if x >= rect.left() && x <= rect.right() {
        g.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, crate::tracks_area::PLAYHEAD_LINE),
        );
    }
    g.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_gray(0x10)),
        StrokeKind::Inside,
    );

    // Editing.
    let near_point = |pos: egui::Pos2| {
        points
            .iter()
            .position(|pt| to_pos(pt.beat, pt.value).distance(pos) <= GRAB)
    };
    let near_handle = |pos: egui::Pos2| {
        (0..points.len().saturating_sub(1)).find(|i| {
            points[*i].value != points[*i + 1].value && handle_pos(*i).distance(pos) <= GRAB
        })
    };
    if resp.drag_started() {
        ed.drag = resp.interact_pointer_pos().and_then(|pos| {
            if let Some(i) = near_point(pos) {
                Some(Drag::Point(i))
            } else if let Some(i) = near_handle(pos) {
                Some(Drag::Curve(i))
            } else {
                // Dragging from empty space adds a point and moves it.
                let (b, v) = beat_value_at(rect, from, span, target, pos);
                p.tracks[ti].auto.set_point(target, b, v);
                p.tracks[ti]
                    .auto
                    .lane(target)
                    .iter()
                    .position(|pt| (pt.beat - b).abs() < 1e-9)
                    .map(Drag::Point)
            }
        });
    }
    if let (Some(drag), Some(pos), true) = (ed.drag, resp.interact_pointer_pos(), resp.dragged()) {
        let (b, v) = beat_value_at(rect, from, span, target, pos);
        let lane = p.tracks[ti].auto.lane_mut(target);
        match drag {
            Drag::Point(i) if i < lane.len() => {
                // A point stays between its neighbours.
                let lo = if i == 0 { 0.0 } else { lane[i - 1].beat + 1e-4 };
                let hi = lane.get(i + 1).map_or(f64::MAX, |n| n.beat - 1e-4);
                lane[i].beat = b.clamp(lo, hi.max(lo));
                lane[i].value = v;
            }
            Drag::Curve(i) if i + 1 < lane.len() => {
                let (a, c) = (lane[i], lane[i + 1]);
                if c.value != a.value {
                    lane[i].curve = curve_from_fraction((v - a.value) / (c.value - a.value));
                }
            }
            _ => {}
        }
    }
    if resp.drag_stopped() {
        ed.drag = None;
    }
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if near_point(pos).is_none() && near_handle(pos).is_none() {
                let (b, v) = beat_value_at(rect, from, span, target, pos);
                p.tracks[ti].auto.set_point(target, b, v);
            }
        }
    }
    if resp.secondary_clicked() || resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let lane = p.tracks[ti].auto.lane_mut(target);
            if let Some(i) = near_point(pos) {
                if i < lane.len() {
                    lane.remove(i);
                }
            } else if let Some(i) = near_handle(pos) {
                // A double click on a handle makes the stretch straight again.
                if i < lane.len() {
                    lane[i].curve = 0.0;
                }
            }
        }
    }
    p.tracks[ti].auto.prune();
    // Hover: the value under the pointer.
    if let Some(pos) = resp.hover_pos() {
        let (b, v) = match near_point(pos) {
            Some(i) => (points[i].beat, points[i].value),
            None => beat_value_at(rect, from, span, target, pos),
        };
        let bar = p.beats_to_bar(b) + 1.0;
        let text = format!("{} · {:.2}", value_text(&p.tracks[ti], target, v), bar);
        resp.on_hover_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prowave_engine::FxKind;

    fn project() -> Project {
        let mut p = Project::empty();
        p.tracks[0].auto.mode = AutoMode::Touch;
        p
    }

    #[test]
    fn touch_writes_while_held_and_returns_to_the_lane() {
        let mut p = project();
        p.tracks[0].auto.set_point(AutoTarget::Volume, 0.0, -6.0);
        p.tracks[0].auto.set_point(AutoTarget::Volume, 100.0, -6.0);
        p.playing = true;
        p.position_beats = 8.0;
        // Nothing held: the strip plays the lane and tick writes nothing.
        tick(&mut p);
        assert_eq!(p.tracks[0].auto.volume.len(), 2);
        assert_eq!(shown_value(&p.tracks[0], AutoTarget::Volume, 8.0), -6.0);
        // The fader is touched and pulled down while playing.
        moved(&mut p.tracks[0], AutoTarget::Volume, -20.0);
        assert_eq!(p.tracks[0].auto.held, Some(AutoTarget::Volume));
        for k in 0..8 {
            p.position_beats = 8.0 + k as f64 * 0.25;
            tick(&mut p);
        }
        released(&mut p.tracks[0], AutoTarget::Volume, p.position_beats, true);
        assert_eq!(p.tracks[0].auto.held, None);
        let written: Vec<_> = p.tracks[0]
            .auto
            .volume
            .iter()
            .filter(|pt| (8.0..=10.0).contains(&pt.beat))
            .collect();
        assert!(written.len() >= 8 && written.iter().all(|pt| pt.value == -20.0));
        // Before and after the written stretch the old lane remains.
        assert_eq!(p.tracks[0].auto.volume[0], AutoPoint::new(0.0, -6.0));
        assert_eq!(
            p.tracks[0].auto.volume.last().copied(),
            Some(AutoPoint::new(100.0, -6.0))
        );
        // The lane plays again once the control is let go: it climbs back to the old value.
        assert_eq!(shown_value(&p.tracks[0], AutoTarget::Volume, 100.0), -6.0);
        assert!(shown_value(&p.tracks[0], AutoTarget::Volume, 50.0) < -6.0);
    }

    #[test]
    fn write_overwrites_the_passed_stretch_and_latch_stops_with_playback() {
        let mut p = project();
        p.tracks[0].auto.mode = AutoMode::Write;
        p.tracks[0].auto.set_point(AutoTarget::Volume, 4.5, 0.0);
        p.tracks[0].volume_db = -12.0;
        p.playing = true;
        for k in 0..12 {
            p.position_beats = 4.0 + k as f64 * 0.25;
            tick(&mut p);
        }
        // The old point at 4.5 lies inside the written stretch and is gone.
        assert!(p.tracks[0].auto.volume.iter().all(|pt| pt.value == -12.0));
        assert!(p.tracks[0].auto.volume.len() >= 6);
        // Write mode is not played back, so the fader shows its own value.
        assert_eq!(shown_value(&p.tracks[0], AutoTarget::Volume, 5.0), -12.0);
        // Latch: held after a touch, still writing after the release, cleared by stop.
        set_mode(&mut p.tracks[0], AutoMode::Latch, 6.0);
        moved(&mut p.tracks[0], AutoTarget::Pan, 0.5);
        released(&mut p.tracks[0], AutoTarget::Pan, 6.0, true);
        assert_eq!(p.tracks[0].auto.held, Some(AutoTarget::Pan));
        p.position_beats = 6.0;
        tick(&mut p);
        assert!(!p.tracks[0].auto.pan.is_empty());
        p.playing = false;
        tick(&mut p);
        assert_eq!(p.tracks[0].auto.held, None);
    }

    #[test]
    fn leaving_read_keeps_the_value_and_read_locks_the_control() {
        let mut p = project();
        let t = &mut p.tracks[0];
        t.auto.mode = AutoMode::Read;
        assert!(editable(t, AutoTarget::Volume), "no lane yet: free");
        t.auto.set_point(AutoTarget::Volume, 0.0, -9.0);
        assert!(!editable(t, AutoTarget::Volume));
        assert!(editable(t, AutoTarget::Pan));
        set_mode(t, AutoMode::Off, 3.0);
        assert_eq!(t.volume_db, -9.0);
        assert!(editable(t, AutoTarget::Volume));
        assert_eq!(shown_value(t, AutoTarget::Volume, 3.0), -9.0);
    }

    #[test]
    fn points_stay_sorted_in_range_and_bounded() {
        let mut a = Automation::default();
        a.set_point(AutoTarget::Volume, 4.0, 3.0);
        a.set_point(AutoTarget::Volume, 2.0, -500.0);
        a.set_point(AutoTarget::Volume, 4.0, -3.0);
        a.set_point(AutoTarget::Volume, f64::NAN, 0.0);
        a.set_point(AutoTarget::Pan, 1.0, 7.0);
        assert_eq!(
            a.volume,
            [AutoPoint::new(2.0, -60.0), AutoPoint::new(4.0, -3.0)]
        );
        assert_eq!(a.pan, [AutoPoint::new(1.0, 1.0)]);
        let t = Project::empty().tracks.remove(0);
        assert_eq!(value_text(&t, AutoTarget::Pan, 0.0), "C");
        assert_eq!(value_text(&t, AutoTarget::Pan, -0.5), "L 32");
        assert_eq!(value_text(&t, AutoTarget::Volume, -3.25), "-3.2 dB");
        for target in [AutoTarget::Volume, AutoTarget::Pan] {
            let (lo, hi) = Automation::range(target);
            assert!((value_of(target, frac_of(target, lo)) - lo).abs() < 0.5);
            assert!((value_of(target, frac_of(target, hi)) - hi).abs() < 0.5);
        }
    }

    #[test]
    fn the_bend_handle_puts_the_middle_of_a_stretch_where_it_is_dragged() {
        for f in [0.05f32, 0.2, 0.4, 0.5, 0.6, 0.8, 0.95] {
            let c = curve_from_fraction(f);
            assert!((-1.0..=1.0).contains(&c));
            let back = curve_shape(0.5, c);
            // Within what the bend can reach (the ends of its range hold the nearest).
            let expected = f.clamp(curve_shape(0.5, 1.0), curve_shape(0.5, -1.0));
            assert!((back - expected).abs() < 0.01, "{f}: {back} vs {expected}");
        }
        assert_eq!(curve_from_fraction(0.5), 0.0);
    }

    #[test]
    fn effect_parameters_are_written_played_and_follow_their_effects() {
        let mut p = project();
        let t = &mut p.tracks[0];
        t.inserts = vec![
            crate::model::Insert::new(FxKind::Compressor),
            crate::model::Insert::new(FxKind::ChannelEq),
        ];
        let thr = AutoTarget::Fx { slot: 0, param: 0 };
        // Touch the Threshold slider while playing: its value is written as a position.
        p.playing = true;
        p.position_beats = 2.0;
        moved(&mut p.tracks[0], thr, -30.0);
        assert_eq!(p.tracks[0].auto.held, Some(thr));
        assert_eq!(p.tracks[0].inserts[0].params[0], -30.0);
        tick(&mut p);
        released(&mut p.tracks[0], thr, 2.5, true);
        let lane = p.tracks[0].auto.lane(thr).to_vec();
        assert!(!lane.is_empty());
        // -30 dB in -60..0 is half way.
        assert!((lane[0].value - 0.5).abs() < 1e-5, "{}", lane[0].value);
        assert_eq!(p.tracks[0].auto.fx.len(), 1);
        // Played back: the slider shows the lane's value in dB.
        p.tracks[0].auto.mode = AutoMode::Read;
        let v = shown_value(&p.tracks[0], thr, 3.0);
        assert!((natural(&p.tracks[0], thr, v) + 30.0).abs() < 1e-3);
        assert_eq!(value_text(&p.tracks[0], thr, v), "-30.0 dB");
        assert_eq!(lane_label(&p.tracks[0], thr), "Compressor · Threshold");
        assert!(!editable(&p.tracks[0], thr));
        // Leaving Read keeps the value the lane had.
        p.tracks[0].inserts[0].params[0] = -10.0;
        set_mode(&mut p.tracks[0], AutoMode::Off, 3.0);
        assert!((p.tracks[0].inserts[0].params[0] + 30.0).abs() < 1e-3);
        // Effects that move or go take their lanes with them.
        p.tracks[0]
            .auto
            .set_point(AutoTarget::Fx { slot: 1, param: 1 }, 1.0, 0.8);
        p.tracks[0].auto.insert_swapped(0, 1);
        assert!(!p.tracks[0]
            .auto
            .lane(AutoTarget::Fx { slot: 1, param: 0 })
            .is_empty());
        assert!(!p.tracks[0]
            .auto
            .lane(AutoTarget::Fx { slot: 0, param: 1 })
            .is_empty());
        p.tracks[0].auto.insert_removed(0);
        assert_eq!(p.tracks[0].auto.fx.len(), 1);
        assert_eq!(p.tracks[0].auto.fx[0].slot, 0, "the later effect moved up");
        p.tracks[0].auto.insert_replaced(0);
        assert!(p.tracks[0].auto.fx.is_empty());
        // A lane of an effect that is not there has no name and does not break anything.
        let gone = AutoTarget::Fx { slot: 9, param: 0 };
        assert_eq!(lane_label(&p.tracks[0], gone), "?");
        moved(&mut p.tracks[0], gone, 1.0);
        assert_eq!(current(&p.tracks[0], gone), 0.0);
    }

    #[test]
    fn the_editor_opens_draws_edits_and_bends_a_lane() {
        let mut p = Project::empty();
        let mut s = UiState::new();
        s.mixer.auto_editor.track = Some(0);
        let mut h = crate::ui_test::Harness::new(900.0, 600.0);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        }
        assert!(s.mixer.auto_editor.span >= 8.0, "a range was chosen");
        p.tracks[0].auto.set_point(AutoTarget::Volume, 2.0, -6.0);
        p.tracks[0].auto.set_point(AutoTarget::Volume, 6.0, -30.0);
        p.tracks[0].auto.volume[0].curve = 0.6;
        // An effect lane shows too.
        p.tracks[0].inserts = vec![crate::model::Insert::new(FxKind::Limiter)];
        p.tracks[0].auto.shown = AutoTarget::Fx { slot: 0, param: 1 };
        let fx = p.tracks[0].auto.shown;
        p.tracks[0].auto.set_point(fx, 1.0, 0.3);
        p.tracks[0].auto.set_point(fx, 4.0, 0.9);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        }
        assert_eq!(p.tracks[0].auto.volume.len(), 2);
        assert_eq!(p.tracks[0].auto.volume[0].curve, 0.6);
        // The effect goes away: the editor falls back to the fader.
        p.tracks[0].inserts.clear();
        for _ in 0..2 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        }
        assert_eq!(p.tracks[0].auto.shown, AutoTarget::Volume);
        // A track that goes away closes the editor.
        p.tracks.clear();
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        assert!(s.mixer.auto_editor.track.is_none());
    }
}
