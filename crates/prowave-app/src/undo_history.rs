//! Edit > Undo History…: the edits this session can go back to, and a click to jump there.
//!
//! The history keeps whole project states, so a step has no name of its own; the list numbers
//! them from the current state ("Undo 1" is one edit back).

use crate::i18n::{tr, trf};
use crate::model::Project;
use crate::undo::History;
use crate::UiState;

pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState, history: &mut History) {
    let mut open = s.show_undo_history;
    egui::Window::new(tr("Undo History…"))
        .id(egui::Id::unique("undo_history"))
        .open(&mut open)
        .default_size([240.0, 320.0])
        .show(ctx, |ui| {
            let (back, forward) = history.depths();
            let mut undo_to = None;
            let mut redo_to = None;
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    let _ = ui.selectable_label(true, tr("Current"));
                    for step in 1..=back {
                        if ui
                            .selectable_label(false, trf("Undo {}", &[&step]))
                            .clicked()
                        {
                            undo_to = Some(step);
                        }
                    }
                    for step in 1..=forward {
                        if ui
                            .selectable_label(false, trf("Redo {}", &[&step]))
                            .clicked()
                        {
                            redo_to = Some(step);
                        }
                    }
                });
            if back == 0 && forward == 0 {
                ui.label(tr("Nothing to undo yet"));
            }
            if let Some(steps) = undo_to {
                history.undo_steps(p, steps);
            }
            if let Some(steps) = redo_to {
                history.redo_steps(p, steps);
            }
        });
    s.show_undo_history = open;
}
