//! Project Chooser, shown by File > New from Template…: start an empty project, reopen a
//! recent one or start from a template, as in Logic. Choosing queues a File command
//! (`UiState::file_cmd`), so unsaved changes are asked about like everywhere else.
use crate::file_commands::FileCmd;
use crate::i18n::tr;
use crate::model::{Project, Track, TrackKind};
use crate::track_icons::{draw_icon, IconKind};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Key, Rect, Sense, Stroke, StrokeKind};
use std::path::PathBuf;

/// A project to start from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Template {
    /// One software instrument track (File > New).
    Empty,
    Songwriter,
    Electronic,
    Podcast,
}

impl Template {
    pub const ALL: [Template; 4] = [
        Template::Empty,
        Template::Songwriter,
        Template::Electronic,
        Template::Podcast,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Template::Empty => "Empty Project",
            Template::Songwriter => "Songwriter",
            Template::Electronic => "Electronic",
            Template::Podcast => "Podcast",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Template::Empty => "One software instrument track.",
            Template::Songwriter => "Piano, drummer, bass and vocal tracks.",
            Template::Electronic => "Synths, drum machine and a reverb aux.",
            Template::Podcast => "Two voice tracks and a music bed.",
        }
    }

    fn icon(self) -> IconKind {
        match self {
            Template::Empty => IconKind::Note,
            Template::Songwriter => IconKind::Piano,
            Template::Electronic => IconKind::Synth,
            Template::Podcast => IconKind::Mic,
        }
    }

    /// The new, untitled project this template starts.
    pub fn project(self) -> Project {
        let track = |name: &str, kind, instrument: Option<&'static str>| {
            let mut t = Track::new(name, kind);
            if instrument.is_some() {
                t.instrument = instrument;
            }
            t
        };
        let mut p = Project::empty();
        match self {
            Template::Empty => return p,
            Template::Songwriter => {
                p.tracks = vec![
                    track(
                        "Steinway Grand Piano",
                        TrackKind::Instrument,
                        Some("Sampler"),
                    ),
                    track("Drummer", TrackKind::Drummer, Some("Drum Kit")),
                    track("Bass", TrackKind::Audio, None),
                    track("Lead Vocal", TrackKind::Audio, None),
                ];
            }
            Template::Electronic => {
                p.tempo = 124.0;
                p.tracks = vec![
                    track("Drum Machine", TrackKind::Drummer, Some("Drum Machine")),
                    track("Synth Bass", TrackKind::Instrument, Some("Alchemy")),
                    track("Analog Pad", TrackKind::Instrument, Some("Alchemy")),
                    track("Reverb", TrackKind::Aux, None),
                ];
            }
            Template::Podcast => {
                p.tracks = vec![
                    track("Host", TrackKind::Audio, None),
                    track("Guest", TrackKind::Audio, None),
                    track("Music Bed", TrackKind::Audio, None),
                ];
            }
        }
        p.name = "Untitled".into();
        p.selected_track = 0;
        p
    }
}

/// Sidebar sections of the chooser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    NewProject,
    Recent,
    Templates,
}

impl Section {
    const ALL: [Section; 3] = [Section::NewProject, Section::Recent, Section::Templates];

    fn label(self) -> &'static str {
        match self {
            Section::NewProject => tr("New Project"),
            Section::Recent => tr("Recent"),
            Section::Templates => tr("Project Templates"),
        }
    }
}

/// State of the open chooser.
#[derive(Clone, Debug, PartialEq)]
pub struct Chooser {
    pub section: Section,
    /// Highlighted tile or row in the section.
    pub selected: usize,
}

impl Chooser {
    /// File > New from Template…: over the open project, which Cancel goes back to.
    pub fn templates() -> Self {
        Self {
            section: Section::Templates,
            selected: 0,
        }
    }
}

/// What the highlighted item does when chosen.
fn choice(c: &Chooser, recent: &[PathBuf]) -> Option<Choice> {
    match c.section {
        Section::NewProject => Some(Choice::Template(Template::Empty)),
        Section::Templates => Template::ALL.get(c.selected).copied().map(Choice::Template),
        Section::Recent => recent.get(c.selected).cloned().map(Choice::Recent),
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Choice {
    Template(Template),
    Recent(PathBuf),
}

/// Queues the File command for `choice`.
fn choose(choice: Choice, s: &mut UiState) {
    match choice {
        Choice::Template(t) => s.file_cmd = Some(FileCmd::NewFrom(t)),
        Choice::Recent(path) => {
            s.recent_pick = Some(path);
            s.file_cmd = Some(FileCmd::OpenRecent);
        }
    }
}

const BG: Color32 = Color32::from_rgb(0x26, 0x26, 0x28);
const SIDEBAR: Color32 = Color32::from_rgb(0x2f, 0x2f, 0x32);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xe6);
const DIM: Color32 = Color32::from_rgb(0x96, 0x96, 0x9a);
const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x6f, 0xd8);
const TILE: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3d);

/// A tile in the New Project / Templates grid. Returns (clicked, double-clicked).
fn tile(ui: &egui::Ui, r: Rect, t: Template, selected: bool, key: usize) -> (bool, bool) {
    let resp = ui.interact(r, egui::Id::unique(("chooser_tile", key)), Sense::click());
    let g = ui.painter();
    let fill = if resp.hovered() {
        TILE.lerp_to_gamma(Color32::WHITE, 0.06)
    } else {
        TILE
    };
    g.rect_filled(r, 8.0, fill);
    if selected {
        g.rect_stroke(r, 8.0, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    }
    let icon = Rect::from_center_size(pos2(r.center().x, r.top() + 52.0), vec2(56.0, 56.0));
    draw_icon(g, icon, t.icon());
    g.text(
        pos2(r.center().x, r.bottom() - 40.0),
        Align2::CENTER_CENTER,
        tr(t.name()),
        FontId::proportional(14.0),
        TEXT,
    );
    let about = g.layout(
        tr(t.about()).to_string(),
        FontId::proportional(11.0),
        DIM,
        r.width() - 16.0,
    );
    let at = pos2(r.center().x - about.size().x / 2.0, r.bottom() - 30.0);
    g.galley(at, about, DIM);
    (resp.clicked(), resp.double_clicked())
}

/// Draws the chooser over the whole window.
pub fn show(ui: &mut egui::Ui, s: &mut UiState) {
    let Some(mut c) = s.chooser.clone() else {
        return;
    };
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let g = ui.painter();
    g.rect_filled(area, 0.0, BG);

    // Sidebar.
    let side = Rect::from_min_size(area.min, vec2(220.0, area.height()));
    g.rect_filled(side, 0.0, SIDEBAR);
    g.text(
        side.left_top() + vec2(20.0, 28.0),
        Align2::LEFT_CENTER,
        "Pro Wave",
        FontId::proportional(18.0),
        TEXT,
    );
    for (k, sec) in Section::ALL.into_iter().enumerate() {
        let r = Rect::from_min_size(
            side.left_top() + vec2(10.0, 60.0 + k as f32 * 32.0),
            vec2(side.width() - 20.0, 28.0),
        );
        let resp = ui.interact(r, egui::Id::unique(("chooser_section", k)), Sense::click());
        if c.section == sec {
            g.rect_filled(r, 6.0, ACCENT);
        } else if resp.hovered() {
            g.rect_filled(r, 6.0, Color32::from_white_alpha(12));
        }
        g.text(
            r.left_center() + vec2(12.0, 0.0),
            Align2::LEFT_CENTER,
            sec.label(),
            FontId::proportional(13.5),
            TEXT,
        );
        if resp.clicked() && c.section != sec {
            c.section = sec;
            c.selected = 0;
        }
    }

    // Content.
    let main = Rect::from_min_max(pos2(side.right(), area.top()), area.max);
    let footer_h = 56.0;
    let body = Rect::from_min_max(
        main.min + vec2(28.0, 24.0),
        pos2(main.right() - 28.0, main.bottom() - footer_h),
    );
    g.text(
        body.left_top(),
        Align2::LEFT_TOP,
        c.section.label(),
        FontId::proportional(20.0),
        TEXT,
    );
    let list_top = body.top() + 44.0;
    let recent: Vec<PathBuf> = s.recent.paths().to_vec();
    let mut chosen = None;
    match c.section {
        Section::NewProject | Section::Templates => {
            let items: &[Template] = if c.section == Section::NewProject {
                &[Template::Empty]
            } else {
                &Template::ALL
            };
            let (w, h, gap) = (190.0, 170.0, 18.0);
            let per_row = (((body.width() + gap) / (w + gap)).floor() as usize).max(1);
            for (k, t) in items.iter().enumerate() {
                let (row, col) = (k / per_row, k % per_row);
                let r = Rect::from_min_size(
                    pos2(
                        body.left() + col as f32 * (w + gap),
                        list_top + row as f32 * (h + gap),
                    ),
                    vec2(w, h),
                );
                let (clicked, double) = tile(ui, r, *t, c.selected == k, k);
                if clicked {
                    c.selected = k;
                }
                if double {
                    chosen = Some(Choice::Template(*t));
                }
            }
        }
        Section::Recent => {
            if recent.is_empty() {
                ui.painter().text(
                    pos2(body.left(), list_top),
                    Align2::LEFT_TOP,
                    tr("No recent projects. Start one from New Project or Project Templates."),
                    FontId::proportional(13.0),
                    DIM,
                );
            }
            for (k, path) in recent.iter().enumerate() {
                let r = Rect::from_min_size(
                    pos2(body.left(), list_top + k as f32 * 44.0),
                    vec2(body.width(), 40.0),
                );
                let resp = ui.interact(r, egui::Id::unique(("chooser_recent", k)), Sense::click());
                let g = ui.painter();
                if c.selected == k {
                    g.rect_filled(r, 6.0, ACCENT);
                } else if resp.hovered() {
                    g.rect_filled(r, 6.0, Color32::from_white_alpha(12));
                }
                let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("?");
                let folder = path.parent().map(|d| d.display().to_string());
                g.text(
                    r.left_center() + vec2(12.0, -7.0),
                    Align2::LEFT_CENTER,
                    name,
                    FontId::proportional(14.0),
                    TEXT,
                );
                g.text(
                    r.left_center() + vec2(12.0, 9.0),
                    Align2::LEFT_CENTER,
                    folder.unwrap_or_default(),
                    FontId::proportional(11.0),
                    if c.selected == k { TEXT } else { DIM },
                );
                if resp.clicked() {
                    c.selected = k;
                }
                if resp.double_clicked() {
                    chosen = Some(Choice::Recent(path.clone()));
                }
            }
        }
    }

    // Footer: Open… on the left; Cancel and Choose on the right.
    let footer = Rect::from_min_max(pos2(main.left(), main.bottom() - footer_h), main.max);
    ui.painter().line_segment(
        [footer.left_top(), footer.right_top()],
        Stroke::new(1.0, Color32::from_gray(0x3c)),
    );
    let mut cancel = false;
    let footer_ui = footer.shrink2(vec2(24.0, 12.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(footer_ui), |ui| {
        ui.horizontal_centered(|ui| {
            if ui.button(tr("Open an existing project…")).clicked() {
                s.file_cmd = Some(FileCmd::Open);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_choose = choice(&c, &recent).is_some();
                let choose_btn =
                    egui::Button::new(egui::RichText::new(tr("Choose")).color(Color32::WHITE))
                        .fill(ACCENT);
                if ui.add_enabled(can_choose, choose_btn).clicked() {
                    chosen = choice(&c, &recent);
                }
                if ui.button(tr("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    });

    // Return chooses, Escape goes back to the open project.
    if !ui.ctx().egui_wants_keyboard_input() {
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            chosen = chosen.or_else(|| choice(&c, &recent));
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            cancel = true;
        }
    }

    if cancel {
        s.chooser = None;
        return;
    }
    // The chooser stays up until the File command has opened a project, so a cancelled
    // "save changes?" prompt or a failed open lands back here.
    s.chooser = Some(c);
    if let Some(choice) = chosen {
        choose(choice, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_start_untitled_projects_with_their_tracks() {
        for t in Template::ALL {
            let p = t.project();
            assert_eq!(p.name, "Untitled", "{t:?}");
            assert!(!p.tracks.is_empty(), "{t:?}");
            assert_eq!(p.selected_track, 0);
        }
        assert_eq!(Template::Empty.project().tracks.len(), 1);
        let kinds: Vec<_> = Template::Electronic
            .project()
            .tracks
            .iter()
            .map(|t| t.kind)
            .collect();
        assert!(kinds.contains(&TrackKind::Aux) && kinds.contains(&TrackKind::Drummer));
    }

    #[test]
    fn choosing_queues_the_matching_file_command() {
        let recent = vec![PathBuf::from("/music/Night Drive.prowave")];
        let mut c = Chooser::templates();
        c.selected = 1;
        assert_eq!(
            choice(&c, &recent),
            Some(Choice::Template(Template::Songwriter))
        );
        c.section = Section::Recent;
        c.selected = 0;
        let mut s = UiState::new();
        choose(choice(&c, &recent).unwrap(), &mut s);
        assert_eq!(s.file_cmd, Some(FileCmd::OpenRecent));
        assert_eq!(s.recent_pick, Some(recent[0].clone()));

        c.selected = 3;
        assert_eq!(choice(&c, &recent), None, "nothing to choose past the list");
        c.section = Section::NewProject;
        choose(choice(&c, &[]).unwrap(), &mut s);
        assert_eq!(s.file_cmd, Some(FileCmd::NewFrom(Template::Empty)));
    }
}
