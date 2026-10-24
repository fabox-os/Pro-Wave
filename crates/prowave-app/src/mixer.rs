//! Mixer (Logic Pro: Mixer in the main window): local toolbar, row labels and channel strips
//! with Setting, Gain Reduction, EQ, Input, Audio FX, Sends, Output, Group, Automation, Pan,
//! dB readouts, fader with scale, stereo meter, M/S and the coloured name label.
//!
//! Works today: fader, pan, mute, solo (all reach the engine), real post-fader meters with
//! peak hold, strip selection, Single/Tracks/All, the strip-type filters, scrolling; the Audio
//! FX slots (pick an effect, open its window, bypass, reorder), the Gain Reduction meter and
//! the EQ curve, Sends to buses (with Sends on Faders) and the Aux strips that play them, and
//! the Automation modes (Read, Touch, Latch, Write) with the Automation editor. Groups are
//! drawn but not functional yet.
//!
//! [`channel_strip`] is also used by the Inspector's two strips.
use crate::i18n::{tr, trf};
use crate::model::{AutoMode, AutoTarget, Insert, Project, SendSlot, TrackKind, MAX_SENDS};
use crate::widgets::{
    back_chevron, menu_title, popup_button, stroke, todo_item, BLUE, TEXT, TEXT_DIM,
};
use crate::UiState;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Painter, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
};
use std::collections::HashMap;

const TOOLBAR_H: f32 = 30.0;
const LABEL_W: f32 = 88.0;
const GAP: f32 = 2.0;

const TOOLBAR_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
const MIXER_BG: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const STRIP_BG: Color32 = Color32::from_rgb(0x3e, 0x3e, 0x3e);
const STRIP_SEL: Color32 = Color32::from_rgb(0x4d, 0x4d, 0x4d);
const SLOT_EMPTY: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x2c);
const SLOT_GRAY: Color32 = Color32::from_rgb(0x55, 0x55, 0x55);
const PLUGIN_BLUE: Color32 = Color32::from_rgb(0x36, 0x6f, 0xd2);
/// A strip that makes no sound (muted, or another track is soloed): instruments, plug-ins
/// and sends turn grey, and so do the meters.
const PLUGIN_OFF: Color32 = Color32::from_rgb(0x4a, 0x4a, 0x4a);
const METER_OFF: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);
const READ_GREEN: Color32 = Color32::from_rgb(0x6c, 0xd0, 0x5a);
const MUTE_ON: Color32 = Color32::from_rgb(0x3b, 0x8c, 0xe6);
const SOLO_ON: Color32 = Color32::from_rgb(0xe6, 0xc2, 0x2e);
const OUT_PURPLE: Color32 = Color32::from_rgb(0x8a, 0x3f, 0xc8);
const MASTER_PINK: Color32 = Color32::from_rgb(0xb0, 0x3c, 0x9a);

/// Fader/meter scale marks in dB, top to bottom (Logic's channel strip scale).
const SCALE: [f32; 11] = [
    6.0, 3.0, 0.0, -3.0, -6.0, -10.0, -15.0, -20.0, -30.0, -40.0, -60.0,
];
pub(crate) const FILTERS: [&str; 8] = [
    "Audio",
    "Inst",
    "Aux",
    "Bus",
    "Input",
    "Output",
    "Master/VCA",
    "MIDI",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MixerMode {
    Single,
    Tracks,
    All,
}

pub struct MixerView {
    pub mode: MixerMode,
    /// Visibility per strip type, in `FILTERS` order.
    pub filters: [bool; 8],
    pub wide: bool,
    pub scroll_x: f32,
    /// Peak hold per strip (linear), shared with the Inspector strips.
    pub peak_hold: HashMap<StripKey, f32>,
    /// The effect whose window is open: (track, slot).
    pub fx_window: Option<(usize, usize)>,
    /// The Automation editor window.
    pub auto_editor: crate::automation::AutoEditor,
    /// Sends on Faders: the bus whose send levels the faders set.
    pub sends_on_faders: Option<u8>,
    /// Edit > Copy Channel Strip.
    pub copied: Option<CopiedStrip>,
}

/// A channel strip's effects, sends, level and pan, as Copy Channel Strip keeps them.
#[derive(Clone)]
pub struct CopiedStrip {
    inserts: Vec<Insert>,
    sends: Vec<SendSlot>,
    volume_db: f32,
    pan: f32,
}

impl Default for MixerView {
    fn default() -> Self {
        Self {
            mode: MixerMode::Tracks,
            filters: [true; 8],
            wide: false,
            scroll_x: 0.0,
            peak_hold: HashMap::new(),
            fx_window: None,
            auto_editor: Default::default(),
            sends_on_faders: None,
            copied: None,
        }
    }
}

/// Live meter values handed in by the app each frame.
#[derive(Default)]
pub struct Meters {
    pub tracks: Vec<(f32, f32)>,
    /// Gain reduction in dB of each track's compressors.
    pub reduction: Vec<f32>,
    pub master: (f32, f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StripKey {
    Track(usize),
    StereoOut,
    Master,
}

fn id(ns: &'static str, key: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    egui::Id::unique((ns, key))
}

/// 0 (bottom, -60 dB and below) .. 1 (top, +6 dB).
pub(crate) fn db_to_frac(db: f32) -> f32 {
    if db >= SCALE[0] {
        return 1.0;
    }
    let n = SCALE.len() - 1;
    for i in 0..n {
        let (hi, lo) = (SCALE[i], SCALE[i + 1]);
        if db <= hi && db >= lo {
            let idx = i as f32 + (hi - db) / (hi - lo);
            return 1.0 - idx / n as f32;
        }
    }
    0.0
}

pub(crate) fn frac_to_db(f: f32) -> f32 {
    let n = SCALE.len() - 1;
    let idx = ((1.0 - f.clamp(0.0, 1.0)) * n as f32).min(n as f32 - 1e-4);
    let i = idx.floor() as usize;
    SCALE[i] + (SCALE[i + 1] - SCALE[i]) * (idx - i as f32)
}

pub(crate) fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-6 {
        -120.0
    } else {
        20.0 * x.log10()
    }
}

/// Shortens `text` with a trailing "…" so it fits in `max_w` (Logic truncates strip names).
pub fn elide(g: &Painter, text: &str, font: &FontId, max_w: f32) -> String {
    let width = |t: &str| {
        g.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "…";
        if width(&candidate) <= max_w {
            return candidate;
        }
    }
    String::new()
}

fn slot(g: &Painter, r: Rect, text: &str, fill: Color32, color: Color32) {
    g.rect_filled(r, 3.0, fill);
    if !text.is_empty() {
        let font = FontId::proportional(10.5);
        let shown = elide(g, text, &font, r.width() - 6.0);
        g.text(r.center(), Align2::CENTER_CENTER, shown, font, color);
    }
}

fn small_button(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    id: egui::Id,
    label: &str,
    on: bool,
    on_col: Color32,
) -> bool {
    let resp = ui.interact(r, id, Sense::click());
    g.rect_filled(r, 3.0, if on { on_col } else { SLOT_GRAY });
    let txt = if on && on_col == SOLO_ON {
        Color32::BLACK
    } else {
        Color32::from_gray(0xe8)
    };
    g.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        txt,
    );
    resp.clicked()
}

/// The strip's EQ thumbnail: the curve of its Channel EQ (`params`), or just "EQ" without one.
fn eq_thumb(
    g: &Painter,
    r: Rect,
    eq: Option<(prowave_engine::FxKind, [f32; prowave_engine::MAX_FX_PARAMS])>,
) {
    g.rect_filled(r, 3.0, Color32::from_gray(0x24));
    g.rect_stroke(
        r,
        3.0,
        Stroke::new(1.0, Color32::from_gray(0x1a)),
        StrokeKind::Inside,
    );
    if let Some((kind, params)) = eq {
        let n = 24;
        let freqs: Vec<f32> = (0..=n)
            .map(|i| 20.0 * 1000f32.powf(i as f32 / n as f32))
            .collect();
        let curve = prowave_engine::response_db(kind, params, &freqs, 48_000.0);
        let pts: Vec<_> = curve
            .iter()
            .enumerate()
            .map(|(i, db)| {
                pos2(
                    r.left() + 3.0 + i as f32 / n as f32 * (r.width() - 6.0),
                    r.center().y - db.clamp(-18.0, 18.0) / 18.0 * (r.height() / 2.0 - 3.0),
                )
            })
            .collect();
        g.add(Shape::line(
            pts,
            Stroke::new(1.2, Color32::from_rgb(0x8a, 0xb8, 0xf0)),
        ));
    } else {
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            "EQ",
            FontId::proportional(10.0),
            TEXT_DIM,
        );
    }
}

/// What a fader or knob did this frame.
#[derive(Default, Clone, Copy)]
struct Moved {
    /// The new value, when it was dragged or double-clicked.
    value: Option<f32>,
    /// The gesture began / ended (a double click is both).
    started: bool,
    stopped: bool,
}

/// The pan knob at `c` showing `value` (-1..1); drag it, or double-click for the centre. Not
/// moved when `editable` is false (the strip plays its automation, or has no pan).
fn pan_knob(
    ui: &Ui,
    g: &Painter,
    c: egui::Pos2,
    id: egui::Id,
    value: f32,
    editable: bool,
) -> Moved {
    let r = Rect::from_center_size(c, vec2(26.0, 26.0));
    let mut out = Moved::default();
    if editable {
        let resp = ui.interact(r, id, Sense::click_and_drag());
        if resp.dragged() {
            out.value = Some((value - resp.drag_delta().y * 0.01).clamp(-1.0, 1.0));
        }
        if resp.double_clicked() {
            out.value = Some(0.0);
            out.started = true;
            out.stopped = true;
        }
        out.started |= resp.drag_started();
        out.stopped |= resp.drag_stopped();
    }
    crate::widgets::draw_knob(g, c, 12.0, value);
    out
}

/// Logic-style meter colour for a level in dB: green, yellow from -6 dB, red above 0 dB.
pub(crate) fn meter_colour(db: f32) -> Color32 {
    if db > 0.0 {
        Color32::from_rgb(0xe8, 0x3a, 0x2c)
    } else if db > -6.0 {
        Color32::from_rgb(0xe6, 0xc8, 0x2a)
    } else {
        Color32::from_rgb(0x34, 0xc2, 0x3e)
    }
}

/// Fader (scale + slot + cap) and stereo meter with peak-hold lines, laid out like Logic's
/// channel strip: fader scale on the left, the cap, the meter scale numbers, then L/R bars.
/// Returns the new fader value in dB when it was dragged or double-clicked.
#[allow(clippy::too_many_arguments)]
fn fader_and_meter(
    ui: &Ui,
    g: &Painter,
    area: Rect,
    id: egui::Id,
    db: f32,
    editable: bool,
    peaks: (f32, f32),
    inactive: bool,
) -> Moved {
    let top = area.top() + 8.0;
    let bottom = area.bottom() - 6.0;
    let h = (bottom - top).max(1.0);
    let y_of = |f: f32| bottom - f * h;
    let w = area.width();
    let scale_col = Color32::from_gray(0x8e);
    let tiny = FontId::proportional(6.5);

    // Columns, proportional to the strip width (Logic: fader left of centre, meter right).
    let fader_x = area.left() + (w * 0.30).max(22.0);
    let num_x = area.left() + w * 0.56;
    let bar_w = 5.0;
    let meter_left = (area.left() + w * 0.66).min(area.right() - 2.0 * bar_w - 6.0);

    // Fader scale: tick marks with small numbers, left of the slot.
    for mark in SCALE {
        let y = y_of(db_to_frac(mark));
        let tick_len = if mark == 0.0 { 6.0 } else { 4.0 };
        let tx = fader_x - 13.0;
        g.line_segment(
            [pos2(tx - tick_len, y), pos2(tx, y)],
            Stroke::new(1.0, scale_col),
        );
        if w >= 70.0 {
            g.text(
                pos2(tx - tick_len - 2.0, y),
                Align2::RIGHT_CENTER,
                format!("{}", mark.abs() as i32),
                tiny.clone(),
                scale_col,
            );
        }
        // Meter scale numbers between the fader and the meter.
        g.text(
            pos2(num_x, y),
            Align2::CENTER_CENTER,
            format!("{}", mark.abs() as i32),
            tiny.clone(),
            scale_col,
        );
    }

    // Fader slot.
    let slot_r = Rect::from_min_max(pos2(fader_x - 2.0, top), pos2(fader_x + 2.0, bottom));
    g.rect_filled(slot_r, 2.0, Color32::from_gray(0x14));
    g.line_segment(
        [slot_r.right_top(), slot_r.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x50)),
    );

    // Meter bars with peak-hold lines (hold falls back at 12 dB/s).
    let hold_id = id.with("hold");
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let mut hold: (f32, f32) = ui
        .ctx()
        .data(|d| d.get_temp(hold_id).unwrap_or((-120.0, -120.0)));
    let levels = (lin_to_db(peaks.0), lin_to_db(peaks.1));
    hold.0 = levels.0.max(hold.0 - 12.0 * dt);
    hold.1 = levels.1.max(hold.1 - 12.0 * dt);
    ui.ctx().data_mut(|d| d.insert_temp(hold_id, hold));
    // One dark housing holds both bars (Logic draws L and R side by side in one well).
    let housing = Rect::from_min_max(
        pos2(meter_left - 2.0, top - 1.0),
        pos2(meter_left + 2.0 * bar_w + 3.0, bottom + 1.0),
    );
    g.rect_filled(housing, 2.5, Color32::from_gray(0x10));
    g.rect_stroke(
        housing,
        2.5,
        Stroke::new(1.0, Color32::from_gray(0x2e)),
        StrokeKind::Inside,
    );
    for (i, (lvl, hd)) in [(levels.0, hold.0), (levels.1, hold.1)]
        .into_iter()
        .enumerate()
    {
        let x0 = meter_left + i as f32 * (bar_w + 1.0);
        let bar = Rect::from_min_max(pos2(x0, top), pos2(x0 + bar_w, bottom));
        g.rect_filled(bar, 0.5, Color32::from_gray(0x08));
        let f = db_to_frac(lvl);
        if f > 0.0 {
            let bands = [(-120.0f32, -6.0f32), (-6.0, 0.0), (0.0, 12.0)];
            for (lo, hi) in bands {
                let (fa, fb) = (db_to_frac(lo), db_to_frac(hi).min(f));
                if fb > fa {
                    let seg =
                        Rect::from_min_max(pos2(bar.left(), y_of(fb)), pos2(bar.right(), y_of(fa)));
                    let col = if inactive {
                        METER_OFF
                    } else {
                        meter_colour((lo + hi) * 0.5)
                    };
                    g.rect_filled(seg, 0.0, col);
                    // Lit left edge, like the LED column in Logic.
                    g.rect_filled(
                        Rect::from_min_max(seg.min, pos2(seg.left() + bar_w * 0.4, seg.bottom())),
                        0.0,
                        col.lerp_to_gamma(Color32::WHITE, 0.28),
                    );
                }
            }
            // Thin dark gaps every few pixels make the bar look segmented.
            let mut y = bottom - 2.0;
            while y > y_of(f) {
                g.line_segment(
                    [pos2(bar.left(), y), pos2(bar.right(), y)],
                    Stroke::new(0.8, Color32::from_black_alpha(110)),
                );
                y -= 2.5;
            }
        }
        if hd > -60.0 {
            let y = y_of(db_to_frac(hd));
            g.line_segment(
                [pos2(bar.left(), y), pos2(bar.right(), y)],
                Stroke::new(
                    1.5,
                    if inactive {
                        METER_OFF
                    } else {
                        meter_colour(hd)
                    }
                    .lerp_to_gamma(Color32::WHITE, 0.4),
                ),
            );
        }
    }
    if levels.0 > -60.0 || levels.1 > -60.0 || hold.0 > -60.0 || hold.1 > -60.0 {
        ui.ctx().request_repaint();
    }

    // Fader cap: light grey block with a dark centre groove and fine ridges.
    let cap_y = y_of(db_to_frac(db));
    let cap = Rect::from_center_size(pos2(fader_x, cap_y), vec2(16.0, 27.0));
    let mut out = Moved::default();
    if editable {
        let hit = Rect::from_min_max(
            pos2(fader_x - 14.0, top - 17.0),
            pos2(fader_x + 14.0, bottom + 17.0),
        );
        let resp = ui.interact(hit, id, Sense::click_and_drag());
        if resp.dragged() {
            let f = db_to_frac(db) - resp.drag_delta().y / h;
            out.value = Some(frac_to_db(f).clamp(-60.0, 6.0));
        }
        if resp.double_clicked() {
            out.value = Some(0.0);
            out.started = true;
            out.stopped = true;
        }
        out.started |= resp.drag_started();
        out.stopped |= resp.drag_stopped();
    }
    // Logic's cap: a narrow metal block. Soft shadow, silver body that is a little darker at the
    // edges, a dark groove across the middle with a bright line under it, and engraved ridges
    // (a dark line with a light line under it) above and below.
    g.rect_filled(
        cap.translate(vec2(0.5, 1.5)),
        2.0,
        Color32::from_black_alpha(110),
    );
    g.rect_filled(cap, 2.0, Color32::from_gray(0x9c));
    g.rect_filled(
        Rect::from_min_max(
            pos2(cap.left() + 1.5, cap.top() + 1.0),
            pos2(cap.right() - 1.5, cap.center().y),
        ),
        egui::CornerRadius {
            nw: 1,
            ne: 1,
            sw: 0,
            se: 0,
        },
        Color32::from_gray(0xd4),
    );
    g.rect_filled(
        Rect::from_min_max(
            pos2(cap.left() + 1.5, cap.center().y),
            pos2(cap.right() - 1.5, cap.bottom() - 1.0),
        ),
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 1,
            se: 1,
        },
        Color32::from_gray(0xb2),
    );
    g.rect_stroke(
        cap,
        2.0,
        Stroke::new(1.0, Color32::from_gray(0x44)),
        StrokeKind::Inside,
    );
    for dy in [-9.0f32, -6.0, -3.0, 4.5, 7.5, 10.5] {
        g.line_segment(
            [
                pos2(cap.left() + 2.5, cap_y + dy),
                pos2(cap.right() - 2.5, cap_y + dy),
            ],
            Stroke::new(0.8, Color32::from_gray(0x80)),
        );
        g.line_segment(
            [
                pos2(cap.left() + 2.5, cap_y + dy + 1.0),
                pos2(cap.right() - 2.5, cap_y + dy + 1.0),
            ],
            Stroke::new(0.6, Color32::from_gray(0xe4)),
        );
    }
    g.line_segment(
        [
            pos2(cap.left() + 1.0, cap_y),
            pos2(cap.right() - 1.0, cap_y),
        ],
        Stroke::new(1.8, Color32::from_gray(0x24)),
    );
    g.line_segment(
        [
            pos2(cap.left() + 1.0, cap_y + 1.4),
            pos2(cap.right() - 1.0, cap_y + 1.4),
        ],
        Stroke::new(0.8, Color32::from_gray(0xee)),
    );
    out
}

/// Vertical positions of the strip rows (shared by every strip so rows line up).
pub struct Rows {
    setting: f32,
    gain_red: f32,
    eq: f32,
    input: f32,
    fx: f32,
    fx_rows: usize,
    sends: f32,
    send_rows: usize,
    output: f32,
    group: f32,
    automation: f32,
    pan: f32,
    db: f32,
    fader_top: f32,
}

pub fn rows(top: f32, fx_rows: usize, send_rows: usize) -> Rows {
    let fx_rows = fx_rows.max(2);
    let send_rows = send_rows.clamp(1, MAX_SENDS);
    let setting = top + 6.0;
    let gain_red = setting + 20.0;
    let eq = gain_red + 12.0;
    let input = eq + 28.0;
    let fx = input + 22.0;
    let sends = fx + fx_rows as f32 * 17.0 + 4.0;
    let output = sends + send_rows as f32 * 17.0 + 3.0;
    let group = output + 20.0;
    let automation = group + 20.0;
    let pan = automation + 22.0;
    let db = pan + 30.0;
    Rows {
        setting,
        gain_red,
        eq,
        input,
        fx,
        fx_rows,
        sends,
        send_rows,
        output,
        group,
        automation,
        pan,
        db,
        fader_top: db + 18.0,
    }
}

/// Audio FX rows needed so every strip in `keys` fits (inserts + one empty slot).
pub fn fx_rows_for(p: &Project, keys: &[StripKey]) -> usize {
    keys.iter()
        .map(|k| match k {
            StripKey::Track(i) => p.tracks.get(*i).map_or(0, |t| t.inserts.len()),
            StripKey::StereoOut => 2,
            StripKey::Master => 0,
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// Send slots needed so every strip in `keys` fits (its sends + one empty slot).
pub fn send_rows_for(p: &Project, keys: &[StripKey]) -> usize {
    keys.iter()
        .map(|k| match k {
            StripKey::Track(i) => p.tracks.get(*i).map_or(0, |t| t.sends.len()),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// The strip's send slots, one per row: the sends it has, then one empty slot to add another.
/// `x`, `w` and `top` place the rows (see [`Rows`]).
#[allow(clippy::too_many_arguments)]
fn send_slots(
    ui: &Ui,
    g: &Painter,
    area: Rect,
    top: f32,
    rows: usize,
    sends: &mut Vec<SendSlot>,
    uid: egui::Id,
    inactive: bool,
) {
    let mut remove = None;
    for k in 0..rows {
        let r = Rect::from_min_size(
            pos2(area.left(), top + k as f32 * 17.0),
            vec2(area.width(), 16.0),
        );
        if k < sends.len() {
            if !send_slot(ui, g, r, &mut sends[k], uid.with(k), inactive) {
                remove = Some(k);
            }
        } else if k == sends.len() && sends.len() < MAX_SENDS {
            let resp = ui.interact(r, uid.with(("empty", k)), Sense::click());
            slot(g, r, "", SLOT_EMPTY, TEXT);
            let mut picked = None;
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(110.0);
                for b in 1..=8u8 {
                    if ui.button(trf("Bus {}", &[&b])).clicked() {
                        picked = Some(b);
                        ui.close();
                    }
                }
            });
            if let Some(bus) = picked {
                sends.push(SendSlot {
                    bus: Some(bus),
                    level: 0.5,
                    pre: false,
                });
            }
        }
    }
    if let Some(k) = remove {
        sends.remove(k);
    }
}

/// Logic's send slot: a blue "Bus N" button with a round level knob beside it. Click the slot
/// to pick a bus; the eye marks a pre-fader send. The engine copies the strip's signal to the
/// bus, where the Aux strip that listens to it plays it. Returns false when the user chose
/// "No Send".
fn send_slot(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    send: &mut crate::model::SendSlot,
    uid: egui::Id,
    inactive: bool,
) -> bool {
    let Some(bus) = send.bus else {
        return false;
    };
    let mut keep = true;
    let knob_c = pos2(r.right() - 8.0, r.center().y);
    let slot_r = Rect::from_min_max(r.min, pos2(r.right() - 19.0, r.bottom()));
    let resp = ui.interact(slot_r, uid, Sense::click());
    slot(
        g,
        slot_r,
        &trf("Bus {}", &[&bus]),
        if inactive { PLUGIN_OFF } else { PLUGIN_BLUE },
        if inactive { TEXT_DIM } else { Color32::WHITE },
    );
    if send.pre {
        // The eye icon Logic shows on pre-fader sends.
        let c = pos2(slot_r.left() + 9.0, slot_r.center().y);
        g.circle_stroke(c, 4.5, Stroke::new(1.0, Color32::WHITE));
        g.circle_filled(c, 1.6, Color32::WHITE);
    }
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(130.0);
        for b in 1..=8u8 {
            if ui
                .selectable_label(send.bus == Some(b), trf("Bus {}", &[&b]))
                .clicked()
            {
                send.bus = Some(b);
                ui.close();
            }
        }
        ui.separator();
        if ui.selectable_label(send.pre, tr("Pre Fader")).clicked() {
            send.pre = !send.pre;
            ui.close();
        }
        if ui.button(tr("No Send")).clicked() {
            keep = false;
            ui.close();
        }
    });
    // Level knob: drag vertically; the indicator sweeps like a pan knob.
    let kresp = ui.interact(
        Rect::from_center_size(knob_c, vec2(16.0, 16.0)),
        uid.with("level"),
        Sense::click_and_drag(),
    );
    if kresp.dragged() {
        send.level = (send.level - kresp.drag_delta().y * 0.01).clamp(0.0, 1.0);
    }
    if kresp.double_clicked() {
        send.level = 0.0;
    }
    g.circle_filled(knob_c, 6.5, Color32::from_gray(0x5e));
    g.circle_stroke(knob_c, 6.5, Stroke::new(1.0, Color32::from_gray(0x26)));
    let a = -std::f32::consts::FRAC_PI_2 - 2.3 + send.level * 4.6;
    g.line_segment(
        [
            knob_c + vec2(a.cos(), a.sin()) * 2.0,
            knob_c + vec2(a.cos(), a.sin()) * 5.5,
        ],
        Stroke::new(1.4, Color32::WHITE),
    );
    keep
}

/// The effects an Audio FX slot can hold, in the order of its menu.
const FX_MENU: [prowave_engine::FxKind; 11] = prowave_engine::FxKind::ALL;

/// What the user chose in an Audio FX slot's menu.
enum InsertAct {
    Add(prowave_engine::FxKind),
    Replace(prowave_engine::FxKind),
    Remove,
    Bypass,
    Open,
    Move(isize),
}

/// One Audio FX slot of track `i`: the effect in it (blue, grey when bypassed or silenced),
/// or the empty slot after the last. Click for the menu, double-click for the effect's window.
#[allow(clippy::too_many_arguments)]
fn insert_slot(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    p: &mut Project,
    v: &mut MixerView,
    i: usize,
    k: usize,
    uid: egui::Id,
    inactive: bool,
) {
    let have = p.tracks[i].inserts.get(k).copied();
    let count = p.tracks[i].inserts.len();
    match have {
        Some(fx) => {
            let off = inactive || fx.bypass;
            slot(
                g,
                r,
                fx.name(),
                if off { PLUGIN_OFF } else { PLUGIN_BLUE },
                if off { TEXT_DIM } else { Color32::WHITE },
            );
        }
        None => slot(g, r, "", SLOT_EMPTY, TEXT),
    }
    if k > count || (have.is_none() && count >= prowave_engine::MAX_INSERTS) {
        return;
    }
    let resp = ui.interact(r, uid, Sense::click());
    if have.is_some() && resp.double_clicked() {
        v.fx_window = Some((i, k));
    }
    let mut act = None;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(150.0);
        if let Some(fx) = have {
            if ui.button(tr("Open")).clicked() {
                act = Some(InsertAct::Open);
                ui.close();
            }
            if ui.selectable_label(fx.bypass, tr("Bypass")).clicked() {
                act = Some(InsertAct::Bypass);
                ui.close();
            }
            ui.add_enabled_ui(k > 0, |ui| {
                if ui.button(tr("Move Up")).clicked() {
                    act = Some(InsertAct::Move(-1));
                    ui.close();
                }
            });
            ui.add_enabled_ui(k + 1 < count, |ui| {
                if ui.button(tr("Move Down")).clicked() {
                    act = Some(InsertAct::Move(1));
                    ui.close();
                }
            });
            ui.separator();
            ui.menu_button(tr("Replace With"), |ui| {
                for kind in FX_MENU {
                    if ui.selectable_label(kind == fx.kind, kind.name()).clicked() {
                        act = Some(InsertAct::Replace(kind));
                        ui.close();
                    }
                }
            });
            if ui.button(tr("Remove")).clicked() {
                act = Some(InsertAct::Remove);
                ui.close();
            }
        } else {
            for kind in FX_MENU {
                if ui.button(kind.name()).clicked() {
                    act = Some(InsertAct::Add(kind));
                    ui.close();
                }
            }
        }
    });
    if let Some(act) = act {
        apply_insert(p, v, i, k, act);
    }
}

/// Carries out what the user chose in the menu of Audio FX slot `k` of track `i`. Choices that
/// do not fit (a slot that is not there) do nothing.
fn apply_insert(p: &mut Project, v: &mut MixerView, i: usize, k: usize, act: InsertAct) {
    let Some(t) = p.tracks.get_mut(i) else {
        return;
    };
    let inserts = &mut t.inserts;
    let filled = k < inserts.len();
    match act {
        InsertAct::Add(kind) => {
            if inserts.len() < prowave_engine::MAX_INSERTS {
                inserts.push(Insert::new(kind));
                v.fx_window = Some((i, inserts.len() - 1));
            }
        }
        InsertAct::Replace(kind) if filled => {
            inserts[k] = Insert::new(kind);
            t.auto.insert_replaced(k);
        }
        InsertAct::Remove if filled => {
            inserts.remove(k);
            t.auto.insert_removed(k);
            v.fx_window = match v.fx_window {
                Some((t, s)) if t == i && s == k => None,
                Some((t, s)) if t == i && s > k => Some((t, s - 1)),
                other => other,
            };
        }
        InsertAct::Bypass if filled => inserts[k].bypass = !inserts[k].bypass,
        InsertAct::Open if filled => v.fx_window = Some((i, k)),
        InsertAct::Move(d) if filled => {
            let to = k as isize + d;
            if to < 0 || to as usize >= inserts.len() {
                return;
            }
            let to = to as usize;
            inserts.swap(k, to);
            t.auto.insert_swapped(k, to);
            if v.fx_window == Some((i, k)) {
                v.fx_window = Some((i, to));
            } else if v.fx_window == Some((i, to)) {
                v.fx_window = Some((i, k));
            }
        }
        _ => {}
    }
}

/// The colour of an automation mode's label in the strip.
fn mode_colour(mode: AutoMode, held: bool) -> Color32 {
    match mode {
        AutoMode::Off => TEXT_DIM,
        AutoMode::Read => READ_GREEN,
        AutoMode::Touch | AutoMode::Latch if held => Color32::from_rgb(0xff, 0xa8, 0x3c),
        AutoMode::Touch | AutoMode::Latch => Color32::from_rgb(0xe6, 0x9a, 0x2e),
        AutoMode::Write => Color32::from_rgb(0xff, 0x5a, 0x4a),
    }
}

/// The strip's Automation slot: the mode, in its colour. Click to pick the mode or open the
/// Automation editor.
fn automation_slot(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    p: &mut Project,
    v: &mut MixerView,
    i: usize,
    uid: egui::Id,
) {
    let (mode, held) = (p.tracks[i].auto.mode, p.tracks[i].auto.held.is_some());
    slot(g, r, tr(mode.label()), SLOT_EMPTY, mode_colour(mode, held));
    let has_lane = !p.tracks[i].auto.volume.is_empty() || !p.tracks[i].auto.pan.is_empty();
    if has_lane {
        // A small dot: this strip has automation.
        g.circle_filled(
            pos2(r.left() + 7.0, r.center().y),
            2.0,
            mode_colour(mode, held),
        );
    }
    let resp = ui.interact(r, uid, Sense::click());
    let mut picked = None;
    let mut open_editor = false;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(150.0);
        for m in AutoMode::ALL {
            if ui.selectable_label(m == mode, tr(m.label())).clicked() {
                picked = Some(m);
                ui.close();
            }
        }
        ui.separator();
        if ui.button(tr("Show Automation")).clicked() {
            open_editor = true;
            ui.close();
        }
    });
    if let Some(m) = picked {
        let beat = p.position_beats;
        crate::automation::set_mode(&mut p.tracks[i], m, beat);
    }
    if open_editor {
        v.auto_editor.track = Some(i);
    }
}

/// How the strip's name is shown at the bottom.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NameStyle {
    /// Coloured label (Mixer).
    Coloured,
    /// Plain text (Inspector).
    Plain,
}

/// Draws one channel strip in `strip` and applies its edits to the project.
/// `ns` keeps widget ids unique when the same strip is shown in two places.
#[allow(clippy::too_many_arguments)]
pub fn channel_strip(
    ui: &Ui,
    g: &Painter,
    strip: Rect,
    key: StripKey,
    p: &mut Project,
    v: &mut MixerView,
    meters: &Meters,
    r: &Rows,
    ns: &'static str,
    name_style: NameStyle,
) {
    let selected = matches!(key, StripKey::Track(i) if i == p.selected_track);
    let inactive = matches!(key, StripKey::Track(i)
        if p.is_muted(i) || crate::tracks_area::silenced_by_solo(p, i));
    let (plug, plug_text) = if inactive {
        (PLUGIN_OFF, TEXT_DIM)
    } else {
        (PLUGIN_BLUE, Color32::WHITE)
    };
    // Whole-strip click selects (registered first so the controls on top win).
    if let StripKey::Track(i) = key {
        if ui
            .interact(strip, id(ns, (key, "strip")), Sense::click())
            .clicked()
        {
            p.selected_track = i;
        }
    }
    let bg = if selected && name_style == NameStyle::Coloured {
        STRIP_SEL
    } else {
        STRIP_BG
    };
    g.rect_filled(strip, 2.0, bg);
    let inner_w = strip.width() - 8.0;
    let sx = strip.left() + 4.0;
    let row = |y: f32, h: f32| Rect::from_min_size(pos2(sx, y), vec2(inner_w, h));

    let (name, color, inserts, kind, instrument) = match key {
        StripKey::Track(i) => {
            let t = &p.tracks[i];
            (
                t.name.clone(),
                t.color,
                t.inserts
                    .iter()
                    .map(|x| (x.name(), x.bypass))
                    .collect::<Vec<_>>(),
                Some(t.kind),
                t.instrument,
            )
        }
        StripKey::StereoOut => (
            tr("Stereo Out").to_string(),
            OUT_PURPLE,
            vec![("Compressor", false), ("MultiMeter", false)],
            None,
            None,
        ),
        StripKey::Master => (tr("Master").to_string(), MASTER_PINK, vec![], None, None),
    };
    let beat = p.position_beats;
    let eq_params = match key {
        StripKey::Track(i) => p.tracks[i]
            .inserts
            .iter()
            .find(|x| {
                matches!(
                    x.kind,
                    prowave_engine::FxKind::ChannelEq | prowave_engine::FxKind::MultiEq
                ) && !x.bypass
            })
            .map(|x| (x.kind, x.params)),
        _ => None,
    };
    let reduction = match key {
        StripKey::Track(i) => meters.reduction.get(i).copied().unwrap_or(0.0),
        _ => 0.0,
    };

    // A VCA strip is a fader with Mute and Solo: none of the channel strip's slots.
    let vca_strip = kind == Some(TrackKind::Vca);
    if vca_strip {
        g.text(
            pos2(strip.center().x, r.setting + 8.0),
            Align2::CENTER_CENTER,
            tr("VCA"),
            FontId::proportional(12.0),
            TEXT_DIM,
        );
    }
    if !vca_strip {
        slot(g, row(r.setting, 16.0), tr("Setting"), SLOT_GRAY, TEXT);
        let gr_bar = row(r.gain_red + 1.0, 6.0);
        g.rect_filled(gr_bar, 1.0, SLOT_EMPTY);
        if reduction > 0.05 {
            // Gain reduction grows from the right, in orange.
            let w = gr_bar.width() * (reduction / 24.0).min(1.0);
            g.rect_filled(
                Rect::from_min_max(pos2(gr_bar.right() - w, gr_bar.top()), gr_bar.max),
                1.0,
                if inactive {
                    METER_OFF
                } else {
                    Color32::from_rgb(0xe6, 0xa0, 0x2a)
                },
            );
        }
        eq_thumb(g, row(r.eq, 24.0), eq_params);
        let input_r = row(r.input, 16.0);
        match (kind, instrument) {
            (Some(TrackKind::Instrument | TrackKind::Drummer), Some(inst)) => {
                slot(g, input_r, inst, plug, plug_text)
            }
            // An aux strip's input is the bus it listens to: Aux 1 takes Bus 1, and so on.
            (Some(TrackKind::Aux), _) => {
                let n = match key {
                    StripKey::Track(i) => p.aux_bus_of(i),
                    _ => 1,
                };
                let text = if n == 0 {
                    String::new()
                } else {
                    trf("Bus {}", &[&n])
                };
                slot(g, input_r, &text, plug, plug_text);
            }
            // An external MIDI strip shows where its notes go: the port and the channel.
            (Some(TrackKind::ExternalMidi), _) => {
                let text = match key {
                    StripKey::Track(i) => {
                        let t = &p.tracks[i];
                        let port = t
                            .midi_port
                            .clone()
                            .unwrap_or_else(|| tr("No Port").to_string());
                        format!("{port} · {}", t.midi_channel)
                    }
                    _ => String::new(),
                };
                let fitted = elide(g, &text, &FontId::proportional(10.5), input_r.width() - 8.0);
                slot(g, input_r, &fitted, plug, plug_text);
            }
            (Some(TrackKind::Audio), _) => {
                slot(g, input_r, &trf("Input {}", &[&1]), SLOT_GRAY, TEXT);
                g.circle_stroke(
                    pos2(input_r.left() + 8.0, input_r.center().y),
                    3.5,
                    stroke(TEXT),
                );
            }
            _ => slot(g, input_r, "", SLOT_EMPTY, TEXT),
        }
        for k in 0..r.fx_rows {
            let rr = row(r.fx + k as f32 * 17.0, 15.0);
            match key {
                StripKey::Track(i) => {
                    insert_slot(ui, g, rr, p, v, i, k, id(ns, (key, "fx", k)), inactive)
                }
                _ => match inserts.get(k) {
                    Some((fx, _)) => slot(g, rr, fx, plug, plug_text),
                    None if k == inserts.len() => slot(g, rr, "", SLOT_EMPTY, TEXT),
                    None => {}
                },
            }
        }
        match key {
            // An Aux strip takes its signal from a bus: it has no sends of its own.
            StripKey::Track(i) if p.tracks[i].kind != TrackKind::Aux => {
                let area = row(r.sends, 16.0);
                send_slots(
                    ui,
                    g,
                    area,
                    r.sends,
                    r.send_rows,
                    &mut p.tracks[i].sends,
                    id(ns, (key, "send")),
                    inactive,
                );
            }
            StripKey::StereoOut => slot(g, row(r.sends, 16.0), "", SLOT_EMPTY, TEXT),
            _ => {}
        }
        if let StripKey::Track(_) = key {
            let out = if name_style == NameStyle::Plain {
                tr("Stereo Out")
            } else {
                tr("St Out")
            };
            slot(g, row(r.output, 16.0), out, SLOT_GRAY, TEXT);
            if let StripKey::Track(i) = key {
                group_slot(ui, g, row(r.group, 16.0), p, i, id(ns, (key, "vca")));
            }
        }
        if matches!(key, StripKey::StereoOut | StripKey::Master) {
            let rr = row(r.automation, 16.0);
            slot(
                g,
                rr,
                tr("Read"),
                SLOT_EMPTY,
                Color32::from_rgb(0xa8, 0x9c, 0xf2),
            );
            g.rect_stroke(
                rr,
                3.0,
                Stroke::new(1.0, Color32::from_rgb(0x6a, 0x5a, 0xc0)),
                StrokeKind::Inside,
            );
        } else if let StripKey::Track(i) = key {
            automation_slot(
                ui,
                g,
                row(r.automation, 16.0),
                p,
                v,
                i,
                id(ns, (key, "auto")),
            );
        }
    }
    let pan_c = pos2(strip.center().x, r.pan + 12.0);
    match key {
        _ if vca_strip => {}
        StripKey::Track(i) => {
            let t = &p.tracks[i];
            let shown = crate::automation::shown_value(t, AutoTarget::Pan, beat);
            let editable = crate::automation::editable(t, AutoTarget::Pan);
            let m = pan_knob(ui, g, pan_c, id(ns, (key, "pan")), shown, editable);
            if let Some(value) = m.value {
                let delta = value - shown;
                crate::automation::moved(&mut p.tracks[i], AutoTarget::Pan, value);
                for j in p.group_mates(i, |g| g.pan) {
                    let to = p.tracks[j].pan + delta;
                    crate::automation::moved(&mut p.tracks[j], AutoTarget::Pan, to);
                }
            }
            if m.stopped {
                let playing = p.playing;
                for j in std::iter::once(i).chain(p.group_mates(i, |g| g.pan)) {
                    crate::automation::released(&mut p.tracks[j], AutoTarget::Pan, beat, playing);
                }
            }
        }
        _ => {
            pan_knob(ui, g, pan_c, id(ns, (key, "pan")), 0.0, false);
        }
    }

    // Volume (dB) and peak-hold readouts.
    // Sends on Faders: the fader sets the strip's send to that bus instead of its level.
    let send_bus = match key {
        StripKey::Track(i) if !matches!(p.tracks[i].kind, TrackKind::Aux | TrackKind::Vca) => {
            v.sends_on_faders
        }
        _ => None,
    };
    let send_db = |sends: &[SendSlot], bus: u8| {
        let level = sends
            .iter()
            .find(|s| s.bus == Some(bus))
            .map_or(0.0, |s| s.level);
        lin_to_db(prowave_engine::send_gain(level)).max(-60.0)
    };
    let vol_db = match key {
        StripKey::Track(i) => match send_bus {
            Some(bus) => send_db(&p.tracks[i].sends, bus),
            None => crate::automation::shown_value(&p.tracks[i], AutoTarget::Volume, beat),
        },
        StripKey::StereoOut => lin_to_db(p.master_vol).max(-60.0),
        StripKey::Master => 0.0,
    };
    let peaks = match key {
        StripKey::Track(i) => meters.tracks.get(i).copied().unwrap_or((0.0, 0.0)),
        _ => meters.master,
    };
    let hold = v.peak_hold.entry(key).or_insert(0.0);
    *hold = hold.max(peaks.0.max(peaks.1));
    let half = (inner_w - 4.0) / 2.0;
    let vol_r = Rect::from_min_size(pos2(sx, r.db), vec2(half, 14.0));
    let pk_r = Rect::from_min_size(pos2(sx + half + 4.0, r.db), vec2(half, 14.0));
    slot(
        g,
        vol_r,
        &format!("{vol_db:.1}"),
        Color32::from_gray(0x24),
        TEXT,
    );
    let hold_db = lin_to_db(*hold);
    let pk_text = if *hold <= 1e-5 {
        String::new()
    } else {
        format!("{hold_db:.1}")
    };
    let pk_col = if hold_db > 0.0 {
        Color32::from_rgb(0xff, 0x5a, 0x4a)
    } else {
        READ_GREEN
    };
    slot(g, pk_r, &pk_text, Color32::from_gray(0x24), pk_col);
    if ui
        .interact(pk_r, id(ns, (key, "peak")), Sense::click())
        .clicked()
    {
        *hold = 0.0;
    }

    // Fader + scale + meter.
    let name_h = 22.0;
    let fader_area = Rect::from_min_max(
        pos2(sx, r.fader_top),
        pos2(strip.right() - 4.0, strip.bottom() - 26.0 - name_h),
    );
    let fader_free = match key {
        StripKey::Track(i) => {
            send_bus.is_some() || crate::automation::editable(&p.tracks[i], AutoTarget::Volume)
        }
        StripKey::StereoOut => true,
        StripKey::Master => false,
    };
    let moved = fader_and_meter(
        ui,
        g,
        fader_area,
        id(ns, (key, "fader")),
        vol_db,
        fader_free,
        peaks,
        inactive,
    );
    if let Some(db) = moved.value {
        match key {
            StripKey::Track(i) => match send_bus {
                Some(bus) => set_send_level(&mut p.tracks[i].sends, bus, db),
                None => {
                    let delta = db - vol_db;
                    crate::automation::moved(&mut p.tracks[i], AutoTarget::Volume, db);
                    for j in p.group_mates(i, |g| g.volume) {
                        let to = p.tracks[j].volume_db + delta;
                        crate::automation::moved(&mut p.tracks[j], AutoTarget::Volume, to);
                    }
                }
            },
            StripKey::StereoOut => p.master_vol = 10f32.powf(db / 20.0).min(2.0),
            StripKey::Master => {}
        }
    }
    if let (true, StripKey::Track(i), None) = (moved.stopped, key, send_bus) {
        let playing = p.playing;
        for j in std::iter::once(i).chain(p.group_mates(i, |g| g.volume)) {
            crate::automation::released(&mut p.tracks[j], AutoTarget::Volume, beat, playing);
        }
    }

    // M / S
    let by = strip.bottom() - 22.0 - name_h;
    let bw = (inner_w - 6.0) / 2.0;
    let m_r = Rect::from_min_size(pos2(sx, by), vec2(bw, 16.0));
    let s_r = Rect::from_min_size(pos2(sx + bw + 6.0, by), vec2(bw, 16.0));
    match key {
        StripKey::Track(i) => {
            let t = &mut p.tracks[i];
            if small_button(ui, g, m_r, id(ns, (key, "m")), "M", t.mute, MUTE_ON) {
                t.mute = !t.mute;
            }
            if small_button(ui, g, s_r, id(ns, (key, "s")), "S", t.solo, SOLO_ON) {
                t.solo = !t.solo;
            }
        }
        _ => {
            slot(g, m_r, "M", SLOT_GRAY, TEXT_DIM);
            if key == StripKey::StereoOut {
                slot(g, s_r, tr("Bnc"), SLOT_GRAY, TEXT_DIM);
            }
        }
    }

    // Name.
    let name_r = Rect::from_min_max(pos2(strip.left(), strip.bottom() - name_h), strip.max);
    let font = FontId::proportional(10.5);
    let shown = elide(g, &name, &font, name_r.width() - 6.0);
    match name_style {
        NameStyle::Coloured => {
            g.rect_filled(name_r, 2.0, color);
            g.text(
                name_r.center(),
                Align2::CENTER_CENTER,
                shown,
                font,
                Color32::WHITE,
            );
        }
        NameStyle::Plain => {
            g.text(name_r.center(), Align2::CENTER_CENTER, shown, font, TEXT);
        }
    }
    if key == StripKey::StereoOut && name_style == NameStyle::Coloured {
        g.rect_stroke(
            strip,
            2.0,
            Stroke::new(1.5, Color32::from_rgb(0xb8, 0x3c, 0xc8)),
            StrokeKind::Inside,
        );
    }
    if selected && name_style == NameStyle::Coloured {
        g.rect_stroke(
            strip,
            2.0,
            Stroke::new(1.0, Color32::from_gray(0x8a)),
            StrokeKind::Inside,
        );
    }
}

/// Sets the send to `bus` from a fader position in dB (a new send is made when needed, and
/// one turned right down is taken away).
fn set_send_level(sends: &mut Vec<SendSlot>, bus: u8, db: f32) {
    let level = if db <= -59.5 {
        0.0
    } else {
        (10f32.powf(db / 20.0) / 2.0).sqrt().clamp(0.0, 1.0)
    };
    match sends.iter().position(|s| s.bus == Some(bus)) {
        Some(k) => sends[k].level = level,
        None if level > 0.0 && sends.len() < MAX_SENDS => sends.push(SendSlot {
            bus: Some(bus),
            level,
            pre: false,
        }),
        None => {}
    }
}

/// The strip's Group slot: the mix group of the track, else the VCA track that moves its level
/// (empty for none). Click to pick a mix group, make a new one, change what the group moves
/// together, or pick one of the project's VCA tracks.
fn group_slot(ui: &Ui, g: &Painter, r: Rect, p: &mut Project, i: usize, id: egui::Id) {
    let group = p.tracks[i].group.filter(|g| *g < p.groups.len());
    let name = match group {
        Some(g) => p.groups[g].name.clone(),
        None => p.vca_of(i).map(|v| v.name.clone()).unwrap_or_default(),
    };
    let fill = if name.is_empty() {
        SLOT_EMPTY
    } else {
        SLOT_GRAY
    };
    slot(
        g,
        r,
        &elide(g, &name, &FontId::proportional(10.5), r.width() - 8.0),
        fill,
        TEXT,
    );
    let vcas: Vec<(usize, String)> = p
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TrackKind::Vca)
        .map(|(j, t)| (j, t.name.clone()))
        .collect();
    let resp = ui.interact(r, id, Sense::click());
    let current = p.tracks[i].vca;
    let mut picked = None;
    let mut group_pick: Option<Option<usize>> = None;
    let mut new_group = false;
    let mut delete = None;
    let names: Vec<String> = p.groups.iter().map(|g| g.name.clone()).collect();
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(150.0);
        if ui
            .selectable_label(group.is_none(), tr("No Group"))
            .clicked()
        {
            group_pick = Some(None);
            ui.close();
        }
        for (k, name) in names.iter().enumerate() {
            if ui.selectable_label(group == Some(k), name).clicked() {
                group_pick = Some(Some(k));
                ui.close();
            }
        }
        if ui.button(tr("New Group")).clicked() {
            new_group = true;
            ui.close();
        }
        if let Some(k) = group {
            ui.menu_button(tr("Group Settings"), |ui| {
                let grp = &mut p.groups[k];
                ui.checkbox(&mut grp.volume, tr("Volume"));
                ui.checkbox(&mut grp.pan, tr("Pan"));
                ui.checkbox(&mut grp.mute, tr("Mute"));
                ui.checkbox(&mut grp.solo, tr("Solo"));
            });
            if ui.button(tr("Delete Group")).clicked() {
                delete = Some(k);
                ui.close();
            }
        }
        if !vcas.is_empty() {
            ui.separator();
            if ui
                .selectable_label(current.is_none(), tr("No VCA"))
                .clicked()
            {
                picked = Some(None);
                ui.close();
            }
            for (j, name) in &vcas {
                if ui.selectable_label(current == Some(*j), name).clicked() {
                    picked = Some(Some(*j));
                    ui.close();
                }
            }
        }
    });
    if let Some(vca) = picked {
        p.assign_vca(i, vca);
    }
    if let Some(pick) = group_pick {
        p.assign_group(i, pick);
    }
    if new_group {
        let n = p.groups.len() + 1;
        if let Some(k) = p.add_group(&format!("Group {n}")) {
            p.assign_group(i, Some(k));
        }
    }
    if let Some(k) = delete {
        p.delete_group(k);
    }
}

fn toolbar(ui: &Ui, p: &mut Project, s: &mut UiState, bar: Rect) {
    const NS: &str = "mixer_toolbar";
    let g = ui.painter_at(bar);
    g.rect_filled(bar, 0.0, TOOLBAR_BG);
    g.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x24)),
    );
    let cy = bar.center().y;
    back_chevron(&g, pos2(bar.left() + 16.0, cy));
    let (can_undo, can_redo) = (s.can_undo, s.can_redo);
    let mut undo_pick = None;
    let v = &mut s.mixer;

    let mut x = bar.left() + 32.0;
    let (edit, w) = menu_title(ui, &g, x, cy, tr("Edit"), id(NS, "Edit"));
    x += w + 2.0;
    egui::Popup::menu(&edit).show(|ui| {
        ui.set_min_width(220.0);
        crate::widgets::undo_items(ui, can_undo, can_redo, &mut undo_pick);
        if ui.button(tr("Copy Channel Strip")).clicked() {
            if let Some(t) = p.tracks.get(p.selected_track) {
                v.copied = Some(CopiedStrip {
                    inserts: t.inserts.clone(),
                    sends: t.sends.clone(),
                    volume_db: t.volume_db,
                    pan: t.pan,
                });
            }
            ui.close();
        }
        let can_paste = v.copied.is_some() && !p.tracks.is_empty();
        if ui
            .add_enabled(can_paste, egui::Button::new(tr("Paste Channel Strip")))
            .clicked()
        {
            if let (Some(c), Some(t)) = (v.copied.clone(), p.tracks.get_mut(p.selected_track)) {
                t.inserts = c.inserts;
                t.sends = c.sends;
                t.volume_db = c.volume_db;
                t.pan = c.pan;
            }
            ui.close();
        }
        todo_item(ui, tr("Select Audio Channel Strips"));
    });
    if undo_pick.is_some() {
        s.undo_cmd = undo_pick;
    }
    let (options, w) = menu_title(ui, &g, x, cy, tr("Options"), id(NS, "Options"));
    x += w + 2.0;
    egui::Popup::menu(&options).show(|ui| {
        ui.set_min_width(220.0);
        if ui
            .button(tr("Create New Auxiliary Channel Strip"))
            .clicked()
        {
            p.add_track(TrackKind::Aux);
            ui.close();
        }
        todo_item(ui, tr("Create New Output Channel Strip"));
        if ui.button(tr("Create New VCA")).clicked() {
            p.add_track(TrackKind::Vca);
            ui.close();
        }
        if ui.button(tr("Create Group…")).clicked() {
            let n = p.groups.len() + 1;
            if let Some(k) = p.add_group(&format!("Group {n}")) {
                let sel = p.selected_track;
                p.assign_group(sel, Some(k));
            }
            ui.close();
        }
    });
    let (view, w) = menu_title(ui, &g, x, cy, tr("View"), id(NS, "View"));
    x += w + 12.0;
    egui::Popup::menu(&view).show(|ui| {
        ui.set_min_width(220.0);
        if ui
            .selectable_label(v.wide, tr("Wide Channel Strips"))
            .clicked()
        {
            v.wide = !v.wide;
            ui.close();
        }
        todo_item(ui, tr("Show Signal Flow Channel Strips"));
    });

    let menus_end = x;
    // Right-hand side first (strip width buttons, then filters), so the rest can be fitted
    // into what is left.
    let mut rx = bar.right() - 8.0;
    for (wide, key) in [(true, "wide"), (false, "narrow")] {
        let r = Rect::from_min_size(pos2(rx - 26.0, cy - 10.0), vec2(26.0, 20.0));
        let resp = ui.interact(r, id(NS, key), Sense::click());
        if v.wide == wide {
            g.rect_filled(r, 4.0, Color32::from_gray(0x5a));
        }
        let c = r.center();
        let iw = if wide { 13.0 } else { 9.0 };
        g.rect_stroke(
            Rect::from_center_size(c, vec2(iw, 11.0)),
            1.5,
            stroke(TEXT),
            StrokeKind::Middle,
        );
        g.line_segment([c + vec2(0.0, -5.5), c + vec2(0.0, 5.5)], stroke(TEXT));
        if resp.clicked() {
            v.wide = wide;
        }
        rx = r.left() - 4.0;
    }
    rx -= 10.0;
    let filter_widths: Vec<f32> = FILTERS
        .iter()
        .map(|label| {
            g.layout_no_wrap(tr(label).into(), FontId::proportional(12.5), TEXT)
                .size()
                .x
                + 16.0
        })
        .collect();
    let filters_w = filter_widths.iter().map(|w| w + 3.0).sum::<f32>();
    let fit = fit_toolbar(menus_end, bar.center().x, rx, filters_w);

    if fit.filters_inline {
        for (i, label) in FILTERS.iter().enumerate().rev() {
            let w = filter_widths[i];
            let r = Rect::from_min_size(pos2(rx - w, cy - 10.0), vec2(w, 20.0));
            let resp = ui.interact(r, id(NS, ("filter", i)), Sense::click());
            let on = v.filters[i];
            g.rect_filled(r, 4.0, if on { BLUE } else { Color32::from_gray(0x50) });
            g.text(
                r.center(),
                Align2::CENTER_CENTER,
                tr(label),
                FontId::proportional(12.5),
                if on { Color32::WHITE } else { TEXT_DIM },
            );
            if resp.clicked() {
                v.filters[i] = !v.filters[i];
            }
            rx = r.left() - 3.0;
        }
    } else {
        // Not enough room for a button per strip type: one menu with a check for each.
        let r = Rect::from_min_size(
            pos2(rx - FILTER_MENU_W, cy - 10.0),
            vec2(FILTER_MENU_W, 20.0),
        );
        let resp = popup_button(ui, &g, r, id(NS, "filters_menu"), tr("Show"));
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(160.0);
            for (i, label) in FILTERS.iter().enumerate() {
                if ui.checkbox(&mut v.filters[i], tr(label)).clicked() {
                    ui.close();
                }
            }
        });
    }

    if fit.show_sends {
        draw_sends_on_faders(ui, &g, menus_end, cy, &mut v.sends_on_faders);
    }
    let left = fit.modes_left;

    // Single | Tracks | All
    let modes = [
        (MixerMode::Single, tr("Single")),
        (MixerMode::Tracks, tr("Tracks")),
        (MixerMode::All, tr("All")),
    ];
    let seg_w = MODE_W / 3.0;
    for (i, (m, label)) in modes.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(left + i as f32 * seg_w, cy - 10.0),
            vec2(seg_w - 2.0, 20.0),
        );
        let resp = ui.interact(r, id(NS, ("mode", *label)), Sense::click());
        let on = v.mode == *m;
        if on {
            g.rect_filled(r, 4.0, BLUE);
        } else if resp.hovered() {
            g.rect_filled(r, 4.0, Color32::from_white_alpha(14));
        }
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(12.5),
            if on { Color32::WHITE } else { TEXT },
        );
        if resp.clicked() {
            v.mode = *m;
        }
    }
}

/// Width of the Single | Tracks | All switch, of the "Sends on Faders" group and of the
/// "Show" menu that replaces the filter buttons when they do not fit.
const MODE_W: f32 = 174.0;
const SENDS_W: f32 = 236.0;
const FILTER_MENU_W: f32 = 70.0;
const TOOLBAR_GAP: f32 = 10.0;

/// Where the Mixer toolbar's groups go so that none overlaps another: the mode switch at the
/// centre when there is room, else just left of the filters; the filters as buttons when they
/// fit, else as one menu; "Sends on Faders" only in the room left over.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ToolbarFit {
    modes_left: f32,
    filters_inline: bool,
    show_sends: bool,
}

fn fit_toolbar(menus_end: f32, center: f32, right_end: f32, filters_w: f32) -> ToolbarFit {
    let place = |filters_left: f32| {
        let centred = center - MODE_W / 2.0 + 60.0;
        centred.min(filters_left - TOOLBAR_GAP - MODE_W)
    };
    let mut filters_inline = true;
    let mut modes_left = place(right_end - filters_w);
    if modes_left < menus_end + TOOLBAR_GAP {
        filters_inline = false;
        modes_left = place(right_end - FILTER_MENU_W);
    }
    // When even that is too tight, the modes stay just after the menus (the panel has a
    // minimum width, so this only happens in very narrow windows).
    modes_left = modes_left.max(menus_end + TOOLBAR_GAP);
    let show_sends = menus_end + SENDS_W + TOOLBAR_GAP <= modes_left;
    ToolbarFit {
        modes_left,
        filters_inline,
        show_sends,
    }
}

fn draw_sends_on_faders(ui: &Ui, g: &egui::Painter, x: f32, cy: f32, bus: &mut Option<u8>) {
    const NS: &str = "mixer_toolbar";
    // Sends on Faders: the faders set the sends to a bus.
    g.text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        tr("Sends on Faders:"),
        FontId::proportional(12.5),
        TEXT_DIM,
    );
    let pw = pos2(x + 112.0, cy + 1.0);
    g.circle_stroke(pw, 5.5, stroke(TEXT_DIM));
    g.line_segment(
        [pw + vec2(0.0, -7.5), pw + vec2(0.0, -1.5)],
        Stroke::new(3.0, TOOLBAR_BG),
    );
    g.line_segment(
        [pw + vec2(0.0, -7.0), pw + vec2(0.0, -2.0)],
        stroke(TEXT_DIM),
    );
    let sof = Rect::from_min_size(pos2(x + 126.0, cy - 10.0), vec2(110.0, 20.0));
    let label = match bus {
        Some(b) => trf("Bus {}", &[b]),
        None => tr("Off").to_string(),
    };
    let resp = popup_button(ui, g, sof, id(NS, "sends_on_faders"), &label);
    egui::Popup::menu(&resp).show(|ui| {
        if ui.selectable_label(bus.is_none(), tr("Off")).clicked() {
            *bus = None;
            ui.close();
        }
        for b in 1..=8u8 {
            if ui
                .selectable_label(*bus == Some(b), trf("Bus {}", &[&b]))
                .clicked()
            {
                *bus = Some(b);
                ui.close();
            }
        }
    });
}

/// Whether a strip of `kind` is shown with the toolbar's filter buttons set as `filters`
/// (Audio, Inst, Aux, Bus, Input, Output, Master/VCA, MIDI).
fn strip_visible(kind: TrackKind, filters: &[bool; 8]) -> bool {
    match kind {
        TrackKind::Audio => filters[0],
        TrackKind::Instrument | TrackKind::Drummer => filters[1],
        TrackKind::Aux => filters[2],
        TrackKind::Bus => filters[3],
        TrackKind::Output => filters[5],
        TrackKind::Vca => filters[6],
        TrackKind::ExternalMidi => filters[7],
        // A folder is only a way to group tracks; it has no channel strip.
        TrackKind::Folder => false,
    }
}

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState, meters: &Meters) {
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let bar = Rect::from_min_size(area.min, vec2(area.width(), TOOLBAR_H));
    toolbar(ui, p, s, bar);
    let v = &mut s.mixer;

    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), area.max);
    let labels = Rect::from_min_max(body.min, pos2(body.left() + LABEL_W, body.bottom()));
    let strips_area = Rect::from_min_max(pos2(labels.right(), body.top()), body.max);
    let g = ui.painter_at(body);
    g.rect_filled(body, 0.0, MIXER_BG);

    // Which strips, in order.
    let filters = v.filters;
    let mut keys: Vec<StripKey> = if p.tracks.is_empty() {
        Vec::new()
    } else {
        match v.mode {
            MixerMode::Single => vec![StripKey::Track(p.selected_track.min(p.tracks.len() - 1))],
            MixerMode::Tracks | MixerMode::All => (0..p.tracks.len())
                .filter(|i| strip_visible(p.tracks[*i].kind, &filters))
                .map(StripKey::Track)
                .collect(),
        }
    };
    if filters[5] {
        keys.push(StripKey::StereoOut);
    }
    if filters[6] && v.mode != MixerMode::Single {
        keys.push(StripKey::Master);
    }

    let strip_w = if v.wide { 96.0 } else { 80.0 };
    let total_w = keys.len() as f32 * (strip_w + GAP);
    if ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| strips_area.contains(pt))
    {
        let d = ui.input(|i| i.smooth_scroll_delta);
        v.scroll_x -= d.x + d.y;
    }
    v.scroll_x = v
        .scroll_x
        .clamp(0.0, (total_w - strips_area.width() + 8.0).max(0.0));
    let r = rows(body.top(), fx_rows_for(p, &keys), send_rows_for(p, &keys));

    // Row labels.
    let gl = ui.painter_at(labels);
    let lx = labels.right() - 6.0;
    let label_at = |y: f32, h: f32, t: &str| {
        gl.text(
            pos2(lx, y + h / 2.0),
            Align2::RIGHT_CENTER,
            t,
            FontId::proportional(10.5),
            TEXT_DIM,
        );
    };
    label_at(r.setting, 16.0, tr("Setting"));
    label_at(r.gain_red, 8.0, tr("Gain Reduction"));
    label_at(r.eq, 24.0, "EQ");
    label_at(r.input, 16.0, tr("Input"));
    label_at(r.fx, 15.0, tr("Audio FX"));
    label_at(r.sends, 16.0, tr("Sends"));
    label_at(r.output, 16.0, tr("Output"));
    label_at(r.group, 16.0, tr("Group"));
    label_at(r.automation, 16.0, tr("Automation"));
    label_at(r.pan, 24.0, tr("Pan"));
    label_at(r.db, 14.0, "dB");

    let gs = ui.painter_at(strips_area);
    for (n, key) in keys.iter().enumerate() {
        let x = strips_area.left() + 4.0 + n as f32 * (strip_w + GAP) - v.scroll_x;
        let strip = Rect::from_min_max(
            pos2(x, body.top() + 2.0),
            pos2(x + strip_w, body.bottom() - 2.0),
        );
        if strip.right() < strips_area.left() || strip.left() > strips_area.right() {
            continue;
        }
        channel_strip(
            ui,
            &gs,
            strip,
            *key,
            p,
            v,
            meters,
            &r,
            "mixer",
            NameStyle::Coloured,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::PointerButton;
    use prowave_engine::FxKind;

    /// A project with an audio-style track and an Aux track, and its mixer drawn in a window
    /// 1400 by 700 points.
    fn mixer_project() -> Project {
        let mut p = Project::empty();
        p.add_track(TrackKind::Aux);
        p.selected_track = 0;
        p
    }

    /// Where the fader cap of the first strip sits at `db`, and the rows of the mixer.
    fn fader_at(p: &Project, db: f32) -> egui::Pos2 {
        let keys: Vec<StripKey> = (0..p.tracks.len())
            .map(StripKey::Track)
            .chain([StripKey::StereoOut, StripKey::Master])
            .collect();
        let r = rows(TOOLBAR_H, fx_rows_for(p, &keys), send_rows_for(p, &keys));
        let strip_bottom = 700.0 - 2.0;
        let top = r.fader_top + 8.0;
        let bottom = strip_bottom - 26.0 - 22.0 - 6.0;
        let sx = LABEL_W + 4.0 + 4.0;
        pos2(sx + 22.0, bottom - db_to_frac(db) * (bottom - top))
    }

    fn run(h: &mut crate::ui_test::Harness, p: &mut Project, s: &mut UiState, n: usize) {
        for _ in 0..n {
            h.frame(vec![], |ui| show(ui, p, s, &Meters::default()));
        }
    }

    #[test]
    fn the_fader_moves_the_level_unless_the_strip_plays_its_automation() {
        let mut h = crate::ui_test::Harness::new(1400.0, 700.0);
        let mut s = UiState::new();
        let mut p = mixer_project();
        run(&mut h, &mut p, &mut s, 2);
        let cap = fader_at(&p, 0.0);
        let up = cap - vec2(0.0, 40.0);
        let frame = |h: &mut crate::ui_test::Harness, p: &mut Project, s: &mut UiState| {
            h.drag(
                &[cap, cap - vec2(0.0, 20.0), up],
                PointerButton::Primary,
                |ui| show(ui, p, s, &Meters::default()),
            )
        };
        frame(&mut h, &mut p, &mut s);
        assert!(p.tracks[0].volume_db > 0.5, "{}", p.tracks[0].volume_db);

        // Read mode with a lane: the fader shows the lane and cannot be moved.
        p.tracks[0].volume_db = -6.0;
        p.tracks[0].auto.mode = AutoMode::Read;
        p.tracks[0].auto.set_point(AutoTarget::Volume, 0.0, -6.0);
        let cap = fader_at(&p, -6.0);
        h.drag(
            &[cap, cap - vec2(0.0, 40.0)],
            PointerButton::Primary,
            |ui| show(ui, &mut p, &mut s, &Meters::default()),
        );
        assert_eq!(p.tracks[0].volume_db, -6.0);

        // Touch: the same drag holds the control for its length and lets go after.
        p.tracks[0].auto.mode = AutoMode::Touch;
        h.press(cap, PointerButton::Primary, |ui| {
            show(ui, &mut p, &mut s, &Meters::default())
        });
        h.frame(
            vec![egui::Event::PointerMoved(cap - vec2(0.0, 30.0))],
            |ui| show(ui, &mut p, &mut s, &Meters::default()),
        );
        assert_eq!(p.tracks[0].auto.held, Some(AutoTarget::Volume));
        assert!(p.tracks[0].volume_db > -6.0);
        h.release(cap - vec2(0.0, 30.0), PointerButton::Primary, |ui| {
            show(ui, &mut p, &mut s, &Meters::default())
        });
        run(&mut h, &mut p, &mut s, 2);
        assert_eq!(p.tracks[0].auto.held, None);
    }

    #[test]
    fn sends_on_faders_set_the_send_to_the_bus_and_a_fader_at_the_bottom_takes_it_away() {
        let mut h = crate::ui_test::Harness::new(1400.0, 700.0);
        let mut s = UiState::new();
        let mut p = mixer_project();
        s.mixer.sends_on_faders = Some(2);
        run(&mut h, &mut p, &mut s, 2);
        let bottom = fader_at(&p, -60.0);
        let top = fader_at(&p, 0.0);
        h.drag(
            &[bottom, bottom - vec2(0.0, 20.0), top],
            PointerButton::Primary,
            |ui| show(ui, &mut p, &mut s, &Meters::default()),
        );
        let sends = &p.tracks[0].sends;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].bus, Some(2));
        assert!(sends[0].level > 0.3, "{}", sends[0].level);
        // The track's own level was not touched.
        assert_eq!(p.tracks[0].volume_db, 0.0);
        // The conversions agree both ways, and the bottom of the scale removes the send.
        let mut list = vec![];
        set_send_level(&mut list, 3, 0.0);
        assert!((prowave_engine::send_gain(list[0].level) - 1.0).abs() < 0.01);
        set_send_level(&mut list, 3, -60.0);
        assert_eq!(list[0].level, 0.0);
        set_send_level(&mut list, 4, -60.0);
        assert_eq!(list.len(), 1, "nothing to add for a silent send");
    }

    #[test]
    fn a_fader_moves_its_whole_group_by_the_same_amount() {
        let mut h = crate::ui_test::Harness::new(1400.0, 700.0);
        let mut s = UiState::new();
        let mut p = mixer_project();
        // Track 0 (the instrument) and a new second track are one group; the aux is not.
        let second = p.add_track(TrackKind::Instrument);
        let g = p.add_group("Keys").unwrap_or(0);
        p.assign_group(0, Some(g));
        p.assign_group(second, Some(g));
        p.tracks[second].volume_db = -10.0;
        run(&mut h, &mut p, &mut s, 2);
        let cap = fader_at(&p, 0.0);
        let up = cap - vec2(0.0, 30.0);
        h.drag(
            &[cap, cap - vec2(0.0, 15.0), up],
            PointerButton::Primary,
            |ui| show(ui, &mut p, &mut s, &Meters::default()),
        );
        let moved = p.tracks[0].volume_db;
        assert!(moved > 0.5, "{moved}");
        assert!(
            (p.tracks[second].volume_db - (-10.0 + moved)).abs() < 0.01,
            "{} vs {}",
            p.tracks[second].volume_db,
            moved
        );
        assert_eq!(
            p.tracks[1].volume_db, 0.0,
            "the Aux strip is not in the group"
        );
        // With Volume off in the group the others stay.
        p.groups[g].volume = false;
        let cap = fader_at(&p, moved);
        let before = p.tracks[second].volume_db;
        h.drag(
            &[cap, cap + vec2(0.0, 10.0), cap + vec2(0.0, 20.0)],
            PointerButton::Primary,
            |ui| show(ui, &mut p, &mut s, &Meters::default()),
        );
        assert!(p.tracks[0].volume_db < moved);
        assert_eq!(p.tracks[second].volume_db, before);
    }

    #[test]
    fn the_slot_menus_add_replace_move_bypass_and_remove_effects() {
        let mut p = mixer_project();
        let mut v = MixerView::default();
        for kind in [FxKind::ChannelEq, FxKind::Compressor, FxKind::Reverb] {
            apply_insert(&mut p, &mut v, 0, 99, InsertAct::Add(kind));
        }
        let kinds = |p: &Project| {
            p.tracks[0]
                .inserts
                .iter()
                .map(|x| x.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&p),
            [FxKind::ChannelEq, FxKind::Compressor, FxKind::Reverb]
        );
        assert_eq!(v.fx_window, Some((0, 2)), "a new effect opens its window");
        apply_insert(&mut p, &mut v, 0, 2, InsertAct::Move(-1));
        assert_eq!(
            kinds(&p),
            [FxKind::ChannelEq, FxKind::Reverb, FxKind::Compressor]
        );
        assert_eq!(v.fx_window, Some((0, 1)), "the window follows its effect");
        apply_insert(&mut p, &mut v, 0, 0, InsertAct::Move(-1));
        apply_insert(&mut p, &mut v, 0, 2, InsertAct::Move(1));
        assert_eq!(kinds(&p).len(), 3, "moving past an end does nothing");
        apply_insert(&mut p, &mut v, 0, 1, InsertAct::Bypass);
        assert!(p.tracks[0].inserts[1].bypass);
        apply_insert(&mut p, &mut v, 0, 1, InsertAct::Replace(FxKind::Delay));
        assert_eq!(p.tracks[0].inserts[1], Insert::new(FxKind::Delay));
        apply_insert(&mut p, &mut v, 0, 0, InsertAct::Remove);
        assert_eq!(
            v.fx_window,
            Some((0, 0)),
            "the window moved up with the others"
        );
        apply_insert(&mut p, &mut v, 0, 0, InsertAct::Remove);
        assert_eq!(v.fx_window, None, "its effect is gone");
        apply_insert(&mut p, &mut v, 0, 5, InsertAct::Remove);
        apply_insert(&mut p, &mut v, 9, 0, InsertAct::Open);
        for _ in 0..20 {
            apply_insert(&mut p, &mut v, 0, 0, InsertAct::Add(FxKind::Chorus));
        }
        assert_eq!(p.tracks[0].inserts.len(), prowave_engine::MAX_INSERTS);
    }

    #[test]
    fn rows_make_room_for_every_send_and_the_strip_draws_with_all_of_it() {
        let mut p = mixer_project();
        p.tracks[0].sends = vec![
            SendSlot {
                bus: Some(1),
                level: 0.5,
                pre: true,
            },
            SendSlot {
                bus: Some(2),
                level: 0.7,
                pre: false,
            },
        ];
        p.tracks[0].inserts = vec![
            Insert::new(FxKind::ChannelEq),
            Insert::new(FxKind::Compressor),
        ];
        p.tracks[0].auto.mode = AutoMode::Latch;
        p.tracks[0].auto.set_point(AutoTarget::Pan, 0.0, 0.5);
        let keys = [StripKey::Track(0), StripKey::Track(1)];
        assert_eq!(send_rows_for(&p, &keys), 3);
        assert_eq!(send_rows_for(&Project::empty(), &[StripKey::Track(0)]), 1);
        let r = rows(0.0, 3, 3);
        assert!(r.output - r.sends >= 3.0 * 17.0);
        let mut h = crate::ui_test::Harness::new(1400.0, 700.0);
        let mut s = UiState::new();
        let meters = Meters {
            tracks: vec![(0.5, 0.4), (0.0, 0.0)],
            reduction: vec![6.0, 0.0],
            master: (0.3, 0.3),
        };
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui, &mut p, &mut s, &meters));
        }
    }

    #[test]
    fn toolbar_groups_never_overlap_and_give_way_in_order() {
        // Wide panel: modes centred, every filter a button, Sends on Faders shown.
        let f = fit_toolbar(200.0, 1200.0, 2300.0, 520.0);
        assert!(f.filters_inline && f.show_sends);
        assert_eq!(f.modes_left, 1200.0 - MODE_W / 2.0 + 60.0);

        // Narrower: the modes move left of the filters, Sends on Faders goes first.
        let f = fit_toolbar(200.0, 700.0, 1100.0, 520.0);
        assert!(f.filters_inline && !f.show_sends);
        assert!(f.modes_left + MODE_W + TOOLBAR_GAP <= 1100.0 - 520.0);
        assert!(f.modes_left >= 200.0 + TOOLBAR_GAP);

        // Narrower still: the filters fold into one menu.
        let f = fit_toolbar(200.0, 500.0, 700.0, 520.0);
        assert!(!f.filters_inline);
        assert!(f.modes_left + MODE_W + TOOLBAR_GAP <= 700.0 - FILTER_MENU_W);
    }

    #[test]
    fn strips_follow_the_filter_buttons_and_folders_have_none() {
        let all = [true; 8];
        let mut only_aux = [false; 8];
        only_aux[2] = true;
        assert!(strip_visible(TrackKind::Aux, &all));
        assert!(strip_visible(TrackKind::Aux, &only_aux));
        assert!(!strip_visible(TrackKind::Audio, &only_aux));
        assert!(!strip_visible(TrackKind::Instrument, &only_aux));
        assert!(!strip_visible(TrackKind::Aux, &[false; 8]));
        assert!(
            !strip_visible(TrackKind::Folder, &all),
            "a folder has no channel strip"
        );
    }
}
