//! The small panes behind control bar buttons: the Toolbar, the Note Pad and Quick Help.
use crate::i18n::tr;
use crate::model::Project;

/// Width of the Note Pad.
pub const NOTES_WIDTH: f32 = 260.0;
/// Height of the Toolbar row.
pub const TOOLBAR_HEIGHT: f32 = 30.0;

/// The longest note a project keeps, in characters.
pub const MAX_NOTES: usize = 100_000;

/// What the Toolbar offers: menu entries, so each is greyed out when the menu's is.
const TOOLBAR: &[&str] = &[
    "Undo",
    "Redo",
    "-",
    "New",
    "Create Marker",
    "Capture Recording",
    "-",
    "Zoom In",
    "Zoom Out",
    "Zoom to Fit",
];

/// The Toolbar under the control bar (Logic: View > Show Toolbar).
pub fn toolbar(ui: &mut egui::Ui, p: &mut Project, s: &mut crate::UiState) {
    ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        for label in TOOLBAR {
            if *label == "-" {
                ui.separator();
                continue;
            }
            let on = crate::menus::enabled(label, p, s);
            if ui.add_enabled(on, egui::Button::new(tr(label))).clicked() {
                crate::menus::apply(label, p, s);
            }
        }
    });
}

/// The Note Pad: free text saved with the project (Logic: Project Notes).
pub fn note_pad(ui: &mut egui::Ui, p: &mut Project) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong(tr("Notes"));
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_sized(
            ui.available_size(),
            egui::TextEdit::multiline(&mut p.notes).char_limit(MAX_NOTES),
        );
    });
}

/// How to do the main things, in a window of its own.
pub fn quick_help(ctx: &egui::Context, s: &mut crate::UiState) {
    egui::Window::new(tr("Quick Help"))
        .open(&mut s.show_quick_help)
        .resizable(false)
        .default_width(320.0)
        .show(ctx, |ui| {
            for line in [
                tr("Space plays and stops. Drag a file onto the window to add it."),
                tr("To record audio, pick the input in the track's Input slot, press R, then Record and Play."),
                tr("To record MIDI, press R on an instrument track, then Record and Play."),
                tr("The I button lets you hear an audio track's input. Use headphones to avoid feedback."),
                tr("With Count-in on, recording starts after one bar of clicks."),
                tr("Capture Recording turns what you just played on a MIDI keyboard into a region."),
            ] {
                ui.label(line);
                ui.add_space(4.0);
            }
        });
}
