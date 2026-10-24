//! Library (Logic Pro: Library pane): "Library / All Sounds" header, the selected patch's
//! icon and name, Search Sounds, the category column with the patch column next to it,
//! and the Revert / Delete / Save… footer.
//!
//! Choosing a patch loads it on the selected track: the track takes the patch name, its
//! instrument slot and Audio FX chain change, and the icon follows the category. Revert
//! restores what the track had before. The built-in engine has a single synth voice, so a
//! patch does not change the sound yet. Patch names are ProWave's own.
use crate::i18n::tr;
use crate::model::{Project, TrackKind};
use crate::track_icons::{draw_icon, track_icon};
use crate::widgets::{stroke, BTN_BG, TEXT, TEXT_DIM};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Painter, Rect, Sense, Shape, Stroke, Ui};

const PANEL_BG: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);
const ROW_H: f32 = 19.0;
const SEL_BG: Color32 = Color32::from_rgb(0x5a, 0x5a, 0x5a);

pub struct Category {
    pub name: &'static str,
    /// Instrument shown in the channel strip's instrument slot.
    pub instrument: Option<&'static str>,
    pub fx: &'static [&'static str],
    pub patches: &'static [&'static str],
}

const EQ_COMP: &[&str] = &["Channel EQ", "Compressor"];
const EQ_COMP_VERB: &[&str] = &["Channel EQ", "Compressor", "ChromaVerb"];
const KEYS_FX: &[&str] = &["Channel EQ", "Tremolo", "Tape Delay"];
const SYNTH_FX: &[&str] = &["Channel EQ", "Chorus", "Tape Delay", "Space D"];

pub static CATEGORIES: &[Category] = &[
    Category {
        name: "User Patches",
        instrument: None,
        fx: &[],
        patches: &[],
    },
    Category {
        name: "Bass",
        instrument: Some("Sampler"),
        fx: EQ_COMP,
        patches: &[
            "Deep Finger Bass",
            "Fretless Glide",
            "Picked Rock Bass",
            "Round Upright",
            "Slap Funk Bass",
            "Sub Synth Bass",
            "Vintage Muted Bass",
        ],
    },
    Category {
        name: "Drum Kit",
        instrument: Some("Drum Kit"),
        fx: EQ_COMP_VERB,
        patches: &[
            "Brushed Jazz Kit",
            "Dry Studio Kit",
            "Garage Room Kit",
            "Live Arena Kit",
            "Tight Funk Kit",
            "Warm Vintage Kit",
        ],
    },
    Category {
        name: "Electronic Drum Kit",
        instrument: Some("Drum Machine"),
        fx: EQ_COMP,
        patches: &[
            "Boom Bap Machine",
            "Classic 808 Kit",
            "Classic 909 Kit",
            "Electro Pulse Kit",
            "Lo-Fi Beat Box",
            "Trap Night Kit",
        ],
    },
    Category {
        name: "Guitar",
        instrument: Some("Sampler"),
        fx: &["Channel EQ", "Compressor", "Space D"],
        patches: &[
            "Bright Steel String",
            "Clean Strat Chords",
            "Nylon Concert",
            "Twelve String Shimmer",
            "Warm Jazz Hollow",
        ],
    },
    Category {
        name: "Keyboards",
        instrument: Some("E-Piano"),
        fx: KEYS_FX,
        patches: &[
            "Bright Tine Piano",
            "Classic Tine Piano",
            "Dark Tine Ballad",
            "Glassy Reed Piano",
            "Phased Tines",
            "Stage Suitcase",
            "Velvet Reed Piano",
            "Wide Tine Stereo",
        ],
    },
    Category {
        name: "Mallet",
        instrument: Some("Sampler"),
        fx: &["Channel EQ", "Space D"],
        patches: &[
            "Concert Marimba",
            "Soft Vibraphone",
            "Toy Glockenspiel",
            "Warm Xylophone",
        ],
    },
    Category {
        name: "Orchestral",
        instrument: Some("Studio Strings"),
        fx: &["Channel EQ", "Space D"],
        patches: &[
            "Chamber Ensemble",
            "Film Score Tutti",
            "Pizzicato Section",
            "Soft Woodwinds",
        ],
    },
    Category {
        name: "Percussion",
        instrument: Some("Sampler"),
        fx: EQ_COMP,
        patches: &[
            "Conga Set",
            "Frame Drum",
            "Shaker Loop Kit",
            "Tambourine Kit",
        ],
    },
    Category {
        name: "Piano",
        instrument: Some("Sampler"),
        fx: &["Channel EQ", "Compressor", "Space D"],
        patches: &[
            "Bright Concert Grand",
            "Felt Upright",
            "Intimate Studio Grand",
            "Mellow Grand",
            "Pop Rock Grand",
        ],
    },
    Category {
        name: "Quick Sampler",
        instrument: Some("Q-Sampler"),
        fx: EQ_COMP,
        patches: &["Chopped Vocal", "One Shot Hit", "Sliced Breakbeat"],
    },
    Category {
        name: "Studio Horns",
        instrument: Some("Studio Horns"),
        fx: &["Channel EQ", "Space D"],
        patches: &[
            "Brass Section",
            "Soft Flugelhorn",
            "Tenor Sax Solo",
            "Trombone Section",
        ],
    },
    Category {
        name: "Studio Strings",
        instrument: Some("Studio Strings"),
        fx: &["Channel EQ", "Space D"],
        patches: &[
            "Cello Section",
            "Full Strings Legato",
            "Strings Spiccato",
            "Violin Section",
        ],
    },
    Category {
        name: "Synthesizer",
        instrument: Some("Alchemy"),
        fx: SYNTH_FX,
        patches: &[
            "Airy Glass Pad",
            "Analog Drift Pad",
            "Bright Saw Lead",
            "Classic Analog Pad",
            "Detuned Supersaw",
            "Evolving Motion Pad",
            "Fat Mono Bass",
            "Hollow Pulse Lead",
            "Lush Poly Strings",
            "Night Drive Arp",
            "Soft Brass Synth",
            "Warm Juno-style Pad",
        ],
    },
    Category {
        name: "Vintage B3 Organ",
        instrument: Some("Vintage B3"),
        fx: &["Channel EQ", "Overdrive"],
        patches: &[
            "Classic Rock Organ",
            "Gospel Drawbars",
            "Jazz Combo Organ",
            "Soft Ballad Organ",
        ],
    },
    Category {
        name: "Vintage Clav",
        instrument: Some("Vintage Clav"),
        fx: &["Channel EQ", "Phaser"],
        patches: &["Funky Clav", "Muted Clav", "Wah Clav"],
    },
    Category {
        name: "Vintage Electric Piano",
        instrument: Some("E-Piano"),
        fx: KEYS_FX,
        patches: &[
            "Classic Tine Piano",
            "Mellow Reed Piano",
            "Suitcase Tremolo",
        ],
    },
    Category {
        name: "Vintage Mellotron",
        instrument: Some("Mellotron"),
        fx: &["Channel EQ", "Space D"],
        patches: &["Tape Choir", "Tape Flutes", "Tape Strings"],
    },
    Category {
        name: "World",
        instrument: Some("Sampler"),
        fx: &["Channel EQ", "Space D"],
        patches: &["Kalimba", "Koto", "Sitar", "Steel Drums"],
    },
    Category {
        name: "Arpeggiator",
        instrument: Some("Alchemy"),
        fx: SYNTH_FX,
        patches: &["Bouncing Arp", "Gated Pluck Arp", "Rising Sequence"],
    },
    Category {
        name: "Cinematic",
        instrument: Some("Alchemy"),
        fx: SYNTH_FX,
        patches: &["Dark Atmosphere", "Epic Riser", "Tension Drone"],
    },
    Category {
        name: "Legacy",
        instrument: Some("ES2"),
        fx: EQ_COMP,
        patches: &["Analog Brass", "Classic Pluck", "Retro Lead"],
    },
];

/// What the track had before patches were auditioned on it (for Revert).
#[derive(Clone)]
struct Saved {
    track: usize,
    name: String,
    instrument: Option<&'static str>,
    inserts: Vec<crate::model::Insert>,
    category: Option<&'static str>,
}

#[derive(Default)]
pub struct LibraryView {
    pub category: Option<usize>,
    pub search: String,
    /// (category, patch) last loaded.
    pub loaded: Option<(usize, usize)>,
    saved: Option<Saved>,
    scroll_left: f32,
    scroll_right: f32,
}

fn id(key: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    egui::Id::unique(("library", key))
}

fn load_patch(p: &mut Project, v: &mut LibraryView, cat: usize, patch: usize) {
    let Some(c) = CATEGORIES.get(cat) else { return };
    let Some(name) = c.patches.get(patch) else {
        return;
    };
    let ti = p.selected_track;
    let Some(t) = p.tracks.get_mut(ti) else {
        return;
    };
    // A folder only groups tracks; there is no channel strip to load a patch into.
    if t.kind == TrackKind::Folder {
        return;
    }
    if v.saved.as_ref().is_none_or(|s| s.track != ti) {
        v.saved = Some(Saved {
            track: ti,
            name: t.name.clone(),
            instrument: t.instrument,
            inserts: t.inserts.clone(),
            category: t.category,
        });
    }
    t.name = (*name).to_string();
    t.category = Some(c.name);
    t.inserts = crate::model::inserts_from_names(c.fx);
    // Only software instruments get an instrument: audio tracks keep their input and an aux
    // track its bus.
    if matches!(t.kind, TrackKind::Instrument | TrackKind::Drummer) {
        t.instrument = c.instrument;
    }
    v.loaded = Some((cat, patch));
}

fn revert(p: &mut Project, v: &mut LibraryView) {
    if let Some(s) = v.saved.take() {
        if let Some(t) = p.tracks.get_mut(s.track) {
            t.name = s.name;
            t.instrument = s.instrument;
            t.inserts = s.inserts;
            t.category = s.category;
        }
        v.loaded = None;
    }
}

/// A list row; returns the click response.
fn list_row(
    ui: &Ui,
    g: &Painter,
    r: Rect,
    key: (u8, usize, usize),
    text: &str,
    selected: bool,
    chevron: bool,
) -> egui::Response {
    let resp = ui.interact(r, id(key), Sense::click());
    if selected {
        g.rect_filled(r, 3.0, SEL_BG);
    } else if resp.hovered() {
        g.rect_filled(r, 3.0, Color32::from_white_alpha(10));
    }
    let font = FontId::proportional(12.5);
    let max_w = r.width() - if chevron { 22.0 } else { 10.0 };
    let shown = crate::mixer::elide(g, text, &font, max_w);
    g.text(
        pos2(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        shown,
        font,
        TEXT,
    );
    if chevron {
        let c = pos2(r.right() - 9.0, r.center().y);
        g.add(Shape::line(
            vec![
                c + vec2(-2.0, -4.0),
                c + vec2(2.0, 0.0),
                c + vec2(-2.0, 4.0),
            ],
            stroke(TEXT_DIM),
        ));
    }
    resp
}

/// Scrollable column of rows; returns the clicked index.
#[allow(clippy::too_many_arguments)]
fn column(
    ui: &Ui,
    g: &Painter,
    rect: Rect,
    which: u8,
    scroll: &mut f32,
    items: &[String],
    selected: Option<usize>,
    chevrons: bool,
) -> Option<usize> {
    if ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|pt| rect.contains(pt))
    {
        *scroll -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    let content = items.len() as f32 * ROW_H;
    *scroll = scroll.clamp(0.0, (content - rect.height()).max(0.0));
    let gc = g.with_clip_rect(rect.intersect(g.clip_rect()));
    let mut clicked = None;
    for (i, text) in items.iter().enumerate() {
        let y = rect.top() + i as f32 * ROW_H - *scroll;
        if y + ROW_H < rect.top() || y > rect.bottom() {
            continue;
        }
        let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(rect.width() - 8.0, ROW_H))
            .intersect(rect);
        if list_row(
            ui,
            &gc,
            r,
            (which, i, items.len()),
            text,
            selected == Some(i),
            chevrons,
        )
        .clicked()
        {
            clicked = Some(i);
        }
    }
    clicked
}

fn footer_button(ui: &Ui, g: &Painter, r: Rect, key: &str, label: &str, enabled: bool) -> bool {
    let resp = enabled.then(|| ui.interact(r, id(("footer", key)), Sense::click()));
    let hovered = resp.as_ref().is_some_and(|r| r.hovered());
    g.rect_filled(
        r,
        4.0,
        if hovered {
            Color32::from_gray(0x62)
        } else {
            BTN_BG
        },
    );
    g.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.5),
        if enabled {
            TEXT
        } else {
            Color32::from_gray(0x78)
        },
    );
    resp.is_some_and(|r| r.clicked())
}

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState) {
    let panel = ui.available_rect_before_wrap();
    ui.allocate_rect(panel, Sense::hover());
    let g = ui.painter_at(panel);
    g.rect_filled(panel, 0.0, PANEL_BG);
    g.line_segment(
        [panel.right_top(), panel.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(0x22)),
    );
    let v = &mut s.library;
    let left = panel.left();
    let w = panel.width();

    // Header: "Library  All Sounds ⇕"
    let hy = panel.top() + 14.0;
    let lib = g.text(
        pos2(panel.center().x - 6.0, hy),
        Align2::RIGHT_CENTER,
        tr("Library"),
        FontId::proportional(13.0),
        TEXT,
    );
    let all = g.text(
        pos2(panel.center().x + 2.0, hy),
        Align2::LEFT_CENTER,
        tr("All Sounds"),
        FontId::proportional(12.5),
        TEXT_DIM,
    );
    let c = pos2(all.right() + 8.0, hy);
    g.add(Shape::line(
        vec![
            c + vec2(-2.5, -1.5),
            c + vec2(0.0, -4.0),
            c + vec2(2.5, -1.5),
        ],
        stroke(TEXT_DIM),
    ));
    g.add(Shape::line(
        vec![c + vec2(-2.5, 1.5), c + vec2(0.0, 4.0), c + vec2(2.5, 1.5)],
        stroke(TEXT_DIM),
    ));
    let _ = lib;

    // Big icon of the selected track's sound, and its name.
    let icon = Rect::from_center_size(pos2(panel.center().x, panel.top() + 82.0), vec2(84.0, 84.0));
    if let Some(t) = p.tracks.get(p.selected_track) {
        draw_icon(&g, icon, track_icon(t));
        let font = FontId::proportional(12.5);
        let name = crate::mixer::elide(&g, &t.name, &font, w - 20.0);
        g.text(
            pos2(panel.center().x, icon.bottom() + 16.0),
            Align2::CENTER_CENTER,
            name,
            font,
            TEXT,
        );
    }

    // Search field.
    let search_r =
        Rect::from_min_size(pos2(left + 8.0, icon.bottom() + 32.0), vec2(w - 16.0, 22.0));
    g.rect_filled(search_r, 5.0, Color32::from_gray(0x2a));
    let mc = pos2(search_r.left() + 12.0, search_r.center().y);
    g.circle_stroke(mc + vec2(-1.0, -1.0), 4.0, stroke(TEXT_DIM));
    g.line_segment([mc + vec2(2.0, 2.0), mc + vec2(5.0, 5.0)], stroke(TEXT_DIM));
    let edit_r = Rect::from_min_max(
        pos2(search_r.left() + 22.0, search_r.top() + 2.0),
        pos2(search_r.right() - 6.0, search_r.bottom() - 2.0),
    );
    ui.put(
        edit_r,
        egui::TextEdit::singleline(&mut v.search)
            .hint_text(tr("Search Sounds"))
            .frame(egui::Frame::NONE)
            .text_color(TEXT)
            .font(FontId::proportional(12.5)),
    );

    // Columns.
    let footer_h = 34.0;
    let cols = Rect::from_min_max(
        pos2(left, search_r.bottom() + 8.0),
        pos2(panel.right(), panel.bottom() - footer_h),
    );
    let query = v.search.trim().to_lowercase();
    if !query.is_empty() {
        // Search results: one column of "Patch" rows across every category.
        let hits: Vec<(usize, usize)> = CATEGORIES
            .iter()
            .enumerate()
            .flat_map(|(ci, c)| {
                c.patches
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.to_lowercase().contains(&query))
                    .map(move |(pi, _)| (ci, pi))
            })
            .collect();
        let items: Vec<String> = hits
            .iter()
            .map(|(ci, pi)| CATEGORIES[*ci].patches[*pi].to_string())
            .collect();
        let sel = v.loaded.and_then(|l| hits.iter().position(|h| *h == l));
        if let Some(i) = column(ui, &g, cols, 2, &mut v.scroll_right, &items, sel, false) {
            let (ci, pi) = hits[i];
            load_patch(p, v, ci, pi);
        }
        if hits.is_empty() {
            g.text(
                pos2(cols.center().x, cols.top() + 20.0),
                Align2::CENTER_CENTER,
                tr("No results"),
                FontId::proportional(12.0),
                TEXT_DIM,
            );
        }
    } else {
        let split = if v.category.is_some() {
            left + w * 0.5
        } else {
            panel.right()
        };
        let left_col = Rect::from_min_max(cols.min, pos2(split, cols.bottom()));
        let cat_items: Vec<String> = CATEGORIES.iter().map(|c| tr(c.name).to_string()).collect();
        if let Some(i) = column(
            ui,
            &g,
            left_col,
            0,
            &mut v.scroll_left,
            &cat_items,
            v.category,
            true,
        ) {
            v.category = if v.category == Some(i) { None } else { Some(i) };
            v.scroll_right = 0.0;
        }
        if let Some(ci) = v.category {
            let right_col = Rect::from_min_max(pos2(split, cols.top()), cols.max);
            g.line_segment(
                [pos2(split, cols.top()), pos2(split, cols.bottom())],
                Stroke::new(1.0, Color32::from_gray(0x2c)),
            );
            let items: Vec<String> = CATEGORIES[ci]
                .patches
                .iter()
                .map(|n| n.to_string())
                .collect();
            let sel = v.loaded.filter(|(c, _)| *c == ci).map(|(_, pi)| pi);
            if let Some(pi) = column(
                ui,
                &g,
                right_col,
                1,
                &mut v.scroll_right,
                &items,
                sel,
                false,
            ) {
                load_patch(p, v, ci, pi);
            }
        }
    }

    // Footer: Revert / Delete / Save…
    let fy = panel.bottom() - footer_h + 6.0;
    g.line_segment(
        [pos2(left, fy - 6.0), pos2(panel.right(), fy - 6.0)],
        Stroke::new(1.0, Color32::from_gray(0x2a)),
    );
    let bw = (w - 32.0) / 3.0;
    let can_revert = v.saved.is_some();
    if footer_button(
        ui,
        &g,
        Rect::from_min_size(pos2(left + 8.0, fy), vec2(bw, 22.0)),
        "revert",
        tr("Revert"),
        can_revert,
    ) {
        revert(p, v);
    }
    footer_button(
        ui,
        &g,
        Rect::from_min_size(pos2(left + 16.0 + bw, fy), vec2(bw, 22.0)),
        "delete",
        tr("Delete"),
        false,
    );
    footer_button(
        ui,
        &g,
        Rect::from_min_size(pos2(left + 24.0 + 2.0 * bw, fy), vec2(bw, 22.0)),
        "save",
        tr("Save…"),
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn category(name: &str) -> usize {
        CATEGORIES
            .iter()
            .position(|c| c.name == name)
            .unwrap_or(usize::MAX)
    }

    #[test]
    fn loading_a_patch_updates_the_selected_track_and_revert_restores_it() {
        let mut p = Project::demo();
        p.selected_track = 0;
        let before = (
            p.tracks[0].name.clone(),
            p.tracks[0].instrument,
            p.tracks[0].inserts.clone(),
        );
        let mut v = LibraryView::default();
        let synth = category("Synthesizer");
        load_patch(&mut p, &mut v, synth, 0);
        let t = &p.tracks[0];
        assert_eq!(t.name, CATEGORIES[synth].patches[0]);
        assert_eq!(t.instrument, Some("Alchemy"));
        assert_eq!(
            t.inserts,
            crate::model::inserts_from_names(CATEGORIES[synth].fx)
        );
        assert_eq!(t.category, Some("Synthesizer"));
        // A second audition keeps the original state for Revert.
        load_patch(&mut p, &mut v, category("Bass"), 1);
        revert(&mut p, &mut v);
        let t = &p.tracks[0];
        assert_eq!((t.name.clone(), t.instrument, t.inserts.clone()), before);
        assert_eq!(t.category, None);
        assert!(v.loaded.is_none());
    }

    #[test]
    fn aux_tracks_take_the_channel_strip_but_not_an_instrument_and_folders_nothing() {
        let mut p = Project::demo();
        let aux = p.add_track(TrackKind::Aux);
        let mut v = LibraryView::default();
        load_patch(&mut p, &mut v, category("Guitar"), 0);
        assert_eq!(p.tracks[aux].instrument, None);
        assert_eq!(p.tracks[aux].category, Some("Guitar"));

        let folder = p.add_track(TrackKind::Folder);
        let name = p.tracks[folder].name.clone();
        load_patch(&mut p, &mut v, category("Bass"), 0);
        assert_eq!(p.tracks[folder].name, name);
        assert_eq!(p.tracks[folder].category, None);
    }

    #[test]
    fn audio_tracks_keep_their_input_instead_of_an_instrument() {
        let mut p = Project::demo();
        let audio = p
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Audio)
            .unwrap_or(0);
        p.selected_track = audio;
        let mut v = LibraryView::default();
        load_patch(&mut p, &mut v, category("Guitar"), 0);
        assert_eq!(p.tracks[audio].instrument, None);
        assert_eq!(p.tracks[audio].category, Some("Guitar"));
    }

    #[test]
    fn every_category_except_user_patches_has_patches() {
        for c in CATEGORIES.iter().skip(1) {
            assert!(!c.patches.is_empty(), "{}", c.name);
        }
    }
}
