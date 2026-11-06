//! Logic's List Editors (View > Show List Editors, D): the Event list on the right. It lists
//! the MIDI events of the selected region (or of the selected track) with their position,
//! note, velocity and length, and a line marks where the playhead is. While the song plays the
//! line moves down the list at the speed of the notes (fast passages go quickly, slow ones
//! slowly) and the list pages along with it.

use crate::i18n::tr;
use crate::model::{Project, TrackKind};
use crate::widgets::{BLUE, TEXT, TEXT_DIM};
use crate::UiState;
use egui::{pos2, vec2, Align2, Color32, FontId, Painter, Rect, Sense, Stroke, Ui};

pub const WIDTH: f32 = 330.0;
const ROW_H: f32 = 17.5;
const TICKS_PER_BEAT: i64 = 960;
const TICKS_PER_DIVISION: i64 = 240;

const BG: Color32 = Color32::from_rgb(33, 33, 33);
const TAB_ON: Color32 = Color32::from_rgb(64, 64, 64);
const TAB_OFF: Color32 = Color32::from_rgb(47, 47, 47);
const TOOLBAR_BG: Color32 = Color32::from_rgb(55, 55, 55);
const FILTER_BG: Color32 = Color32::from_rgb(66, 66, 66);
const FILTER_BLUE: Color32 = Color32::from_rgb(29, 74, 134);
const FILTER_TEXT: Color32 = Color32::from_rgb(190, 214, 245);
const HEAD_BG: Color32 = Color32::from_rgb(45, 45, 45);

/// The list's own state.
pub struct ListView {
    /// Pixels the list is scrolled down.
    pub scroll: f32,
    /// Logic's Catch Playhead: the list follows the playhead while the song plays.
    pub follow: bool,
}

impl Default for ListView {
    fn default() -> Self {
        Self {
            scroll: 0.0,
            follow: true,
        }
    }
}

/// One note of the list.
struct Event {
    /// Position in beats from the start of the song.
    beats: f64,
    /// Belongs to another region of the track: shown grey, as context.
    dim: bool,
    /// A controller event (the sustain pedal) rather than a note.
    control: bool,
    /// The note's pitch, or the controller's number.
    pitch: u8,
    /// The note's velocity, or the controller's value.
    vel: u8,
    len: f64,
}

/// The events to show: those of the selected MIDI region, with the other regions of its track
/// in grey around them (Logic shows them as context, and while a take is recorded the notes
/// of the earlier ones stay in view). With no region selected: all of the selected track's.
fn events(p: &Project, s: &UiState) -> Vec<Event> {
    let (track, selected) = match s.tracks.selected {
        Some((t, r))
            if p.tracks
                .get(t)
                .is_some_and(|tr| tr.kind != TrackKind::Audio && r < tr.regions.len()) =>
        {
            (t, Some(r))
        }
        _ => (p.selected_track, None),
    };
    let Some(track) = p.tracks.get(track).filter(|t| t.kind != TrackKind::Audio) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (ri, r) in track.regions.iter().enumerate() {
        let offset = p.region_start_beats(r);
        let dim = selected.is_some_and(|sr| sr != ri);
        for c in &r.controls {
            out.push(Event {
                beats: offset + c.start as f64,
                dim,
                control: true,
                pitch: c.num,
                // The list shows one byte: a pitch bend (14 bits) is clamped here.
                vel: c.val.min(255) as u8,
                len: 0.0,
            });
        }
        for n in &r.notes {
            out.push(Event {
                beats: offset + n.start as f64,
                dim,
                control: false,
                pitch: n.pitch,
                vel: n.vel,
                len: n.len as f64,
            });
        }
    }
    // A controller at the same place as a note comes first, as in Logic.
    out.sort_by(|a, b| {
        a.beats
            .total_cmp(&b.beats)
            .then(b.control.cmp(&a.control))
            .then(a.pitch.cmp(&b.pitch))
    });
    out
}

/// Logic's position "bar beat division" and the ticks, from a position in beats.
fn position_parts(beats: f64, bpb: f64) -> ([i64; 3], i64) {
    let total = (beats * TICKS_PER_BEAT as f64).round().max(0.0) as i64;
    let per_bar = ((bpb * TICKS_PER_BEAT as f64).round() as i64).max(1);
    let rem = total % per_bar;
    let in_beat = rem % TICKS_PER_BEAT;
    (
        [
            total / per_bar + 1,
            rem / TICKS_PER_BEAT + 1,
            in_beat / TICKS_PER_DIVISION + 1,
        ],
        in_beat % TICKS_PER_DIVISION + 1,
    )
}

/// [`position_parts`] for a position in the song: bars have the length of their signature.
fn position_in(p: &Project, beats: f64) -> ([i64; 3], i64) {
    let bar0 = p.beats_to_bar(beats).floor();
    let (mut parts, tick) = position_parts(beats - p.bar_to_beats(bar0), p.bar_len(bar0 as u32));
    parts[0] += bar0 as i64;
    (parts, tick)
}

/// A length as Logic shows it: bars, beats, divisions and ticks, all counted from zero.
fn length_parts(len: f64, bpb: f64) -> [i64; 4] {
    let total = (len * TICKS_PER_BEAT as f64).round().max(0.0) as i64;
    let per_bar = ((bpb * TICKS_PER_BEAT as f64).round() as i64).max(1);
    let rem = total % per_bar;
    let in_beat = rem % TICKS_PER_BEAT;
    [
        total / per_bar,
        rem / TICKS_PER_BEAT,
        in_beat / TICKS_PER_DIVISION,
        in_beat % TICKS_PER_DIVISION,
    ]
}

/// Logic names the octaves so that pitch 60 is C3.
fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[(pitch % 12) as usize],
        (pitch / 12) as i32 - 2
    )
}

/// What the Length/Info column says for a controller.
fn controller_name(num: u8) -> String {
    match num {
        crate::recorder::SUSTAIN => tr("Sustain").to_string(),
        n => format!("{} {n}", tr("Controller")),
    }
}

/// Index of the first event the playhead has not reached yet: the line goes above it.
fn playhead_row(events: &[Event], position: f64) -> usize {
    events.partition_point(|e| e.beats <= position + 1e-9)
}

/// The scroll that follows the playhead: it stays where it is while the line is in view and
/// pages along (line near the top) when the line reaches the bottom or leaves at the top.
fn followed_scroll(scroll: f32, line_y: f32, view_h: f32, max_scroll: f32) -> f32 {
    let rel = line_y - scroll;
    let new = if rel > view_h * 0.85 || rel < 0.0 {
        line_y - view_h * 0.15
    } else {
        scroll
    };
    new.clamp(0.0, max_scroll.max(0.0))
}

fn chevron_down(g: &Painter, c: egui::Pos2) {
    let st = Stroke::new(1.2, TEXT_DIM);
    g.line_segment([c + vec2(-3.0, -1.5), c + vec2(0.0, 1.5)], st);
    g.line_segment([c + vec2(0.0, 1.5), c + vec2(3.0, -1.5)], st);
}

pub fn show(ui: &mut Ui, p: &mut Project, s: &mut UiState) {
    let area = ui.max_rect();
    let g = ui.painter_at(area);
    g.rect_filled(area, 0.0, BG);
    let events = events(p, s);

    // Tabs: Event / Marker / Tempo / Signature (only the Event list exists so far).
    let mut y = area.top();
    let tabs = [tr("Event"), tr("Marker"), tr("Tempo"), tr("Signature")];
    let tab_w = area.width() / tabs.len() as f32;
    for (i, label) in tabs.iter().enumerate() {
        let r = Rect::from_min_size(pos2(area.left() + i as f32 * tab_w, y), vec2(tab_w, 26.0));
        g.rect_filled(r, 0.0, if i == 0 { TAB_ON } else { TAB_OFF });
        g.text(
            r.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(12.0),
            if i == 0 {
                TEXT
            } else {
                Color32::from_gray(0xb4)
            },
        );
    }
    y += 26.0;

    // Toolbar: back, Edit / Functions / View, Catch Playhead.
    let bar = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 28.0));
    g.rect_filled(bar, 0.0, TOOLBAR_BG);
    let st = Stroke::new(1.4, Color32::from_gray(0xc8));
    let cb = pos2(bar.left() + 16.0, bar.center().y);
    g.line_segment([cb + vec2(3.0, -4.0), cb + vec2(-2.0, 0.0)], st);
    g.line_segment([cb + vec2(-2.0, 0.0), cb + vec2(3.0, 4.0)], st);
    let mut x = bar.left() + 44.0;
    for label in [tr("Edit"), tr("Functions"), tr("View")] {
        let galley = g.layout_no_wrap(label.into(), FontId::proportional(12.5), TEXT);
        let w = galley.size().x;
        g.text(
            pos2(x, bar.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(12.5),
            TEXT,
        );
        chevron_down(&g, pos2(x + w + 8.0, bar.center().y + 1.0));
        x += w + 24.0;
    }
    let catch = Rect::from_center_size(pos2(bar.right() - 54.0, bar.center().y), vec2(28.0, 20.0));
    let resp = ui.interact(
        catch,
        egui::Id::unique(("event_list", "catch")),
        Sense::click(),
    );
    if resp.clicked() {
        s.lists.follow = !s.lists.follow;
    }
    g.rect_filled(
        catch,
        4.0,
        if s.lists.follow {
            BLUE
        } else {
            Color32::from_gray(0x58)
        },
    );
    g.text(
        catch.center(),
        Align2::CENTER_CENTER,
        ">T<",
        FontId::proportional(11.0),
        Color32::WHITE,
    );
    y += 28.0;

    // Event type filters.
    let filters = [
        [
            tr("Notes"),
            tr("Progr. Change"),
            tr("Pitch Bend"),
            tr("Controller"),
        ],
        [
            tr("Aftertouch"),
            tr("Poly Aftertouch"),
            tr("Syst. Exclusive"),
            tr("Additional Info"),
        ],
    ];
    let block = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 52.0));
    g.rect_filled(block, 0.0, FILTER_BG);
    let bw = (area.width() - 10.0 - 3.0 * 4.0) / 4.0;
    for (row, labels) in filters.iter().enumerate() {
        for (col, label) in labels.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(
                    area.left() + 5.0 + col as f32 * (bw + 4.0),
                    y + 5.0 + row as f32 * 22.0,
                ),
                vec2(bw, 18.0),
            );
            g.rect_filled(r, 3.0, FILTER_BLUE);
            let font = FontId::proportional(10.5);
            let text = crate::mixer::elide(&g, label, &font, bw - 6.0);
            g.text(r.center(), Align2::CENTER_CENTER, text, font, FILTER_TEXT);
        }
    }
    y += 52.0;

    // The list's own header: add, title and quantize.
    let head = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 26.0));
    g.rect_filled(head, 0.0, HEAD_BG);
    let plus = pos2(head.left() + 16.0, head.center().y);
    let st = Stroke::new(1.4, Color32::from_gray(0xc8));
    g.line_segment([plus + vec2(-5.0, 0.0), plus + vec2(5.0, 0.0)], st);
    g.line_segment([plus + vec2(0.0, -5.0), plus + vec2(0.0, 5.0)], st);
    g.text(
        pos2(head.left() + 42.0, head.center().y),
        Align2::LEFT_CENTER,
        tr("Notes"),
        FontId::proportional(12.5),
        TEXT,
    );
    g.text(
        pos2(head.right() - 110.0, head.center().y),
        Align2::RIGHT_CENTER,
        tr("Quantize:"),
        FontId::proportional(11.5),
        TEXT_DIM,
    );
    let qbox = Rect::from_min_size(
        pos2(head.right() - 104.0, head.top() + 4.0),
        vec2(70.0, 18.0),
    );
    g.rect_filled(qbox, 3.0, Color32::from_gray(0x25));
    g.text(
        qbox.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        tr("Off"),
        FontId::proportional(11.5),
        TEXT,
    );
    chevron_down(&g, qbox.right_center() - vec2(9.0, 0.0));
    g.text(
        pos2(head.right() - 16.0, head.center().y),
        Align2::CENTER_CENTER,
        "Q",
        FontId::proportional(11.5),
        TEXT,
    );
    y += 26.0;

    // Column titles.
    let cols = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 20.0));
    g.rect_filled(cols, 0.0, Color32::from_rgb(47, 47, 47));
    let f = FontId::proportional(10.5);
    let dim = Color32::from_gray(0xc0);
    let cx = |v: f32| area.left() + v;
    for (v, label) in [(6.0, "L"), (20.0, "M")] {
        g.text(
            pos2(cx(v), cols.center().y),
            Align2::CENTER_CENTER,
            label,
            f.clone(),
            dim,
        );
    }
    for (v, label) in [
        (68.0, tr("Position")),
        (133.0, tr("Status")),
        (170.0, tr("Ch")),
        (200.0, tr("Num")),
        (232.0, tr("Val")),
        (290.0, tr("Length/Info")),
    ] {
        let t = crate::mixer::elide(&g, label, &f, 60.0);
        g.text(
            pos2(cx(v), cols.center().y),
            Align2::CENTER_CENTER,
            t,
            f.clone(),
            dim,
        );
    }
    y += 20.0;

    // The rows.
    let list = Rect::from_min_max(pos2(area.left(), y), area.max);
    let content_h = events.len() as f32 * ROW_H;
    let max_scroll = (content_h - list.height() + ROW_H).max(0.0);
    let resp = ui.interact(
        list,
        egui::Id::unique(("event_list", "rows")),
        Sense::hover(),
    );
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        s.lists.scroll = (s.lists.scroll - dy).clamp(0.0, max_scroll);
    }
    let line_row = playhead_row(&events, p.position_beats);
    let line_y = line_row as f32 * ROW_H;
    if s.lists.follow && p.playing {
        s.lists.scroll = followed_scroll(s.lists.scroll, line_y, list.height(), max_scroll);
    }
    s.lists.scroll = s.lists.scroll.clamp(0.0, max_scroll);

    let gl = ui.painter_at(list);
    let first = (s.lists.scroll / ROW_H).floor() as usize;
    let last = (((s.lists.scroll + list.height()) / ROW_H).ceil() as usize).min(events.len());
    let font = FontId::monospace(11.0);
    let ink = Color32::from_gray(0xde);
    for (i, e) in events.iter().enumerate().take(last).skip(first) {
        let cy = list.top() + i as f32 * ROW_H - s.lists.scroll + ROW_H / 2.0;
        let ([bar, beat, div], tick) = position_in(p, e.beats);
        let l = length_parts(e.len, p.bar_len_at(e.beats));
        let color = if e.dim { Color32::from_gray(0x76) } else { ink };
        let at = |x: f32, align: Align2, text: String| {
            gl.text(pos2(cx(x), cy), align, text, font.clone(), color);
        };
        at(80.0, Align2::RIGHT_CENTER, format!("{bar} {beat} {div}"));
        at(103.0, Align2::RIGHT_CENTER, format!("{tick}"));
        at(173.0, Align2::RIGHT_CENTER, "1".to_string());
        at(241.0, Align2::RIGHT_CENTER, format!("{}", e.vel));
        if e.control {
            at(115.0, Align2::LEFT_CENTER, tr("Control").to_string());
            at(200.0, Align2::CENTER_CENTER, format!("{}", e.pitch));
            at(250.0, Align2::LEFT_CENTER, controller_name(e.pitch));
        } else {
            at(115.0, Align2::LEFT_CENTER, tr("Note").to_string());
            at(200.0, Align2::CENTER_CENTER, note_name(e.pitch));
            at(
                323.0,
                Align2::RIGHT_CENTER,
                format!("{} {} {} {}", l[0], l[1], l[2], l[3]),
            );
        }
    }
    // The playhead line, between the notes that have sounded and those still to come.
    let ly = list.top() + line_y - s.lists.scroll;
    if ly >= list.top() && ly <= list.bottom() && !events.is_empty() {
        gl.line_segment(
            [pos2(list.left(), ly), pos2(list.right(), ly)],
            Stroke::new(1.5, crate::tracks_area::playhead_colors(p.recording).0),
        );
    }
    // The edge towards the tracks.
    g.line_segment(
        [area.left_top(), area.left_bottom()],
        Stroke::new(1.5, Color32::from_rgb(70, 96, 140)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_bars_beats_divisions_and_ticks_from_one() {
        assert_eq!(position_parts(0.0, 4.0), ([1, 1, 1], 1));
        // Bar 29, beat 2, division 3, 10 ticks in: Logic shows "29 2 3 11".
        let beats = 28.0 * 4.0 + 1.0 + 0.5 + 10.0 / 960.0;
        assert_eq!(position_parts(beats, 4.0), ([29, 2, 3], 11));
    }

    #[test]
    fn lengths_count_from_zero() {
        assert_eq!(length_parts(0.0, 4.0), [0, 0, 0, 0]);
        // Two divisions and 214 ticks, as in "0 0 2 214".
        let len = (2.0 * 240.0 + 214.0) / 960.0;
        assert_eq!(length_parts(len, 4.0), [0, 0, 2, 214]);
        assert_eq!(length_parts(4.0 + 1.0, 4.0), [1, 1, 0, 0]);
    }

    #[test]
    fn notes_are_named_like_logic() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(63), "D#3");
        assert_eq!(note_name(0), "C-2");
    }

    #[test]
    fn the_line_sits_between_played_and_coming_notes() {
        let ev = |beats| Event {
            beats,
            dim: false,
            control: false,
            pitch: 60,
            vel: 100,
            len: 1.0,
        };
        let events = [ev(0.0), ev(1.0), ev(1.0), ev(2.0)];
        assert_eq!(playhead_row(&events, -0.5), 0);
        assert_eq!(playhead_row(&events, 0.0), 1);
        assert_eq!(playhead_row(&events, 1.0), 3);
        assert_eq!(playhead_row(&events, 9.0), 4);
    }

    #[test]
    fn the_list_pages_along_with_the_line() {
        // In view: stays.
        assert_eq!(followed_scroll(100.0, 200.0, 400.0, 1000.0), 100.0);
        // Past the bottom: the line goes back near the top.
        assert_eq!(followed_scroll(100.0, 480.0, 400.0, 1000.0), 420.0);
        // Above the view (after a loop): follows up.
        assert!((followed_scroll(500.0, 100.0, 400.0, 1000.0) - 40.0).abs() < 1e-3);
        // Never past the end.
        assert_eq!(followed_scroll(0.0, 2000.0, 400.0, 600.0), 600.0);
    }

    #[test]
    fn faster_notes_move_the_line_faster() {
        let ev = |beats| Event {
            beats,
            dim: false,
            control: false,
            pitch: 60,
            vel: 100,
            len: 0.25,
        };
        let fast: Vec<Event> = (0..32).map(|i| ev(i as f64 * 0.25)).collect();
        let slow: Vec<Event> = (0..32).map(|i| ev(i as f64 * 1.0)).collect();
        assert!(playhead_row(&fast, 2.0) > playhead_row(&slow, 2.0));
    }
}
