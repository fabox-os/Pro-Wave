//! The right-click menu of the SMPTE time in the control bar (Logic Pro: Project Settings >
//! Synchronization): the frame rate and start time the time is shown with, and the MIDI ports
//! MIDI Time Code goes out to and is followed from.
use crate::i18n::{tr, trf};
use crate::model::Project;
use crate::project_io::valid_smpte_start;
use prowave_engine::SmpteRate;

/// The start times the menu offers: midnight, Logic's own 01:00:00:00, and the 00:59:55:00 of
/// film work, which leaves five seconds before the hour for a countdown.
const STARTS: [[u8; 4]; 3] = [[0, 0, 0, 0], [0, 59, 55, 0], [1, 0, 0, 0]];

/// A frame rate as the menu names it.
pub fn rate_label(rate: SmpteRate) -> String {
    match rate {
        SmpteRate::Fps24 => trf("{} fps", &[&"24"]),
        SmpteRate::Fps25 => trf("{} fps", &[&"25"]),
        SmpteRate::Fps29_97NonDrop => trf("{} fps", &[&"29.97"]),
        SmpteRate::Fps29_97Drop => trf("{} fps Drop Frame", &[&"29.97"]),
        SmpteRate::Fps30 => trf("{} fps", &[&"30"]),
    }
}

fn start_label(start: [u8; 4], rate: SmpteRate) -> String {
    let sep = if rate.is_drop_frame() { ';' } else { ':' };
    format!(
        "{:02}:{:02}:{:02}{sep}{:02}",
        start[0], start[1], start[2], start[3]
    )
}

/// The menu's entries.
pub fn items(ui: &mut egui::Ui, p: &mut Project) {
    ui.menu_button(tr("Frame Rate"), |ui| {
        for rate in SmpteRate::ALL {
            if ui
                .selectable_label(p.smpte_rate == rate, rate_label(rate))
                .clicked()
            {
                p.smpte_rate = rate;
                p.smpte_start = valid_smpte_start(p.smpte_start, rate);
                ui.close();
            }
        }
    });
    ui.menu_button(tr("Start Time"), |ui| {
        for start in STARTS {
            let start = valid_smpte_start(start, p.smpte_rate);
            let text = start_label(start, p.smpte_rate);
            if ui.selectable_label(p.smpte_start == start, text).clicked() {
                p.smpte_start = start;
                ui.close();
            }
        }
    });
    ui.separator();
    ui.menu_button(tr("Follow MIDI Clock"), |ui| {
        let current = p.midi_clock_in.clone();
        if ui.selectable_label(current.is_none(), tr("Off")).clicked() {
            p.midi_clock_in = None;
            ui.close();
        }
        let ports = crate::clock_in::ports();
        if ports.is_empty() {
            ui.add_enabled(false, egui::Button::new(tr("No MIDI ports found")));
        }
        for name in ports {
            if ui
                .selectable_label(current.as_deref() == Some(&name), &name)
                .clicked()
            {
                p.midi_clock_in = Some(name);
                ui.close();
            }
        }
    });
    ui.menu_button(tr("Send MIDI Clock"), |ui| {
        let current = p.midi_clock_out.clone();
        if ui.selectable_label(current.is_none(), tr("Off")).clicked() {
            p.midi_clock_out = None;
            ui.close();
        }
        let ports = crate::midi_out::ports();
        if ports.is_empty() {
            ui.add_enabled(false, egui::Button::new(tr("No MIDI ports found")));
        }
        for name in ports {
            if ui
                .selectable_label(current.as_deref() == Some(&name), &name)
                .clicked()
            {
                p.midi_clock_out = Some(name);
                ui.close();
            }
        }
    });
    ui.menu_button(tr("Send MIDI Time Code"), |ui| {
        let current = p.mtc_out.clone();
        if ui.selectable_label(current.is_none(), tr("Off")).clicked() {
            p.mtc_out = None;
            ui.close();
        }
        let ports = crate::midi_out::ports();
        if ports.is_empty() {
            ui.add_enabled(false, egui::Button::new(tr("No MIDI ports found")));
        }
        for name in ports {
            if ui
                .selectable_label(current.as_deref() == Some(&name), &name)
                .clicked()
            {
                // A port that is both followed and fed would chase its own time code.
                p.mtc_in = None;
                p.mtc_out = Some(name);
                ui.close();
            }
        }
    });
    ui.menu_button(tr("Follow MIDI Time Code"), |ui| {
        let current = p.mtc_in.clone();
        if ui.selectable_label(current.is_none(), tr("Off")).clicked() {
            p.mtc_in = None;
            ui.close();
        }
        let ports = crate::mtc_in::ports();
        if ports.is_empty() {
            ui.add_enabled(false, egui::Button::new(tr("No MIDI ports found")));
        }
        for name in ports {
            if ui
                .selectable_label(current.as_deref() == Some(&name), &name)
                .clicked()
            {
                p.mtc_out = None;
                p.mtc_in = Some(name);
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_times_exist_at_every_rate() {
        for rate in SmpteRate::ALL {
            for start in STARTS {
                let fixed = valid_smpte_start(start, rate);
                assert_eq!(valid_smpte_start(fixed, rate), fixed);
            }
        }
        assert_eq!(
            start_label([1, 0, 0, 2], SmpteRate::Fps29_97Drop),
            "01:00:00;02"
        );
        assert_eq!(start_label([1, 0, 0, 0], SmpteRate::Fps25), "01:00:00:00");
    }
}
