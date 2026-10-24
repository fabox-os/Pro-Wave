//! Inspector (Logic Pro: the "i" panel): Region Inspector, Track Inspector and the two
//! channel strips (selected track and its output).
//!
//! Works today: Region Mute, Quantize, Transpose, Velocity, and for audio regions Gain, Fade
//! In / Out, Curve and Reverse; Track Transpose, Velocity,
//! Key Limit and Vel Limit (all applied non-destructively when the engine song is built);
//! both channel strips (fader, pan, M/S, meters). Fields Logic has that are not implemented
//! yet are shown dimmed and do nothing.
use crate::i18n::tr;
use crate::mixer::{channel_strip, fx_rows_for, rows, send_rows_for, Meters, NameStyle, StripKey};
use crate::model::{Project, TrackKind, MAX_REGION_GAIN, MIN_REGION_GAIN};
use crate::track_icons::{draw_icon, track_icon};
use crate::widgets::{stroke, BTN_BG, TEXT, TEXT_DIM};
use crate::UiState;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
};
use prowave_engine::FadeCurve;

const PANEL_BG: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
const HEADER_BG: Color32 = Color32::from_rgb(0x42, 0x42, 0x42);
const ROW_H: f32 = 20.0;
const HEADER_H: f32 = 24.0;
const LABEL_RIGHT: f32 = 104.0;
const VALUE_LEFT: f32 = 112.0;
const STRIPS_MIN_H: f32 = 430.0;

/// Fade lengths move in 1/16 notes.
const FADE_STEPS_PER_BEAT: i32 = 4;

pub(crate) const FADE_CURVES: [(&str, FadeCurve); 3] = [
    ("Equal Power", FadeCurve::EqualPower),
    ("Linear", FadeCurve::Linear),
    ("S-Curve", FadeCurve::SCurve),
];

fn gain_text(db: f32) -> String {
    if db == 0.0 {
        "0 dB".into()
    } else {
        format!("{db:+.1} dB")
    }
}

/// A fade of `beats` at `bpm`, in milliseconds like Logic shows it.
fn fade_text(beats: f32, bpm: f32) -> String {
    if beats <= 0.0 {
        "0".into()
    } else {
        format!("{:.0} ms", beats * 60_000.0 / bpm.max(1.0))
    }
}

pub(crate) const QUANTIZE: [(&str, Option<f32>); 7] = [
    ("Off", None),
    ("1/4 Note", Some(1.0)),
    ("1/8 Note", Some(0.5)),
    ("1/16 Note", Some(0.25)),
    ("1/32 Note", Some(0.125)),
    ("1/8 Triplet", Some(1.0 / 3.0)),
    ("1/16 Triplet", Some(1.0 / 6.0)),
];

pub struct InspectorView {
    pub region_open: bool,
    pub track_open: bool,
    pub more_open: bool,
    scroll_y: f32,
    /// Edits waiting for the mouse to be released before the engine song is rebuilt.
    pending_song_change: bool,
}

impl Default for InspectorView {
    fn default() -> Self {
        Self {
            region_open: true,
            track_open: false,
            more_open: false,
            scroll_y: 0.0,
            pending_song_change: false,
        }
    }
}

fn id(key: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    egui::Id::unique(("inspector", key))
}

/// Logic note names: 0 = C-2, 127 = G8.
fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[(n % 12) as usize], n as i32 / 12 - 2)
}

/// Disclosure triangle + "Title: name" section header. Returns true when toggled.
fn section_header(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    key: &str,
    open: bool,
    title: &str,
    name: &str,
) -> bool {
    let resp = ui.interact(r, id(("section", key)), Sense::click());
    g.rect_filled(r, 0.0, HEADER_BG);
    g.line_segment(
        [r.left_bottom(), r.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x2a)),
    );
    let c = pos2(r.left() + 12.0, r.center().y);
    let tri = if open {
        vec![
            c + vec2(-4.0, -2.0),
            c + vec2(4.0, -2.0),
            c + vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + vec2(-2.0, -4.0),
            c + vec2(3.0, 0.0),
            c + vec2(-2.0, 4.0),
        ]
    };
    g.add(Shape::convex_polygon(tri, TEXT, Stroke::NONE));
    let t = g.text(
        pos2(r.left() + 24.0, r.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(12.5),
        TEXT_DIM,
    );
    let name_left = t.right() + 5.0;
    let font = FontId::proportional(12.5);
    let shown = crate::mixer::elide(g, name, &font, r.right() - 8.0 - name_left);
    g.text(
        pos2(name_left, r.center().y),
        Align2::LEFT_CENTER,
        shown,
        font,
        TEXT,
    );
    resp.clicked()
}

fn label(g: &Painter, panel_left: f32, y: f32, text: &str, enabled: bool) {
    let col = if enabled {
        Color32::from_gray(0xb0)
    } else {
        Color32::from_gray(0x6c)
    };
    g.text(
        pos2(panel_left + LABEL_RIGHT, y + ROW_H / 2.0),
        Align2::RIGHT_CENTER,
        text,
        FontId::proportional(11.5),
        col,
    );
}

/// Small checkbox. Returns true when toggled.
fn checkbox(ui: &Ui, g: &Painter, at: Pos2, key: &str, value: bool, enabled: bool) -> bool {
    let r = Rect::from_min_size(at + vec2(0.0, (ROW_H - 12.0) / 2.0), vec2(12.0, 12.0));
    let clicked = enabled
        && ui
            .interact(r.expand(3.0), id(("check", key)), Sense::click())
            .clicked();
    let border = if enabled {
        Color32::from_gray(0x8a)
    } else {
        Color32::from_gray(0x55)
    };
    g.rect_filled(
        r,
        2.5,
        if value {
            crate::widgets::BLUE
        } else {
            Color32::from_gray(0x2c)
        },
    );
    g.rect_stroke(r, 2.5, Stroke::new(1.0, border), StrokeKind::Inside);
    if value {
        g.add(Shape::line(
            vec![
                r.left_center() + vec2(2.5, 0.0),
                r.center_bottom() + vec2(-1.0, -3.0),
                r.right_top() + vec2(-2.5, 3.0),
            ],
            Stroke::new(1.6, Color32::WHITE),
        ));
    }
    clicked
}

/// Compact popup ("Off ⇕"). Disabled ones are drawn dim and never open.
fn popup(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    key: &str,
    text: &str,
    enabled: bool,
) -> Option<egui::Response> {
    let col = if enabled {
        TEXT
    } else {
        Color32::from_gray(0x70)
    };
    let resp = enabled.then(|| ui.interact(r, id(("popup", key)), Sense::click()));
    let hovered = resp.as_ref().is_some_and(|r| r.hovered());
    g.rect_filled(
        r,
        3.0,
        if hovered {
            Color32::from_gray(0x5c)
        } else if enabled {
            BTN_BG
        } else {
            Color32::from_gray(0x44)
        },
    );
    g.text(
        pos2(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(11.5),
        col,
    );
    let c = pos2(r.right() - 8.0, r.center().y);
    g.add(Shape::line(
        vec![
            c + vec2(-2.5, -1.5),
            c + vec2(0.0, -4.0),
            c + vec2(2.5, -1.5),
        ],
        stroke(col),
    ));
    g.add(Shape::line(
        vec![c + vec2(-2.5, 1.5), c + vec2(0.0, 4.0), c + vec2(2.5, 1.5)],
        stroke(col),
    ));
    resp
}

/// Logic-style value field: drag up/down to change, double-click to reset.
#[allow(clippy::too_many_arguments)]
fn drag_value(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    key: &str,
    value: i32,
    range: (i32, i32),
    reset: i32,
    text: String,
    enabled: bool,
) -> Option<i32> {
    let col = if enabled {
        TEXT
    } else {
        Color32::from_gray(0x70)
    };
    g.rect_filled(r, 3.0, Color32::from_gray(0x30));
    g.text(
        pos2(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(11.5),
        col,
    );
    if !enabled {
        return None;
    }
    let resp = ui.interact(r, id(("drag", key)), Sense::click_and_drag());
    if resp.double_clicked() {
        return Some(reset);
    }
    if resp.dragged() {
        // Accumulate fractional drag in egui memory so slow drags still move the value.
        let acc_id = id(("drag_acc", key));
        let acc: f32 =
            ui.ctx().data(|d| d.get_temp(acc_id).unwrap_or(0.0)) - resp.drag_delta().y / 4.0;
        let steps = acc.trunc() as i32;
        ui.ctx()
            .data_mut(|d| d.insert_temp(acc_id, acc - steps as f32));
        if steps != 0 {
            return Some((value + steps).clamp(range.0, range.1));
        }
    }
    None
}

fn signed(v: i32) -> String {
    match v {
        0 => String::new(),
        v if v > 0 => format!("+{v}"),
        v => v.to_string(),
    }
}

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState, meters: &Meters) {
    let panel = ui.available_rect_before_wrap();
    ui.allocate_rect(panel, Sense::hover());
    let g = ui.painter_at(panel);
    g.rect_filled(panel, 0.0, PANEL_BG);
    let left = panel.left();
    let w = panel.width();

    if ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| panel.contains(pt))
    {
        s.inspector.scroll_y -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    let mut y = panel.top() - s.inspector.scroll_y;
    let mut changed_song = false;

    // ---------------- Region
    let sel = s
        .tracks
        .selected
        .filter(|(t, r)| p.tracks.get(*t).is_some_and(|tr| *r < tr.regions.len()));
    let region_name = sel.map_or(tr("MIDI Defaults").to_string(), |(t, r)| {
        p.tracks[t].regions[r].name.clone()
    });
    let hdr = Rect::from_min_size(pos2(left, y), vec2(w, HEADER_H));
    if section_header(
        ui,
        &g,
        hdr,
        "region",
        s.inspector.region_open,
        tr("Region:"),
        &region_name,
    ) {
        s.inspector.region_open = !s.inspector.region_open;
    }
    y += HEADER_H + 4.0;
    if s.inspector.region_open {
        let is_audio = sel.is_some_and(|(t, r)| {
            p.tracks[t].regions[r].audio.is_some() || p.tracks[t].kind == TrackKind::Audio
        });
        let has = sel.is_some();
        let midi = has && !is_audio;
        let mut params = sel
            .map(|(t, r)| p.tracks[t].regions[r].params)
            .unwrap_or_default();
        let vx = left + VALUE_LEFT;
        let vw = w - VALUE_LEFT - 10.0;

        label(&g, left, y, tr("Mute:"), has);
        if checkbox(ui, &g, pos2(vx, y), "region_mute", params.mute, has) {
            params.mute = !params.mute;
        }
        y += ROW_H;
        label(&g, left, y, tr("Loop:"), false);
        checkbox(ui, &g, pos2(vx, y), "region_loop", false, false);
        y += ROW_H;
        label(&g, left, y, tr("Quantize:"), midi);
        let q_text = QUANTIZE
            .iter()
            .find(|(_, q)| *q == params.quantize)
            .map_or(tr("Off"), |(t, _)| tr(t));
        if let Some(resp) = popup(
            ui,
            &g,
            Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
            "quantize",
            q_text,
            midi,
        ) {
            egui::Popup::menu(&resp).show(|ui| {
                for (t, q) in QUANTIZE {
                    if ui.selectable_label(params.quantize == q, tr(t)).clicked() {
                        params.quantize = q;
                        ui.close();
                    }
                }
            });
        }
        y += ROW_H;
        label(&g, left, y, tr("Q-Swing:"), false);
        y += ROW_H;
        label(&g, left, y, tr("Transpose:"), midi);
        let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
        if let Some(v) = drag_value(
            ui,
            &g,
            r,
            "region_transpose",
            params.transpose as i32,
            (-96, 96),
            0,
            signed(params.transpose as i32),
            midi,
        ) {
            params.transpose = v as i8;
        }
        y += ROW_H;
        label(&g, left, y, tr("Velocity:"), midi);
        let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
        if let Some(v) = drag_value(
            ui,
            &g,
            r,
            "region_velocity",
            params.velocity as i32,
            (-99, 99),
            0,
            signed(params.velocity as i32),
            midi,
        ) {
            params.velocity = v as i8;
        }
        y += ROW_H;

        // Audio regions: gain, fades and reverse (applied by the engine, file unchanged).
        let audio = has && is_audio;
        let (start_beat, len_beats) = sel.map_or((0.0, 0.0), |(t, r)| {
            let reg = &p.tracks[t].regions[r];
            (p.region_start_beats(reg), p.region_len_beats(reg))
        });
        let bpm = p.tempo_at(start_beat);
        label(&g, left, y, tr("Gain:"), audio);
        let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
        if let Some(v) = drag_value(
            ui,
            &g,
            r,
            "region_gain",
            (params.gain_db * 2.0).round() as i32,
            (
                (MIN_REGION_GAIN * 2.0) as i32,
                (MAX_REGION_GAIN * 2.0) as i32,
            ),
            0,
            gain_text(params.gain_db),
            audio,
        ) {
            params.gain_db = v as f32 / 2.0;
        }
        y += ROW_H;
        // Fades move in 1/16-note steps and show in milliseconds, as in Logic.
        let max_fade = (len_beats * FADE_STEPS_PER_BEAT as f64) as i32;
        for (name, key, which) in [
            (tr("Fade In:"), "region_fade_in", 0),
            (tr("Fade Out:"), "region_fade_out", 1),
        ] {
            let beats = if which == 0 {
                params.fade_in
            } else {
                params.fade_out
            };
            label(&g, left, y, name, audio);
            let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
            if let Some(v) = drag_value(
                ui,
                &g,
                r,
                key,
                (beats * FADE_STEPS_PER_BEAT as f32).round() as i32,
                (0, max_fade.max(0)),
                0,
                fade_text(beats, bpm),
                audio,
            ) {
                let beats = v as f32 / FADE_STEPS_PER_BEAT as f32;
                if which == 0 {
                    params.fade_in = beats;
                } else {
                    params.fade_out = beats;
                }
            }
            y += ROW_H;
        }
        label(&g, left, y, tr("Curve:"), audio);
        let curve_name = FADE_CURVES
            .iter()
            .find(|(_, c)| *c == params.fade_curve)
            .map_or(tr("Equal Power"), |(t, _)| tr(t));
        if let Some(resp) = popup(
            ui,
            &g,
            Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
            "fade_curve",
            curve_name,
            audio,
        ) {
            egui::Popup::menu(&resp).show(|ui| {
                for (t, c) in FADE_CURVES {
                    if ui.selectable_label(params.fade_curve == c, tr(t)).clicked() {
                        params.fade_curve = c;
                        ui.close();
                    }
                }
            });
        }
        y += ROW_H;
        label(&g, left, y, tr("Reverse:"), audio);
        if checkbox(ui, &g, pos2(vx, y), "region_reverse", params.reverse, audio) {
            params.reverse = !params.reverse;
        }
        y += ROW_H;
        // Crossfades with the audio region this one overlaps (Drag: X-Fade sets it too).
        label(&g, left, y, &format!("{}:", tr("X-Fade")), audio);
        if checkbox(ui, &g, pos2(vx, y), "region_xfade", params.crossfade, audio) {
            params.crossfade = !params.crossfade;
        }
        y += ROW_H;

        // "More" disclosure with the rest of Logic's region parameters (not implemented yet).
        let more = Rect::from_min_size(pos2(left, y), vec2(w, ROW_H));
        if ui.interact(more, id("more"), Sense::click()).clicked() {
            s.inspector.more_open = !s.inspector.more_open;
        }
        let c = pos2(left + 12.0, more.center().y);
        let tri = if s.inspector.more_open {
            vec![
                c + vec2(-3.5, -2.0),
                c + vec2(3.5, -2.0),
                c + vec2(0.0, 2.5),
            ]
        } else {
            vec![
                c + vec2(-2.0, -3.5),
                c + vec2(2.5, 0.0),
                c + vec2(-2.0, 3.5),
            ]
        };
        g.add(Shape::convex_polygon(tri, TEXT_DIM, Stroke::NONE));
        g.text(
            pos2(left + 22.0, more.center().y),
            Align2::LEFT_CENTER,
            tr("More"),
            FontId::proportional(11.5),
            TEXT_DIM,
        );
        y += ROW_H;
        if s.inspector.more_open {
            for t in [
                tr("Dynamics:"),
                tr("Gate Time:"),
                tr("Clip Length:"),
                tr("Score:"),
                tr("Q-Velocity:"),
                tr("Q-Length:"),
                tr("Q-Flam:"),
                tr("Q-Range:"),
                tr("Q-Strength:"),
            ] {
                label(&g, left, y, t, false);
                y += ROW_H;
            }
        }
        if let Some((t, r)) = sel {
            if p.tracks[t].regions[r].params != params {
                p.tracks[t].regions[r].params = params;
                changed_song = true;
            }
        }
        y += 4.0;
    }

    // ---------------- Track
    let ti = p.selected_track.min(p.tracks.len().saturating_sub(1));
    let track_name = p.tracks.get(ti).map_or(String::new(), |t| t.name.clone());
    let hdr = Rect::from_min_size(pos2(left, y), vec2(w, HEADER_H));
    if section_header(
        ui,
        &g,
        hdr,
        "track",
        s.inspector.track_open,
        tr("Track:"),
        &track_name,
    ) {
        s.inspector.track_open = !s.inspector.track_open;
    }
    y += HEADER_H + 4.0;
    if s.inspector.track_open && ti < p.tracks.len() {
        let vx = left + VALUE_LEFT;
        let vw = w - VALUE_LEFT - 10.0;
        let audio = p.tracks[ti].kind == TrackKind::Audio;
        let mut tp = p.tracks[ti].params;

        label(&g, left, y, tr("Icon:"), true);
        draw_icon(
            &g,
            Rect::from_min_size(pos2(vx, y + 1.0), vec2(26.0, 26.0)),
            track_icon(&p.tracks[ti]),
        );
        y += 30.0;
        let channel = p.channel_name(ti);
        label(&g, left, y, tr("Channel:"), false);
        popup(
            ui,
            &g,
            Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
            "channel",
            &channel,
            false,
        );
        y += ROW_H;
        if audio {
            label(&g, left, y, tr("Freeze Mode:"), false);
            popup(
                ui,
                &g,
                Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                "freeze",
                tr("Pre Fader"),
                false,
            );
            y += ROW_H;
            label(&g, left, y, tr("Q-Reference:"), false);
            checkbox(ui, &g, pos2(vx, y), "qref", true, false);
            y += ROW_H;
            label(&g, left, y, tr("Flex Mode:"), false);
            popup(
                ui,
                &g,
                Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                "flex",
                tr("Off"),
                false,
            );
            y += ROW_H;
        } else {
            let external = p.tracks[ti].kind == TrackKind::ExternalMidi;
            if external {
                // Where this track's notes go: a MIDI output port and a channel.
                label(&g, left, y, tr("MIDI Out Port:"), true);
                let port = p.tracks[ti]
                    .midi_port
                    .clone()
                    .unwrap_or_else(|| tr("No Port").to_string());
                if let Some(resp) = popup(
                    ui,
                    &g,
                    Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                    "midi_out_port",
                    &port,
                    true,
                ) {
                    egui::Popup::menu(&resp).show(|ui| {
                        let current = p.tracks[ti].midi_port.clone();
                        if ui
                            .selectable_label(current.is_none(), tr("No Port"))
                            .clicked()
                        {
                            p.tracks[ti].midi_port = None;
                            ui.close();
                        }
                        for name in crate::midi_out::ports() {
                            if ui
                                .selectable_label(current.as_deref() == Some(&name), &name)
                                .clicked()
                            {
                                p.tracks[ti].midi_port = Some(name);
                                ui.close();
                            }
                        }
                    });
                }
                y += ROW_H;
                label(&g, left, y, tr("MIDI Out Channel:"), true);
                let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
                let channel = p.tracks[ti].midi_channel.clamp(1, 16);
                if let Some(v) = drag_value(
                    ui,
                    &g,
                    r,
                    "midi_out_channel",
                    channel as i32,
                    (1, 16),
                    1,
                    channel.to_string(),
                    true,
                ) {
                    p.tracks[ti].midi_channel = v as u8;
                }
                y += ROW_H;
            }
            for (t, v) in [
                (tr("MIDI In Port:"), tr("All")),
                (tr("MIDI In Channel:"), tr("All")),
                (tr("MIDI Out Channel:"), tr("All")),
                (tr("Freeze Mode:"), tr("Pre Fader")),
            ]
            .into_iter()
            .filter(|(t, _)| !(external && *t == tr("MIDI Out Channel:")))
            {
                label(&g, left, y, t, false);
                popup(
                    ui,
                    &g,
                    Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                    t,
                    v,
                    false,
                );
                y += ROW_H;
            }
            label(&g, left, y, tr("Transpose:"), true);
            let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
            if let Some(v) = drag_value(
                ui,
                &g,
                r,
                "track_transpose",
                tp.transpose as i32,
                (-96, 96),
                0,
                signed(tp.transpose as i32),
                true,
            ) {
                tp.transpose = v as i8;
            }
            y += ROW_H;
            label(&g, left, y, tr("Velocity:"), true);
            let r = Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0));
            if let Some(v) = drag_value(
                ui,
                &g,
                r,
                "track_velocity",
                tp.velocity as i32,
                (-99, 99),
                0,
                signed(tp.velocity as i32),
                true,
            ) {
                tp.velocity = v as i8;
            }
            y += ROW_H;
            let half = (vw - 4.0) / 2.0;
            label(&g, left, y, tr("Key Limit:"), true);
            let (lo, hi) = tp.key_limit;
            let r1 = Rect::from_min_size(pos2(vx, y + 2.0), vec2(half, ROW_H - 4.0));
            let r2 = Rect::from_min_size(pos2(vx + half + 4.0, y + 2.0), vec2(half, ROW_H - 4.0));
            if let Some(v) = drag_value(
                ui,
                &g,
                r1,
                "key_lo",
                lo as i32,
                (0, hi as i32),
                0,
                note_name(lo),
                true,
            ) {
                tp.key_limit.0 = v as u8;
            }
            if let Some(v) = drag_value(
                ui,
                &g,
                r2,
                "key_hi",
                hi as i32,
                (lo as i32, 127),
                127,
                note_name(hi),
                true,
            ) {
                tp.key_limit.1 = v as u8;
            }
            y += ROW_H;
            label(&g, left, y, tr("Vel Limit:"), true);
            let (lo, hi) = tp.vel_limit;
            let r1 = Rect::from_min_size(pos2(vx, y + 2.0), vec2(half, ROW_H - 4.0));
            let r2 = Rect::from_min_size(pos2(vx + half + 4.0, y + 2.0), vec2(half, ROW_H - 4.0));
            if let Some(v) = drag_value(
                ui,
                &g,
                r1,
                "vel_lo",
                lo as i32,
                (1, hi as i32),
                1,
                lo.to_string(),
                true,
            ) {
                tp.vel_limit.0 = v as u8;
            }
            if let Some(v) = drag_value(
                ui,
                &g,
                r2,
                "vel_hi",
                hi as i32,
                (lo as i32, 127),
                127,
                hi.to_string(),
                true,
            ) {
                tp.vel_limit.1 = v as u8;
            }
            y += ROW_H;
            label(&g, left, y, tr("Delay:"), false);
            y += ROW_H;
            for t in [tr("No Transpose:"), tr("No Reset:")] {
                label(&g, left, y, t, false);
                checkbox(ui, &g, pos2(vx, y), t, false, false);
                y += ROW_H;
            }
            label(&g, left, y, tr("Staff Style:"), false);
            popup(
                ui,
                &g,
                Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                "staff",
                tr("Auto"),
                false,
            );
            y += ROW_H;
            label(&g, left, y, tr("Articulation Set:"), false);
            popup(
                ui,
                &g,
                Rect::from_min_size(pos2(vx, y + 2.0), vec2(vw, ROW_H - 4.0)),
                "artic",
                "",
                false,
            );
            y += ROW_H;
        }
        if p.tracks[ti].params != tp {
            p.tracks[ti].params = tp;
            changed_song = true;
        }
        y += 4.0;
    }

    // ---------------- Channel strips: selected track + Stereo Out
    let strips_h = (panel.bottom() - y - 4.0).max(STRIPS_MIN_H);
    if !p.tracks.is_empty() {
        let keys = [StripKey::Track(ti), StripKey::StereoOut];
        let r = rows(y, fx_rows_for(p, &keys), send_rows_for(p, &keys));
        let sw = (w - 12.0) / 2.0;
        for (n, key) in keys.iter().enumerate() {
            let strip = Rect::from_min_size(
                pos2(left + 4.0 + n as f32 * (sw + 4.0), y),
                vec2(sw, strips_h),
            );
            channel_strip(
                ui,
                &g,
                strip,
                *key,
                p,
                &mut s.mixer,
                meters,
                &r,
                "inspector",
                NameStyle::Plain,
            );
        }
    }

    let content_h = y + strips_h + s.inspector.scroll_y - panel.top();
    s.inspector.scroll_y = s
        .inspector
        .scroll_y
        .clamp(0.0, (content_h - panel.height()).max(0.0));
    // Rebuild the engine song once per gesture, not on every frame of a drag.
    s.inspector.pending_song_change |= changed_song;
    if s.inspector.pending_song_change && !ui.input(|i| i.pointer.any_down()) {
        s.inspector.pending_song_change = false;
        p.song_rev += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_gain_and_fades_read_like_logic() {
        assert_eq!(gain_text(0.0), "0 dB");
        assert_eq!(gain_text(-3.5), "-3.5 dB");
        assert_eq!(gain_text(2.0), "+2.0 dB");
        assert_eq!(fade_text(0.0, 120.0), "0");
        assert_eq!(fade_text(1.0, 120.0), "500 ms", "a beat at 120 bpm");
    }
}
