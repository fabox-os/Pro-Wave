//! The Audio FX plug-in picker (Logic: the insert slot's plug-in menu): a search field, the
//! categories as submenus, and a flat result list while a search is typed.
//!
//! The categories are Logic's, in its order, so the menu reads the same as a Logic channel
//! strip's. Categories with no effect yet are shown disabled rather than hidden, which keeps
//! the list stable as effects are added. Third-party plug-ins will join the same picker when
//! hosting lands (`plugins.rs`).
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    Effect(FxKind),
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

/// Draws the picker into `ui` and returns the choice, if the user made one. `search` is kept
/// by the caller so the field survives the frames the menu is open.
pub fn show(ui: &mut Ui, search: &mut String) -> Option<Pick> {
    ui.set_min_width(200.0);
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text(tr("Search"))
            .desired_width(f32::INFINITY),
    );
    let mut picked = None;
    if search.trim().is_empty() {
        for name in CATEGORIES {
            let kinds: Vec<FxKind> = FxKind::ALL
                .iter()
                .copied()
                .filter(|kind| category(*kind) == name)
                .collect();
            ui.add_enabled_ui(!kinds.is_empty(), |ui| {
                ui.menu_button(tr(name), |ui| {
                    for kind in kinds {
                        if ui.button(kind.name()).clicked() {
                            picked = Some(Pick::Effect(kind));
                        }
                    }
                });
            });
        }
    } else {
        let mut any = false;
        for kind in FxKind::ALL {
            if matches(kind, search) {
                any = true;
                if ui.button(kind.name()).clicked() {
                    picked = Some(Pick::Effect(kind));
                }
            }
        }
        if !any {
            ui.weak(tr("No plug-ins match"));
        }
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
            if let Some(found) = show(ui, search) {
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
}
