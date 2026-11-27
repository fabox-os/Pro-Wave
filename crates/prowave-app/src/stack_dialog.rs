//! Track > Create Track Stack…: Logic's dialog that asks which kind of track stack to make, from
//! the tracks selected in the Tracks area. A Folder Stack is a folder track that holds the
//! tracks, opened and closed with its arrow; a Summing Stack is a sub-mix: an Aux strip that the
//! tracks send their output to. The dialog has a Details panel that describes the choice.
use egui::{
    pos2, vec2, Align, Align2, Color32, Layout, Margin, Painter, Pos2, RichText, Sense, Stroke,
};

use crate::i18n::tr;
use crate::model::Project;
use crate::UiState;

const WIDTH: f32 = 420.0;
const MARGIN: f32 = 16.0;
const WINDOW_BG: Color32 = Color32::from_rgb(32, 35, 38);
const PANEL_BG: Color32 = Color32::from_rgb(27, 30, 33);
const CARD_BORDER: Color32 = Color32::from_rgb(70, 70, 72);
const CANCEL_BG: Color32 = Color32::from_rgb(84, 86, 90);
const BLUE: Color32 = Color32::from_rgb(20, 100, 220);
const LIGHT_TEXT: Color32 = Color32::from_rgb(220, 220, 222);

/// The kind of stack the dialog creates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StackChoice {
    /// A folder track with the tracks inside it: the default.
    #[default]
    Folder,
    /// An Aux strip (a sub-mix) that the tracks inside it send their output to.
    Summing,
}

/// The Create Track Stack dialog: whether it is showing, the choice (kept for the next time, as
/// Logic keeps the last one) and whether the Details panel is folded away.
#[derive(Default)]
pub struct StackDialog {
    pub open: bool,
    pub choice: StackChoice,
    details_closed: bool,
}

impl StackDialog {
    /// Track > Create Track Stack… opens the dialog.
    pub fn open(&mut self) {
        self.open = true;
    }
}

/// What the Details panel says about a choice.
fn description(choice: StackChoice) -> &'static str {
    match choice {
        StackChoice::Folder => tr("Organises the tracks in a folder that can be opened and closed. Every track keeps its own channel strip and output; mute, solo and level of the folder track act on all of them."),
        StackChoice::Summing => tr("A multifunctional Track Stack that works like a mixing subgroup and can be saved as a patch. You can record and play MIDI regions on the same track for software instrument sub-tracks, or control recording remotely on audio sub-tracks."),
    }
}

/// Shows the dialog while it is open. Create makes the stack from the selected tracks (there
/// must be two or more); Cancel and Escape close it.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    if !s.stack_dialog.open {
        return;
    }
    // Enter creates and Escape cancels, as the dialog's default and cancel buttons do.
    let mut create = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let mut cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape));

    egui::Window::new(tr("Create Track Stack…"))
        .id(egui::Id::unique("create_track_stack"))
        .title_bar(false)
        .resizable(false)
        .default_width(WIDTH)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .frame(
            egui::Frame::new()
                .fill(WINDOW_BG)
                .stroke(Stroke::new(1.0, CARD_BORDER))
                .corner_radius(10)
                .inner_margin(Margin::same(MARGIN as i8)),
        )
        .show(ctx, |ui| {
            let content_w = WIDTH - 2.0 * MARGIN;
            ui.set_width(content_w);
            ui.label(
                RichText::new(tr("Which type of Track Stack do you want to create?"))
                    .strong()
                    .size(14.0)
                    .color(Color32::WHITE),
            );
            ui.add_space(10.0);
            let choice = &mut s.stack_dialog.choice;
            ui.radio_value(choice, StackChoice::Folder, tr("Folder Stack"));
            ui.radio_value(choice, StackChoice::Summing, tr("Summing Stack"));

            ui.add_space(12.0);
            details_header(ui, &mut s.stack_dialog.details_closed);
            if !s.stack_dialog.details_closed {
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(Margin::same(MARGIN as i8))
                    .show(ui, |ui| {
                        ui.set_width(content_w - 2.0 * MARGIN);
                        ui.label(
                            RichText::new(description(s.stack_dialog.choice)).color(LIGHT_TEXT),
                        );
                    });
            }

            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let create_button =
                    egui::Button::new(RichText::new(tr("Create")).color(Color32::WHITE))
                        .fill(BLUE)
                        .corner_radius(5)
                        .min_size(vec2(90.0, 26.0));
                if ui.add(create_button).clicked() {
                    create = true;
                }
                let cancel_button =
                    egui::Button::new(RichText::new(tr("Cancel")).color(Color32::WHITE))
                        .fill(CANCEL_BG)
                        .corner_radius(5)
                        .min_size(vec2(90.0, 26.0));
                if ui.add(cancel_button).clicked() {
                    cancel = true;
                }
            });
        });

    if create {
        let picked = s.tracks.selected_tracks(p);
        if picked.len() >= 2 {
            let summing = s.stack_dialog.choice == StackChoice::Summing;
            if p.create_track_stack(&picked, summing).is_some() {
                // Track numbers shift: the remembered selections may point at other tracks.
                s.tracks.clear_selection();
                s.tracks.track_extra.clear();
                s.piano.selected.clear();
            }
        }
        s.stack_dialog.open = false;
    } else if cancel {
        s.stack_dialog.open = false;
    }
}

/// The "Details" line that folds the panel below the cards away and back.
fn details_header(ui: &mut egui::Ui, closed: &mut bool) {
    ui.horizontal(|ui| {
        let (chevron_rect, chevron) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
        draw_chevron(ui.painter(), chevron_rect.center(), *closed);
        let label = ui.add(
            egui::Label::new(RichText::new(tr("Details")).size(14.0).color(LIGHT_TEXT))
                .sense(Sense::click()),
        );
        if chevron.clicked() || label.clicked() {
            *closed = !*closed;
        }
    });
}

/// A small chevron pointing down when the panel is open and right when it is folded away.
fn draw_chevron(painter: &Painter, c: Pos2, pointing_right: bool) {
    let stroke = Stroke::new(1.5, LIGHT_TEXT);
    let points = if pointing_right {
        vec![
            pos2(c.x - 2.0, c.y - 4.0),
            pos2(c.x + 2.0, c.y),
            pos2(c.x - 2.0, c.y + 4.0),
        ]
    } else {
        vec![
            pos2(c.x - 4.0, c.y - 2.0),
            pos2(c.x, c.y + 2.0),
            pos2(c.x + 4.0, c.y - 2.0),
        ]
    };
    painter.line(points, stroke);
}
