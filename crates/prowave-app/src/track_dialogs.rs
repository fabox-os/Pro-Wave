//! The Track menu's windows (Logic Pro: Track > Rename Track, Assign Track Color, Assign Track
//! Icon, and Configure Track Header in the Tracks area's View menu).
//!
//! Each one changes the track or the header display at once and closes; Cancel and the window's
//! close button change nothing.

use crate::i18n::tr;
use crate::model::{HeaderPart, Project};
use crate::track_icons::{draw_icon, IconKind};
use egui::{vec2, Color32, Sense, Stroke};

/// The colour swatches of Assign Track Color.
pub const TRACK_COLORS: [Color32; 16] = [
    Color32::from_rgb(0x4e, 0xb0, 0x4a),
    Color32::from_rgb(0x3f, 0x5f, 0xc0),
    Color32::from_rgb(0xd9, 0xaa, 0x2a),
    Color32::from_rgb(0x7a, 0x4f, 0xc0),
    Color32::from_rgb(0xc0, 0x3f, 0x3f),
    Color32::from_rgb(0xe0, 0x7a, 0x2a),
    Color32::from_rgb(0x2a, 0xb0, 0xb0),
    Color32::from_rgb(0xd0, 0x5a, 0x9a),
    Color32::from_rgb(0x8a, 0xc0, 0x3f),
    Color32::from_rgb(0x3f, 0x9a, 0xe0),
    Color32::from_rgb(0xa0, 0x6a, 0x3a),
    Color32::from_rgb(0x9a, 0x9a, 0x9a),
    Color32::from_rgb(0xe0, 0xe0, 0x6a),
    Color32::from_rgb(0x6a, 0x3a, 0x80),
    Color32::from_rgb(0x4a, 0x4a, 0x4a),
    Color32::from_rgb(0xf0, 0xa0, 0xb0),
];

/// The longest name Rename Track accepts, in characters.
pub const MAX_TRACK_NAME: usize = 64;

/// A window of the Track menu, and the track it is about (by index).
#[derive(Clone, Debug, PartialEq)]
pub enum TrackDialog {
    Rename { track: usize, name: String },
    Color { track: usize },
    Icon { track: usize },
    Header,
}

/// The track a dialog is about, if it is about one.
impl TrackDialog {
    pub fn track(&self) -> Option<usize> {
        match self {
            TrackDialog::Rename { track, .. }
            | TrackDialog::Color { track }
            | TrackDialog::Icon { track } => Some(*track),
            TrackDialog::Header => None,
        }
    }
}

/// The name of a header part in Configure Track Header. The texts are literal `tr` calls so the
/// translation test finds them.
fn part_label(part: HeaderPart) -> &'static str {
    match part {
        HeaderPart::Number => tr("Track Number"),
        HeaderPart::Icon => tr("Track Icon"),
        HeaderPart::Name => tr("Track Name"),
        HeaderPart::Buttons => tr("Mute, Solo, Record and Monitor"),
        HeaderPart::Volume => tr("Volume Slider and Meter"),
        HeaderPart::Pan => tr("Pan Knob"),
    }
}

/// Track > Rename Track: the name without its surrounding spaces, at most [`MAX_TRACK_NAME`]
/// characters. An empty name, or a track that is not there, changes nothing (`false`).
pub fn rename_track(p: &mut Project, track: usize, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || track >= p.tracks.len() {
        return false;
    }
    p.tracks[track].name = name.chars().take(MAX_TRACK_NAME).collect();
    true
}

/// Configure Track Header: shows or hides one part of every track header.
pub fn set_header_part(p: &mut Project, part: HeaderPart, shown: bool) {
    p.header_hidden.retain(|h| *h != part);
    if !shown {
        p.header_hidden.push(part);
    }
}

/// Draws the open window, if any, and applies what the user chose.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut crate::UiState) {
    let Some(mut dialog) = s.track_dialog.take() else {
        return;
    };
    // A dialog about a track that is gone (deleted while the window was open) just closes.
    if dialog.track().is_some_and(|t| t >= p.tracks.len()) {
        return;
    }
    let mut open = true;
    let mut done = false;
    let title = match &dialog {
        TrackDialog::Rename { .. } => tr("Rename Track"),
        TrackDialog::Color { .. } => tr("Assign Track Color…"),
        TrackDialog::Icon { .. } => tr("Assign Track Icon…"),
        TrackDialog::Header => tr("Configure Track Header…"),
    };
    egui::Window::new(title)
        .id(egui::Id::unique("track_dialog"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            match &mut dialog {
                TrackDialog::Rename { track, name } => {
                    let track = *track;
                    ui.add(egui::TextEdit::singleline(name).char_limit(MAX_TRACK_NAME));
                    let ok = !name.trim().is_empty();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ok, egui::Button::new(tr("OK"))).clicked()
                            && rename_track(p, track, name.as_str())
                        {
                            done = true;
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            done = true;
                        }
                    });
                }
                TrackDialog::Color { track } => {
                    let track = *track;
                    let current = p.tracks[track].color;
                    ui.horizontal_wrapped(|ui| {
                        for color in TRACK_COLORS {
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                            let g = ui.painter_at(r);
                            g.rect_filled(r.shrink(2.0), 3.0, color);
                            if color == current {
                                g.rect_stroke(
                                    r.shrink(1.0),
                                    3.0,
                                    Stroke::new(2.0, Color32::WHITE),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if resp.clicked() {
                                p.tracks[track].color = color;
                                done = true;
                            }
                        }
                    });
                }
                TrackDialog::Icon { track } => {
                    let track = *track;
                    let chosen = p.tracks[track].icon;
                    if ui
                        .selectable_label(chosen.is_none(), tr("Automatic"))
                        .clicked()
                    {
                        p.tracks[track].icon = None;
                        done = true;
                    }
                    ui.horizontal_wrapped(|ui| {
                        for kind in IconKind::ALL {
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                            let g = ui.painter_at(r);
                            if chosen == Some(kind) {
                                g.rect_filled(r, 4.0, Color32::from_gray(0x44));
                            }
                            draw_icon(&g, r.shrink(4.0), kind);
                            if resp.clicked() {
                                p.tracks[track].icon = Some(kind);
                                done = true;
                            }
                        }
                    });
                }
                TrackDialog::Header => {
                    for part in HeaderPart::ALL {
                        let mut shown = !p.header_hidden.contains(&part);
                        if ui.checkbox(&mut shown, part_label(part)).changed() {
                            set_header_part(p, part, shown);
                        }
                    }
                }
            }
        });
    if open && !done {
        s.track_dialog = Some(dialog);
    }
}

/// The dialog a Track menu entry opens for the selected track, if it opens one.
pub fn open_for(label: &str, p: &Project) -> Option<TrackDialog> {
    let t = p.selected_track;
    let name = || p.tracks.get(t).map(|tr| tr.name.clone());
    match label {
        "Rename Track" => Some(TrackDialog::Rename {
            track: t,
            name: name()?,
        }),
        "Assign Track Color…" => (t < p.tracks.len()).then_some(TrackDialog::Color { track: t }),
        "Assign Track Icon…" => (t < p.tracks.len()).then_some(TrackDialog::Icon { track: t }),
        "Configure Track Header…" => Some(TrackDialog::Header),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_test::Harness;

    #[test]
    fn every_colour_and_icon_has_a_distinct_choice() {
        for (i, a) in TRACK_COLORS.iter().enumerate() {
            assert!(
                TRACK_COLORS[i + 1..].iter().all(|b| b != a),
                "{a:?} repeats"
            );
        }
        for (i, a) in IconKind::ALL.iter().enumerate() {
            assert!(IconKind::ALL[i + 1..].iter().all(|b| b != a));
            assert_eq!(IconKind::from_name(a.name()), Some(*a));
        }
        assert_eq!(IconKind::from_name("nope"), None);
    }

    #[test]
    fn every_header_part_has_a_name_that_round_trips() {
        for part in HeaderPart::ALL {
            assert_eq!(HeaderPart::from_name(part.name()), Some(part));
        }
        assert_eq!(HeaderPart::from_name("nope"), None);
    }

    #[test]
    fn rename_trims_limits_and_refuses_an_empty_name() {
        let mut p = Project::demo();
        assert!(rename_track(&mut p, 0, "  Lead Vox  "));
        assert_eq!(p.tracks[0].name, "Lead Vox");
        assert!(!rename_track(&mut p, 0, "   "), "blank");
        assert_eq!(p.tracks[0].name, "Lead Vox");
        assert!(!rename_track(&mut p, 999, "x"), "no such track");
        let long = "é".repeat(MAX_TRACK_NAME + 10);
        assert!(rename_track(&mut p, 1, &long));
        assert_eq!(p.tracks[1].name.chars().count(), MAX_TRACK_NAME);
    }

    #[test]
    fn hiding_and_showing_a_header_part_is_undone_by_showing_it_again() {
        let mut p = Project::demo();
        set_header_part(&mut p, HeaderPart::Pan, false);
        set_header_part(&mut p, HeaderPart::Pan, false);
        assert_eq!(p.header_hidden, vec![HeaderPart::Pan], "no duplicates");
        set_header_part(&mut p, HeaderPart::Pan, true);
        assert!(p.header_hidden.is_empty());
    }

    #[test]
    fn the_menu_opens_the_right_window_for_the_selected_track() {
        let mut p = Project::demo();
        p.selected_track = 1;
        let name = p.tracks[1].name.clone();
        assert_eq!(
            open_for("Rename Track", &p),
            Some(TrackDialog::Rename { track: 1, name })
        );
        assert_eq!(
            open_for("Assign Track Color…", &p),
            Some(TrackDialog::Color { track: 1 })
        );
        assert_eq!(
            open_for("Assign Track Icon…", &p),
            Some(TrackDialog::Icon { track: 1 })
        );
        assert_eq!(
            open_for("Configure Track Header…", &p),
            Some(TrackDialog::Header)
        );
        assert_eq!(open_for("Cut", &p), None);
        // No track selected (past the end): the track dialogs do not open.
        p.selected_track = 999;
        assert_eq!(open_for("Assign Track Color…", &p), None);
        assert_eq!(open_for("Rename Track", &p), None);
    }

    /// Runs the window for a few frames; returns what it left open.
    fn run(p: &mut Project, s: &mut crate::UiState) {
        let mut h = Harness::new(900.0, 700.0);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), p, s));
        }
    }

    #[test]
    fn every_dialog_draws_and_stays_open_until_chosen() {
        for dialog in [
            TrackDialog::Rename {
                track: 0,
                name: "Vox".into(),
            },
            TrackDialog::Color { track: 0 },
            TrackDialog::Icon { track: 0 },
            TrackDialog::Header,
        ] {
            let (mut p, mut s) = (Project::demo(), crate::UiState::new());
            s.track_dialog = Some(dialog.clone());
            run(&mut p, &mut s);
            assert_eq!(s.track_dialog, Some(dialog));
        }
    }

    #[test]
    fn a_dialog_about_a_removed_track_closes() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        s.track_dialog = Some(TrackDialog::Color { track: 999 });
        run(&mut p, &mut s);
        assert!(s.track_dialog.is_none());
    }
}
