//! Window > Plugin Manager: the LADSPA effects found on this computer, the folders they were
//! looked for in, and the libraries that could not be read.
//!
//! Logic Pro equivalent: the Plug-in Manager. Libraries are read in a child process (see
//! `plugins.rs`), so a plug-in that crashes shows up here as "could not be read" instead of
//! closing the program; after three failures the library is left alone until **Retry Failed
//! Plug-ins** is pressed.
use crate::i18n::{tr, trf};
use crate::plugins::{self, Catalog, PluginInfo};
use crate::UiState;

/// The window, while `UiState::show_plugin_manager` is on.
pub fn show(ctx: &egui::Context, s: &mut UiState) {
    if !s.show_plugin_manager {
        return;
    }
    plugins::ensure_scanned();
    let catalog = plugins::catalog();
    let mut open = true;
    egui::Window::new(tr("Plugin Manager"))
        .id(egui::Id::unique("plugin_manager_window"))
        .open(&mut open)
        .default_width(560.0)
        .show(ctx, |ui| contents(ui, &catalog));
    if !open {
        s.show_plugin_manager = false;
    }
}

fn contents(ui: &mut egui::Ui, catalog: &Catalog) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!catalog.scanning, egui::Button::new(tr("Rescan")))
            .clicked()
        {
            plugins::rescan(false);
        }
        let blocked = !catalog.skipped.is_empty();
        if ui
            .add_enabled(
                blocked && !catalog.scanning,
                egui::Button::new(tr("Retry Failed Plug-ins")),
            )
            .clicked()
        {
            plugins::rescan(true);
        }
        if catalog.scanning || !catalog.scanned {
            ui.spinner();
            ui.label(tr("Scanning…"));
        } else {
            ui.label(trf("{} plug-ins", &[&catalog.plugins.len()]));
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .show(ui, |ui| {
            ui.strong(tr("Effects (LADSPA)"));
            let effects: Vec<&PluginInfo> = catalog.effects().collect();
            if effects.is_empty() {
                ui.weak(tr("No plug-ins found"));
            } else {
                egui::Grid::new("plugin_manager_effects")
                    .num_columns(5)
                    .striped(true)
                    .spacing([12.0, 3.0])
                    .show(ui, |ui| {
                        ui.label(tr("Name"));
                        ui.label(tr("Maker"));
                        ui.label(tr("Audio In/Out"));
                        ui.label(tr("Settings"));
                        ui.label(tr("File"));
                        ui.end_row();
                        for p in effects {
                            ui.label(&p.name);
                            ui.label(&p.maker);
                            ui.label(format!("{} / {}", p.audio_in, p.audio_out));
                            ui.label(p.controls.len().to_string());
                            ui.label(
                                p.library
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            )
                            .on_hover_text(p.library.display().to_string());
                            ui.end_row();
                        }
                    });
            }
            if !catalog.failed.is_empty() {
                ui.add_space(6.0);
                ui.strong(tr("Could not be read"));
                for (path, why) in &catalog.failed {
                    ui.label(format!("{} — {why}", path.display()));
                }
            }
            if !catalog.skipped.is_empty() {
                ui.add_space(6.0);
                ui.strong(tr("Left alone after failing three times"));
                for path in &catalog.skipped {
                    ui.label(path.display().to_string());
                }
            }
            ui.add_space(6.0);
            ui.strong(tr("Plug-in folders"));
            for dir in plugins::search_paths() {
                ui.weak(dir.display().to_string());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_draws_with_plugins_failures_and_an_empty_catalogue() {
        let mut s = UiState::new();
        s.show_plugin_manager = true;
        let mut h = crate::ui_test::Harness::new(900.0, 700.0);
        // Not scanned yet would start a scan (the test program is no scanner): mark it scanned.
        plugins::set_catalog(Catalog {
            scanned: true,
            ..Catalog::default()
        });
        h.frame(vec![], |ui| show(ui.ctx(), &mut s));
        let info = PluginInfo {
            library: "/usr/lib/ladspa/amp.so".into(),
            index: 0,
            unique_id: 1,
            label: "amp".into(),
            name: "Amplifier".into(),
            maker: "Someone".into(),
            audio_in: 1,
            audio_out: 1,
            controls: Vec::new(),
            latency_port: None,
        };
        plugins::set_catalog(Catalog {
            plugins: vec![info],
            failed: vec![("/x/bad.so".into(), "stopped the scan".into())],
            skipped: vec!["/x/worse.so".into()],
            scanned: true,
            scanning: false,
        });
        h.frame(vec![], |ui| show(ui.ctx(), &mut s));
        assert!(s.show_plugin_manager, "looking does not close it");
    }
}
