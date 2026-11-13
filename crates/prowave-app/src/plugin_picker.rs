//! The Audio FX plug-in picker (Logic: the insert slot's plug-in menu): a search field, the
//! categories as submenus, and a flat result list while a search is typed.
//!
//! The categories are Logic's, in its order, so the menu reads the same as a Logic channel
//! strip's. Categories with no effect yet are shown disabled rather than hidden, which keeps
//! the list stable as effects are added. Scanned VST3, Audio Unit and CLAP plug-ins join the
//! same list, and the search matches all of them.
use crate::i18n::tr;
use egui::Ui;
use prowave_engine::FxKind;

/// Logic's Audio FX categories, in the order of its menu.
pub const CATEGORIES: [&str; 14] = [
    "Amps and Pedals",
    "Delay",
    "Distortion",
    "Dynamics",
    "EQ",
    "Filter",
    "Imaging",
    "Metering",
    "Modulation",
    "Multi Effects",
    "Pitch",
    "Reverb",
    "Specialized",
    "Utility",
];

/// What the user chose in the picker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pick {
    /// A built-in effect.
    Effect(FxKind),
    /// A scanned hosted plug-in, by its class key.
    Hosted(String),
}

/// A scanned hosted plug-in as the picker lists it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hosted {
    /// The class key saved with the project.
    pub key: String,
    pub name: String,
    /// The manufacturer, for the menu's vendor submenus.
    pub vendor: String,
    /// The format's short label (`VST3`, `Audio Unit`, `CLAP`).
    pub format: String,
}

/// The menu heading for a plug-in format, as Logic names the categories.
fn format_heading(format: &str) -> &'static str {
    match format {
        "Audio Unit" => "Audio Units",
        "VST3" => "VST3",
        "CLAP" => "CLAP",
        _ => "Plug-ins",
    }
}

/// Groups hosted plug-ins as Logic's menu does: format, then manufacturer, each sorted.
type HostedTree<'a> = Vec<(&'static str, Vec<(String, Vec<&'a Hosted>)>)>;

fn hosted_tree(hosted: &[Hosted]) -> HostedTree<'_> {
    let mut tree: HostedTree<'_> = Vec::new();
    for plugin in hosted {
        let heading = format_heading(&plugin.format);
        let format = match tree.iter_mut().find(|(name, _)| *name == heading) {
            Some((_, formats)) => formats,
            None => {
                tree.push((heading, Vec::new()));
                &mut tree.last_mut().expect("just pushed").1
            }
        };
        let vendor = if plugin.vendor.trim().is_empty() {
            "Unknown".to_string()
        } else {
            plugin.vendor.clone()
        };
        match format.iter_mut().find(|(name, _)| *name == vendor) {
            Some((_, plugins)) => plugins.push(plugin),
            None => format.push((vendor, vec![plugin])),
        }
    }
    // Logic's order: Audio Units, VST3, CLAP; then manufacturers and names alphabetically.
    let rank = |heading: &str| match heading {
        "Audio Units" => 0,
        "VST3" => 1,
        "CLAP" => 2,
        _ => 3,
    };
    tree.sort_by_key(|(heading, _)| rank(heading));
    for (_, vendors) in &mut tree {
        vendors.sort_by_key(|(vendor, _)| vendor.to_lowercase());
        for (_, plugins) in vendors {
            plugins.sort_by_key(|plugin| plugin.name.to_lowercase());
        }
    }
    tree
}

/// Draws the scanned plug-ins as format → manufacturer → plug-in submenus. Each level has
/// limited height and scrolls, so a large plug-in library stays usable.
fn draw_hosted_menu(ui: &mut Ui, hosted: &[Hosted], picked: &mut Option<Pick>) {
    for (heading, vendors) in hosted_tree(hosted) {
        ui.menu_button(heading, |ui| {
            ui.set_min_width(220.0);
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for (vendor, plugins) in &vendors {
                        ui.menu_button(vendor, |ui| {
                            ui.set_min_width(220.0);
                            egui::ScrollArea::vertical()
                                .max_height(420.0)
                                .show(ui, |ui| {
                                    for plugin in plugins {
                                        if hosted_row(ui, plugin) {
                                            *picked = Some(Pick::Hosted(plugin.key.clone()));
                                        }
                                    }
                                });
                        });
                    }
                });
        });
    }
}

/// The category Logic files `kind` under.
pub fn category(kind: FxKind) -> &'static str {
    match kind {
        FxKind::Amp => "Amps and Pedals",
        FxKind::Delay => "Delay",
        FxKind::Overdrive => "Distortion",
        FxKind::Compressor | FxKind::MultiComp | FxKind::Gate | FxKind::Limiter => "Dynamics",
        FxKind::ChannelEq | FxKind::MultiEq => "EQ",
        FxKind::Filter => "Filter",
        FxKind::Chorus | FxKind::Phaser => "Modulation",
        FxKind::Reverb => "Reverb",
    }
}

/// Whether `kind` matches a search: its name, ignoring case and surrounding spaces. An empty
/// search matches everything.
pub fn matches(kind: FxKind, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || kind.name().to_lowercase().contains(&query)
}

/// Whether a hosted plug-in matches a search: its name or its format.
pub fn matches_hosted(plugin: &Hosted, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || plugin.name.to_lowercase().contains(&query)
        || plugin.format.to_lowercase().contains(&query)
}

/// Shows one hosted plug-in as a row: the name, with the format in parentheses.
fn hosted_row(ui: &mut Ui, plugin: &Hosted) -> bool {
    ui.button(format!("{}  ({})", plugin.name, plugin.format))
        .clicked()
}

/// Draws the picker's search field, focused (with its text selected) when asked.
fn search_field(ui: &mut Ui, search: &mut String, focus: bool) {
    let output = egui::TextEdit::singleline(search)
        .hint_text(tr("Search"))
        .desired_width(f32::INFINITY)
        .show(ui);
    if focus {
        output.response.request_focus();
        if !search.is_empty() {
            // Select the previous search so typing replaces it.
            let mut state = output.state;
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(search.chars().count()),
                )));
            state.store(ui.ctx(), output.response.id);
        }
    }
}

/// Draws the picker into `ui` and returns the choice, if the user made one. `search` is kept
/// by the caller so the field survives the frames the menu is open. `focus` selects the search
/// field (and its text) when the picker has just opened, so typing can start immediately.
pub fn show(ui: &mut Ui, search: &mut String, hosted: &[Hosted], focus: bool) -> Option<Pick> {
    ui.set_min_width(200.0);
    search_field(ui, search, focus);
    let mut picked = None;
    if search.trim().is_empty() {
        egui::ScrollArea::vertical()
            .max_height(480.0)
            .show(ui, |ui| {
                for name in CATEGORIES {
                    let kinds: Vec<FxKind> = FxKind::ALL
                        .iter()
                        .copied()
                        .filter(|kind| category(*kind) == name)
                        .collect();
                    ui.add_enabled_ui(!kinds.is_empty(), |ui| {
                        ui.menu_button(tr(name), |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(420.0)
                                .show(ui, |ui| {
                                    for kind in kinds {
                                        if ui.button(kind.name()).clicked() {
                                            picked = Some(Pick::Effect(kind));
                                        }
                                    }
                                });
                        });
                    });
                }
                if !hosted.is_empty() {
                    ui.separator();
                    draw_hosted_menu(ui, hosted, &mut picked);
                }
            });
    } else {
        egui::ScrollArea::vertical()
            .max_height(480.0)
            .show(ui, |ui| {
                let mut any = false;
                for kind in FxKind::ALL {
                    if matches(kind, search) {
                        any = true;
                        if ui.button(kind.name()).clicked() {
                            picked = Some(Pick::Effect(kind));
                        }
                    }
                }
                for plugin in hosted {
                    if matches_hosted(plugin, search) {
                        any = true;
                        if hosted_row(ui, plugin) {
                            picked = Some(Pick::Hosted(plugin.key.clone()));
                        }
                    }
                }
                if !any {
                    ui.weak(tr("No plug-ins match"));
                }
            });
    }
    picked
}

/// Draws the instrument picker: the scanned instrument plug-ins, filtered by the search field.
/// Used by the Instrument slot instead of the Audio FX categories.
pub fn show_instruments(
    ui: &mut Ui,
    search: &mut String,
    hosted: &[Hosted],
    focus: bool,
) -> Option<Pick> {
    ui.set_min_width(200.0);
    search_field(ui, search, focus);
    let mut picked = None;
    let searching = !search.trim().is_empty();
    if searching {
        egui::ScrollArea::vertical()
            .max_height(480.0)
            .show(ui, |ui| {
                let mut any = false;
                for plugin in hosted {
                    if matches_hosted(plugin, search) {
                        any = true;
                        if hosted_row(ui, plugin) {
                            picked = Some(Pick::Hosted(plugin.key.clone()));
                        }
                    }
                }
                if !any {
                    ui.weak(tr("No plug-ins match"));
                }
            });
    } else if hosted.is_empty() {
        ui.weak(tr("No plug-ins match"));
    } else {
        egui::ScrollArea::vertical()
            .max_height(480.0)
            .show(ui, |ui| draw_hosted_menu(ui, hosted, &mut picked));
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_is_in_a_category_of_the_menu() {
        for kind in FxKind::ALL {
            assert!(
                CATEGORIES.contains(&category(kind)),
                "{kind:?} is in {:?}, which the menu does not show",
                category(kind)
            );
        }
    }

    #[test]
    fn the_categories_follow_logics_menu() {
        assert_eq!(CATEGORIES[0], "Amps and Pedals");
        assert_eq!(CATEGORIES[1], "Delay");
        assert_eq!(CATEGORIES[4], "EQ");
        assert_eq!(CATEGORIES[11], "Reverb");
    }

    #[test]
    fn typing_a_search_lists_the_matching_effects() {
        use crate::ui_test::Harness;
        let mut h = Harness::new(400.0, 600.0);
        let mut search = String::new();
        let mut picked = None;
        let keep = |ui: &mut Ui, search: &mut String, picked: &mut Option<Pick>| {
            if let Some(found) = show(ui, search, &[], false) {
                *picked = Some(found);
            }
        };
        h.frame(vec![], |ui| keep(ui, &mut search, &mut picked));
        // The search field is the first widget, at the top left.
        h.click(egui::pos2(80.0, 12.0), |ui| {
            keep(ui, &mut search, &mut picked)
        });
        h.frame(vec![egui::Event::Text("channel".to_string())], |ui| {
            keep(ui, &mut search, &mut picked)
        });
        assert_eq!(search, "channel", "the field keeps what was typed");
        assert_eq!(picked, None, "nothing is chosen yet");
        // The one result is the first row below the field; its exact band depends on the
        // text style, so try the few rows it can occupy.
        for y in [26.0, 30.0, 34.0, 38.0, 42.0, 46.0] {
            h.click(egui::pos2(80.0, y), |ui| keep(ui, &mut search, &mut picked));
            if picked.is_some() {
                break;
            }
        }
        assert_eq!(picked, Some(Pick::Effect(FxKind::ChannelEq)));
    }

    #[test]
    fn search_matches_a_name_ignoring_case_and_spaces() {
        assert!(matches(FxKind::ChannelEq, " channel "));
        assert!(matches(FxKind::ChannelEq, "EQ"));
        assert!(matches(FxKind::MultiComp, "band"));
        assert!(!matches(FxKind::Delay, "reverb"));
        assert!(matches(FxKind::Delay, ""), "an empty search shows all");
    }

    #[test]
    fn the_search_field_is_focused_and_its_text_selected_when_the_picker_opens() {
        use crate::ui_test::Harness;
        let mut h = Harness::new(400.0, 600.0);
        let mut search = String::from("old");
        let mut first = true;
        let keep = |ui: &mut Ui, search: &mut String, first: &mut bool| {
            let _ = show(ui, search, &[], std::mem::take(first));
        };
        h.frame(vec![], |ui| keep(ui, &mut search, &mut first));
        h.frame(vec![egui::Event::Text("delay".to_string())], |ui| {
            keep(ui, &mut search, &mut first)
        });
        assert_eq!(search, "delay", "typing replaces the selected search");
    }

    #[test]
    fn hosted_plug_ins_group_by_format_then_vendor() {
        let hosted = vec![
            Hosted {
                key: "vst3:b".into(),
                name: "Beta".into(),
                vendor: "Zeta".into(),
                format: "VST3".into(),
            },
            Hosted {
                key: "au:a".into(),
                name: "Alpha".into(),
                vendor: "Apple".into(),
                format: "Audio Unit".into(),
            },
            Hosted {
                key: "au:b".into(),
                name: "Beta AU".into(),
                vendor: "Apple".into(),
                format: "Audio Unit".into(),
            },
        ];
        let tree = hosted_tree(&hosted);
        assert_eq!(tree[0].0, "Audio Units");
        assert_eq!(tree[0].1[0].0, "Apple");
        assert_eq!(tree[0].1[0].1.len(), 2);
        assert_eq!(tree[0].1[0].1[0].name, "Alpha", "names are sorted");
        assert_eq!(tree[1].0, "VST3");
        assert_eq!(tree[1].1[0].0, "Zeta");
    }

    #[test]
    fn scanned_plug_ins_are_listed_and_searchable() {
        use crate::ui_test::Harness;
        let hosted = [Hosted {
            key: "au:pro-q".into(),
            name: "Pro-Q".into(),
            vendor: "FabFilter".into(),
            format: "Audio Unit".into(),
        }];
        assert!(matches_hosted(&hosted[0], "pro-q"));
        assert!(matches_hosted(&hosted[0], "audio"));
        assert!(!matches_hosted(&hosted[0], "vst3"));

        let mut h = Harness::new(400.0, 600.0);
        let mut search = String::new();
        let mut picked = None;
        let mut first = true;
        let keep =
            |ui: &mut Ui, search: &mut String, picked: &mut Option<Pick>, first: &mut bool| {
                if let Some(found) = show(ui, search, &hosted, std::mem::take(first)) {
                    *picked = Some(found);
                }
            };
        h.frame(vec![], |ui| keep(ui, &mut search, &mut picked, &mut first));
        h.frame(vec![egui::Event::Text("pro-q".to_string())], |ui| {
            keep(ui, &mut search, &mut picked, &mut first)
        });
        for y in [26.0, 30.0, 34.0, 38.0, 42.0, 46.0] {
            h.click(egui::pos2(80.0, y), |ui| {
                keep(ui, &mut search, &mut picked, &mut first)
            });
            if picked.is_some() {
                break;
            }
        }
        assert_eq!(picked, Some(Pick::Hosted("au:pro-q".to_string())));
    }
}
