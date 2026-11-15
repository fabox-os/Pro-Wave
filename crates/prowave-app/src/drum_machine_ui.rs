//! The Drum Machine window (Logic: Drum Machine Designer): sixteen pads on the keys 36 to 51,
//! played by clicking them, and below them the settings of the selected pad: which drum it
//! plays (or a sample you load), Tune, Decay, Tone, Level, Pan and its Choke group.
//!
//! The kit lives in the track (`Track::drums`); the window edits it, and `engine_sync.rs` moves
//! the changes into the engine while it plays.

use crate::drum_kit::DrumsUse;
use crate::i18n::tr;
use crate::piano_roll::Audition;
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui};
use prowave_engine::drum_machine::{key_of, PadKind, MAX_CHOKE, PADS};

const PAD_W: f32 = 104.0;
const PAD_H: f32 = 58.0;
const PAD_BG: Color32 = Color32::from_rgb(0x3a, 0x3d, 0x44);
const PAD_HOT: Color32 = Color32::from_rgb(0x5b, 0x8d, 0xd9);
const SELECT: Color32 = Color32::from_rgb(0xff, 0x9f, 0x40);

/// The name of MIDI key `key` (60 is C3, as in Logic).
fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[usize::from(key % 12)], i32::from(key / 12) - 2)
}

/// The window, while `UiState::show_drum_machine` names a track that has a Drum Machine.
pub fn show(ctx: &egui::Context, p: &mut crate::model::Project, s: &mut UiState) {
    let Some(ti) = s.show_drum_machine else {
        return;
    };
    let Some(kit) = p.tracks.get(ti).and_then(|t| t.drums.clone()) else {
        release(s);
        s.show_drum_machine = None;
        return;
    };
    let mut edited = kit.clone();
    let mut open = true;
    let title = format!("{} · {}", tr("Drum Machine"), p.tracks[ti].name);
    egui::Window::new(title)
        .id(egui::Id::unique("drum_machine_window"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            pads(ui, &edited, s, ti);
            ui.separator();
            editor(ui, &mut edited, s);
        });
    if edited != kit {
        p.tracks[ti].drums = Some(edited);
    }
    if !open {
        release(s);
        s.show_drum_machine = None;
    }
}

/// Lets go of the pad being held, if any (its note off).
fn release(s: &mut UiState) {
    if let Some(pad) = s.drum_held.take() {
        s.audition.push(Audition::Off { pitch: key_of(pad) });
    }
}

/// The 4 x 4 grid of pads: the bottom row is pads 1 to 4, as on a drum machine. Pressing a pad
/// selects it and plays it; letting go ends its note.
fn pads(ui: &mut Ui, kit: &DrumsUse, s: &mut UiState, ti: usize) {
    for row in (0..4).rev() {
        ui.horizontal(|ui| {
            for col in 0..4 {
                let pad = row * 4 + col;
                let (rect, resp) = ui.allocate_exact_size(vec2(PAD_W, PAD_H), Sense::click_and_drag());
                let down = resp.is_pointer_button_down_on();
                if down && s.drum_held != Some(pad) {
                    release(s);
                    s.drum_pad = pad;
                    s.drum_held = Some(pad);
                    s.audition.push(Audition::On {
                        pitch: key_of(pad),
                        vel: 110,
                        track: ti as u16,
                    });
                }
                draw_pad(ui, rect, kit, pad, s.drum_pad == pad, down);
            }
        });
    }
    // Letting go anywhere ends the note.
    if s.drum_held.is_some() && !ui.input(|i| i.pointer.primary_down()) {
        release(s);
    }
}

fn draw_pad(ui: &Ui, rect: Rect, kit: &DrumsUse, pad: usize, selected: bool, down: bool) {
    let g = ui.painter_at(rect);
    g.rect_filled(rect, 6.0, if down { PAD_HOT } else { PAD_BG });
    g.rect_stroke(
        rect,
        6.0,
        Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { SELECT } else { Color32::from_gray(0x22) }),
        StrokeKind::Inside,
    );
    g.text(
        rect.center() + vec2(0.0, -6.0),
        Align2::CENTER_CENTER,
        kit.pad_label(pad),
        FontId::proportional(12.0),
        Color32::WHITE,
    );
    g.text(
        pos2(rect.left() + 8.0, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        key_name(key_of(pad)),
        FontId::proportional(10.0),
        Color32::from_gray(0xb0),
    );
    let choke = kit.pads[pad].settings.choke;
    if choke > 0 {
        g.text(
            pos2(rect.right() - 8.0, rect.bottom() - 8.0),
            Align2::RIGHT_CENTER,
            format!("C{choke}"),
            FontId::proportional(10.0),
            Color32::from_rgb(0xe0, 0xb0, 0x60),
        );
    }
}

/// The settings of the selected pad.
fn editor(ui: &mut Ui, kit: &mut DrumsUse, s: &mut UiState) {
    let pad = s.drum_pad.min(PADS - 1);
    let selected = &mut kit.pads[pad];
    ui.label(format!("{} · {}", key_name(key_of(pad)), kit_label(selected)));
    egui::Grid::new("drum_pad_settings")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label(tr("Drum"));
            let old = selected.settings.kind;
            egui::ComboBox::from_id_salt("drum_pad_kind")
                .selected_text(tr(old.name()))
                .show_ui(ui, |ui| {
                    for kind in PadKind::ALL {
                        if ui.selectable_label(kind == old, tr(kind.name())).clicked() && kind != old {
                            selected.settings = kind.settings();
                        }
                    }
                });
            ui.end_row();
            if selected.settings.kind == PadKind::Sample {
                ui.label(tr("Sample"));
                ui.horizontal(|ui| {
                    if ui.button(tr("Load Sample…")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Audio", &["wav", "aif", "aiff", "flac", "ogg", "mp3", "caf", "m4a"])
                            .pick_file()
                        {
                            match crate::drum_kit::check_sample(&path) {
                                Ok(()) => {
                                    selected.sample = Some(path);
                                    s.drum_error = None;
                                }
                                Err(e) => s.drum_error = Some(e),
                            }
                        }
                    }
                    if let Some(path) = &selected.sample {
                        let missing = crate::drum_kit::error(path).is_some();
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.label(if missing { format!("{name} · {}", tr("missing")) } else { name });
                    }
                });
                ui.end_row();
            }
            let sample = selected.settings.kind == PadKind::Sample;
            let st = &mut selected.settings;
            ui.label(tr("Tune"));
            ui.add(egui::Slider::new(&mut st.tune, -12.0..=12.0).suffix(" st"));
            ui.end_row();
            ui.label(if sample { tr("Length") } else { tr("Decay") });
            ui.add(egui::Slider::new(&mut st.decay, 0.2..=4.0).logarithmic(true).suffix(" x"));
            ui.end_row();
            ui.label(tr("Tone"));
            ui.add(egui::Slider::new(&mut st.tone, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)));
            ui.end_row();
            ui.label(tr("Level"));
            ui.add(egui::Slider::new(&mut st.level_db, -24.0..=6.0).suffix(" dB"));
            ui.end_row();
            ui.label(tr("Pan"));
            ui.add(egui::Slider::new(&mut st.pan, -1.0..=1.0).custom_formatter(|v, _| format!("{:+.0}", v * 100.0)));
            ui.end_row();
            ui.label(tr("Choke Group"));
            let mut choke = i32::from(st.choke);
            ui.add(egui::Slider::new(&mut choke, 0..=i32::from(MAX_CHOKE)).custom_formatter(|v, _| {
                if v < 0.5 {
                    tr("None").to_string()
                } else {
                    format!("{v:.0}")
                }
            }));
            st.choke = choke.clamp(0, i32::from(MAX_CHOKE)) as u8;
            ui.end_row();
        });
    if let Some(e) = &s.drum_error {
        ui.colored_label(Color32::from_rgb(0xe0, 0x70, 0x60), crate::i18n::trf("Cannot use that sample: {}", &[e]));
    }
    ui.add_space(4.0);
    if ui.button(tr("Reset Kit")).clicked() {
        *kit = DrumsUse::new();
    }
}

/// The selected pad's name for the line above its settings.
fn kit_label(pad: &crate::drum_kit::PadUse) -> String {
    match (&pad.sample, pad.settings.kind) {
        (Some(p), PadKind::Sample) => p
            .file_name()
            .map_or_else(|| tr("Sample").to_string(), |n| n.to_string_lossy().into_owned()),
        (_, kind) => tr(kind.name()).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, Track, TrackKind};

    fn project_with_kit() -> Project {
        let mut p = Project::empty();
        p.tracks.clear();
        let mut t = Track::new("Beats", TrackKind::Instrument);
        t.drums = Some(DrumsUse::new());
        p.tracks.push(t);
        p
    }

    #[test]
    fn key_names_follow_logic() {
        assert_eq!(key_name(36), "C1");
        assert_eq!(key_name(51), "D#2");
        assert_eq!(key_name(60), "C3");
    }

    #[test]
    fn the_window_draws_every_pad_and_looking_changes_nothing() {
        let mut p = project_with_kit();
        let before = p.tracks[0].drums.clone();
        let mut s = UiState::new();
        s.show_drum_machine = Some(0);
        let mut h = crate::ui_test::Harness::new(700.0, 800.0);
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        }
        assert_eq!(s.show_drum_machine, Some(0), "it stays open");
        assert_eq!(p.tracks[0].drums, before);
        assert!(s.audition.is_empty(), "nothing is played by looking");
        // A sample pad with a missing file draws too, and so does a track whose kit is gone.
        let mut sample = project_with_kit();
        if let Some(k) = sample.tracks[0].drums.as_mut() {
            k.pads[3].settings = PadKind::Sample.settings();
            k.pads[3].sample = Some("/nowhere/snare.wav".into());
        }
        s.drum_pad = 3;
        for _ in 0..3 {
            h.frame(vec![], |ui| show(ui.ctx(), &mut sample, &mut s));
        }
        sample.tracks[0].drums = None;
        h.frame(vec![], |ui| show(ui.ctx(), &mut sample, &mut s));
        assert_eq!(s.show_drum_machine, None, "no kit, no window");
    }

    #[test]
    fn pressing_a_pad_selects_and_plays_it_and_letting_go_ends_the_note() {
        let mut p = project_with_kit();
        let mut s = UiState::new();
        s.show_drum_machine = Some(0);
        let mut h = crate::ui_test::Harness::new(700.0, 800.0);
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        // Press the bottom-left pad (pad 0, the kick): find a point inside it by scanning the
        // window's pad area for the press that makes a note.
        let mut played = false;
        'scan: for y in (300..700).step_by(20) {
            for x in (30..300).step_by(20) {
                h.press(egui::pos2(x as f32, y as f32), |ui| show(ui.ctx(), &mut p, &mut s));
                if s.audition.iter().any(|a| matches!(a, Audition::On { pitch: 36, .. })) {
                    played = true;
                    break 'scan;
                }
                h.release(egui::pos2(x as f32, y as f32), |ui| show(ui.ctx(), &mut p, &mut s));
                s.audition.clear();
                s.drum_held = None;
            }
        }
        assert!(played, "pressing the kick pad plays key 36");
        assert_eq!(s.drum_pad, 0);
        assert_eq!(s.drum_held, Some(0));
        h.release(egui::pos2(0.0, 0.0), |ui| show(ui.ctx(), &mut p, &mut s));
        h.frame(vec![], |ui| show(ui.ctx(), &mut p, &mut s));
        assert!(s.audition.iter().any(|a| matches!(a, Audition::Off { pitch: 36 })), "{:?}", s.audition);
        assert_eq!(s.drum_held, None);
    }
}
