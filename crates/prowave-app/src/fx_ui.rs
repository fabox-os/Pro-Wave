//! The window of an effect in a channel strip's Audio FX slot (Logic: the plug-in window):
//! a slider for each setting (automatable: moving one while the project plays writes it, and
//! it moves by itself when its lane is played), Bypass and Reset, the curve of the EQs and the
//! gain reduction of the Compressor, Noise Gate and Limiter.
use crate::automation;
use crate::i18n::{tr, trf};
use crate::mixer::Meters;
use crate::model::{AutoTarget, Project};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Sense, Shape, Stroke, StrokeKind, Ui};
use prowave_engine::FxKind;

/// Sample rate the curve of the EQs is drawn for.
const CURVE_RATE: f32 = 48_000.0;
const CURVE_H: f32 = 110.0;

/// A slider the user moved: parameter number, new value, and whether the gesture ended.
struct Edit {
    param: usize,
    value: f32,
    ended: bool,
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
                _ => {}
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
