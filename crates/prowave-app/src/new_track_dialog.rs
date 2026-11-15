//! Logic's "Create New Track" dialog: the + above the track headers opens it, rather than
//! adding a software instrument track straight away. It offers the same four families as
//! Logic, a choice inside each, and how many tracks to create.

use crate::model::{Project, TrackKind};
use crate::UiState;

/// The track a dialog choice creates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Choice {
    #[default]
    SoftwareInstrument,
    ExternalMidi,
    Pattern,
    PatternExternalMidi,
    Drummer,
    AudioMic,
    AudioGuitar,
}

impl Choice {
    fn kind(self) -> TrackKind {
        match self {
            Self::SoftwareInstrument | Self::Pattern => TrackKind::Instrument,
            Self::ExternalMidi | Self::PatternExternalMidi => TrackKind::ExternalMidi,
            Self::Drummer => TrackKind::Drummer,
            Self::AudioMic | Self::AudioGuitar => TrackKind::Audio,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SoftwareInstrument | Self::Pattern => "Software Instrument",
            Self::ExternalMidi | Self::PatternExternalMidi => "External MIDI",
            Self::Drummer => "Drummer",
            Self::AudioMic => "Mic or Line",
            Self::AudioGuitar => "Guitar or Bass",
        }
    }
}

/// The dialog's state, kept in [`UiState`] between frames.
#[derive(Default)]
pub struct NewTrackDialog {
    pub open: bool,
    choice: Choice,
    count: u32,
}

impl NewTrackDialog {
    /// Opens the dialog at one track with the default (software instrument) choice.
    pub fn open(&mut self) {
        self.open = true;
        self.count = 1;
    }
}

/// Draws the dialog when it is open and creates the tracks when Create is pressed.
pub fn show(ctx: &egui::Context, p: &mut Project, s: &mut UiState) {
    if !s.new_track.open {
        return;
    }
    let mut open = true;
    let mut create = false;
    let mut cancel = false;
    egui::Window::new("Create New Track")
        .id(egui::Id::unique("create_new_track"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(660.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -30.0))
        .show(ctx, |ui| {
            let families: [(&str, &[Choice]); 4] = [
                ("MIDI", &[Choice::SoftwareInstrument, Choice::ExternalMidi]),
                ("Pattern", &[Choice::Pattern, Choice::PatternExternalMidi]),
                ("Session Player", &[Choice::Drummer]),
                ("Audio", &[Choice::AudioMic, Choice::AudioGuitar]),
            ];
            ui.horizontal_top(|ui| {
                for (family, choices) in families {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(150.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(family).strong().size(15.0));
                            ui.add_space(4.0);
                            for choice in choices {
                                let selected = s.new_track.choice == *choice;
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 28.0],
                                        egui::Button::new(choice.label()).selected(selected),
                                    )
                                    .clicked()
                                {
                                    s.new_track.choice = *choice;
                                }
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Number of tracks to create:");
                let mut count = s.new_track.count.max(1);
                if ui
                    .add(egui::DragValue::new(&mut count).range(1..=64))
                    .changed()
                {
                    s.new_track.count = count;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Create").clicked() {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        });
    if !open || cancel {
        s.new_track.open = false;
    }
    if create {
        let kind = s.new_track.choice.kind();
        for _ in 0..s.new_track.count.clamp(1, 64) {
            p.add_track(kind);
        }
        p.selected_track = p.tracks.len().saturating_sub(1);
        s.new_track.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_choice_creates_its_track_kind() {
        assert_eq!(Choice::SoftwareInstrument.kind(), TrackKind::Instrument);
        assert_eq!(Choice::Pattern.kind(), TrackKind::Instrument);
        assert_eq!(Choice::ExternalMidi.kind(), TrackKind::ExternalMidi);
        assert_eq!(Choice::PatternExternalMidi.kind(), TrackKind::ExternalMidi);
        assert_eq!(Choice::Drummer.kind(), TrackKind::Drummer);
        assert_eq!(Choice::AudioMic.kind(), TrackKind::Audio);
        assert_eq!(Choice::AudioGuitar.kind(), TrackKind::Audio);
    }
}
