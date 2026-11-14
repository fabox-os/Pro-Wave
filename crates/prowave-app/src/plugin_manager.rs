//! Window > Plugin Manager (Logic Pro: Settings > Plug-in Manager), laid out as Logic's: the
//! manufacturers in a list on the left ("Show All" first), the plug-ins of the chosen one in a
//! table (Name, Type, Manufacturer), a search field at the top right, and the scan and Done
//! buttons along the bottom. Type is the plug-in's format (Logic has only Audio Units, and
//! shows their kind there). The Latency column is the delay a plug-in in use adds, in samples.
//!
//! Not done: Logic's Version, Compatibility and Use columns (turning single plug-ins off).
use crate::i18n::{tr, trf};
use crate::mixer::PLUGIN_NOT_FOUND;
use crate::model::Project;
use crate::plugins::{self, Format, PluginInfo, Scan};
use crate::UiState;

const ROW: f32 = 18.0;
/// The widths of the Type, Manufacturer and Latency columns; Name takes the rest.
const COLUMNS: [f32; 3] = [78.0, 150.0, 64.0];

/// One row of the table: four cells, the first as wide as what is left.
fn row(ui: &mut egui::Ui, cells: [egui::RichText; 4]) {
    ui.horizontal(|ui| {
        let name = (ui.available_width() - COLUMNS.iter().sum::<f32>() - 24.0).max(80.0);
        let widths = [name, COLUMNS[0], COLUMNS[1], COLUMNS[2]];
        for (cell, width) in cells.into_iter().zip(widths) {
            ui.allocate_ui_with_layout(
                egui::vec2(width, ROW),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(width);
                    ui.add(egui::Label::new(cell).truncate());
                },
            );
        }
    });
}

/// The plug-ins the project uses that are not installed here, once each: name and format.
fn not_found(p: &Project) -> Vec<(String, Format)> {
    let mut found: Vec<(String, Format)> = Vec::new();
    let inserts = p
        .tracks
        .iter()
        .flat_map(|t| t.inserts.iter())
        .chain(p.master_inserts.iter());
    for insert in inserts {
        if insert.plugin == 0 || !plugins::missing(insert.plugin) {
            continue;
        }
        if let Some(info) = plugins::info(insert.plugin) {
            let used = (info.name, info.format);
            if !found.contains(&used) {
                found.push(used);
            }
        }
    }
    found
}

/// The Plugin Manager window, while `UiState::show_plugin_manager` is on.
pub fn show(ctx: &egui::Context, p: &Project, s: &mut UiState) {
    if !s.show_plugin_manager {
        return;
    }
    plugins::scan(false);
    let status = plugins::scan_status();
    let catalog = plugins::catalog();
    let loaded = plugins::loaded();
    let in_use = not_found(p);
    let mut open = true;
    let mut done = false;
    egui::Window::new(tr("Plugin Manager"))
        .id(egui::Id::unique("plugin_manager_window"))
        .open(&mut open)
        .default_width(680.0)
        .default_height(440.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                match status {
                    Scan::Running(done, total) => {
                        ui.weak(format!("{} {done} / {total}", tr("Scanning plug-ins…")));
                        // The scan runs on another thread: keep drawing until it is done.
                        ui.ctx().request_repaint();
                    }
                    _ => {
                        ui.weak(trf("{} plug-ins installed", &[&catalog.len()]));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut s.plugin_manager_search)
                            .hint_text(tr("Search"))
                            .desired_width(180.0),
                    );
                });
            });
            ui.separator();
            egui::Panel::bottom("plugin_manager_buttons").show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let scanning = matches!(status, Scan::Running(..));
                    if ui
                        .add_enabled(!scanning, egui::Button::new(tr("Rescan")))
                        .clicked()
                    {
                        plugins::scan(true);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr("Done")).clicked() {
                            done = true;
                        }
                    });
                });
            });
            // The catalog is sorted by manufacturer, so one pass names each once.
            let mut vendors: Vec<&str> = catalog.iter().map(|i| i.vendor.as_str()).collect();
            vendors.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            egui::Panel::left("plugin_manager_vendors")
                .resizable(false)
                .exact_size(170.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("plugin_manager_vendor_list")
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let all = s.plugin_manager_vendor.is_none();
                            if ui.selectable_label(all, tr("Show All")).clicked() {
                                s.plugin_manager_vendor = None;
                            }
                            for vendor in vendors {
                                let chosen = s.plugin_manager_vendor.as_deref() == Some(vendor);
                                let label = if vendor.is_empty() { "—" } else { vendor };
                                if ui.selectable_label(chosen, label).clicked() {
                                    s.plugin_manager_vendor = Some(vendor.to_string());
                                }
                            }
                        });
                });
            egui::CentralPanel::default().show(ui, |ui| {
                // The project's plug-ins that are not installed: they keep their place in the
                // project and pass the sound through, until the plug-in is installed again.
                if !in_use.is_empty() {
                    ui.weak(tr("In Use"));
                    for (name, format) in &in_use {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(name).color(PLUGIN_NOT_FOUND));
                            ui.weak(format.name());
                            ui.label(egui::RichText::new(tr("Not found")).color(PLUGIN_NOT_FOUND));
                        });
                    }
                    ui.separator();
                }
                let query = s.plugin_manager_search.trim().to_lowercase();
                let rows: Vec<&PluginInfo> = catalog
                    .iter()
                    .filter(|info| {
                        s.plugin_manager_vendor
                            .as_ref()
                            .is_none_or(|v| v.eq_ignore_ascii_case(&info.vendor))
                    })
                    .filter(|info| {
                        query.is_empty()
                            || info.name.to_lowercase().contains(&query)
                            || info.vendor.to_lowercase().contains(&query)
                    })
                    .collect();
                let head = |text: &str| egui::RichText::new(text).weak();
                row(
                    ui,
                    [
                        head(tr("Name")),
                        head(tr("Type")),
                        head(tr("Manufacturer")),
                        head(tr("Latency")),
                    ],
                );
                ui.separator();
                if rows.is_empty() && status == Scan::Done {
                    ui.weak(tr("No plug-ins match"));
                }
                egui::ScrollArea::vertical()
                    .id_salt("plugin_manager_table")
                    .auto_shrink(false)
                    .show_rows(ui, ROW, rows.len(), |ui, range| {
                        for info in &rows[range] {
                            let latency = loaded
                                .iter()
                                .find(|(name, format, _)| {
                                    name == &info.name && *format == info.format
                                })
                                .map_or(String::new(), |(_, _, samples)| samples.to_string());
                            row(
                                ui,
                                [
                                    egui::RichText::new(&info.name),
                                    egui::RichText::new(info.format.name()),
                                    egui::RichText::new(&info.vendor),
                                    egui::RichText::new(latency),
                                ],
                            );
                        }
                    });
            });
        });
    if !open || done {
        s.show_plugin_manager = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_opens_from_the_menu_and_closes_again() {
        let mut p = crate::model::Project::demo();
        let mut s = UiState::new();
        assert!(crate::menus::enabled("Plugin Manager", &p, &s));
        crate::menus::apply("Plugin Manager", &mut p, &mut s);
        assert!(s.show_plugin_manager);
        let mut h = crate::ui_test::Harness::new(900.0, 600.0);
        h.settle(|ui| show(ui.ctx(), &p, &mut s));
        assert!(s.show_plugin_manager, "drawing it does not close it");
        assert_eq!(
            plugins::scan_status(),
            Scan::Done,
            "the window starts the scan"
        );
    }
}
