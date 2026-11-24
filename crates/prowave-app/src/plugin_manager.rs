//! Window > Plugin Manager (Logic Pro: Settings > Plug-in Manager), laid out as Logic's: the
//! manufacturers in a list on the left ("Show All" first), the plug-ins of the chosen one in a
//! table (Name, Type, Manufacturer), a search field at the top right, and the scan and Done
//! buttons along the bottom. Type is the plug-in's format (Logic has only Audio Units, and
//! shows their kind there). The Latency column is the delay a plug-in in use adds, in samples.
//!
//! Hosted CLAP, VST3 and Audio Unit plug-ins and LADSPA effects are listed together; a LADSPA
//! library that could not be read, or that was left alone after failing three times, is listed
//! at the bottom with the folders the scan looks in.
//!
//! Not done: Logic's Version, Compatibility and Use columns (turning single plug-ins off).
use crate::hosted_plugins::{self, Scan};
use crate::i18n::{tr, trf};
use crate::mixer::PLUGIN_NOT_FOUND;
use crate::model::Project;
use crate::plugin_ui_prefs::PluginUiMode;
use crate::plugins;
use crate::vamp;
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
fn not_found(p: &Project) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let inserts = p
        .tracks
        .iter()
        .flat_map(|t| t.inserts.iter())
        .chain(p.master_inserts.iter());
    for insert in inserts {
        let used = if insert.hosted != 0 && hosted_plugins::missing(insert.hosted) {
            hosted_plugins::info(insert.hosted).map(|i| (i.name, i.format.name().to_string()))
        } else if let Some(id) = insert.plugin {
            plugins::lookup(id)
                .filter(|r| plugins::resolve_library(r).is_none())
                .map(|r| (r.name, "LADSPA".to_string()))
        } else {
            None
        };
        if let Some(used) = used {
            if !found.contains(&used) {
                found.push(used);
            }
        }
    }
    found
}

/// One plug-in of the table, whatever its format.
struct Entry {
    name: String,
    format: String,
    vendor: String,
    latency: String,
}

/// Everything installed: hosted plug-ins, LADSPA effects and Vamp analysers, by manufacturer.
fn entries(
    hosted: &[hosted_plugins::PluginInfo],
    ladspa: &plugins::Catalog,
    vamp_catalog: &vamp::Catalog,
) -> Vec<Entry> {
    let loaded = hosted_plugins::loaded();
    let mut all: Vec<Entry> = hosted
        .iter()
        .map(|info| Entry {
            latency: loaded
                .iter()
                .find(|(name, format, _)| name == &info.name && *format == info.format)
                .map_or(String::new(), |(_, _, samples)| samples.to_string()),
            name: info.name.clone(),
            format: info.format.name().to_string(),
            vendor: info.vendor.clone(),
        })
        .collect();
    all.extend(ladspa.effects().map(|info| Entry {
        name: info.name.clone(),
        format: "LADSPA".to_string(),
        vendor: info.maker.clone(),
        latency: String::new(),
    }));
    all.extend(vamp_catalog.plugins.iter().map(|info| Entry {
        name: info.name.clone(),
        format: "Vamp".to_string(),
        vendor: info.maker.clone(),
        latency: String::new(),
    }));
    all.sort_by_cached_key(|e| (e.vendor.to_lowercase(), e.name.to_lowercase()));
    all
}

/// The LADSPA libraries that could not be read, and where they are looked for.
fn ladspa_problems(ui: &mut egui::Ui, ladspa: &plugins::Catalog) {
    if !ladspa.failed.is_empty() {
        ui.strong(tr("Could not be read"));
        for (path, why) in &ladspa.failed {
            ui.label(format!("{} — {why}", path.display()));
        }
    }
    if !ladspa.skipped.is_empty() {
        ui.strong(tr("Left alone after failing three times"));
        for path in &ladspa.skipped {
            ui.label(path.display().to_string());
        }
    }
    ui.strong(tr("Plug-in folders"));
    for dir in plugins::search_paths() {
        ui.weak(dir.display().to_string());
    }
}

/// The Vamp libraries that could not be read, and where analysers are looked for.
fn vamp_problems(ui: &mut egui::Ui, vamp_catalog: &vamp::Catalog) {
    if !vamp_catalog.failed.is_empty() {
        ui.strong(tr("Could not be read"));
        for (path, why) in &vamp_catalog.failed {
            ui.label(format!("{} — {why}", path.display()));
        }
    }
    if !vamp_catalog.skipped.is_empty() {
        ui.strong(tr("Left alone after failing three times"));
        for path in &vamp_catalog.skipped {
            ui.label(path.display().to_string());
        }
    }
    ui.strong(tr("Plug-in folders"));
    for dir in vamp::search_paths() {
        ui.weak(dir.display().to_string());
    }
}

/// The Plugin Manager window, while `UiState::show_plugin_manager` is on.
pub fn show(ctx: &egui::Context, p: &Project, s: &mut UiState) {
    if !s.show_plugin_manager {
        return;
    }
    hosted_plugins::scan(false);
    plugins::ensure_scanned();
    vamp::ensure_scanned();
    let status = hosted_plugins::scan_status();
    let ladspa = plugins::catalog();
    let vamp_catalog = vamp::catalog();
    let catalog = entries(&hosted_plugins::catalog(), &ladspa, &vamp_catalog);
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
                    _ if ladspa.scanning || vamp_catalog.scanning => {
                        ui.weak(tr("Scanning plug-ins…"));
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
                    let scanning = matches!(status, Scan::Running(..))
                        || ladspa.scanning
                        || vamp_catalog.scanning;
                    if ui
                        .add_enabled(!scanning, egui::Button::new(tr("Rescan")))
                        .clicked()
                    {
                        hosted_plugins::scan(true);
                        plugins::rescan(false);
                        vamp::rescan(false);
                    }
                    if ui
                        .add_enabled(
                            !scanning
                                && (!ladspa.skipped.is_empty() || !vamp_catalog.skipped.is_empty()),
                            egui::Button::new(tr("Retry Failed Plug-ins")),
                        )
                        .clicked()
                    {
                        plugins::rescan(true);
                        vamp::rescan(true);
                    }
                    ui.label(tr("Plug-in UI:"));
                    // Logic has no Linux editor setting; this is the portable sliders view by
                    // default, with the plug-in's own window available where it can be hosted.
                    let sliders = ui
                        .selectable_label(
                            s.mixer.preferred_plugin_ui == PluginUiMode::Sliders,
                            tr("Sliders"),
                        )
                        .clicked();
                    let editor = ui
                        .selectable_label(
                            s.mixer.preferred_plugin_ui == PluginUiMode::Editor,
                            tr("Editor"),
                        )
                        .clicked();
                    if sliders || editor {
                        let mode = if sliders {
                            PluginUiMode::Sliders
                        } else {
                            PluginUiMode::Editor
                        };
                        s.mixer.preferred_plugin_ui = mode;
                        s.mixer.plugin_view_override = None;
                        crate::plugin_ui_prefs::save(mode);
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
            vendors.sort_by_key(|v| v.to_lowercase());
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
                            ui.weak(format);
                            ui.label(egui::RichText::new(tr("Not found")).color(PLUGIN_NOT_FOUND));
                        });
                    }
                    ui.separator();
                }
                let query = s.plugin_manager_search.trim().to_lowercase();
                let rows: Vec<&Entry> = catalog
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
                if rows.is_empty()
                    && status == Scan::Done
                    && !ladspa.scanning
                    && !vamp_catalog.scanning
                {
                    ui.weak(tr("No plug-ins match"));
                }
                egui::ScrollArea::vertical()
                    .id_salt("plugin_manager_table")
                    .auto_shrink(false)
                    .show_rows(ui, ROW, rows.len(), |ui, range| {
                        for info in &rows[range] {
                            row(
                                ui,
                                [
                                    egui::RichText::new(&info.name),
                                    egui::RichText::new(&info.format),
                                    egui::RichText::new(&info.vendor),
                                    egui::RichText::new(&info.latency),
                                ],
                            );
                        }
                    });
                ui.add_space(6.0);
                egui::CollapsingHeader::new(tr("LADSPA scan"))
                    .id_salt("plugin_manager_ladspa")
                    .show(ui, |ui| ladspa_problems(ui, &ladspa));
                egui::CollapsingHeader::new(tr("Vamp scan"))
                    .id_salt("plugin_manager_vamp")
                    .show(ui, |ui| vamp_problems(ui, &vamp_catalog));
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
    fn entries_include_vamp_analysers() {
        let mut vamp_catalog = vamp::Catalog::default();
        vamp_catalog.plugins.push(prowave_vamp::PluginInfo {
            api_version: 2,
            identifier: "vamp-example:rms".into(),
            name: "Test RMS".into(),
            description: "A test analyser".into(),
            maker: "Pro Wave".into(),
            plugin_version: 1,
            copyright: "CC0".into(),
            input_domain: prowave_vamp::InputDomain::Time,
            parameters: Vec::new(),
            programs: Vec::new(),
        });
        let found = entries(&[], &plugins::Catalog::default(), &vamp_catalog);
        assert!(
            found
                .iter()
                .any(|e| e.name == "Test RMS" && e.format == "Vamp"),
            "the analyser is in the catalog"
        );
    }

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
            hosted_plugins::scan_status(),
            Scan::Done,
            "the window starts the scan"
        );
    }
}
