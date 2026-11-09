//! The window of an effect in a channel strip's Audio FX slot (Logic: the plug-in window).
//!
//! Each window has the same chrome: the effect's name, a family chip (EQ, Dynamics, Space,
//! Time, Drive, Filter), Bypass and Reset, one bespoke visualization, and a rack of knobs.
//! Every control is automatable: moving one while the project plays writes it, and it moves by
//! itself when its lane is played.
use crate::automation;
use crate::i18n::{tr, trf};
use crate::mixer::Meters;
use crate::model::{AutoTarget, Insert, Project, Track};
use crate::UiState;
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui,
};
use prowave_engine::{FxKind, ParamInfo, AMP_CABS, AMP_MODELS};

/// Sample rate the curve of the EQs is drawn for.
const CURVE_RATE: f32 = 48_000.0;

/// Width and height of the Amp Designer's faceplate.
const AMP_W: f32 = 470.0;
const AMP_H: f32 = 146.0;

// The window palette: a warm charcoal rack, one signal accent per effect family.
const WIN_BG: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x1e);
const HEAD_BG: Color32 = Color32::from_rgb(0x23, 0x25, 0x2a);
const INSET: Color32 = Color32::from_rgb(0x12, 0x13, 0x16);
const LINE: Color32 = Color32::from_rgb(0x30, 0x33, 0x3a);
const TXT: Color32 = Color32::from_rgb(0xe8, 0xe6, 0xe1);
const DIM: Color32 = Color32::from_rgb(0x96, 0x9d, 0xa8);
const FAINT: Color32 = Color32::from_rgb(0x5f, 0x66, 0x70);

const EQ_BLUE: Color32 = Color32::from_rgb(0x6f, 0xb2, 0xff);
const DYN_AMBER: Color32 = Color32::from_rgb(0xe8, 0xb4, 0x5a);
const SPACE_VIOLET: Color32 = Color32::from_rgb(0xa9, 0x8b, 0xf5);
const TIME_BLUE: Color32 = Color32::from_rgb(0x8f, 0x9c, 0xff);
const DRIVE_RUST: Color32 = Color32::from_rgb(0xe0, 0x7a, 0x5f);
const FILTER_TEAL: Color32 = Color32::from_rgb(0x5f, 0xd1, 0xc5);

/// A slider the user moved: parameter number, new value, and whether the gesture ended.
pub(crate) struct Edit {
    pub(crate) param: usize,
    pub(crate) value: f32,
    pub(crate) ended: bool,
}

/// The family an effect belongs to; it chooses the accent and the copy in the header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Eq,
    Dynamics,
    Space,
    Time,
    Drive,
    Filter,
}

fn family(kind: FxKind) -> (Family, &'static str, Color32) {
    match kind {
        FxKind::ChannelEq | FxKind::MultiEq => (Family::Eq, "EQ", EQ_BLUE),
        FxKind::Compressor | FxKind::MultiComp | FxKind::Gate | FxKind::Limiter => {
            (Family::Dynamics, "Dynamics", DYN_AMBER)
        }
        FxKind::Reverb => (Family::Space, "Space", SPACE_VIOLET),
        FxKind::Delay | FxKind::Chorus | FxKind::Phaser => (Family::Time, "Time", TIME_BLUE),
        FxKind::Overdrive | FxKind::Amp => (Family::Drive, "Drive", DRIVE_RUST),
        FxKind::Filter => (Family::Filter, "Filter", FILTER_TEAL),
    }
}

fn window_width(kind: FxKind) -> f32 {
    match kind {
        FxKind::ChannelEq | FxKind::MultiEq => 632.0,
        FxKind::Amp => AMP_W + 28.0,
        FxKind::MultiComp => 604.0,
        _ => 560.0,
    }
}

/// Draws the effect window for the insert the Mixer opened, if any.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState, meters: &Meters) {
    let Some((ti, slot)) = s.mixer.fx_window else {
        return;
    };
    // The Stereo Out strip has effects but no automation lanes: a stand-in track with none.
    let master = ti == crate::mixer::MASTER_STRIP;
    let found = if master {
        p.master_inserts.get(slot)
    } else {
        p.tracks.get(ti).and_then(|t| t.inserts.get(slot))
    };
    let Some(insert) = found.copied() else {
        s.mixer.fx_window = None;
        return;
    };
    let stand_in;
    let track: &Track = if master {
        stand_in = Project::empty().tracks[0].clone();
        &stand_in
    } else {
        &p.tracks[ti]
    };
    let (reduction, input_level) = if master {
        (meters.master_reduction, meters.master_fx_input)
    } else {
        (
            meters.reduction.get(ti).copied().unwrap_or(0.0),
            meters.fx_input.get(ti).copied().unwrap_or(0.0),
        )
    };
    if insert.kind == FxKind::Reverb {
        #[allow(deprecated)]
        let id = egui::Id::new(("space_designer", ti, slot));
        let (edited, edits, open) = crate::space_designer_ui::window(
            ctx,
            &insert,
            track,
            slot,
            p.position_beats,
            id,
            input_level,
        );
        finish(p, s, ti, slot, insert, edited, edits, open);
        return;
    }
    let mut open = true;
    let title = if master {
        format!("{} · {}", tr("Stereo Out"), insert.name())
    } else {
        format!("{} · {}", p.tracks[ti].name, insert.name())
    };
    let beat = p.position_beats;
    let mut edited = insert;
    let mut edits: Vec<Edit> = Vec::new();
    let (_, family_name, accent) = family(edited.kind);
    let frame = egui::Frame::NONE
        .fill(WIN_BG)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::ZERO);
    egui::Window::new(title)
        .id(egui::Id::unique("fx_window"))
        .open(&mut open)
        .resizable(false)
        .frame(frame)
        .show(ctx, |ui| {
            header(ui, &mut edited, &mut edits, family_name, accent);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(window_width(edited.kind));
                    visual(ui, &edited, reduction, beat, accent);
                    ui.add_space(10.0);
                    if edited.kind == FxKind::Amp {
                        amp_faceplate(ui, &mut edited, track, slot, beat, &mut edits);
                    } else {
                        knob_rack(ui, &mut edited, track, slot, beat, accent, &mut edits);
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Drag a knob to adjust · double-click to reset")
                            .size(9.5)
                            .color(FAINT),
                    );
                });
        });
    finish(p, s, ti, slot, insert, edited, edits, open);
}

/// The title strip: the effect's family, Bypass and Reset.
fn header(
    ui: &mut Ui,
    edited: &mut Insert,
    _edits: &mut [Edit],
    family_name: &str,
    accent: Color32,
) {
    egui::Frame::NONE
        .fill(HEAD_BG)
        .inner_margin(egui::Margin::symmetric(14, 7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(family_name).size(9.5).color(accent));
                ui.label(RichText::new("·").size(10.0).color(FAINT));
                ui.label(RichText::new(edited.kind.name()).size(13.5).color(TXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(tr("Reset")).size(10.5))
                                .min_size(vec2(48.0, 20.0)),
                        )
                        .clicked()
                    {
                        edited.params = edited.kind.defaults();
                    }
                    let mut bypass = edited.bypass;
                    let response = ui.add(
                        egui::Button::new(
                            RichText::new(tr("Bypass")).size(10.5).color(if bypass {
                                Color32::BLACK
                            } else {
                                DIM
                            }),
                        )
                        .fill(if bypass {
                            accent
                        } else {
                            Color32::from_gray(0x2f)
                        })
                        .min_size(vec2(52.0, 20.0)),
                    );
                    if response.clicked() {
                        bypass = !bypass;
                        edited.bypass = bypass;
                    }
                });
            });
        });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, LINE);
}

/// The bespoke visualization of each effect.
fn visual(ui: &mut Ui, insert: &Insert, reduction: f32, beat: f64, accent: Color32) {
    let params = insert.kind.clamp_params(insert.params);
    match insert.kind {
        FxKind::ChannelEq | FxKind::MultiEq => eq_curve(ui, insert.kind, &params, accent),
        FxKind::Compressor | FxKind::Gate | FxKind::Limiter => {
            transfer_curve(ui, insert.kind, &params, reduction, accent)
        }
        FxKind::MultiComp => multicomp(ui, &params, reduction, accent),
        FxKind::Delay => delay_panel(ui, &params, accent),
        FxKind::Chorus => chorus_panel(ui, &params, accent),
        FxKind::Phaser => phaser_panel(ui, &params, beat, accent),
        FxKind::Overdrive => drive_panel(ui, &params, accent),
        FxKind::Filter => filter_panel(ui, &params, accent),
        FxKind::Amp | FxKind::Reverb => {}
    }
}

/// The knob rack: every parameter as a knob, in rows that fit the window.
fn knob_rack(
    ui: &mut Ui,
    insert: &mut Insert,
    track: &Track,
    slot: usize,
    beat: f64,
    accent: Color32,
    edits: &mut Vec<Edit>,
) {
    let cells = insert.kind.params().len();
    ui.horizontal_wrapped(|ui| {
        for k in 0..cells {
            let info = &insert.kind.params()[k];
            let target = AutoTarget::Fx {
                slot: slot as u8,
                param: k as u8,
            };
            let played = track.auto.played(target, beat);
            let value = played
                .map(|n| info.from_norm(n))
                .unwrap_or(insert.params[k]);
            let locked = played.is_some() || !automation::editable(track, target);
            let automated = !track.auto.lane(target).is_empty();
            let change = param_knob(ui, info, value, locked, automated, accent, insert.kind);
            if let Some((value, ended)) = change {
                edits.push(Edit {
                    param: k,
                    value,
                    ended,
                });
            }
        }
    });
}

/// One knob cell: label, knob, value. Returns a change when the user moves it.
#[allow(clippy::too_many_arguments)]
fn param_knob(
    ui: &mut Ui,
    info: &ParamInfo,
    value: f32,
    locked: bool,
    automated: bool,
    accent: Color32,
    kind: FxKind,
) -> Option<(f32, bool)> {
    let mut change = None;
    ui.allocate_ui_with_layout(vec2(94.0, 74.0), Layout::top_down(Align::Center), |ui| {
        ui.add_space(1.0);
        let label =
            ui.label(
                RichText::new(info.name)
                    .size(9.5)
                    .color(if locked { FAINT } else { DIM }),
            );
        if automated {
            label.on_hover_text(tr("Automated"));
        }
        let (rect, response) = ui.allocate_exact_size(vec2(44.0, 38.0), Sense::click_and_drag());
        let norm = info.to_norm(value);
        let painter = ui.painter_at(rect);
        let c = rect.center();
        // The value arc behind the knob.
        arc(
            &painter,
            c,
            17.0,
            -225f32.to_radians(),
            (-225.0 + 270.0 * norm).to_radians(),
            accent.gamma_multiply(0.75),
            2.2,
        );
        crate::widgets::draw_knob(&painter, c, 13.0, norm * 2.0 - 1.0);
        if automated {
            painter.circle_filled(pos2(rect.left() + 2.0, rect.top() + 3.0), 2.2, accent);
        }
        if !locked {
            if response.dragged() {
                let n = (norm - response.drag_delta().y * 0.006).clamp(0.0, 1.0);
                change = Some((info.from_norm(n), false));
            } else if response.drag_stopped() {
                change = Some((value, true));
            } else if response.double_clicked() {
                change = Some((info.default, true));
            }
        }
        let v = change.map_or(value, |(v, _)| v);
        ui.label(
            RichText::new(param_text(kind, info, v))
                .monospace()
                .size(9.5)
                .color(if locked { FAINT } else { TXT }),
        );
    });
    change
}

/// A parameter's value with the units the effect uses (and names for mode switches).
fn param_text(kind: FxKind, info: &ParamInfo, v: f32) -> String {
    if kind == FxKind::Filter && info.name == "Mode" {
        return ["Low Pass", "High Pass", "Band Pass"][(v.round() as usize).min(2)].to_string();
    }
    if matches!(info.name, "Amp" | "Cabinet") {
        return format!("{:.0}", v + 1.0);
    }
    if info.name == "Reverse" {
        return if v >= 0.5 { "On".into() } else { "Off".into() };
    }
    knob_text(info, v)
}

/// Draws an arc between two angles (radians, screen space) around `c`.
fn arc(g: &egui::Painter, c: Pos2, r: f32, from: f32, to: f32, color: Color32, width: f32) {
    let n = 28;
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let a = from + (to - from) * i as f32 / n as f32;
        pts.push(c + vec2(a.cos(), a.sin()) * r);
    }
    g.add(Shape::line(pts, Stroke::new(width, color)));
}

/// A panel with the window's inset background and a hairline border.
fn inset(ui: &mut Ui, height: f32, add: impl FnOnce(&egui::Painter, Rect)) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let g = ui.painter_at(rect);
    g.rect_filled(rect, CornerRadius::same(5), INSET);
    g.rect_stroke(
        rect,
        CornerRadius::same(5),
        Stroke::new(1.0, LINE),
        StrokeKind::Inside,
    );
    add(&g, rect.shrink(1.0));
}

fn graph_grid(g: &egui::Painter, rect: Rect, xs: &[f32], ys: &[f32], color: Color32) {
    for x in xs {
        let x = rect.left() + rect.width() * x;
        g.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, color),
        );
    }
    for y in ys {
        let y = rect.top() + rect.height() * y;
        g.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, color),
        );
    }
}

/// Puts an edited effect and the setting moves of its window into the project (and the
/// automation), and closes the window when it was closed.
#[allow(clippy::too_many_arguments)]
fn finish(
    p: &mut Project,
    s: &mut UiState,
    ti: usize,
    slot: usize,
    insert: Insert,
    mut edited: Insert,
    edits: Vec<Edit>,
    open: bool,
) {
    let beat = p.position_beats;
    if edited != insert {
        edited.params = edited.kind.clamp_params(edited.params);
        if ti == crate::mixer::MASTER_STRIP {
            if let Some(i) = p.master_inserts.get_mut(slot) {
                *i = edited;
            }
        } else {
            p.tracks[ti].inserts[slot] = edited;
        }
    }
    if ti == crate::mixer::MASTER_STRIP {
        // No automation lanes on the Stereo Out strip.
        if !open {
            s.mixer.fx_window = None;
        }
        return;
    }
    let playing = p.playing;
    for e in edits {
        let target = AutoTarget::Fx {
            slot: slot as u8,
            param: e.param as u8,
        };
        automation::moved(&mut p.tracks[ti], target, e.value);
        if e.ended {
            automation::released(&mut p.tracks[ti], target, beat, playing);
        }
    }
    if !open {
        s.mixer.fx_window = None;
    }
}

/// What a control of the amplifier shows at `beat`: its lane's value while the strip plays
/// one, else the setting the user made.
fn shown(insert: &Insert, track: &Track, slot: usize, param: usize, beat: f64) -> f32 {
    let info = &insert.kind.params()[param];
    let target = AutoTarget::Fx {
        slot: slot as u8,
        param: param as u8,
    };
    track
        .auto
        .played(target, beat)
        .map(|n| info.from_norm(n))
        .unwrap_or(insert.params[param])
}

/// A setting under a knob, in its own units.
fn knob_text(info: &ParamInfo, v: f32) -> String {
    if info.unit == "dB" {
        format!("{v:+.1} dB")
    } else if info.unit.is_empty() && info.min >= 0.0 && info.max <= 10.0 {
        format!("{v:.1}")
    } else if info.unit.is_empty() {
        format!("{:.0} %", v * 100.0)
    } else {
        format!("{v:.1} {}", info.unit)
    }
}

/// The Amp Designer's window: the faceplate with its six knobs, the amplifier and cabinet
/// choices and the microphone position. All of it is automatable, like the sliders of the
/// other effects.
fn amp_faceplate(
    ui: &mut Ui,
    insert: &mut Insert,
    track: &Track,
    slot: usize,
    beat: f64,
    edits: &mut Vec<Edit>,
) {
    let model = shown(insert, track, slot, 0, beat).round().clamp(0.0, 4.0) as usize;
    let cab = shown(insert, track, slot, 7, beat).round().clamp(0.0, 3.0) as usize;

    // The amplifier itself: dark tolex, a cream piping and a speaker grille on the right.
    let (rect, _) = ui.allocate_exact_size(vec2(AMP_W, AMP_H), Sense::hover());
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 7.0, Color32::from_rgb(0x23, 0x21, 0x1e));
    g.rect_stroke(
        rect,
        7.0,
        Stroke::new(1.0, Color32::from_gray(0x0c)),
        StrokeKind::Inside,
    );
    g.rect_stroke(
        rect.shrink(4.0),
        5.0,
        Stroke::new(1.0, Color32::from_rgb(0x6d, 0x5f, 0x43)),
        StrokeKind::Inside,
    );
    let cream = Color32::from_rgb(0xf1, 0xe7, 0xcd);
    let dim = Color32::from_rgb(0x9c, 0x8f, 0x74);
    g.text(
        pos2(rect.left() + 18.0, rect.top() + 17.0),
        Align2::LEFT_CENTER,
        "PRO WAVE",
        FontId::proportional(10.0),
        Color32::from_rgb(0xc8, 0xb6, 0x8c),
    );
    g.text(
        pos2(rect.left() + 18.0, rect.top() + 39.0),
        Align2::LEFT_CENTER,
        AMP_MODELS[model],
        FontId::proportional(19.0),
        cream,
    );
    g.text(
        pos2(rect.left() + 18.0, rect.top() + 57.0),
        Align2::LEFT_CENTER,
        AMP_CABS[cab],
        FontId::proportional(10.5),
        dim,
    );

    let grille = Rect::from_min_max(
        pos2(rect.right() - 130.0, rect.top() + 12.0),
        pos2(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    let cloth = g.with_clip_rect(grille);
    cloth.rect_filled(grille, 4.0, Color32::from_rgb(0x3a, 0x32, 0x25));
    let mut x = grille.left() - grille.height();
    while x < grille.right() {
        cloth.line_segment(
            [
                pos2(x, grille.bottom()),
                pos2(x + grille.height(), grille.top()),
            ],
            Stroke::new(1.0, Color32::from_rgb(0x47, 0x3d, 0x2d)),
        );
        x += 5.0;
    }
    let c = grille.center();
    g.circle_filled(c, 40.0, Color32::from_rgb(0x1b, 0x18, 0x13));
    g.circle_stroke(
        c,
        40.0,
        Stroke::new(2.0, Color32::from_rgb(0x5c, 0x50, 0x3a)),
    );
    g.circle_stroke(
        c,
        27.0,
        Stroke::new(1.0, Color32::from_rgb(0x35, 0x2f, 0x24)),
    );
    g.circle_filled(c, 11.0, Color32::from_rgb(0x2a, 0x25, 0x1d));

    // The six knobs: Gain, Bass, Mid, Treble, Presence, Master.
    const KNOBS: [(usize, &str); 6] = [
        (1, "Gain"),
        (2, "Bass"),
        (3, "Mid"),
        (4, "Treble"),
        (5, "Presence"),
        (6, "Master"),
    ];
    for (i, (param, label)) in KNOBS.iter().enumerate() {
        let info = &insert.kind.params()[*param];
        let target = AutoTarget::Fx {
            slot: slot as u8,
            param: *param as u8,
        };
        let played = track.auto.played(target, beat);
        let v = shown(insert, track, slot, *param, beat);
        let kc = pos2(rect.left() + 42.0 + i as f32 * 54.0, rect.top() + 104.0);
        g.text(
            pos2(kc.x, rect.top() + 76.0),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(9.5),
            dim,
        );
        g.text(
            pos2(kc.x, rect.top() + 130.0),
            Align2::CENTER_CENTER,
            knob_text(info, v),
            FontId::proportional(9.5),
            cream,
        );
        crate::widgets::draw_knob(&g, kc, 13.0, info.to_norm(v) * 2.0 - 1.0);
        if played.is_none() && automation::editable(track, target) {
            let r = Rect::from_center_size(kc, vec2(32.0, 32.0));
            let resp = ui.interact(
                r,
                ui.scope_id().with(("amp_knob", slot, param)),
                Sense::click_and_drag(),
            );
            if resp.dragged() {
                let n = info.to_norm(v) - resp.drag_delta().y * 0.006;
                edits.push(Edit {
                    param: *param,
                    value: info.from_norm(n),
                    ended: false,
                });
            }
            if resp.drag_stopped() {
                edits.push(Edit {
                    param: *param,
                    value: v,
                    ended: true,
                });
            }
            if resp.double_clicked() {
                edits.push(Edit {
                    param: *param,
                    value: info.default,
                    ended: true,
                });
            }
        }
    }

    // The amplifier and cabinet choices, and where the microphone stands.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let choices = |ui: &mut Ui,
                       param: usize,
                       label: &str,
                       names: &[&'static str],
                       current: usize,
                       edits: &mut Vec<Edit>| {
            ui.label(label);
            let target = AutoTarget::Fx {
                slot: slot as u8,
                param: param as u8,
            };
            ui.add_enabled_ui(automation::editable(track, target), |ui| {
                egui::ComboBox::from_id_salt(("amp_choice", slot, param))
                    .width(140.0)
                    .selected_text(names[current])
                    .show_ui(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            if ui.selectable_label(i == current, *name).clicked() && i != current {
                                edits.push(Edit {
                                    param,
                                    value: i as f32,
                                    ended: true,
                                });
                            }
                        }
                    });
            });
        };
        choices(ui, 0, tr("Amp"), &AMP_MODELS, model, edits);
        ui.add_space(10.0);
        choices(ui, 7, tr("Cabinet"), &AMP_CABS, cab, edits);
        ui.add_space(10.0);
        ui.label(tr("Mic"));
        let target = AutoTarget::Fx {
            slot: slot as u8,
            param: 8,
        };
        let mut mic = shown(insert, track, slot, 8, beat);
        let resp = ui.add_enabled(
            automation::editable(track, target),
            egui::Slider::new(&mut mic, 0.0..=1.0)
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
        );
        if resp.changed() {
            edits.push(Edit {
                param: 8,
                value: mic,
                ended: !resp.dragged(),
            });
        } else if resp.drag_stopped() {
            edits.push(Edit {
                param: 8,
                value: mic,
                ended: true,
            });
        }
        resp.on_hover_text(tr("From the edge of the cone to its centre"));
    });
}

/// The EQ's gain over 20 Hz to 20 kHz, ±18 dB, with a node for each band.
fn eq_curve(
    ui: &mut Ui,
    kind: FxKind,
    params: &[f32; prowave_engine::MAX_FX_PARAMS],
    accent: Color32,
) {
    inset(ui, 150.0, |g, rect| {
        let x_of = |hz: f32| rect.left() + (hz / 20.0).log10() / 3.0 * rect.width();
        let y_of = |db: f32| rect.center().y - db / 18.0 * rect.height() / 2.0;
        graph_grid(
            g,
            rect,
            &[
                x_of(50.0),
                x_of(100.0),
                x_of(200.0),
                x_of(500.0),
                x_of(1_000.0),
                x_of(2_000.0),
                x_of(5_000.0),
                x_of(10_000.0),
            ]
            .iter()
            .map(|x| (x - rect.left()) / rect.width())
            .collect::<Vec<_>>(),
            &[0.1667, 0.3333, 0.5, 0.6667, 0.8333],
            Color32::from_gray(0x25),
        );
        // The 0 dB line is brighter.
        g.line_segment(
            [pos2(rect.left(), y_of(0.0)), pos2(rect.right(), y_of(0.0))],
            Stroke::new(1.0, Color32::from_gray(0x44)),
        );
        let n = 180;
        let freqs: Vec<f32> = (0..=n)
            .map(|i| 20.0 * 1000f32.powf(i as f32 / n as f32))
            .collect();
        let curve = prowave_engine::response_db(kind, *params, &freqs, CURVE_RATE);
        let pts: Vec<_> = freqs
            .iter()
            .zip(&curve)
            .map(|(f, db)| pos2(x_of(*f), y_of(db.clamp(-18.0, 18.0))))
            .collect();
        // A soft fill under the curve, then the line itself.
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            let mut shape = pts.clone();
            shape.push(pos2(last.x, y_of(0.0)));
            shape.push(pos2(first.x, y_of(0.0)));
            g.add(Shape::convex_polygon(
                shape,
                accent.gamma_multiply(0.10),
                Stroke::NONE,
            ));
        }
        g.add(Shape::line(pts, Stroke::new(1.8, accent)));
        let (low, mid, high) = if kind == FxKind::ChannelEq {
            (params[0], params[1], params[4])
        } else {
            (params[0], params[6], params[12])
        };
        let mid_f = if kind == FxKind::ChannelEq {
            params[2]
        } else {
            1_000.0
        };
        for (hz, db) in [(70.0, low), (mid_f, mid), (11_000.0, high)] {
            let c = pos2(x_of(hz), y_of(db.clamp(-18.0, 18.0)));
            g.circle_filled(c, 4.5, accent);
            g.circle_stroke(c, 4.5, Stroke::new(1.2, Color32::from_black_alpha(180)));
        }
        g.text(
            pos2(rect.left() + 6.0, rect.top() + 8.0),
            Align2::LEFT_TOP,
            "20 Hz",
            FontId::monospace(9.0),
            FAINT,
        );
        g.text(
            pos2(rect.right() - 6.0, rect.top() + 8.0),
            Align2::RIGHT_TOP,
            "20 kHz",
            FontId::monospace(9.0),
            FAINT,
        );
        g.text(
            pos2(rect.right() - 6.0, rect.bottom() - 6.0),
            Align2::RIGHT_BOTTOM,
            "±18 dB",
            FontId::monospace(9.0),
            FAINT,
        );
    });
}

/// Compressor, gate and limiter: the static transfer curve plus live gain reduction.
fn transfer_curve(
    ui: &mut Ui,
    kind: FxKind,
    params: &[f32; prowave_engine::MAX_FX_PARAMS],
    reduction: f32,
    accent: Color32,
) {
    inset(ui, 150.0, |g, rect| {
        let plot = Rect::from_min_max(
            pos2(rect.left() + 10.0, rect.top() + 12.0),
            pos2(rect.right() - 58.0, rect.bottom() - 12.0),
        );
        graph_grid(
            g,
            plot,
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            Color32::from_gray(0x25),
        );
        // The unity diagonal, faint.
        g.line_segment(
            [plot.left_bottom(), plot.right_top()],
            Stroke::new(1.0, Color32::from_gray(0x38)),
        );
        let db_x = |x: f32| plot.left() + x.clamp(-60.0, 0.0) / 60.0 * plot.width();
        let db_y = |y: f32| plot.bottom() + (y.clamp(-60.0, 0.0) / 60.0) * plot.height();
        let curve: Vec<Pos2> = (0..=120)
            .map(|i| {
                let x = -60.0 + 60.0 * i as f32 / 120.0;
                let y = match kind {
                    FxKind::Compressor => {
                        let threshold = params[0];
                        let ratio = params[1].max(1.0);
                        let makeup = params[4];
                        let y = if x <= threshold {
                            x
                        } else {
                            threshold + (x - threshold) / ratio
                        };
                        y + makeup
                    }
                    FxKind::Gate => {
                        let threshold = params[0];
                        let range = params[1];
                        if x >= threshold {
                            x
                        } else {
                            x + range
                        }
                    }
                    _ => (x + params[0]).min(params[1]),
                };
                pos2(db_x(x), db_y(y))
            })
            .collect();
        g.add(Shape::line(curve, Stroke::new(2.0, accent)));
        // The live gain-reduction meter on the right.
        let meter = Rect::from_min_max(
            pos2(rect.right() - 34.0, plot.top()),
            pos2(rect.right() - 18.0, plot.bottom()),
        );
        g.rect_filled(meter, CornerRadius::same(2), Color32::from_gray(0x0c));
        let amount = reduction.clamp(0.0, 24.0) / 24.0;
        let fill = Rect::from_min_max(
            pos2(meter.left(), meter.top()),
            pos2(meter.right(), meter.top() + meter.height() * amount),
        );
        g.rect_filled(fill, CornerRadius::same(2), accent);
        g.text(
            pos2(rect.left() + 12.0, rect.bottom() - 6.0),
            Align2::LEFT_BOTTOM,
            trf(
                "Gain Reduction: {} dB",
                &[&format!("{:.1}", reduction.clamp(0.0, 24.0))],
            ),
            FontId::monospace(8.5),
            accent,
        );
        g.text(
            pos2(plot.left(), plot.top() - 10.0),
            Align2::LEFT_BOTTOM,
            "IN",
            FontId::monospace(8.5),
            FAINT,
        );
        g.text(
            pos2(plot.right(), plot.top() - 10.0),
            Align2::RIGHT_BOTTOM,
            "OUT",
            FontId::monospace(8.5),
            FAINT,
        );
    });
}

/// The three-band compressor: where the bands split and how each one is set.
fn multicomp(
    ui: &mut Ui,
    params: &[f32; prowave_engine::MAX_FX_PARAMS],
    reduction: f32,
    accent: Color32,
) {
    inset(ui, 150.0, |g, rect| {
        let plot = rect.shrink2(vec2(12.0, 18.0));
        let low_mid = params[3].clamp(20.0, 20_000.0);
        let mid_high = params[7].clamp(low_mid, 20_000.0);
        let x = |hz: f32| plot.left() + (hz / 20.0).log10() / 3.0 * plot.width();
        let bands = [
            (plot.left(), x(low_mid), "LOW", params[0], params[1]),
            (x(low_mid), x(mid_high), "MID", params[4], params[5]),
            (x(mid_high), plot.right(), "HIGH", params[8], params[9]),
        ];
        for (a, b, name, threshold, ratio) in bands {
            let band = Rect::from_x_y_ranges(a..=b, plot.top()..=plot.bottom());
            g.rect_filled(
                band.shrink(2.0),
                CornerRadius::same(4),
                accent.gamma_multiply(0.08),
            );
            g.rect_stroke(
                band.shrink(2.0),
                CornerRadius::same(4),
                Stroke::new(1.0, accent.gamma_multiply(0.35)),
                StrokeKind::Inside,
            );
            g.text(
                band.center() - vec2(0.0, 12.0),
                Align2::CENTER_CENTER,
                name,
                FontId::monospace(10.0),
                accent,
            );
            g.text(
                band.center() + vec2(0.0, 6.0),
                Align2::CENTER_CENTER,
                format!("{:.0} dB  {:.1}:1", threshold, ratio),
                FontId::monospace(8.5),
                DIM,
            );
        }
        for crossover in [low_mid, mid_high] {
            let cx = x(crossover);
            g.line_segment(
                [pos2(cx, plot.top()), pos2(cx, plot.bottom())],
                Stroke::new(1.0, Color32::from_gray(0x4a)),
            );
            g.text(
                pos2(cx, plot.top() - 4.0),
                Align2::CENTER_BOTTOM,
                format!("{crossover:.0} Hz"),
                FontId::monospace(8.0),
                FAINT,
            );
        }
        let meter = Rect::from_min_max(
            pos2(rect.right() - 16.0, plot.top()),
            pos2(rect.right() - 6.0, plot.bottom()),
        );
        g.rect_filled(meter, CornerRadius::same(2), Color32::from_gray(0x0c));
        let amount = reduction.clamp(0.0, 24.0) / 24.0;
        g.rect_filled(
            Rect::from_min_max(
                pos2(meter.left(), meter.top()),
                pos2(meter.right(), meter.top() + meter.height() * amount),
            ),
            CornerRadius::same(2),
            accent,
        );
    });
}

/// The delay line: taps at multiples of the delay, with feedback fading them out.
fn delay_panel(ui: &mut Ui, params: &[f32; prowave_engine::MAX_FX_PARAMS], accent: Color32) {
    inset(ui, 150.0, |g, rect| {
        let time = params[0].max(1.0);
        let feedback = params[1].clamp(0.0, 0.95);
        let ping = params[3] >= 0.5;
        let mix = params[4].clamp(0.0, 1.0);
        let mid = rect.center().y;
        g.line_segment(
            [
                pos2(rect.left() + 16.0, mid),
                pos2(rect.right() - 16.0, mid),
            ],
            Stroke::new(1.0, Color32::from_gray(0x3a)),
        );
        g.text(
            pos2(rect.left() + 16.0, mid - 10.0),
            Align2::LEFT_BOTTOM,
            "dry",
            FontId::monospace(8.5),
            DIM,
        );
        let span = rect.width() - 52.0;
        let last = 4.0f32;
        for n in 1..=4 {
            let frac = (time * n as f32 / 1000.0 / (time * last / 1000.0)).min(1.0);
            let x = rect.left() + 26.0 + span * frac;
            let level = feedback.powi(n - 1) * mix.max(0.1);
            let y = if ping && n % 2 == 0 {
                mid - 26.0
            } else {
                mid + 12.0
            };
            let alpha = (40.0 + 180.0 * level).min(220.0) as u8;
            g.circle_filled(pos2(x, y), 5.5, accent.gamma_multiply(alpha as f32 / 255.0));
            g.line_segment(
                [pos2(x, mid), pos2(x, y)],
                Stroke::new(1.0, accent.gamma_multiply(0.25)),
            );
            g.text(
                pos2(x, y + 9.0),
                Align2::CENTER_TOP,
                format!("{:.0}", time * n as f32),
                FontId::monospace(8.0),
                DIM,
            );
        }
        g.text(
            pos2(rect.right() - 16.0, mid - 10.0),
            Align2::RIGHT_BOTTOM,
            format!("{:.0} ms · fb {:.0}%", time, feedback * 100.0),
            FontId::monospace(8.5),
            DIM,
        );
    });
}

/// Chorus: two slow LFOs, one per side.
fn chorus_panel(ui: &mut Ui, params: &[f32; prowave_engine::MAX_FX_PARAMS], accent: Color32) {
    inset(ui, 150.0, |g, rect| {
        let rate = params[0].max(0.01);
        let depth = params[1].clamp(0.0, 8.0) / 8.0;
        let mid = rect.center().y;
        let amp = rect.height() * 0.3 * depth;
        for (offset, alpha) in [(0.0f32, 0.9f32), (std::f32::consts::PI, 0.55f32)] {
            let pts: Vec<Pos2> = (0..=140)
                .map(|i| {
                    let t = i as f32 / 140.0;
                    let a = t * rate * std::f32::consts::TAU + offset;
                    pos2(
                        rect.left() + 10.0 + t * (rect.width() - 20.0),
                        mid + a.sin() * amp,
                    )
                })
                .collect();
            g.add(Shape::line(
                pts,
                Stroke::new(1.6, accent.gamma_multiply(alpha)),
            ));
        }
        g.line_segment(
            [
                pos2(rect.left() + 10.0, mid),
                pos2(rect.right() - 10.0, mid),
            ],
            Stroke::new(1.0, Color32::from_gray(0x30)),
        );
        g.text(
            pos2(rect.left() + 12.0, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            format!("{:.2} Hz · depth {:.1} ms", rate, params[1]),
            FontId::monospace(8.5),
            DIM,
        );
    });
}

/// Phaser: the notches sweep with the LFO; `beat` drives the animation.
fn phaser_panel(
    ui: &mut Ui,
    params: &[f32; prowave_engine::MAX_FX_PARAMS],
    beat: f64,
    accent: Color32,
) {
    inset(ui, 150.0, |g, rect| {
        let rate = params[0].max(0.01);
        let depth = params[1].clamp(0.0, 1.0);
        let phase = ((beat * f64::from(rate)) as f32).fract() * std::f32::consts::TAU;
        let mid = rect.center().y;
        let pts: Vec<Pos2> = (0..=160)
            .map(|i| {
                let t = i as f32 / 160.0;
                let hz = 20.0 * 1000f32.powf(t);
                let mut gain = 0.0f32;
                for notch in 0..4 {
                    let center = 180.0 * 2.4f32.powi(notch) * (1.0 + 0.35 * depth * phase.sin());
                    let d = (hz / center.max(1.0)).ln().abs();
                    gain -= 14.0 * depth * (-(d * d) / 0.08).exp();
                }
                pos2(
                    rect.left() + 10.0 + t * (rect.width() - 20.0),
                    mid - gain / 28.0 * rect.height(),
                )
            })
            .collect();
        g.add(Shape::line(pts, Stroke::new(1.8, accent)));
        g.text(
            pos2(rect.left() + 12.0, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            format!("{:.2} Hz · feedback {:.0}%", rate, params[2] * 100.0),
            FontId::monospace(8.5),
            DIM,
        );
    });
}

/// Overdrive: the waveshaper's transfer curve, from the drive and tone.
fn drive_panel(ui: &mut Ui, params: &[f32; prowave_engine::MAX_FX_PARAMS], accent: Color32) {
    inset(ui, 150.0, |g, rect| {
        let plot = rect.shrink2(vec2(24.0, 14.0));
        graph_grid(
            g,
            plot,
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            Color32::from_gray(0x25),
        );
        g.line_segment(
            [plot.left_bottom(), plot.right_top()],
            Stroke::new(1.0, Color32::from_gray(0x38)),
        );
        let drive = 10f32.powf(params[0].clamp(0.0, 40.0) / 20.0);
        let norm = drive.tanh().max(1e-3);
        let pts: Vec<Pos2> = (0..=140)
            .map(|i| {
                let x = -1.0 + 2.0 * i as f32 / 140.0;
                let y = (x * drive).tanh() / norm;
                pos2(
                    plot.center().x + x * plot.width() / 2.0,
                    plot.center().y - y * plot.height() / 2.0,
                )
            })
            .collect();
        g.add(Shape::line(pts, Stroke::new(2.0, accent)));
        g.text(
            pos2(rect.left() + 12.0, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            format!("drive {:.0} dB · tone {:.0} Hz", params[0], params[1]),
            FontId::monospace(8.5),
            DIM,
        );
    });
}

/// Filter: the magnitude response of the selected mode, with a marker at the cutoff.
fn filter_panel(ui: &mut Ui, params: &[f32; prowave_engine::MAX_FX_PARAMS], accent: Color32) {
    inset(ui, 150.0, |g, rect| {
        let plot = rect.shrink2(vec2(10.0, 12.0));
        let cutoff = params[0].clamp(20.0, 20_000.0);
        let resonance = params[1].clamp(0.5, 10.0);
        let mode = params[2].round().clamp(0.0, 2.0) as usize;
        graph_grid(
            g,
            plot,
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            Color32::from_gray(0x25),
        );
        let x_of = |hz: f32| plot.left() + (hz / 20.0).log10() / 3.0 * plot.width();
        let pts: Vec<Pos2> = (0..=160)
            .map(|i| {
                let hz = 20.0 * 1000f32.powf(i as f32 / 160.0);
                let x = hz / cutoff;
                let (mut db, peak) = match mode {
                    0 => (-10.0 * (1.0 + x.powi(4)).log10(), 12.0),
                    1 => (-10.0 * (1.0 + (1.0 / x).powi(4)).log10(), 12.0),
                    _ => (-10.0 * (1.0 + ((x - 1.0 / x) * 2.0).powi(2)).log10(), 9.0),
                };
                db += peak * (resonance - 0.5) / 9.5 * (-((hz / cutoff).ln() * 4.0).powi(2)).exp();
                let y = plot.center().y - db.clamp(-36.0, 18.0) / 36.0 * plot.height() / 2.0;
                pos2(x_of(hz), y)
            })
            .collect();
        g.add(Shape::line(pts, Stroke::new(2.0, accent)));
        let cx = x_of(cutoff);
        g.line_segment(
            [pos2(cx, plot.top()), pos2(cx, plot.bottom())],
            Stroke::new(1.0, accent.gamma_multiply(0.35)),
        );
        let mode_name = ["Low Pass", "High Pass", "Band Pass"][mode];
        g.text(
            pos2(rect.left() + 12.0, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            format!("{mode_name} · {cutoff:.0} Hz · Q {resonance:.1}"),
            FontId::monospace(8.5),
            DIM,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Insert;

    #[test]
    fn the_window_edits_the_insert_and_closes_with_it() {
        let mut p = Project::empty();
        p.tracks[0].inserts = vec![
            Insert::new(FxKind::Compressor),
            Insert::new(FxKind::ChannelEq),
            Insert::new(FxKind::MultiEq),
            Insert::new(FxKind::Limiter),
            Insert::new(FxKind::Gate),
        ];
        let mut s = UiState::new();
        let meters = Meters::default();
        let mut h = crate::ui_test::Harness::new(800.0, 700.0);
        for slot in 0..5 {
            s.mixer.fx_window = Some((0, slot));
            for _ in 0..3 {
                h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
            }
            assert_eq!(s.mixer.fx_window, Some((0, slot)));
        }
        // Nothing was changed by looking.
        assert_eq!(p.tracks[0].inserts[0], Insert::new(FxKind::Compressor));
        assert_eq!(p.tracks[0].inserts[2], Insert::new(FxKind::MultiEq));
        // The insert goes away: the window closes.
        p.tracks[0].inserts.clear();
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        assert_eq!(s.mixer.fx_window, None);
    }

    #[test]
    fn the_space_designer_window_draws_and_changes_nothing_by_looking() {
        let mut p = Project::empty();
        p.tracks[0].inserts = vec![Insert::new(FxKind::Reverb)];
        let mut s = UiState::new();
        s.mixer.fx_window = Some((0, 0));
        let meters = Meters::default();
        let mut h = crate::ui_test::Harness::new(1000.0, 800.0);
        for _ in 0..4 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        }
        assert_eq!(s.mixer.fx_window, Some((0, 0)));
        assert_eq!(p.tracks[0].inserts[0], Insert::new(FxKind::Reverb));
    }

    #[test]
    fn the_stereo_out_effect_window_opens_edits_and_closes() {
        let mut p = Project::empty();
        p.master_inserts = vec![Insert::new(FxKind::Reverb), Insert::new(FxKind::Compressor)];
        let mut s = UiState::new();
        let meters = Meters::default();
        let mut h = crate::ui_test::Harness::new(1000.0, 800.0);
        for slot in 0..2 {
            s.mixer.fx_window = Some((crate::mixer::MASTER_STRIP, slot));
            for _ in 0..3 {
                h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
            }
            assert_eq!(s.mixer.fx_window, Some((crate::mixer::MASTER_STRIP, slot)));
        }
        assert_eq!(
            p.master_inserts[0],
            Insert::new(FxKind::Reverb),
            "looking changes nothing"
        );
        // The effect goes away: the window closes.
        p.master_inserts.clear();
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        assert_eq!(s.mixer.fx_window, None);
    }

    #[test]
    fn the_amp_designer_window_draws_and_locks_its_automated_knobs() {
        use crate::model::AutoMode;
        let mut p = Project::empty();
        p.tracks[0].inserts = vec![Insert::new(FxKind::Amp)];
        let mut s = UiState::new();
        s.mixer.fx_window = Some((0, 0));
        let meters = Meters::default();
        let mut h = crate::ui_test::Harness::new(900.0, 760.0);
        for _ in 0..4 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        }
        // Looking at the faceplate changes nothing.
        assert_eq!(s.mixer.fx_window, Some((0, 0)));
        assert_eq!(p.tracks[0].inserts[0], Insert::new(FxKind::Amp));
        // A played Gain lane locks the knob in Read mode, as the sliders are locked.
        let gain = AutoTarget::Fx { slot: 0, param: 1 };
        p.tracks[0].auto.mode = AutoMode::Read;
        p.tracks[0].auto.set_point(gain, 0.0, 0.25);
        assert!(!automation::editable(&p.tracks[0], gain));
        for _ in 0..4 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        }
        assert_eq!(p.tracks[0].inserts[0].params[1], 12.0);
        assert_eq!(p.tracks[0].auto.lane(gain).len(), 1);
    }

    #[test]
    fn a_played_lane_moves_the_slider_and_locks_it_in_read_mode() {
        use crate::model::AutoMode;
        let mut p = Project::empty();
        p.tracks[0].inserts = vec![Insert::new(FxKind::Compressor)];
        let thr = AutoTarget::Fx { slot: 0, param: 0 };
        p.tracks[0].auto.mode = AutoMode::Read;
        p.tracks[0].auto.set_point(thr, 0.0, 0.25);
        let mut s = UiState::new();
        s.mixer.fx_window = Some((0, 0));
        let meters = Meters::default();
        let mut h = crate::ui_test::Harness::new(800.0, 600.0);
        for _ in 0..4 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s, &meters));
        }
        // Looking at a played lane changes neither the setting nor the lane.
        assert_eq!(p.tracks[0].inserts[0].params[0], -18.0);
        assert_eq!(p.tracks[0].auto.lane(thr).len(), 1);
        assert_eq!(p.tracks[0].auto.held, None);
        assert!(!automation::editable(&p.tracks[0], thr));
    }
}
