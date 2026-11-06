//! The window of an effect in a channel strip's Audio FX slot (Logic: the plug-in window):
//! a slider for each setting (automatable: moving one while the project plays writes it, and
//! it moves by itself when its lane is played), Bypass and Reset, the curve of the EQs and the
//! gain reduction of the Compressor, Noise Gate and Limiter.
use crate::automation;
use crate::i18n::{tr, trf};
use crate::mixer::Meters;
use crate::model::{AutoTarget, Insert, Project, Track};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke, StrokeKind, Ui};
use prowave_engine::{FxKind, ParamInfo, AMP_CABS, AMP_MODELS};

/// Sample rate the curve of the EQs is drawn for.
const CURVE_RATE: f32 = 48_000.0;
const CURVE_H: f32 = 110.0;

/// Width and height of the Amp Designer's faceplate.
const AMP_W: f32 = 470.0;
const AMP_H: f32 = 146.0;

/// A slider the user moved: parameter number, new value, and whether the gesture ended.
pub(crate) struct Edit {
    pub(crate) param: usize,
    pub(crate) value: f32,
    pub(crate) ended: bool,
}

/// Draws the effect window for the insert the Mixer opened, if any.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState, meters: &Meters) {
    let Some((ti, slot)) = s.mixer.fx_window else {
        return;
    };
    let Some(insert) = p.tracks.get(ti).and_then(|t| t.inserts.get(slot)).copied() else {
        s.mixer.fx_window = None;
        return;
    };
    if insert.kind == FxKind::Reverb {
        #[allow(deprecated)]
        let id = egui::Id::new(("space_designer", ti, slot));
        let (edited, edits, open) = crate::space_designer_ui::window(
            ctx,
            &insert,
            &p.tracks[ti],
            slot,
            p.position_beats,
            id,
        );
        finish(p, s, ti, slot, insert, edited, edits, open);
        return;
    }
    let mut open = true;
    let title = format!("{} · {}", p.tracks[ti].name, insert.name());
    let reduction = meters.reduction.get(ti).copied().unwrap_or(0.0);
    let beat = p.position_beats;
    let mut edited = insert;
    let mut edits: Vec<Edit> = Vec::new();
    let track = &p.tracks[ti];
    egui::Window::new(title)
        .id(egui::Id::unique("fx_window"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut edited.bypass, tr("Bypass"));
                if ui.button(tr("Reset")).clicked() {
                    edited.params = edited.kind.defaults();
                }
            });
            ui.add_space(4.0);
            match edited.kind {
                FxKind::ChannelEq | FxKind::MultiEq => {
                    eq_curve(ui, edited.kind, &edited.params);
                }
                FxKind::Compressor | FxKind::Gate | FxKind::Limiter => {
                    reduction_bar(ui, reduction, edited.bypass)
                }
                FxKind::Amp => {
                    amp_faceplate(ui, &mut edited, track, slot, beat, &mut edits);
                }
                _ => {}
            }
            if edited.kind == FxKind::Amp {
                return;
            }
            egui::Grid::new("fx_params")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for (k, info) in edited.kind.params().iter().enumerate() {
                        let target = AutoTarget::Fx {
                            slot: slot as u8,
                            param: k as u8,
                        };
                        ui.label(info.name);
                        // A lane that is played moves the slider; it cannot be moved by hand
                        // in Read mode.
                        let played = track.auto.played(target, beat);
                        let mut v = match played {
                            Some(n) => info.from_norm(n),
                            None => edited.params[k],
                        };
                        let mut slider = egui::Slider::new(&mut v, info.min..=info.max)
                            .logarithmic(info.log)
                            .clamping(egui::SliderClamping::Always);
                        if !info.unit.is_empty() {
                            slider = slider.suffix(format!(" {}", info.unit));
                        }
                        if info.unit.is_empty() && info.max <= 1.0 {
                            slider = slider.custom_formatter(|v, _| format!("{:.0} %", v * 100.0));
                        }
                        if matches!(edited.kind, FxKind::Filter) && info.name == "Mode" {
                            slider = slider.step_by(1.0).custom_formatter(|v, _| {
                                ["Low Pass", "High Pass", "Band Pass"][(v.round() as usize).min(2)]
                                    .to_string()
                            });
                        }
                        let resp = ui.add_enabled(automation::editable(track, target), slider);
                        if resp.changed() {
                            edits.push(Edit {
                                param: k,
                                value: v,
                                ended: !resp.dragged(),
                            });
                        } else if resp.drag_stopped() {
                            edits.push(Edit {
                                param: k,
                                value: v,
                                ended: true,
                            });
                        }
                        if !track.auto.lane(target).is_empty() {
                            ui.label("•").on_hover_text(tr("Automated"));
                        }
                        ui.end_row();
                    }
                });
        });
    finish(p, s, ti, slot, insert, edited, edits, open);
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
        p.tracks[ti].inserts[slot] = edited;
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

/// The EQ's gain over the frequencies 20 Hz to 20 kHz, ±18 dB.
fn eq_curve(ui: &mut Ui, kind: FxKind, params: &[f32; prowave_engine::MAX_FX_PARAMS]) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width().max(300.0), CURVE_H),
        Sense::hover(),
    );
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 3.0, Color32::from_gray(0x1c));
    let x_of = |hz: f32| rect.left() + (hz / 20.0).log10() / 3.0 * rect.width();
    let y_of = |db: f32| rect.center().y - db / 18.0 * rect.height() / 2.0;
    for hz in [
        50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0,
    ] {
        g.line_segment(
            [pos2(x_of(hz), rect.top()), pos2(x_of(hz), rect.bottom())],
            Stroke::new(1.0, Color32::from_gray(0x2c)),
        );
    }
    for db in [-12.0f32, -6.0, 0.0, 6.0, 12.0] {
        g.line_segment(
            [pos2(rect.left(), y_of(db)), pos2(rect.right(), y_of(db))],
            Stroke::new(1.0, Color32::from_gray(if db == 0.0 { 0x48 } else { 0x2c })),
        );
    }
    let n = 160;
    let freqs: Vec<f32> = (0..=n)
        .map(|i| 20.0 * 1000f32.powf(i as f32 / n as f32))
        .collect();
    let curve = prowave_engine::response_db(kind, *params, &freqs, CURVE_RATE);
    let pts: Vec<_> = freqs
        .iter()
        .zip(&curve)
        .map(|(f, db)| pos2(x_of(*f), y_of(db.clamp(-18.0, 18.0))))
        .collect();
    g.add(Shape::line(
        pts,
        Stroke::new(1.8, Color32::from_rgb(0x8a, 0xb8, 0xf0)),
    ));
    g.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_gray(0x10)),
        StrokeKind::Inside,
    );
}

/// The gain reduction as a bar, 0 to 24 dB.
fn reduction_bar(ui: &mut Ui, reduction: f32, bypassed: bool) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(300.0), 16.0), Sense::hover());
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 3.0, Color32::from_gray(0x1c));
    let shown = if bypassed {
        0.0
    } else {
        reduction.clamp(0.0, 24.0)
    };
    let w = rect.width() * shown / 24.0;
    g.rect_filled(
        egui::Rect::from_min_size(rect.min, vec2(w, rect.height())),
        3.0,
        Color32::from_rgb(0xe6, 0xc2, 0x2e),
    );
    g.text(
        rect.left_center() + vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        trf("Gain Reduction: {} dB", &[&format!("{shown:.1}")]),
        FontId::proportional(11.0),
        Color32::WHITE,
    );
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
