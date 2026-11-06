//! The window of Space D, Logic Pro's Space Designer: a light grey plug-in window with a dark
//! top bar (power, preset, Compare, Copy, Paste, Undo, Redo, View), the Sampled IR and
//! Synthesized IR tabs, the impulse response drawn with its Volume Envelope and three handles
//! to drag, and the orange controls below (Input, Predelay, Length, Size, X-Over, Lo and Hi
//! Spread, Dry and Wet). Every control is an effect setting, so it is automatable.
use crate::fx_ui::Edit;
use crate::i18n::tr;
use crate::model::{AutoTarget, Insert, Track};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke, StrokeKind, Ui};
use prowave_engine::{sd, FxKind, IR_NAMES, MAX_FX_PARAMS};

const BASE_W: f32 = 760.0;
const TOP_H: f32 = 44.0;
const TAB_H: f32 = 30.0;
const SUB_H: f32 = 34.0;
const DISPLAY_H: f32 = 230.0;
const READOUT_H: f32 = 30.0;
const OPTIONS_H: f32 = 34.0;
const PANEL_H: f32 = 190.0;
const FOOT_H: f32 = 36.0;

const PANEL: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
const BAR: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
const TAB_OFF: Color32 = Color32::from_rgb(0xc0, 0xc0, 0xc0);
const ORANGE: Color32 = Color32::from_rgb(0xff, 0x6a, 0x30);
const ORANGE_LIGHT: Color32 = Color32::from_rgb(0xff, 0x8a, 0x50);
const BLUE: Color32 = Color32::from_rgb(0x2f, 0x7f, 0xe0);
const WAVE: Color32 = Color32::from_rgb(0x4a, 0x6a, 0x8a);
const CURVE: Color32 = Color32::from_rgb(0x7a, 0xc0, 0xe0);
const INK: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);
const TEXT_DIM: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);

/// The drawn response: what it was made for, its min/max bins and its length in seconds.
/// Moves a setting by hand: the insert, the moves so far, which setting, its value, and whether
/// the gesture ended.
type SetFn<'a> = dyn FnMut(&mut Insert, &mut Vec<Edit>, usize, f32, bool) + 'a;

type Preview = (Vec<u32>, Vec<(f32, f32)>, f32);

/// What the window remembers between frames.
#[derive(Clone, Default)]
struct Memory {
    /// Settings when the window opened (what Compare goes back to).
    original: Option<[f32; MAX_FX_PARAMS]>,
    /// Compare is on: the settings that were being edited wait here.
    stashed: Option<[f32; MAX_FX_PARAMS]>,
    undo: Vec<[f32; MAX_FX_PARAMS]>,
    redo: Vec<[f32; MAX_FX_PARAMS]>,
    /// View: window size in percent (100 when unset).
    view: u32,
    link: bool,
    /// The drawn response and what it was made for.
    preview: Option<Preview>,
}

/// The Copy and Paste clipboard, for every Space Designer window.
fn clipboard() -> &'static std::sync::Mutex<Option<[f32; MAX_FX_PARAMS]>> {
    static CLIP: std::sync::OnceLock<std::sync::Mutex<Option<[f32; MAX_FX_PARAMS]>>> =
        std::sync::OnceLock::new();
    CLIP.get_or_init(|| std::sync::Mutex::new(None))
}

/// The settings a preset of the Sampled IR tab sets: the response and its natural length.
fn preset_params(base: &[f32; MAX_FX_PARAMS], index: usize) -> [f32; MAX_FX_PARAMS] {
    let mut p = *base;
    let defaults = FxKind::Reverb.defaults();
    p[sd::IR_TYPE] = 0.0;
    p[sd::IR_INDEX] = index as f32;
    // The preset's own length and shape, the way Logic's presets set them.
    let secs = [3.0, 1.5, 0.8, 6.0, 2.0, 0.4][index.min(5)];
    p[sd::LENGTH] = secs;
    p[sd::DECAY] = secs;
    p[sd::SIZE] = defaults[sd::SIZE];
    p[sd::INIT_LEVEL] = defaults[sd::INIT_LEVEL];
    p[sd::ATTACK] = defaults[sd::ATTACK].min(secs * 1000.0 * 0.05);
    p[sd::END_LEVEL] = defaults[sd::END_LEVEL];
    p[sd::IR_OFFSET] = 0.0;
    p[sd::REVERSE] = 0.0;
    p
}

fn value_of(insert: &Insert, track: &Track, slot: usize, param: usize, beat: f64) -> f32 {
    let info = &insert.kind.params()[param];
    track
        .auto
        .played(
            AutoTarget::Fx {
                slot: slot as u8,
                param: param as u8,
            },
            beat,
        )
        .map(|n| info.from_norm(n))
        .unwrap_or(insert.params[param])
}

/// Draws the window. Returns the insert as edited, the setting moves (for automation), and
/// whether the window is still open.
pub(crate) fn window(
    ctx: &egui::Context,
    insert: &Insert,
    track: &Track,
    slot: usize,
    beat: f64,
    id: egui::Id,
) -> (Insert, Vec<Edit>, bool) {
    let mut open = true;
    let mut edited = *insert;
    let mut edits: Vec<Edit> = Vec::new();
    let mut mem: Memory = ctx.data_mut(|d| d.get_temp(id).unwrap_or_default());
    if mem.view == 0 {
        mem.view = 100;
    }
    if mem.original.is_none() {
        mem.original = Some(insert.params);
    }
    let k = mem.view as f32 / 100.0;
    let frame = egui::Frame::window(&ctx.global_style()).fill(PANEL);
    egui::Window::new("Space D")
        .id(id.with("window"))
        .open(&mut open)
        .resizable(false)
        .frame(frame)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.set_width(BASE_W * k);
            body(ui, &mut edited, &mut edits, &mut mem, track, slot, beat, k);
        });
    // Undo and Redo: a gesture is one step.
    if edited.params != insert.params {
        let (pressed, down) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.any_down()));
        if pressed || !down {
            if mem.undo.last() != Some(&insert.params) {
                mem.undo.push(insert.params);
                if mem.undo.len() > 50 {
                    mem.undo.remove(0);
                }
            }
            mem.redo.clear();
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, mem));
    (edited, edits, open)
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut Ui,
    edited: &mut Insert,
    edits: &mut Vec<Edit>,
    mem: &mut Memory,
    track: &Track,
    slot: usize,
    beat: f64,
    k: f32,
) {
    let w = BASE_W * k;
    let params = edited.kind.params();
    let shown = |e: &Insert, p: usize| value_of(e, track, slot, p, beat);
    let editable = |p: usize| {
        crate::automation::editable(
            track,
            AutoTarget::Fx {
                slot: slot as u8,
                param: p as u8,
            },
        ) && track
            .auto
            .played(
                AutoTarget::Fx {
                    slot: slot as u8,
                    param: p as u8,
                },
                beat,
            )
            .is_none()
    };
    // A setting moved by hand: kept in range and reported for automation.
    let mut set = |e: &mut Insert, edits: &mut Vec<Edit>, p: usize, v: f32, ended: bool| {
        let info = &params[p];
        let v = v.clamp(info.min, info.max);
        e.params[p] = v;
        edits.push(Edit {
            param: p,
            value: v,
            ended,
        });
    };

    // The whole window is one allocation; each section is a slice of it, so the controls
    // placed by hand inside cannot push the next section around.
    let total = TOP_H + TAB_H + SUB_H + DISPLAY_H + READOUT_H + OPTIONS_H + PANEL_H + FOOT_H;
    let (whole, _) = ui.allocate_exact_size(vec2(w, total * k), Sense::hover());
    let mut y_next = whole.top();
    let mut next = |h: f32| {
        let r = Rect::from_min_size(pos2(whole.left(), y_next), vec2(w, h * k));
        y_next += h * k;
        r
    };
    // ---- Top bar -----------------------------------------------------------------------
    let top = next(TOP_H);
    let g = ui.painter_at(top);
    g.rect_filled(top, 0.0, BAR);
    // Power: blue when the effect is on.
    let power = Rect::from_center_size(
        pos2(top.left() + 26.0 * k, top.top() + 22.0 * k),
        vec2(30.0 * k, 30.0 * k),
    );
    let pr = ui.interact(power, ui.scope_id().with("sd_power"), Sense::click());
    g.circle_filled(
        power.center(),
        14.0 * k,
        if edited.bypass {
            Color32::from_gray(0x55)
        } else {
            BLUE
        },
    );
    g.circle_stroke(
        power.center(),
        8.0 * k,
        Stroke::new(2.0 * k, Color32::WHITE),
    );
    g.line_segment(
        [
            pos2(power.center().x, power.center().y - 11.0 * k),
            pos2(power.center().x, power.center().y - 2.0 * k),
        ],
        Stroke::new(
            2.0 * k,
            BLUE.gamma_multiply(if edited.bypass { 0.0 } else { 1.0 }),
        ),
    );
    g.line_segment(
        [
            pos2(power.center().x, power.center().y - 9.0 * k),
            pos2(power.center().x, power.center().y - 2.0 * k),
        ],
        Stroke::new(2.0 * k, Color32::WHITE),
    );
    if pr.clicked() {
        edited.bypass = !edited.bypass;
    }
    pr.on_hover_text(tr("Bypass"));

    // Preset menu (the black field with the response's name).
    let index = (edited.params[sd::IR_INDEX].round().max(0.0) as usize).min(IR_NAMES.len() - 1);
    let preset_rect = Rect::from_min_size(
        pos2(top.left() + 52.0 * k, top.top() + 4.0 * k),
        vec2(236.0 * k, 18.0 * k),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(preset_rect), |ui| {
        let name = if edited.params[sd::IR_TYPE] >= 0.5 {
            tr("Synthesized IR").to_string()
        } else {
            IR_NAMES[index].to_string()
        };
        egui::ComboBox::from_id_salt("sd_preset")
            .selected_text(
                egui::RichText::new(name)
                    .size(11.5 * k)
                    .color(Color32::WHITE),
            )
            .width(236.0 * k)
            .show_ui(ui, |ui| {
                for (i, n) in IR_NAMES.iter().enumerate() {
                    if ui
                        .selectable_label(i == index && edited.params[sd::IR_TYPE] < 0.5, *n)
                        .clicked()
                    {
                        edited.params = preset_params(&edited.params, i);
                        for p in 0..params.len() {
                            edits.push(Edit {
                                param: p,
                                value: edited.params[p],
                                ended: true,
                            });
                        }
                    }
                }
            });
    });

    // Previous / next preset, Compare, Copy, Paste, Undo, Redo.
    let button = |ui: &mut Ui, x: f32, wd: f32, label: &str, on: bool, enabled: bool| {
        let r = Rect::from_min_size(
            pos2(top.left() + x * k, top.top() + 24.0 * k),
            vec2(wd * k, 17.0 * k),
        );
        let resp = ui.interact(r, ui.scope_id().with(("sd_btn", label)), Sense::click());
        let g = ui.painter_at(top);
        g.rect_filled(
            r,
            3.0 * k,
            if on {
                Color32::from_gray(0x70)
            } else {
                Color32::from_gray(0x44)
            },
        );
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.5 * k),
            if enabled {
                Color32::from_gray(0xe6)
            } else {
                Color32::from_gray(0x80)
            },
        );
        resp.clicked() && enabled
    };
    if button(ui, 52.0, 22.0, "◀", false, true) {
        let i = (index + IR_NAMES.len() - 1) % IR_NAMES.len();
        edited.params = preset_params(&edited.params, i);
        edits.extend((0..params.len()).map(|p| Edit {
            param: p,
            value: edited.params[p],
            ended: true,
        }));
    }
    if button(ui, 78.0, 22.0, "▶", false, true) {
        let i = (index + 1) % IR_NAMES.len();
        edited.params = preset_params(&edited.params, i);
        edits.extend((0..params.len()).map(|p| Edit {
            param: p,
            value: edited.params[p],
            ended: true,
        }));
    }
    let comparing = mem.stashed.is_some();
    if button(ui, 108.0, 54.0, tr("Compare"), comparing, true) {
        match mem.stashed.take() {
            Some(back) => edited.params = back,
            None => {
                mem.stashed = Some(edited.params);
                if let Some(o) = mem.original {
                    edited.params = o;
                }
            }
        }
        edits.extend((0..params.len()).map(|p| Edit {
            param: p,
            value: edited.params[p],
            ended: true,
        }));
    }
    if button(ui, 166.0, 40.0, tr("Copy"), false, true) {
        if let Ok(mut c) = clipboard().lock() {
            *c = Some(edited.params);
        }
    }
    let have_clip = clipboard().lock().is_ok_and(|c| c.is_some());
    if button(ui, 210.0, 40.0, tr("Paste"), false, have_clip) {
        if let Some(c) = clipboard().lock().ok().and_then(|c| *c) {
            edited.params = c;
            edits.extend((0..params.len()).map(|p| Edit {
                param: p,
                value: edited.params[p],
                ended: true,
            }));
        }
    }
    if button(ui, 256.0, 40.0, tr("Undo"), false, !mem.undo.is_empty()) {
        if let Some(prev) = mem.undo.pop() {
            mem.redo.push(edited.params);
            edited.params = prev;
            edits.extend((0..params.len()).map(|p| Edit {
                param: p,
                value: edited.params[p],
                ended: true,
            }));
        }
    }
    if button(ui, 300.0, 40.0, tr("Redo"), false, !mem.redo.is_empty()) {
        if let Some(next) = mem.redo.pop() {
            mem.undo.push(edited.params);
            edited.params = next;
            edits.extend((0..params.len()).map(|p| Edit {
                param: p,
                value: edited.params[p],
                ended: true,
            }));
        }
    }
    // View and the link toggle, at the right.
    g.text(
        pos2(top.right() - 150.0 * k, top.top() + 22.0 * k),
        Align2::RIGHT_CENTER,
        tr("View:"),
        FontId::proportional(11.0 * k),
        Color32::from_gray(0xd0),
    );
    let view_rect = Rect::from_min_size(
        pos2(top.right() - 142.0 * k, top.top() + 11.0 * k),
        vec2(74.0 * k, 22.0 * k),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(view_rect), |ui| {
        egui::ComboBox::from_id_salt("sd_view")
            .selected_text(format!("{} %", mem.view))
            .width(74.0 * k)
            .show_ui(ui, |ui| {
                for v in [75u32, 100, 125, 150] {
                    ui.selectable_value(&mut mem.view, v, format!("{v} %"));
                }
            });
    });
    let link = Rect::from_center_size(
        pos2(top.right() - 36.0 * k, top.top() + 22.0 * k),
        vec2(26.0 * k, 22.0 * k),
    );
    let lr = ui.interact(link, ui.scope_id().with("sd_link"), Sense::click());
    if lr.clicked() {
        mem.link = !mem.link;
    }
    g.rect_filled(
        link,
        4.0 * k,
        Color32::from_gray(if mem.link { 0x70 } else { 0x44 }),
    );
    g.text(
        link.center(),
        Align2::CENTER_CENTER,
        "🔗",
        FontId::proportional(12.0 * k),
        Color32::from_gray(0xe6),
    );

    // ---- Tabs --------------------------------------------------------------------------
    let tabs = next(TAB_H);
    let g = ui.painter_at(tabs);
    for (n, label) in [tr("SAMPLED IR"), tr("SYNTHESIZED IR")]
        .into_iter()
        .enumerate()
    {
        let r = Rect::from_min_size(
            pos2(tabs.left() + n as f32 * w / 2.0, tabs.top()),
            vec2(w / 2.0, TAB_H * k),
        );
        let on = (edited.params[sd::IR_TYPE] >= 0.5) == (n == 1);
        let resp = ui.interact(r, ui.scope_id().with(("sd_tab", n)), Sense::click());
        g.rect_filled(r, 0.0, if on { PANEL } else { TAB_OFF });
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.0 * k),
            if on { ORANGE } else { Color32::from_gray(0x80) },
        );
        if on {
            g.line_segment(
                [
                    r.left_bottom() + vec2(0.0, -1.0),
                    r.right_bottom() + vec2(0.0, -1.0),
                ],
                Stroke::new(2.0 * k, ORANGE),
            );
        }
        if resp.clicked() && !on {
            set(edited, edits, sd::IR_TYPE, n as f32, true);
        }
    }

    // ---- Envelope buttons, IR name and Output EQ --------------------------------------
    let sub = next(SUB_H);
    let g = ui.painter_at(sub);
    g.rect_filled(sub, 0.0, Color32::from_rgb(0x16, 0x20, 0x2c));
    let toggle = |ui: &mut Ui, x: f32, wd: f32, label: &str, on: bool| {
        let r = Rect::from_min_size(
            pos2(sub.left() + x * k, sub.top() + 6.0 * k),
            vec2(wd * k, 22.0 * k),
        );
        let resp = ui.interact(r, ui.scope_id().with(("sd_toggle", label)), Sense::click());
        let g = ui.painter_at(sub);
        g.rect_filled(r, 3.0 * k, Color32::from_rgb(0x24, 0x30, 0x3e));
        // The power symbol at the left of the button.
        let c = pos2(r.left() + 12.0 * k, r.center().y);
        g.circle_stroke(
            c,
            4.5 * k,
            Stroke::new(1.3 * k, if on { CURVE } else { Color32::from_gray(0x80) }),
        );
        g.text(
            pos2(r.left() + 24.0 * k, r.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.5 * k),
            if on { CURVE } else { Color32::from_gray(0x9a) },
        );
        resp.clicked()
    };
    if toggle(
        ui,
        10.0,
        120.0,
        tr("Volume Env"),
        edited.params[sd::VOLUME_ENV] >= 0.5,
    ) {
        let v = 1.0 - edited.params[sd::VOLUME_ENV].round();
        set(edited, edits, sd::VOLUME_ENV, v, true);
    }
    if toggle(
        ui,
        138.0,
        110.0,
        tr("Filter Env"),
        edited.params[sd::FILTER_ENV] >= 0.5,
    ) {
        let v = 1.0 - edited.params[sd::FILTER_ENV].round();
        set(edited, edits, sd::FILTER_ENV, v, true);
    }
    let name_rect = Rect::from_min_size(
        pos2(sub.left() + 262.0 * k, sub.top() + 5.0 * k),
        vec2(250.0 * k, 24.0 * k),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(name_rect), |ui| {
        let synth = edited.params[sd::IR_TYPE] >= 0.5;
        let text = if synth {
            tr("Synthesized IR").to_string()
        } else {
            IR_NAMES[index].replace(' ', "_")
        };
        egui::ComboBox::from_id_salt("sd_ir_name")
            .selected_text(egui::RichText::new(text).size(12.0 * k))
            .width(250.0 * k)
            .show_ui(ui, |ui| {
                for (i, n) in IR_NAMES.iter().enumerate() {
                    if ui.selectable_label(!synth && i == index, *n).clicked() {
                        edited.params = preset_params(&edited.params, i);
                        edits.extend((0..params.len()).map(|p| Edit {
                            param: p,
                            value: edited.params[p],
                            ended: true,
                        }));
                    }
                }
            });
    });
    g.text(
        pos2(sub.left() + 528.0 * k, sub.center().y),
        Align2::LEFT_CENTER,
        tr("Stereo"),
        FontId::proportional(11.5 * k),
        Color32::from_gray(0x9a),
    );
    if toggle(
        ui,
        612.0,
        134.0,
        tr("Output EQ"),
        edited.params[sd::OUTPUT_EQ] >= 0.5,
    ) {
        let v = 1.0 - edited.params[sd::OUTPUT_EQ].round();
        set(edited, edits, sd::OUTPUT_EQ, v, true);
    }

    // ---- The response and its envelope --------------------------------------------------
    let display = next(DISPLAY_H);
    draw_display(ui, display, edited, edits, mem, k, &editable, &mut set);

    // ---- Readout under the display -----------------------------------------------------
    let readout = next(READOUT_H);
    g_readout(ui, readout, edited, edits, k, &editable, &mut set);

    // ---- Options row: Quality, IR Offset, Reverse --------------------------------------
    let opt = next(OPTIONS_H);
    let g = ui.painter_at(opt);
    g.rect_filled(opt, 0.0, Color32::from_rgb(0xd4, 0xd4, 0xd4));
    g.text(
        pos2(opt.left() + 18.0 * k, opt.center().y),
        Align2::LEFT_CENTER,
        tr("Quality:"),
        FontId::proportional(11.5 * k),
        INK,
    );
    let q_rect = Rect::from_min_size(
        pos2(opt.left() + 72.0 * k, opt.top() + 6.0 * k),
        vec2(64.0 * k, 22.0 * k),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(q_rect), |ui| {
        let high = edited.params[sd::QUALITY] >= 0.5;
        egui::ComboBox::from_id_salt("sd_quality")
            .selected_text(
                egui::RichText::new(if high { tr("High") } else { tr("Low") })
                    .color(ORANGE)
                    .size(11.5 * k),
            )
            .width(64.0 * k)
            .show_ui(ui, |ui| {
                if ui.selectable_label(high, tr("High")).clicked() {
                    set(edited, edits, sd::QUALITY, 1.0, true);
                }
                if ui.selectable_label(!high, tr("Low")).clicked() {
                    set(edited, edits, sd::QUALITY, 0.0, true);
                }
            });
    });
    g.text(
        pos2(opt.left() + 176.0 * k, opt.center().y),
        Align2::LEFT_CENTER,
        tr("IR Offset:"),
        FontId::proportional(11.5 * k),
        INK,
    );
    let off = shown(edited, sd::IR_OFFSET);
    let off_rect = Rect::from_min_size(
        pos2(opt.left() + 236.0 * k, opt.top() + 6.0 * k),
        vec2(60.0 * k, 22.0 * k),
    );
    if let Some(v) = drag_value(
        ui,
        off_rect,
        "sd_offset",
        &format!("{off:.2} s"),
        off,
        0.0,
        2.0,
        0.005,
        editable(sd::IR_OFFSET),
        k,
    ) {
        set(edited, edits, sd::IR_OFFSET, v, false);
    }
    let rev_rect = Rect::from_min_size(
        pos2(opt.left() + 316.0 * k, opt.top() + 6.0 * k),
        vec2(74.0 * k, 22.0 * k),
    );
    let rr = ui.interact(rev_rect, ui.scope_id().with("sd_reverse"), Sense::click());
    let reversed = edited.params[sd::REVERSE] >= 0.5;
    g.rect_filled(
        rev_rect,
        4.0 * k,
        if reversed {
            ORANGE_LIGHT
        } else {
            Color32::from_gray(0xf4)
        },
    );
    g.rect_stroke(
        rev_rect,
        4.0 * k,
        Stroke::new(1.0, Color32::from_gray(0xaa)),
        StrokeKind::Inside,
    );
    g.text(
        rev_rect.center(),
        Align2::CENTER_CENTER,
        tr("Reverse"),
        FontId::proportional(11.5 * k),
        INK,
    );
    if rr.clicked() && editable(sd::REVERSE) {
        set(
            edited,
            edits,
            sd::REVERSE,
            if reversed { 0.0 } else { 1.0 },
            true,
        );
    }
    g.text(
        pos2(opt.right() - 20.0 * k, opt.center().y),
        Align2::CENTER_CENTER,
        "⚙",
        FontId::proportional(15.0 * k),
        TEXT_DIM,
    );

    // ---- The controls ------------------------------------------------------------------
    let panel = next(PANEL_H);
    let g = ui.painter_at(panel);
    g.rect_filled(panel, 0.0, PANEL);
    let x0 = panel.left();
    let top_y = panel.top();

    // Input fader (left), with the three routing symbols at its right.
    let v = shown(edited, sd::INPUT);
    g.text(
        pos2(x0 + 52.0 * k, top_y + 18.0 * k),
        Align2::CENTER_CENTER,
        tr("Input"),
        FontId::proportional(12.0 * k),
        INK,
    );
    if let Some(n) = fader(
        ui,
        &g,
        pos2(x0 + 44.0 * k, top_y + 34.0 * k),
        130.0 * k,
        "sd_input",
        &params[sd::INPUT],
        v,
        editable(sd::INPUT),
        k,
        false,
    ) {
        set(edited, edits, sd::INPUT, n, false);
    }
    for (n, sym) in ["═", "⊃", "✕"].into_iter().enumerate() {
        g.text(
            pos2(x0 + 82.0 * k, top_y + (54.0 + 40.0 * n as f32) * k),
            Align2::CENTER_CENTER,
            sym,
            FontId::proportional(15.0 * k),
            TEXT_DIM,
        );
    }

    // Predelay: a grey knob with a note symbol above (tempo sync).
    let sync_rect = Rect::from_center_size(
        pos2(x0 + 146.0 * k, top_y + 22.0 * k),
        vec2(28.0 * k, 22.0 * k),
    );
    g.rect_filled(sync_rect, 4.0 * k, Color32::from_gray(0xf6));
    g.rect_stroke(
        sync_rect,
        4.0 * k,
        Stroke::new(1.0, Color32::from_gray(0xaa)),
        StrokeKind::Inside,
    );
    g.text(
        sync_rect.center(),
        Align2::CENTER_CENTER,
        "♪",
        FontId::proportional(13.0 * k),
        INK,
    );

    let knobs: [(usize, &str, f32, bool); 4] = [
        (sd::PREDELAY, "Predelay", 224.0, false),
        (sd::LENGTH, "Length", 308.0, true),
        (sd::SIZE, "Size", 392.0, true),
        (sd::LO_SPREAD, "Lo Spread", 540.0, true),
    ];
    for (p, label, cx, orange) in knobs {
        let v = shown(edited, p);
        g.text(
            pos2(x0 + cx * k, top_y + 18.0 * k),
            Align2::CENTER_CENTER,
            tr(label),
            FontId::proportional(12.0 * k),
            INK,
        );
        g.text(
            pos2(x0 + cx * k, top_y + 36.0 * k),
            Align2::CENTER_CENTER,
            readout_text(p, v),
            FontId::proportional(12.5 * k),
            ORANGE,
        );
        if let Some(n) = knob(
            ui,
            &g,
            pos2(x0 + cx * k, top_y + 78.0 * k),
            25.0 * k,
            ("sd_knob", p),
            &params[p],
            v,
            editable(p),
            orange,
        ) {
            set(edited, edits, p, n, false);
        }
    }
    // Hi Spread is next to Lo Spread.
    {
        let (p, cx) = (sd::HI_SPREAD, 620.0);
        let v = shown(edited, p);
        g.text(
            pos2(x0 + cx * k, top_y + 18.0 * k),
            Align2::CENTER_CENTER,
            tr("Hi Spread"),
            FontId::proportional(12.0 * k),
            INK,
        );
        g.text(
            pos2(x0 + cx * k, top_y + 36.0 * k),
            Align2::CENTER_CENTER,
            readout_text(p, v),
            FontId::proportional(12.5 * k),
            ORANGE,
        );
        if let Some(n) = knob(
            ui,
            &g,
            pos2(x0 + cx * k, top_y + 78.0 * k),
            25.0 * k,
            ("sd_knob", p),
            &params[p],
            v,
            editable(p),
            true,
        ) {
            set(edited, edits, p, n, false);
        }
    }
    // The scales under the knobs.
    g.text(
        pos2(x0 + 372.0 * k, top_y + 124.0 * k),
        Align2::CENTER_CENTER,
        "10",
        FontId::proportional(9.5 * k),
        TEXT_DIM,
    );
    g.text(
        pos2(x0 + 412.0 * k, top_y + 124.0 * k),
        Align2::CENTER_CENTER,
        "1000",
        FontId::proportional(9.5 * k),
        TEXT_DIM,
    );
    for cx in [540.0, 620.0] {
        g.text(
            pos2(x0 + (cx - 22.0) * k, top_y + 124.0 * k),
            Align2::CENTER_CENTER,
            "0",
            FontId::proportional(9.5 * k),
            TEXT_DIM,
        );
        g.text(
            pos2(x0 + (cx + 22.0) * k, top_y + 124.0 * k),
            Align2::CENTER_CENTER,
            "100",
            FontId::proportional(9.5 * k),
            TEXT_DIM,
        );
    }

    // X-Over: a vertical slider with its scale.
    {
        let (p, cx) = (sd::XOVER, 468.0);
        let v = shown(edited, p);
        g.text(
            pos2(x0 + cx * k, top_y + 18.0 * k),
            Align2::CENTER_CENTER,
            tr("X-Over"),
            FontId::proportional(12.0 * k),
            INK,
        );
        g.text(
            pos2(x0 + cx * k, top_y + 36.0 * k),
            Align2::CENTER_CENTER,
            readout_text(p, v),
            FontId::proportional(12.5 * k),
            ORANGE,
        );
        g.text(
            pos2(x0 + (cx + 26.0) * k, top_y + 52.0 * k),
            Align2::LEFT_CENTER,
            "16k",
            FontId::proportional(9.5 * k),
            TEXT_DIM,
        );
        g.text(
            pos2(x0 + (cx + 26.0) * k, top_y + 160.0 * k),
            Align2::LEFT_CENTER,
            "200",
            FontId::proportional(9.5 * k),
            TEXT_DIM,
        );
        if let Some(n) = fader(
            ui,
            &g,
            pos2(x0 + cx * k, top_y + 48.0 * k),
            112.0 * k,
            "sd_xover",
            &params[p],
            v,
            editable(p),
            k,
            true,
        ) {
            set(edited, edits, p, n, false);
        }
    }
    // Dry and Wet faders (right).
    for (p, label, cx) in [(sd::DRY, "Dry", 692.0), (sd::WET, "Wet", 742.0)] {
        let v = shown(edited, p);
        g.text(
            pos2(x0 + cx * k - 6.0 * k, top_y + 18.0 * k),
            Align2::CENTER_CENTER,
            tr(label),
            FontId::proportional(12.0 * k),
            INK,
        );
        g.text(
            pos2(x0 + cx * k - 6.0 * k, top_y + 36.0 * k),
            Align2::CENTER_CENTER,
            readout_text(p, v),
            FontId::proportional(12.0 * k),
            ORANGE,
        );
        if let Some(n) = fader(
            ui,
            &g,
            pos2(x0 + cx * k - 6.0 * k, top_y + 48.0 * k),
            124.0 * k,
            "sd_fader",
            &params[p],
            v,
            editable(p),
            k,
            false,
        ) {
            set(edited, edits, p, n, false);
        }
        let _ = label;
    }
    let _ = (panel,);

    // ---- Footer ------------------------------------------------------------------------
    let foot = next(FOOT_H);
    let g = ui.painter_at(foot);
    g.rect_filled(foot, 0.0, Color32::from_rgb(0xdc, 0xdc, 0xdc));
    g.text(
        foot.center(),
        Align2::CENTER_CENTER,
        "Space Designer",
        FontId::proportional(17.0 * k),
        Color32::from_gray(0x66),
    );
    g.text(
        pos2(foot.right() - 20.0 * k, foot.center().y),
        Align2::CENTER_CENTER,
        "⚙",
        FontId::proportional(15.0 * k),
        TEXT_DIM,
    );
}

/// The text under a control, in its units.
fn readout_text(param: usize, v: f32) -> String {
    match param {
        sd::PREDELAY => format!("{v:.0} ms"),
        sd::LENGTH => format!("{v:.2} s"),
        sd::SIZE | sd::LO_SPREAD | sd::HI_SPREAD => format!("{v:.0} %"),
        sd::XOVER => format!("{v:.0} Hz"),
        sd::DRY | sd::WET | sd::INPUT => {
            if v <= -95.9 {
                "-∞ dB".to_string()
            } else {
                format!("{v:.1} dB")
            }
        }
        _ => format!("{v:.1}"),
    }
}

/// A knob: drag up or down to turn it, double-click for its starting value. Returns the new
/// value while it moves.
#[allow(clippy::too_many_arguments)]
fn knob(
    ui: &mut Ui,
    g: &egui::Painter,
    c: egui::Pos2,
    r: f32,
    id: (&str, usize),
    info: &prowave_engine::ParamInfo,
    v: f32,
    enabled: bool,
    orange: bool,
) -> Option<f32> {
    let n = info.to_norm(v);
    g.circle_filled(c + vec2(0.0, 2.0), r + 1.0, Color32::from_black_alpha(70));
    let body = if orange {
        ORANGE
    } else {
        Color32::from_gray(0xa0)
    };
    g.circle_filled(c, r, body);
    g.circle_stroke(c, r, Stroke::new(1.0, Color32::from_black_alpha(90)));
    // The arc of the range, then the white tick at the value (7 o'clock to 5 o'clock).
    let a = (-225.0 + n * 270.0).to_radians();
    let dir = vec2(a.cos(), a.sin());
    g.line_segment(
        [c + dir * (r * 0.42), c + dir * (r * 0.9)],
        Stroke::new(2.4, Color32::WHITE),
    );
    let resp = ui.interact(
        Rect::from_center_size(c, vec2(r * 2.2, r * 2.2)),
        ui.scope_id().with(id),
        Sense::click_and_drag(),
    );
    if !enabled {
        return None;
    }
    if resp.double_clicked() {
        return Some(info.default);
    }
    resp.dragged()
        .then(|| info.from_norm(n - resp.drag_delta().y * 0.006))
}

/// A vertical fader. `rail` draws a thin slider (X-Over) instead of a fader with a track.
#[allow(clippy::too_many_arguments)]
fn fader(
    ui: &mut Ui,
    g: &egui::Painter,
    top: egui::Pos2,
    height: f32,
    id: &str,
    info: &prowave_engine::ParamInfo,
    v: f32,
    enabled: bool,
    k: f32,
    rail: bool,
) -> Option<f32> {
    let n = if info.min < -90.0 {
        // dB faders: the lower part of the travel is the quiet part.
        db_to_pos(v, info)
    } else {
        info.to_norm(v)
    };
    let track = Rect::from_min_max(
        pos2(top.x - 3.0 * k, top.y),
        pos2(top.x + 3.0 * k, top.y + height),
    );
    g.rect_filled(track, 3.0 * k, Color32::from_gray(0x8a));
    let lit = Rect::from_min_max(
        pos2(track.left(), top.y + height * (1.0 - n)),
        track.right_bottom(),
    );
    g.rect_filled(lit, 3.0 * k, ORANGE);
    let y = top.y + height * (1.0 - n);
    let handle = Rect::from_center_size(
        pos2(top.x, y),
        vec2(if rail { 22.0 } else { 26.0 } * k, 11.0 * k),
    );
    g.rect_filled(
        handle.translate(vec2(0.0, 1.5)),
        3.0 * k,
        Color32::from_black_alpha(60),
    );
    g.rect_filled(handle, 3.0 * k, Color32::from_gray(0xf8));
    g.rect_stroke(
        handle,
        3.0 * k,
        Stroke::new(1.0, Color32::from_gray(0x80)),
        StrokeKind::Inside,
    );
    g.line_segment(
        [
            pos2(handle.left() + 3.0 * k, y),
            pos2(handle.right() - 3.0 * k, y),
        ],
        Stroke::new(1.0, Color32::from_gray(0x90)),
    );
    let area = Rect::from_min_max(
        pos2(top.x - 16.0 * k, top.y - 6.0 * k),
        pos2(top.x + 16.0 * k, top.y + height + 6.0 * k),
    );
    let resp = ui.interact(
        area,
        ui.scope_id().with(id).with((top.x as i32, top.y as i32)),
        Sense::click_and_drag(),
    );
    if !enabled {
        return None;
    }
    if resp.double_clicked() {
        return Some(info.default);
    }
    if resp.dragged() || resp.clicked() {
        let p = resp.interact_pointer_pos()?;
        let pos = (1.0 - (p.y - top.y) / height).clamp(0.0, 1.0);
        return Some(if info.min < -90.0 {
            pos_to_db(pos, info)
        } else {
            info.from_norm(pos)
        });
    }
    None
}

/// Where a dB value sits on a fader (0..1): the top quarter is -12 dB to the maximum, and the
/// bottom reaches minus infinity.
fn db_to_pos(db: f32, info: &prowave_engine::ParamInfo) -> f32 {
    let db = db.clamp(info.min, info.max);
    if db <= -95.9 {
        return 0.0;
    }
    // -60 dB at the bottom (just above off), the maximum at the top, a gentle curve between.
    let t = ((db + 60.0) / (info.max + 60.0)).clamp(0.0, 1.0);
    0.04 + 0.96 * t.sqrt()
}

fn pos_to_db(pos: f32, info: &prowave_engine::ParamInfo) -> f32 {
    if pos <= 0.02 {
        return info.min;
    }
    let t = ((pos - 0.04) / 0.96).clamp(0.0, 1.0);
    (t * t * (info.max + 60.0) - 60.0).clamp(-60.0, info.max)
}

/// A small box showing a value that is dragged up and down.
#[allow(clippy::too_many_arguments)]
fn drag_value(
    ui: &mut Ui,
    rect: Rect,
    id: &str,
    text: &str,
    v: f32,
    min: f32,
    max: f32,
    step: f32,
    enabled: bool,
    k: f32,
) -> Option<f32> {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click_and_drag());
    let g = ui.painter_at(rect.expand(2.0));
    g.rect_filled(rect, 4.0 * k, Color32::from_gray(0xf4));
    g.rect_stroke(
        rect,
        4.0 * k,
        Stroke::new(1.0, Color32::from_gray(0xaa)),
        StrokeKind::Inside,
    );
    g.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.5 * k),
        ORANGE,
    );
    if !enabled {
        return None;
    }
    if resp.double_clicked() {
        return Some(min);
    }
    resp.dragged()
        .then(|| (v - resp.drag_delta().y * step).clamp(min, max))
}

/// "Init Level: 100 %   Attack: 122 ms   Decay: 4.52 s   LIN EXP   End Level: 18 %".
#[allow(clippy::too_many_arguments)]
fn g_readout(
    ui: &mut Ui,
    rect: Rect,
    edited: &mut Insert,
    edits: &mut Vec<Edit>,
    k: f32,
    editable: &dyn Fn(usize) -> bool,
    set: &mut SetFn,
) {
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 0.0, Color32::from_rgb(0x16, 0x20, 0x2c));
    let dim = Color32::from_rgb(0x7a, 0x9a, 0xb8);
    let items: [(usize, &str, f32, f32, f32, f32); 3] = [
        (sd::INIT_LEVEL, "Init Level:", 10.0, 100.0, 0.0, 100.0),
        (sd::ATTACK, "Attack:", 210.0, 112.0, 0.0, 2000.0),
        (sd::DECAY, "Decay:", 400.0, 94.0, 0.01, 10.0),
    ];
    let place = |ui: &mut Ui,
                 p: usize,
                 v: f32,
                 label: &str,
                 x: f32,
                 lo: f32,
                 hi: f32|
     -> Option<(f32, bool)> {
        g.text(
            pos2(rect.left() + x * k, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.5 * k),
            dim,
        );
        let tx = rect.left() + (x + label.len() as f32 * 5.6) * k;
        let text = match p {
            sd::INIT_LEVEL | sd::END_LEVEL => format!("{v:.0}%"),
            sd::ATTACK => format!("{v:.0} ms"),
            _ => format!("{v:.2} s"),
        };
        let r = Rect::from_min_size(
            pos2(tx + 4.0 * k, rect.top() + 4.0 * k),
            vec2(70.0 * k, 22.0 * k),
        );
        let resp = ui.interact(
            r,
            ui.scope_id().with(("sd_read", p)),
            Sense::click_and_drag(),
        );
        g.text(
            pos2(r.left(), r.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(11.5 * k),
            CURVE,
        );
        if editable(p) && resp.dragged() {
            let scale = (hi - lo) / 300.0;
            return Some(((v - resp.drag_delta().y * scale).clamp(lo, hi), false));
        }
        resp.drag_stopped().then_some((v, true))
    };
    for (p, label, x, _wd, lo, hi) in items {
        if let Some((nv, ended)) = place(ui, p, edited.params[p], label, x, lo, hi) {
            set(edited, edits, p, nv, ended);
        }
    }
    // LIN / EXP.
    let exp = edited.params[sd::CURVE] >= 0.5;
    for (n, label) in ["LIN", "EXP"].into_iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(
                rect.left() + (508.0 + n as f32 * 44.0) * k,
                rect.top() + 5.0 * k,
            ),
            vec2(42.0 * k, 20.0 * k),
        );
        let on = exp == (n == 1);
        let resp = ui.interact(r, ui.scope_id().with(("sd_curve", n)), Sense::click());
        g.rect_filled(
            r,
            3.0 * k,
            if on {
                ORANGE_LIGHT
            } else {
                Color32::from_rgb(0x24, 0x30, 0x3e)
            },
        );
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.5 * k),
            if on { Color32::WHITE } else { CURVE },
        );
        if resp.clicked() && editable(sd::CURVE) {
            set(edited, edits, sd::CURVE, n as f32, true);
        }
    }
    if let Some((nv, ended)) = place(
        ui,
        sd::END_LEVEL,
        edited.params[sd::END_LEVEL],
        "End Level:",
        610.0,
        0.0,
        100.0,
    ) {
        set(edited, edits, sd::END_LEVEL, nv, ended);
    }
}

/// The dark display: the response drawn, the Volume Envelope over it and its three handles.
#[allow(clippy::too_many_arguments)]
fn draw_display(
    ui: &mut Ui,
    rect: Rect,
    edited: &mut Insert,
    edits: &mut Vec<Edit>,
    mem: &mut Memory,
    k: f32,
    editable: &dyn Fn(usize) -> bool,
    set: &mut SetFn,
) {
    let g = ui.painter_at(rect);
    // The gradient background.
    let mut mesh = egui::Mesh::default();
    let (top_c, bot_c) = (
        Color32::from_rgb(0x1a, 0x2a, 0x3a),
        Color32::from_rgb(0x2a, 0x3a, 0x4a),
    );
    mesh.colored_vertex(rect.left_top(), top_c);
    mesh.colored_vertex(rect.right_top(), top_c);
    mesh.colored_vertex(rect.right_bottom(), bot_c);
    mesh.colored_vertex(rect.left_bottom(), bot_c);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    g.add(Shape::mesh(mesh));

    let plot = Rect::from_min_max(
        pos2(rect.left() + 30.0 * k, rect.top() + 24.0 * k),
        pos2(rect.right() - 30.0 * k, rect.bottom() - 28.0 * k),
    );
    // The response, made again when a setting that shapes it changes.
    let key: Vec<u32> = [
        sd::LENGTH,
        sd::SIZE,
        sd::XOVER,
        sd::LO_SPREAD,
        sd::HI_SPREAD,
        sd::INIT_LEVEL,
        sd::ATTACK,
        sd::DECAY,
        sd::CURVE,
        sd::END_LEVEL,
        sd::IR_OFFSET,
        sd::REVERSE,
        sd::QUALITY,
        sd::FILTER_ENV,
        sd::VOLUME_ENV,
        sd::IR_TYPE,
        sd::IR_INDEX,
        sd::BRIGHTNESS,
    ]
    .iter()
    .map(|&i| edited.params[i].to_bits())
    .collect();
    let bins = plot.width().max(8.0) as usize;
    if mem
        .preview
        .as_ref()
        .is_none_or(|(kk, b, _)| *kk != key || b.len() != bins)
    {
        let (peaks, secs) = prowave_engine::ir_preview(&edited.params, 48_000.0, bins);
        mem.preview = Some((key, peaks, secs));
    }
    let Some((_, peaks, secs)) = mem.preview.clone() else {
        return;
    };
    let secs = secs.max(0.05);
    let mid = plot.center().y;
    let half = plot.height() * 0.5;
    // The filled waveform.
    for (i, (lo, hi)) in peaks.iter().enumerate() {
        let x = plot.left() + i as f32;
        g.line_segment(
            [
                pos2(x, mid - hi.min(1.0) * half * 0.9),
                pos2(x, mid - lo.max(-1.0) * half * 0.9),
            ],
            Stroke::new(1.0, WAVE),
        );
    }
    // The envelope: filled under the line, then the line.
    let x_of = |t: f32| plot.left() + (t / secs).clamp(0.0, 1.0) * plot.width();
    let y_of = |a: f32| plot.bottom() - a.clamp(0.0, 1.0) * plot.height();
    let env_on = edited.params[sd::VOLUME_ENV] >= 0.5;
    let n = plot.width() as usize;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let t = secs * i as f32 / n.max(1) as f32;
            let a = if env_on {
                prowave_engine::volume_env(&edited.params, t)
            } else {
                1.0
            };
            pos2(plot.left() + i as f32, y_of(a))
        })
        .collect();
    let mut fill = pts.clone();
    fill.push(plot.right_bottom());
    fill.push(plot.left_bottom());
    g.add(Shape::convex_polygon(
        fill,
        Color32::from_rgba_unmultiplied(0x4a, 0x7a, 0xa8, 36),
        Stroke::NONE,
    ));
    g.add(Shape::line(pts, Stroke::new(1.8 * k, CURVE)));

    // The axes: seconds along the bottom.
    let mut t = 0.0;
    while t <= secs + 1e-3 {
        let x = x_of(t);
        g.text(
            pos2(x, plot.bottom() + 8.0 * k),
            if t == 0.0 {
                Align2::LEFT_CENTER
            } else {
                Align2::CENTER_CENTER
            },
            format!("{t:.1}"),
            FontId::proportional(9.5 * k),
            Color32::from_gray(0x80),
        );
        if t > 0.0 {
            g.line_segment(
                [pos2(x, plot.top()), pos2(x, plot.bottom())],
                Stroke::new(1.0, Color32::from_white_alpha(14)),
            );
        }
        t += if secs > 6.0 { 2.0 } else { 1.0 };
    }

    if !env_on {
        return;
    }
    // The three handles: Init Level (left), Attack (on the baseline), End Level (where the
    // decay ends).
    let attack = edited.params[sd::ATTACK] / 1000.0;
    let decay = edited.params[sd::DECAY];
    let end_t = (attack + decay).min(secs);
    let init = edited.params[sd::INIT_LEVEL] / 100.0;
    let end = edited.params[sd::END_LEVEL] / 100.0;
    let dots = [
        (0, pos2(x_of(0.0), y_of(init))),
        (1, pos2(x_of(attack), y_of(0.0))),
        (2, pos2(x_of(end_t), y_of(end))),
    ];
    for (which, c) in dots {
        let hit = Rect::from_center_size(c, vec2(18.0 * k, 18.0 * k));
        let resp = ui.interact(hit, ui.scope_id().with(("sd_dot", which)), Sense::drag());
        g.circle_filled(c, 5.5 * k, CURVE);
        g.circle_stroke(
            c,
            5.5 * k,
            Stroke::new(1.0, Color32::from_rgb(0x1a, 0x3a, 0x5a)),
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if !resp.dragged() {
            if resp.drag_stopped() {
                let p = match which {
                    0 => sd::INIT_LEVEL,
                    1 => sd::ATTACK,
                    _ => sd::END_LEVEL,
                };
                set(edited, edits, p, edited.params[p], true);
            }
            continue;
        }
        let Some(pt) = resp.interact_pointer_pos() else {
            continue;
        };
        let time = ((pt.x - plot.left()) / plot.width()).clamp(0.0, 1.0) * secs;
        let level = ((plot.bottom() - pt.y) / plot.height()).clamp(0.0, 1.0);
        match which {
            0 if editable(sd::INIT_LEVEL) => {
                set(edited, edits, sd::INIT_LEVEL, level * 100.0, false)
            }
            1 if editable(sd::ATTACK) => set(
                edited,
                edits,
                sd::ATTACK,
                (time * 1000.0).min(secs * 1000.0 * 0.9),
                false,
            ),
            2 => {
                if editable(sd::END_LEVEL) {
                    set(edited, edits, sd::END_LEVEL, level * 100.0, false);
                }
                if editable(sd::DECAY) {
                    set(edited, edits, sd::DECAY, (time - attack).max(0.01), false);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_sets_its_response_and_length_and_keeps_the_mix() {
        let mut base = FxKind::Reverb.defaults();
        base[sd::DRY] = -3.0;
        base[sd::WET] = -9.0;
        base[sd::REVERSE] = 1.0;
        let p = preset_params(&base, 3);
        assert_eq!(p[sd::IR_INDEX], 3.0);
        assert_eq!(p[sd::IR_TYPE], 0.0);
        assert_eq!(p[sd::LENGTH], 6.0);
        assert_eq!(p[sd::REVERSE], 0.0);
        assert_eq!((p[sd::DRY], p[sd::WET]), (-3.0, -9.0));
        assert_eq!(
            FxKind::Reverb.clamp_params(p),
            p,
            "every setting is in range"
        );
        for i in 0..IR_NAMES.len() {
            let q = preset_params(&base, i);
            assert_eq!(FxKind::Reverb.clamp_params(q), q);
        }
    }

    #[test]
    fn a_db_fader_runs_from_minus_infinity_to_the_maximum_and_back() {
        let info = FxKind::Reverb.params()[sd::DRY];
        assert_eq!(db_to_pos(-96.0, &info), 0.0);
        assert!((db_to_pos(6.0, &info) - 1.0).abs() < 1e-5);
        for db in [-50.0, -20.0, -12.5, -7.0, 0.0, 6.0] {
            let back = pos_to_db(db_to_pos(db, &info), &info);
            assert!((back - db).abs() < 0.05, "{db} -> {back}");
        }
        assert_eq!(pos_to_db(0.0, &info), -96.0);
    }

    #[test]
    fn the_readouts_use_the_units_of_the_reference() {
        assert_eq!(readout_text(sd::PREDELAY, 15.0), "15 ms");
        assert_eq!(readout_text(sd::LENGTH, 4.52), "4.52 s");
        assert_eq!(readout_text(sd::SIZE, 100.0), "100 %");
        assert_eq!(readout_text(sd::XOVER, 710.0), "710 Hz");
        assert_eq!(readout_text(sd::DRY, -12.5), "-12.5 dB");
        assert_eq!(readout_text(sd::WET, -96.0), "-∞ dB");
    }
}
