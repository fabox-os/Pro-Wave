//! Mix > Edit Groups…: every mix group in one window — its name, what it links (Volume, Pan,
//! Mute, Solo) and how many tracks it has. The mixer's Group popup edits the group of one
//! track at a time; this window is where the groups themselves live, as in Logic Pro's
//! Mix > Groups > "Edit Groups…" (where groups are also renamed).
//!
//! A group name is a project label: it is saved with the project and shown in the mixer's
//! Group popup. Ticking a link makes every track of the group follow that control; the app
//! applies that every frame in `groups.rs`.
use crate::i18n::{tr, trf};
use crate::model::{Project, MAX_GROUPS};
use crate::UiState;

/// The Mix > Edit Groups… window, while `UiState::show_groups` is on.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    if !s.show_groups {
        return;
    }
    let mut open = true;
    egui::Window::new(tr("Edit Groups…"))
        .id(egui::Id::unique("edit_groups_window"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            if p.groups.is_empty() {
                ui.label(tr("No groups yet"));
            }
            let mut delete = None;
            egui::Grid::new("edit_groups_grid")
                .num_columns(8)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    if !p.groups.is_empty() {
                        ui.label("");
                        ui.label("");
                        ui.strong(tr("Volume"));
                        ui.strong(tr("Pan"));
                        ui.strong(tr("Mute"));
                        ui.strong(tr("Solo"));
                        ui.strong(tr("Tracks"));
                        ui.label("");
                        ui.end_row();
                    }
                    for (i, g) in p.groups.iter_mut().enumerate() {
                        ui.label(trf("Group {}", &[&(i + 1)]));
                        // Renaming is a normal edit: the name is what the mixer's popup shows.
                        ui.add(
                            egui::TextEdit::singleline(&mut g.name)
                                .desired_width(140.0)
                                .char_limit(64),
                        );
                        ui.checkbox(&mut g.volume, tr("Volume"));
                        ui.checkbox(&mut g.pan, tr("Pan"));
                        ui.checkbox(&mut g.mute, tr("Mute"));
                        ui.checkbox(&mut g.solo, tr("Solo"));
                        let n = p.tracks.iter().filter(|t| t.group == Some(i)).count();
                        ui.label(n.to_string());
                        if ui.button(tr("Delete Group")).clicked() {
                            delete = Some(i);
                        }
                        ui.end_row();
                    }
                });
            // Deleting shifts the groups after it, so it waits until the rows are drawn.
            if let Some(i) = delete {
                p.delete_group(i);
            }
            ui.separator();
            let full = p.groups.len() >= MAX_GROUPS;
            if ui
                .add_enabled(!full, egui::Button::new(tr("New Group")))
                .clicked()
            {
                let n = p.groups.len() + 1;
                p.add_group(&trf("Group {}", &[&n]));
            }
        });
    if !open {
        s.show_groups = false;
    }
}
