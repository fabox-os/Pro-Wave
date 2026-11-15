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
//! - Audio: the output device and the I/O buffer size the engine plays on, with the latency
//!   they give (applied at once: the engine restarts on the choice). Inputs are still chosen on
//!   each audio track. One page, as the Devices page of Logic Pro's Settings > Audio.
//! - MIDI: which MIDI input plays live notes.

use crate::audio_prefs::AudioPrefs;
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
        s.audio_tab_open = false;
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
                SettingsTab::Audio => audio(ui, s),
                SettingsTab::Midi => midi(ui, p, &mut s.midi_panic),
            }
        });
    s.settings_tab = open.then_some(tab);
    // The Audio tab reads the system's outputs when it is opened, not every frame. The frame
    // after a switch draws the lists read now.
    let audio_open = s.settings_tab == Some(SettingsTab::Audio);
    if audio_open && !s.audio_tab_open {
        s.refresh_audio_list();
        ctx.request_repaint();
    }
    s.audio_tab_open = audio_open;
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

/// Logic Pro's Settings > Audio: rows of a right-aligned label and its control. A choice is
/// only asked for here (`UiState::audio_request`); `App` restarts the engine on it.
fn audio(ui: &mut egui::Ui, s: &mut crate::UiState) {
    let prefs = s.audio_prefs.clone();
    let info = s.audio_info.clone();
    let mut request = None;
    egui::Grid::new("audio_settings")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            row_label(ui, tr("Output Device"));
            let shown = prefs
                .device
                .as_deref()
                .and_then(|id| s.audio_devices.iter().find(|d| d.id == id))
                .map_or_else(|| tr("System Default").to_string(), |d| d.name.clone());
            egui::ComboBox::from_id_salt("audio_output_device")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(prefs.device.is_none(), tr("System Default"))
                        .clicked()
                    {
                        request = Some(device_choice(&prefs, None));
                    }
                    for d in &s.audio_devices {
                        let on = prefs.device.as_deref() == Some(d.id.as_str());
                        if ui.selectable_label(on, &d.name).clicked() {
                            request = Some(device_choice(&prefs, Some(d.id.clone())));
                        }
                    }
                });
            ui.end_row();

            row_label(ui, tr("I/O Buffer Size"));
            ui.horizontal(|ui| {
                let effective = prefs.buffer.or(info.as_ref().and_then(|i| i.buffer_frames));
                buffer_popup(ui, &prefs, s.audio_buffer_range, effective, &mut request);
                ui.label(tr("Samples"));
            });
            ui.end_row();

            row_label(ui, tr("Resulting Latency"));
            match info.as_ref().and_then(|i| i.latency_ms()) {
                Some(ms) => ui.label(trf("Output {} ms", &[&format!("{ms:.1}")])),
                None => ui.label(""),
            };
            ui.end_row();

            row_label(ui, tr("Sample Rate"));
            match info.as_ref() {
                Some(i) => ui.label(trf("{} Hz", &[&i.sample_rate])),
                None => ui.label(""),
            };
            ui.end_row();
        });
    ui.separator();
    ui.label(tr(
        "Inputs are chosen with the Input slot of each audio track.",
    ));
    if request.is_some() {
        s.audio_request = request;
    }
}

/// The I/O buffer size popup: the sizes the device takes, with the one in use shown. `effective`
/// is the size the engine runs at (the default's, when no size was chosen). Returns the button's
/// response.
fn buffer_popup(
    ui: &mut egui::Ui,
    prefs: &AudioPrefs,
    range: Option<(u32, u32)>,
    effective: Option<u32>,
    request: &mut Option<AudioPrefs>,
) -> egui::Response {
    let shown = effective.map_or_else(|| tr("Default").to_string(), |n| n.to_string());
    egui::ComboBox::from_id_salt("audio_buffer_size")
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for n in prowave_engine::buffer_choices(range) {
                if ui
                    .selectable_label(prefs.buffer == Some(n), n.to_string())
                    .clicked()
                {
                    *request = Some(prefs.with_buffer(Some(n)));
                }
            }
        })
        .response
}

/// The choice that plays on `device` (`None`: the system's default), keeping the buffer size
/// when that device takes it.
fn device_choice(prefs: &AudioPrefs, device: Option<String>) -> AudioPrefs {
    let range = prowave_engine::buffer_range(device.as_deref());
    prefs.with_device(device, range)
}

/// A label right-aligned in its column, as the rows of Logic Pro's Settings have them.
fn row_label(ui: &mut egui::Ui, text: &str) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(text);
    });
}

fn midi(ui: &mut egui::Ui, p: &mut Project, panic: &mut bool) {
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
    ui.separator();
    // Every note of every MIDI output the tracks use, on every channel (see midi_out.rs).
    if ui.button(tr("Send MIDI Panic")).clicked() {
        *panic = true;
    }
    ui.label(tr("Silences notes stuck on an external instrument."));
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
            s.audio_info = Some(crate::engine_sync::AudioInfo {
                device: "Speakers".into(),
                sample_rate: 48_000,
                buffer_frames: Some(128),
                output_latency_us: None,
            });
            run(&mut p, &mut s);
            assert_eq!(s.settings_tab, Some(tab));
        }
    }

    #[test]
    fn the_audio_tab_draws_its_rows_and_opens_the_buffer_popup() {
        use crate::engine_sync::AudioInfo;
        use prowave_engine::OutputDevice;
        let mut s = crate::UiState::new();
        s.settings_tab = Some(SettingsTab::Audio);
        // Set `audio_tab_open` so the lists below are kept instead of read from the system.
        s.audio_tab_open = true;
        s.audio_prefs = AudioPrefs {
            device: Some("desk".into()),
            buffer: Some(128),
        };
        s.audio_devices = vec![
            OutputDevice {
                id: "desk".into(),
                name: "Desk Speakers".into(),
            },
            OutputDevice {
                id: "usb".into(),
                name: "USB Interface".into(),
            },
        ];
        s.audio_buffer_range = Some((16, 2048));
        s.audio_info = Some(AudioInfo {
            device: "Desk Speakers".into(),
            sample_rate: 48_000,
            buffer_frames: Some(128),
            output_latency_us: Some(5_000),
        });
        let mut h = crate::ui_test::Harness::new(900.0, 600.0);
        let (prefs, range) = (s.audio_prefs.clone(), s.audio_buffer_range);
        let mut request = None;
        let mut button = None;
        for _ in 0..2 {
            h.frame(vec![], |ui| {
                button = Some(buffer_popup(ui, &prefs, range, Some(128), &mut request).rect);
            });
        }
        let rect = button.expect("the buffer size popup is drawn");
        h.click(rect.center(), |ui| {
            buffer_popup(ui, &prefs, range, Some(128), &mut request);
        });
        h.frame(vec![], |ui| {
            buffer_popup(ui, &prefs, range, Some(128), &mut request);
        });
        assert!(
            egui::Popup::is_any_open(&h.ctx),
            "the buffer size popup opened"
        );
        // Only a choice made in the popup asks for a change.
        assert!(request.is_none());
        // The whole tab draws with the lists and the info given above.
        h.frame(vec![], |ui| audio(ui, &mut s));
        assert!(s.audio_tab_open && s.audio_prefs.buffer == Some(128));
    }

    #[test]
    fn nothing_is_drawn_while_no_tab_is_chosen() {
        let (mut p, mut s) = (Project::demo(), crate::UiState::new());
        let before = p.click_volume_db;
        run(&mut p, &mut s);
        assert!(s.settings_tab.is_none() && p.click_volume_db == before);
    }
}
