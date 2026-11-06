//! The Project Settings window (Logic Pro: File > Project Settings): one tab each for General,
//! Synchronization, Metronome, Recording, Tuning, Audio and MIDI.
//!
//! Every control changes something the project or the engine really uses:
//! - General: the project's name.
//! - Synchronization: the SMPTE frame rate and start time, and the MIDI Time Code ports (the
//!   same entries as the right-click menu of the SMPTE time).
//! - Metronome: the click's volume and whether the first beat of a bar clicks higher.
//! - Recording: the bars of count-in before recording.
//! - Tuning: the reference pitch of software instruments, A4 = 427 to 453 Hz.
//! - Audio: which output the engine plays on (read only: the system's default output is used;
//!   inputs are chosen on each audio track).
//! - MIDI: which MIDI input plays live notes.

use crate::i18n::{tr, trf};
use crate::model::{
    Project, DEFAULT_CLICK_DB, MAX_COUNT_IN_BARS, MAX_TUNING_HZ, MIN_CLICK_DB, MIN_TUNING_HZ,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Synchronization,
    Metronome,
    Recording,
    Tuning,
    Audio,
    Midi,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 7] = [
        Self::General,
        Self::Synchronization,
        Self::Metronome,
        Self::Recording,
        Self::Tuning,
        Self::Audio,
        Self::Midi,
    ];

    /// The tab a menu entry opens ("Metronome…" opens the Metronome tab).
    pub fn from_menu(label: &str) -> Option<Self> {
        Some(match label {
            "General…" => Self::General,
            "Synchronization…" => Self::Synchronization,
            "Metronome…" => Self::Metronome,
            "Recording…" => Self::Recording,
            "Tuning…" => Self::Tuning,
            "Audio…" => Self::Audio,
            "MIDI…" => Self::Midi,
            _ => return None,
        })
    }

    fn title(self) -> String {
        match self {
            Self::General => tr("General"),
            Self::Synchronization => tr("Synchronization"),
            Self::Metronome => tr("Metronome"),
            Self::Recording => tr("Recording"),
            Self::Tuning => tr("Tuning"),
            Self::Audio => tr("Audio"),
            Self::Midi => tr("MIDI"),
        }
        .to_string()
    }
}

/// The longest project name the window accepts.
const MAX_NAME_CHARS: usize = 128;

/// Draws the window when a tab is chosen. A change bumps nothing itself: the settings are read
/// by the engine sync on the next frame.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut crate::UiState) {
    let Some(mut tab) = s.settings_tab else {
        return;
    };
    let mut open = true;
    egui::Window::new(tr("Project Settings"))
        .id(egui::Id::unique("project_settings_window"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for t in SettingsTab::ALL {
                    if ui.selectable_label(tab == t, t.title()).clicked() {
                        tab = t;
                    }
                }
            });
            ui.separator();
            ui.set_min_width(360.0);
            match tab {
                SettingsTab::General => general(ui, p),
                SettingsTab::Synchronization => crate::sync_menu::items(ui, p),
                SettingsTab::Metronome => metronome(ui, p),
                SettingsTab::Recording => recording(ui, p),
                SettingsTab::Tuning => tuning(ui, p),
                SettingsTab::Audio => audio(ui, s.audio_info.as_ref()),
                SettingsTab::Midi => midi(ui, p),
            }
        });
    s.settings_tab = open.then_some(tab);
}

fn general(ui: &mut egui::Ui, p: &mut Project) {
    ui.horizontal(|ui| {
        ui.label(tr("Project Name"));
        let mut name = p.name.clone();
        if ui
            .add(egui::TextEdit::singleline(&mut name).char_limit(MAX_NAME_CHARS))
            .changed()
        {
            p.name = name;
        }
    });
}

fn metronome(ui: &mut egui::Ui, p: &mut Project) {
    ui.horizontal(|ui| {
        ui.label(tr("Click Volume"));
        ui.add(egui::Slider::new(&mut p.click_volume_db, MIN_CLICK_DB..=0.0).suffix(" dB"));
        if ui.small_button(tr("Reset")).clicked() {
            p.click_volume_db = DEFAULT_CLICK_DB;
        }
    });
    ui.checkbox(
        &mut p.click_accent,
        tr("Accent First Beat of Each Bar").to_string(),
    );
}

fn recording(ui: &mut egui::Ui, p: &mut Project) {
    ui.add_enabled_ui(p.count_in, |ui| {
        ui.horizontal(|ui| {
            ui.label(tr("Count-in Bars"));
            ui.add(egui::Slider::new(
                &mut p.count_in_bars,
                1..=MAX_COUNT_IN_BARS,
            ));
        });
    });
    // The count-in itself is the purple "1234" button of the control bar.
    ui.checkbox(&mut p.count_in, tr("Count-in").to_string());
}

fn tuning(ui: &mut egui::Ui, p: &mut Project) {
    ui.horizontal(|ui| {
        ui.label(tr("Reference Pitch (A4)"));
        ui.add(
            egui::DragValue::new(&mut p.tuning_hz)
                .range(MIN_TUNING_HZ..=MAX_TUNING_HZ)
                .speed(0.1)
                .fixed_decimals(1)
                .suffix(" Hz"),
        );
        if ui.small_button(tr("Reset")).clicked() {
            p.tuning_hz = 440.0;
        }
    });
}

fn audio(ui: &mut egui::Ui, info: Option<&(String, u32)>) {
    if let Some((device, rate)) = info {
        ui.label(trf("Output Device: {}", &[device]));
        ui.label(trf("Sample Rate: {} Hz", &[rate]));
    }
    ui.label(tr(
        "Inputs are chosen with the Input slot of each audio track.",
    ));
}

fn midi(ui: &mut egui::Ui, p: &mut Project) {
    ui.horizontal(|ui| {
        ui.label(tr("MIDI Input"));
        let shown = p
            .midi_in_port
            .clone()
            .unwrap_or_else(|| tr("Automatic").to_string());
        egui::ComboBox::from_id_salt("midi_input_port")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(p.midi_in_port.is_none(), tr("Automatic"))
                    .clicked()
                {
                    p.midi_in_port = None;
                }
                for name in crate::mtc_in::ports() {
                    let on = p.midi_in_port.as_deref() == Some(&name);
                    if ui.selectable_label(on, &name).clicked() {
                        p.midi_in_port = Some(name);
                    }
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_settings_entry_of_the_menus_opens_its_tab() {
        let mut seen = Vec::new();
        for label in crate::menus::all_entries() {
            if let Some(tab) = SettingsTab::from_menu(label) {
                seen.push(tab);
            }
        }
        for tab in SettingsTab::ALL {
            assert!(seen.contains(&tab), "{tab:?} has a menu entry");
        }
    }

    /// Runs the window for a few frames over `p` and `s`.
    fn run(p: &mut Project, s: &mut crate::UiState) {
        let mut h = crate::ui_test::Harness::new(900.0, 600.0);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), p, s));
        }
    }

    #[test]
    fn every_tab_draws_and_the_window_stays_open() {
        for tab in SettingsTab::ALL {
            let (mut p, mut s) = (Project::demo(), crate::UiState::new());
            s.settings_tab = Some(tab);
            s.audio_info = Some(("Speakers".into(), 48_000));
            run(&mut p, &mut s);
            assert_eq!(s.settings_tab, Some(tab));
        }
    }

    #[test]
    fn nothing_is_drawn_while_no_tab_is_chosen() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        let before = p.click_volume_db;
        run(&mut p, &mut s);
        assert!(s.settings_tab.is_none() && p.click_volume_db == before);
    }
}
