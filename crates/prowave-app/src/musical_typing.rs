//! Musical Typing (Logic: Window > Show Musical Typing, Cmd+K): the computer keyboard as a MIDI
//! keyboard, with a small window that shows it.
//!
//! - **Notes:** the home row plays the white keys (A S D F G H J K L ; ' from C) and the row
//!   above plays the black ones (W E T Y U O P), 18 semitones in all.
//! - **Octave** (Z lower, X higher) and **Velocity** (C lower, V higher) are shown in the window.
//! - **Pitch Bend** (1 down, 2 up, held: it springs back to the centre) and **Modulation**
//!   (3 off, 4 to 7 steps, 8 maximum).
//! - **Sustain** (Tab, held): released keys keep sounding until it is let go.
//!
//! The typed keys become the same live notes a MIDI keyboard makes (`recorder::LiveNote`), so
//! they play the selected instrument and are recorded into the region while recording. The window
//! can also be played with the mouse. Only an instrument track is played (an audio track is not).
//! Keys are read from where they are on the keyboard (not what letter they print), and nothing is
//! taken while a text field has the focus or Ctrl, Cmd or Alt is held.

use crate::recorder::LiveNote;
use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

/// The semitone (from the lowest C) each note key plays.
const NOTE_KEYS: [(Key, u8); 18] = [
    (Key::A, 0),
    (Key::W, 1),
    (Key::S, 2),
    (Key::E, 3),
    (Key::D, 4),
    (Key::F, 5),
    (Key::T, 6),
    (Key::G, 7),
    (Key::Y, 8),
    (Key::H, 9),
    (Key::U, 10),
    (Key::J, 11),
    (Key::K, 12),
    (Key::O, 13),
    (Key::L, 14),
    (Key::P, 15),
    (Key::Semicolon, 16),
    (Key::Quote, 17),
];

/// The modulation each of the keys 3 to 8 sets (the last is the maximum).
const MODULATION: [u8; 6] = [0, 25, 51, 76, 102, 127];

/// The lowest and highest octave of the lowest C (C-2 is note 0).
pub const MIN_OCTAVE: i8 = -2;
pub const MAX_OCTAVE: i8 = 7;
/// How much a velocity key changes the velocity.
const VELOCITY_STEP: i16 = 8;
/// The centre of the pitch bend and its two ends.
const BEND_CENTRE: u16 = 8192;
const BEND_MAX: u16 = 16383;

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Plays the note this many semitones above the lowest C.
    Note(u8),
    OctaveDown,
    OctaveUp,
    VelocityDown,
    VelocityUp,
    BendDown,
    BendUp,
    /// Sets the modulation to this step (0 to 5).
    Modulation(u8),
    Sustain,
}

/// What the key `key` does, if anything.
pub fn action_of(key: Key) -> Option<Action> {
    if let Some((_, semi)) = NOTE_KEYS.iter().find(|(k, _)| *k == key) {
        return Some(Action::Note(*semi));
    }
    Some(match key {
        Key::Z => Action::OctaveDown,
        Key::X => Action::OctaveUp,
        Key::C => Action::VelocityDown,
        Key::V => Action::VelocityUp,
        Key::Num1 => Action::BendDown,
        Key::Num2 => Action::BendUp,
        Key::Num3 => Action::Modulation(0),
        Key::Num4 => Action::Modulation(1),
        Key::Num5 => Action::Modulation(2),
        Key::Num6 => Action::Modulation(3),
        Key::Num7 => Action::Modulation(4),
        Key::Num8 => Action::Modulation(5),
        Key::Tab => Action::Sustain,
        _ => return None,
    })
}

/// The name of the C that is `octave` (Logic's numbering: middle C is C3).
pub fn octave_name(octave: i8) -> String {
    format!("C{octave}")
}

/// One key or mouse press that is down: who is pressing, what it does, and for a note which
/// note it started (the octave may change while it is held).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Down {
    source: Source,
    action: Action,
    pitch: u8,
}

/// Where a press came from: a computer key, or the mouse on the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Key(Key),
    Mouse(Action),
}

/// The state of Musical Typing.
#[derive(Debug)]
pub struct MusicalTyping {
    /// Whether the window is shown (and the keys are taken).
    pub open: bool,
    octave: i8,
    velocity: u8,
    modulation: u8,
    /// Which of the pitch bend keys are held, so both held is the centre.
    bend_down: bool,
    bend_up: bool,
    sustain: bool,
    down: Vec<Down>,
    /// Notes whose key came up while Sustain was held: they end when it is let go.
    sustained: Vec<u8>,
    /// The live notes made since the app last took them.
    pending: Vec<LiveNote>,
}

impl Default for MusicalTyping {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicalTyping {
    pub fn new() -> Self {
        Self {
            open: false,
            octave: 3,
            velocity: 93,
            modulation: 0,
            bend_down: false,
            bend_up: false,
            sustain: false,
            down: Vec::new(),
            sustained: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub fn octave(&self) -> i8 {
        self.octave
    }

    pub fn velocity(&self) -> u8 {
        self.velocity
    }

    pub fn sustaining(&self) -> bool {
        self.sustain
    }

    /// The pitch bend now, 0 to 16383 (8192 is the centre).
    pub fn bend(&self) -> u16 {
        match (self.bend_down, self.bend_up) {
            (true, false) => 1,
            (false, true) => BEND_MAX,
            _ => BEND_CENTRE,
        }
    }

    /// The modulation now, 0 to 127.
    pub fn modulation(&self) -> u8 {
        self.modulation
    }

    /// Whether the note `semi` above the lowest C is held (for the window to light the key).
    pub fn is_down(&self, action: Action) -> bool {
        self.down.iter().any(|d| d.action == action)
    }

    /// The live notes made since the last call.
    pub fn take(&mut self) -> Vec<LiveNote> {
        std::mem::take(&mut self.pending)
    }

    /// The lowest note of the keyboard (the lowest C).
    fn base(&self) -> i32 {
        (i32::from(self.octave) + 2) * 12
    }

    /// A key or the mouse goes down on `action`.
    fn press(&mut self, source: Source, action: Action) {
        if self.down.iter().any(|d| d.source == source) {
            return;
        }
        let mut pitch = 0;
        match action {
            Action::Note(semi) => {
                let note = self.base() + i32::from(semi);
                if !(0..=127).contains(&note) {
                    return;
                }
                pitch = note as u8;
                // The same note again while it is held by the pedal starts it afresh.
                if let Some(i) = self.sustained.iter().position(|p| *p == pitch) {
                    self.sustained.remove(i);
                    self.pending.push(LiveNote::Off { pitch });
                }
                self.pending.push(LiveNote::On {
                    pitch,
                    vel: self.velocity,
                });
            }
            Action::OctaveDown => self.octave = (self.octave - 1).max(MIN_OCTAVE),
            Action::OctaveUp => self.octave = (self.octave + 1).min(MAX_OCTAVE),
            Action::VelocityDown => {
                self.velocity = (i16::from(self.velocity) - VELOCITY_STEP).clamp(1, 127) as u8
            }
            Action::VelocityUp => {
                self.velocity = (i16::from(self.velocity) + VELOCITY_STEP).clamp(1, 127) as u8
            }
            Action::BendDown => {
                self.bend_down = true;
                self.pending.push(LiveNote::Bend { val: self.bend() });
            }
            Action::BendUp => {
                self.bend_up = true;
                self.pending.push(LiveNote::Bend { val: self.bend() });
            }
            Action::Modulation(step) => {
                self.modulation = MODULATION[usize::from(step).min(MODULATION.len() - 1)];
                self.pending.push(LiveNote::Modulation {
                    val: self.modulation,
                });
            }
            Action::Sustain => self.sustain = true,
        }
        self.down.push(Down {
            source,
            action,
            pitch,
        });
    }

    /// The key or the mouse that went down on `source` comes up.
    fn release(&mut self, source: Source) {
        let Some(i) = self.down.iter().position(|d| d.source == source) else {
            return;
        };
        let d = self.down.remove(i);
        match d.action {
            Action::Note(_) => {
                // Another press on the same note (key and mouse together) keeps it sounding.
                let still_held = self
                    .down
                    .iter()
                    .any(|o| o.pitch == d.pitch && matches!(o.action, Action::Note(_)));
                if still_held {
                    return;
                }
                if self.sustain {
                    if !self.sustained.contains(&d.pitch) {
                        self.sustained.push(d.pitch);
                    }
                } else {
                    self.pending.push(LiveNote::Off { pitch: d.pitch });
                }
            }
            Action::BendDown => {
                self.bend_down = false;
                self.pending.push(LiveNote::Bend { val: self.bend() });
            }
            Action::BendUp => {
                self.bend_up = false;
                self.pending.push(LiveNote::Bend { val: self.bend() });
            }
            Action::Sustain => {
                self.sustain = false;
                // The notes the pedal was holding end now, except any that are held again.
                for pitch in std::mem::take(&mut self.sustained) {
                    self.pending.push(LiveNote::Off { pitch });
                }
            }
            _ => {}
        }
    }

    /// Lets go of everything (the window closed, or the app lost the keyboard): every note ends,
    /// the bend returns to the centre and the pedal comes up.
    pub fn release_all(&mut self) {
        let sources: Vec<Source> = self.down.iter().map(|d| d.source).collect();
        self.sustain = false;
        for s in sources {
            self.release(s);
        }
        for pitch in std::mem::take(&mut self.sustained) {
            self.pending.push(LiveNote::Off { pitch });
        }
    }

    /// Reads this frame's key events. Call every frame; it does nothing while the window is
    /// closed (a key that was down then is let go).
    pub fn read_keys(&mut self, ctx: &egui::Context) {
        if !self.open {
            if !self.down.is_empty() || !self.sustained.is_empty() {
                self.release_all();
            }
            return;
        }
        // Typing in a text field is typing, not playing (a focused button is not).
        if ctx.text_edit_focused() {
            self.release_all();
            return;
        }
        let events = ctx.input(|i| i.events.clone());

        for event in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let key = physical_key.unwrap_or(key);
            let Some(action) = action_of(key) else {
                continue;
            };
            if pressed {
                // Ctrl, Cmd and Alt are for the app's own key commands (Ctrl+Z, Cmd+K…).
                if repeat
                    || modifiers.command
                    || modifiers.alt
                    || modifiers.ctrl
                    || modifiers.mac_cmd
                {
                    continue;
                }
                self.press(Source::Key(key), action);
            } else {
                self.release(Source::Key(key));
            }
        }
        // A key that was let go while the window had no focus never sends its release: the
        // keys that are no longer down are let go.
        let held: Vec<Source> = self.down.iter().map(|d| d.source).collect();
        for s in held {
            if let Source::Key(k) = s {
                if !ctx.input(|i| i.key_down(k)) {
                    self.release(s);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The window.

const WHITE: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf4);
const BLACK: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
const LIT: Color32 = Color32::from_rgb(0x4d, 0x9f, 0xff);
const KEY_W: f32 = 46.0;
const KEY_H: f32 = 118.0;
const PAD: f32 = 14.0;

/// The label a key shows (its letter).
fn key_label(key: Key) -> &'static str {
    match key {
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::Tab => "tab",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        other => other.name(),
    }
}

/// The window, while Musical Typing is open. `track_name` is the instrument that is played.
pub fn show(ctx: &egui::Context, typing: &mut MusicalTyping, track_name: Option<&str>) {
    if !typing.open {
        return;
    }
    let mut open = true;
    let title = match track_name {
        Some(n) => format!("{} - {n}", crate::i18n::tr("Musical Typing")),
        None => crate::i18n::tr("Musical Typing").to_string(),
    };
    egui::Window::new(title)
        .id(egui::Id::unique("musical_typing_window"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| draw(ui, typing));
    if !open {
        typing.open = false;
        typing.release_all();
    }
}

/// A small coloured key: its label, and a caption above it. Plays `action` with the mouse.
#[allow(clippy::too_many_arguments)]
fn small_key(
    ui: &mut Ui,
    typing: &mut MusicalTyping,
    rect: Rect,
    key: Key,
    caption: &str,
    colour: Color32,
    action: Action,
    lit: bool,
) {
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("mt_small", key.name())),
        Sense::click_and_drag(),
    );
    mouse(typing, &resp, action);
    let g = ui.painter_at(rect);
    let fill = if lit || resp.is_pointer_button_down_on() {
        colour.gamma_multiply(0.6)
    } else {
        colour
    };
    g.rect_filled(rect, 5.0, fill);
    g.text(
        rect.right_top() + egui::vec2(-6.0, 12.0),
        Align2::RIGHT_CENTER,
        caption,
        FontId::proportional(11.0),
        Color32::WHITE,
    );
    g.text(
        rect.left_bottom() + egui::vec2(8.0, -10.0),
        Align2::LEFT_CENTER,
        key_label(key),
        FontId::proportional(13.0),
        Color32::WHITE,
    );
}

/// Presses and releases `action` as the pointer goes down on and up from `resp`.
fn mouse(typing: &mut MusicalTyping, resp: &egui::Response, action: Action) {
    let source = Source::Mouse(action);
    if resp.is_pointer_button_down_on() {
        typing.press(source, action);
    } else {
        typing.release(source);
    }
}

fn draw(ui: &mut Ui, typing: &mut MusicalTyping) {
    let top = ui.min_rect().min + egui::vec2(PAD, PAD);
    let whites = [
        Key::A,
        Key::S,
        Key::D,
        Key::F,
        Key::G,
        Key::H,
        Key::J,
        Key::K,
        Key::L,
        Key::Semicolon,
        Key::Quote,
    ];
    let width = whites.len() as f32 * KEY_W + 2.0 * PAD + 96.0;
    let height = 34.0 + 50.0 + KEY_H + 20.0 + 56.0 + PAD * 2.0;
    let (area, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let origin = area.min;
    let _ = top;
    let label = |ui: &Ui, pos: Pos2, text: String| {
        ui.painter().text(
            pos,
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(12.0),
            Color32::from_gray(0xb8),
        );
    };

    // Pitch Bend (1, 2) and Modulation (3 to 8).
    let row1 = origin.y + PAD;
    label(
        ui,
        Pos2::new(origin.x + PAD, row1 + 14.0),
        format!(
            "{}: {}",
            crate::i18n::tr("Pitch Bend"),
            match typing.bend() {
                BEND_MAX => "+".to_string(),
                1 => "-".to_string(),
                _ => "0".to_string(),
            }
        ),
    );
    let small = egui::vec2(48.0, 42.0);
    let blue = Color32::from_rgb(0x1e, 0x6f, 0xc8);
    let purple = Color32::from_rgb(0x8a, 0x2b, 0xc2);
    let mut x = origin.x + 96.0 + PAD;
    for (key, caption, action) in [
        (Key::Num1, "-", Action::BendDown),
        (Key::Num2, "+", Action::BendUp),
    ] {
        let lit = typing.is_down(action);
        small_key(
            ui,
            typing,
            Rect::from_min_size(Pos2::new(x, row1 + 26.0), small),
            key,
            caption,
            blue,
            action,
            lit,
        );
        x += small.x + 2.0;
    }
    x += 10.0;
    for (n, key) in [
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
    ]
    .into_iter()
    .enumerate()
    {
        let caption = match n {
            0 => "off",
            5 => "max",
            _ => "",
        };
        let action = Action::Modulation(n as u8);
        let lit = typing.modulation() == MODULATION[n];
        small_key(
            ui,
            typing,
            Rect::from_min_size(Pos2::new(x, row1 + 26.0), small),
            key,
            caption,
            purple,
            action,
            lit,
        );
        x += small.x + 2.0;
    }
    label(
        ui,
        Pos2::new(x + 8.0, row1 + 40.0),
        crate::i18n::tr("Modulation").to_string(),
    );

    // Sustain and the piano.
    let piano_y = row1 + 26.0 + small.y + 14.0;
    let green = Color32::from_rgb(0x1d, 0x8a, 0x3a);
    let sustain_rect = Rect::from_min_size(
        Pos2::new(origin.x + PAD, piano_y + 30.0),
        egui::vec2(76.0, 50.0),
    );
    {
        let lit = typing.sustaining();
        small_key(
            ui,
            typing,
            sustain_rect,
            Key::Tab,
            crate::i18n::tr("sustain"),
            green,
            Action::Sustain,
            lit,
        );
    }
    let piano_x = origin.x + 96.0 + PAD;
    // White keys first, then the black keys on top.
    let white_semis = [0u8, 2, 4, 5, 7, 9, 11, 12, 14, 16, 17];
    for (i, key) in whites.iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(piano_x + i as f32 * KEY_W, piano_y),
            egui::vec2(KEY_W - 1.0, KEY_H),
        );
        piano_key(ui, typing, rect, *key, white_semis[i], false);
    }
    // Black keys sit between white ones: after C, D, F, G, A, C, D.
    let blacks = [
        (Key::W, 1u8, 0usize),
        (Key::E, 3, 1),
        (Key::T, 6, 3),
        (Key::Y, 8, 4),
        (Key::U, 10, 5),
        (Key::O, 13, 7),
        (Key::P, 15, 8),
    ];
    for (key, semi, after) in blacks {
        let rect = Rect::from_min_size(
            Pos2::new(
                piano_x + (after as f32 + 1.0) * KEY_W - KEY_W * 0.3,
                piano_y,
            ),
            egui::vec2(KEY_W * 0.6, KEY_H * 0.62),
        );
        piano_key(ui, typing, rect, key, semi, true);
    }

    // Octave and Velocity.
    let row3 = piano_y + KEY_H + 18.0;
    let gold = Color32::from_rgb(0xcf, 0x9a, 0x0a);
    let orange = Color32::from_rgb(0xd1, 0x5a, 0x0a);
    label(
        ui,
        Pos2::new(origin.x + PAD, row3 + 20.0),
        format!(
            "{}: {}",
            crate::i18n::tr("Octave"),
            octave_name(typing.octave())
        ),
    );
    let mut x = origin.x + 96.0 + PAD;
    for (key, caption, action) in [
        (Key::Z, "-", Action::OctaveDown),
        (Key::X, "+", Action::OctaveUp),
    ] {
        let lit = typing.is_down(action);
        small_key(
            ui,
            typing,
            Rect::from_min_size(Pos2::new(x, row3), small),
            key,
            caption,
            gold,
            action,
            lit,
        );
        x += small.x + 2.0;
    }
    x += 14.0;
    for (key, caption, action) in [
        (Key::C, "-", Action::VelocityDown),
        (Key::V, "+", Action::VelocityUp),
    ] {
        let lit = typing.is_down(action);
        small_key(
            ui,
            typing,
            Rect::from_min_size(Pos2::new(x, row3), small),
            key,
            caption,
            orange,
            action,
            lit,
        );
        x += small.x + 2.0;
    }
    label(
        ui,
        Pos2::new(x + 14.0, row3 + 20.0),
        format!("{}: {}", crate::i18n::tr("Velocity"), typing.velocity()),
    );
}

/// One piano key: its letter at the bottom; it lights while played, by key or mouse.
fn piano_key(ui: &mut Ui, typing: &mut MusicalTyping, rect: Rect, key: Key, semi: u8, black: bool) {
    let action = Action::Note(semi);
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("mt_key", key.name())),
        Sense::click_and_drag(),
    );
    mouse(typing, &resp, action);
    let lit = typing.is_down(action);
    let g = ui.painter_at(rect.expand(1.0));
    g.rect_filled(
        rect,
        3.0,
        if lit {
            LIT
        } else if black {
            BLACK
        } else {
            WHITE
        },
    );
    g.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_gray(0x55)),
        StrokeKind::Inside,
    );
    g.text(
        rect.center_bottom() + egui::vec2(0.0, -14.0),
        Align2::CENTER_CENTER,
        key_label(key),
        FontId::proportional(14.0),
        if black || lit {
            Color32::WHITE
        } else {
            Color32::from_gray(0x33)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing() -> MusicalTyping {
        let mut t = MusicalTyping::new();
        t.open = true;
        t
    }

    fn on(pitch: u8, vel: u8) -> LiveNote {
        LiveNote::On { pitch, vel }
    }

    #[test]
    fn the_home_row_plays_the_white_keys_and_the_row_above_the_black_ones() {
        let semis: Vec<u8> = [
            Key::A,
            Key::S,
            Key::D,
            Key::F,
            Key::G,
            Key::H,
            Key::J,
            Key::K,
            Key::L,
            Key::Semicolon,
            Key::Quote,
        ]
        .into_iter()
        .filter_map(action_of)
        .map(|a| if let Action::Note(s) = a { s } else { 255 })
        .collect();
        // C D E F G A B C D E F: the white keys.
        assert_eq!(semis, vec![0, 2, 4, 5, 7, 9, 11, 12, 14, 16, 17]);
        let blacks: Vec<u8> = [Key::W, Key::E, Key::T, Key::Y, Key::U, Key::O, Key::P]
            .into_iter()
            .filter_map(action_of)
            .map(|a| if let Action::Note(s) = a { s } else { 255 })
            .collect();
        assert_eq!(blacks, vec![1, 3, 6, 8, 10, 13, 15]);
        // Every note key is a different semitone and nothing else is.
        let mut all: Vec<u8> = NOTE_KEYS.iter().map(|(_, s)| *s).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 18);
        assert_eq!(action_of(Key::Q), None);
        assert_eq!(action_of(Key::Space), None, "Space stays the transport's");
        assert_eq!(action_of(Key::Z), Some(Action::OctaveDown));
        assert_eq!(action_of(Key::Tab), Some(Action::Sustain));
    }

    #[test]
    fn a_key_plays_a_note_at_the_octave_and_velocity_and_ends_it_when_it_comes_up() {
        let mut t = typing();
        t.press(Source::Key(Key::A), Action::Note(0));
        // Octave 3 is middle C (60); the default velocity is 93.
        assert_eq!(t.take(), vec![on(60, 93)]);
        t.press(Source::Key(Key::J), Action::Note(11));
        assert_eq!(t.take(), vec![on(71, 93)]);
        t.release(Source::Key(Key::A));
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 60 }]);
        // A key held down does not start again, and one that was never down ends nothing.
        t.press(Source::Key(Key::J), Action::Note(11));
        t.release(Source::Key(Key::Q));
        assert!(t.take().is_empty());
    }

    #[test]
    fn the_octave_keys_move_the_keyboard_and_a_held_note_ends_on_the_note_it_started() {
        let mut t = typing();
        t.press(Source::Key(Key::A), Action::Note(0));
        t.take();
        t.press(Source::Key(Key::X), Action::OctaveUp);
        t.release(Source::Key(Key::X));
        assert_eq!(t.octave(), 4);
        t.release(Source::Key(Key::A));
        assert_eq!(
            t.take(),
            vec![LiveNote::Off { pitch: 60 }],
            "the note that started is the one that ends"
        );
        t.press(Source::Key(Key::A), Action::Note(0));
        assert_eq!(t.take(), vec![on(72, 93)]);
        for _ in 0..20 {
            t.press(Source::Key(Key::X), Action::OctaveUp);
            t.release(Source::Key(Key::X));
        }
        assert_eq!(t.octave(), MAX_OCTAVE);
        for _ in 0..20 {
            t.press(Source::Key(Key::Z), Action::OctaveDown);
            t.release(Source::Key(Key::Z));
        }
        assert_eq!(t.octave(), MIN_OCTAVE);
        // At the top a key past note 127 plays nothing; at the bottom note 0 is the first.
        t.release(Source::Key(Key::A));
        t.take();
        t.press(Source::Key(Key::A), Action::Note(0));
        assert_eq!(t.take(), vec![on(0, 93)]);
        for _ in 0..20 {
            t.press(Source::Key(Key::X), Action::OctaveUp);
            t.release(Source::Key(Key::X));
        }
        t.take();
        t.press(Source::Key(Key::Quote), Action::Note(17));
        assert_eq!(
            t.take(),
            vec![on(125, 93)],
            "C7 and 17 semitones is still note 125"
        );
        assert_eq!(octave_name(3), "C3");
        assert_eq!(octave_name(-2), "C-2");
    }

    #[test]
    fn the_velocity_keys_step_within_one_to_127() {
        let mut t = typing();
        for _ in 0..30 {
            t.press(Source::Key(Key::V), Action::VelocityUp);
            t.release(Source::Key(Key::V));
        }
        assert_eq!(t.velocity(), 127);
        for _ in 0..30 {
            t.press(Source::Key(Key::C), Action::VelocityDown);
            t.release(Source::Key(Key::C));
        }
        assert_eq!(t.velocity(), 1);
        t.press(Source::Key(Key::V), Action::VelocityUp);
        assert_eq!(t.velocity(), 9);
        t.release(Source::Key(Key::V));
        t.press(Source::Key(Key::A), Action::Note(0));
        assert_eq!(t.take(), vec![on(60, 9)]);
    }

    #[test]
    fn sustain_holds_released_notes_until_it_is_let_go_and_a_note_struck_again_starts_afresh() {
        let mut t = typing();
        t.press(Source::Key(Key::Tab), Action::Sustain);
        assert!(t.sustaining());
        t.press(Source::Key(Key::A), Action::Note(0));
        t.press(Source::Key(Key::S), Action::Note(2));
        t.release(Source::Key(Key::A));
        t.release(Source::Key(Key::S));
        assert_eq!(
            t.take(),
            vec![on(60, 93), on(62, 93)],
            "no note ended: the pedal is down"
        );
        // The held note is struck again: it ends and starts.
        t.press(Source::Key(Key::A), Action::Note(0));
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 60 }, on(60, 93)]);
        t.release(Source::Key(Key::A));
        assert!(t.take().is_empty());
        t.release(Source::Key(Key::Tab));
        let mut ended: Vec<u8> = t
            .take()
            .into_iter()
            .filter_map(|n| {
                if let LiveNote::Off { pitch } = n {
                    Some(pitch)
                } else {
                    None
                }
            })
            .collect();
        ended.sort_unstable();
        assert_eq!(ended, vec![60, 62]);
        assert!(!t.sustaining());
        // A note held when the pedal comes up is not cut.
        t.press(Source::Key(Key::Tab), Action::Sustain);
        t.press(Source::Key(Key::D), Action::Note(4));
        t.take();
        t.release(Source::Key(Key::Tab));
        assert!(t.take().is_empty(), "the key is still down");
        t.release(Source::Key(Key::D));
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 64 }]);
    }

    #[test]
    fn pitch_bend_springs_back_and_both_keys_are_the_centre_and_modulation_sets_its_steps() {
        let mut t = typing();
        t.press(Source::Key(Key::Num2), Action::BendUp);
        assert_eq!(t.take(), vec![LiveNote::Bend { val: 16383 }]);
        t.press(Source::Key(Key::Num1), Action::BendDown);
        assert_eq!(t.take(), vec![LiveNote::Bend { val: 8192 }]);
        t.release(Source::Key(Key::Num2));
        assert_eq!(t.take(), vec![LiveNote::Bend { val: 1 }]);
        t.release(Source::Key(Key::Num1));
        assert_eq!(t.take(), vec![LiveNote::Bend { val: 8192 }]);
        assert_eq!(t.bend(), 8192);
        let mut got = Vec::new();
        for (key, step) in [(Key::Num4, 1), (Key::Num8, 5), (Key::Num3, 0)] {
            t.press(Source::Key(key), Action::Modulation(step));
            t.release(Source::Key(key));
            got.extend(t.take());
        }
        assert_eq!(
            got,
            vec![
                LiveNote::Modulation { val: 25 },
                LiveNote::Modulation { val: 127 },
                LiveNote::Modulation { val: 0 }
            ]
        );
    }

    #[test]
    fn the_mouse_plays_the_same_actions_and_key_and_mouse_on_one_note_keep_it_sounding() {
        let mut t = typing();
        t.press(Source::Mouse(Action::Note(0)), Action::Note(0));
        t.press(Source::Key(Key::A), Action::Note(0));
        assert_eq!(t.take(), vec![on(60, 93), on(60, 93)]);
        t.release(Source::Mouse(Action::Note(0)));
        assert!(t.take().is_empty(), "the key still holds it");
        t.release(Source::Key(Key::A));
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 60 }]);
    }

    #[test]
    fn releasing_everything_ends_every_note_the_bend_and_the_pedal() {
        let mut t = typing();
        t.press(Source::Key(Key::Tab), Action::Sustain);
        t.press(Source::Key(Key::A), Action::Note(0));
        t.press(Source::Key(Key::S), Action::Note(2));
        t.release(Source::Key(Key::A));
        t.press(Source::Key(Key::Num2), Action::BendUp);
        t.take();
        t.release_all();
        let out = t.take();
        assert!(
            out.contains(&LiveNote::Off { pitch: 60 })
                && out.contains(&LiveNote::Off { pitch: 62 }),
            "{out:?}"
        );
        assert!(out.contains(&LiveNote::Bend { val: 8192 }));
        assert!(!t.sustaining());
        assert_eq!(t.bend(), 8192);
        assert!(!t.is_down(Action::Note(2)));
    }

    /// A frame of events through a real egui context.
    fn key_event(key: Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn keys_from_egui_play_while_open_and_nothing_is_taken_when_closed_or_with_ctrl() {
        let mut h = crate::ui_test::Harness::new(400.0, 300.0);
        let mut t = MusicalTyping::new();
        // Closed: a key does nothing.
        h.frame(vec![key_event(Key::A, true, egui::Modifiers::NONE)], |ui| {
            t.read_keys(ui.ctx())
        });
        assert!(t.take().is_empty());
        h.frame(
            vec![key_event(Key::A, false, egui::Modifiers::NONE)],
            |ui| t.read_keys(ui.ctx()),
        );
        t.open = true;
        h.frame(vec![key_event(Key::A, true, egui::Modifiers::NONE)], |ui| {
            t.read_keys(ui.ctx())
        });
        assert_eq!(t.take(), vec![on(60, 93)]);
        h.frame(
            vec![key_event(Key::A, false, egui::Modifiers::NONE)],
            |ui| t.read_keys(ui.ctx()),
        );
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 60 }]);
        // Ctrl+A is the app's own command, not a note.
        h.frame(vec![key_event(Key::S, true, egui::Modifiers::CTRL)], |ui| {
            t.read_keys(ui.ctx())
        });
        assert!(t.take().is_empty());
        // The window closes with a key down: it is let go.
        h.frame(vec![key_event(Key::D, true, egui::Modifiers::NONE)], |ui| {
            t.read_keys(ui.ctx())
        });
        t.take();
        t.open = false;
        h.frame(vec![], |ui| t.read_keys(ui.ctx()));
        assert_eq!(t.take(), vec![LiveNote::Off { pitch: 64 }]);
    }

    #[test]
    fn the_window_draws_with_and_without_an_instrument_and_looking_plays_nothing() {
        let mut h = crate::ui_test::Harness::new(900.0, 600.0);
        let mut t = typing();
        for name in [Some("Inst 1"), None] {
            for _ in 0..3 {
                h.frame(vec![], |ui| show(ui.ctx(), &mut t, name));
            }
        }
        assert!(t.open, "it stays open");
        assert!(t.take().is_empty());
        t.open = false;
        h.frame(vec![], |ui| show(ui.ctx(), &mut t, Some("Inst 1")));
    }
}
