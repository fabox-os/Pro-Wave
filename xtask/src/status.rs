//! `cargo xtask status`: how far Pro Wave has come, for the README. Counts the menu entries
//! that work (what `menus::enabled` turns on) and the ROADMAP.md checkboxes of the phases, draws
//! `docs/status.svg` and rewrites the block between the status markers in README.md.
use std::fmt::Write as _;
use std::path::Path;

const SVG: &str = "docs/status.svg";
const START: &str = "<!-- status:start (made by `cargo xtask status`) -->";
const END: &str = "<!-- status:end -->";

const WIDTH: f32 = 1060.0;
const HEIGHT: f32 = 520.0;
const MARGIN: f32 = 16.0;
const CHART_TOP: f32 = 84.0;

const DONE: &str = "#2ea043";
const PARTIAL: &str = "#b08a1e";
const TODO: &str = "#3a3d44";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Done,
    Partial,
    Todo,
}

#[derive(Debug, PartialEq, Eq)]
struct Group {
    name: String,
    items: Vec<State>,
}

impl Group {
    fn count(&self, state: State) -> usize {
        self.items.iter().filter(|s| **s == state).count()
    }
}

fn total(groups: &[Group]) -> usize {
    groups.iter().map(|g| g.items.len()).sum()
}

fn counted(groups: &[Group], state: State) -> usize {
    groups.iter().map(|g| g.count(state)).sum()
}

fn percent(groups: &[Group]) -> f32 {
    100.0 * counted(groups, State::Done) as f32 / total(groups).max(1) as f32
}

/// The double-quoted strings in `text`.
fn quoted(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

/// The text from `from` up to `to`, or nothing.
fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let Some(at) = text.find(from) else {
        return "";
    };
    let rest = &text[at..];
    rest.find(to).map_or("", |end| &rest[..end])
}

/// Each menu of `menus.rs` with its entries, submenus opened. A menu entry that opens the list
/// of languages or of recent projects is left out: it says nothing about the work done.
fn menu_groups(menus: &str, file_commands: &str) -> Vec<Group> {
    let enabled = between(menus, "pub(crate) fn enabled", "/// Start of every region");
    let from_menu = between(file_commands, "pub fn from_menu", "pub fn pressed");
    let works =
        |label: &str| quoted(enabled).contains(&label) || quoted(from_menu).contains(&label);
    let submenus = between(menus, "const SUBMENUS", "/// What is in the submenu");
    let children = |title: &str| -> Option<Vec<&str>> {
        submenus
            .split('(')
            .map(quoted)
            .find(|q| q.first() == Some(&title) && q.len() > 1)
            .map(|q| q[1..].to_vec())
    };
    let table = between(menus, "pub(crate) const MENUS", "];");
    let mut groups = Vec::new();
    for line in table.lines() {
        let Some((name, constant)) = line.trim().strip_prefix("(\"").and_then(|l| {
            let (name, rest) = l.split_once("\", ")?;
            Some((name, rest.trim_end_matches("),")))
        }) else {
            continue;
        };
        let block = between(menus, &format!("const {constant}: &[&str]"), "\n];");
        let mut items = Vec::new();
        for label in quoted(block) {
            if matches!(label, "-" | "Open Recent" | "Language") {
                continue;
            }
            match children(label) {
                Some(list) => items.extend(list.into_iter().map(works)),
                None => items.push(works(label)),
            }
        }
        groups.push(Group {
            name: name.to_string(),
            items: items
                .into_iter()
                .map(|on| if on { State::Done } else { State::Todo })
                .collect(),
        });
    }
    groups
}

/// A phase's short name: `### Phase 2: MIDI & Sequencer (Weeks 11-18)` is `2 MIDI & Sequencer`.
fn phase_name(heading: &str) -> String {
    if let Some(name) = heading.strip_prefix("### Platform: ") {
        return name.to_string();
    }
    let text = heading.trim_start_matches("### Phase ");
    let (number, title) = text.split_once(": ").unwrap_or(("", text));
    let title = title.split(" (").next().unwrap_or(title);
    let title = title.split(" - ").next().unwrap_or(title);
    let title = title.split(" + ").next().unwrap_or(title);
    format!("{number} {title}")
}

/// The checkboxes of each phase and platform of `ROADMAP.md`: `[x]` done, `[~]` started, `[ ]` to do.
fn roadmap_groups(roadmap: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut inside = false;
    for line in roadmap.lines() {
        if line.starts_with("### Phase ") || line.starts_with("### Platform: ") {
            inside = true;
            groups.push(Group {
                name: phase_name(line),
                items: Vec::new(),
            });
        } else if line.starts_with("## ") {
            inside = false;
        } else if let (true, Some(group)) = (inside, groups.last_mut()) {
            let state = match line.get(..6) {
                Some("- [x] ") => State::Done,
                Some("- [~] ") => State::Partial,
                Some("- [ ] ") => State::Todo,
                _ => continue,
            };
            group.items.push(state);
        }
    }
    groups
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Shares `area` out among `sizes` (index, size), each piece as large as its size: splits the
/// list in two of about equal weight, across the longer side, and goes on with each half.
fn split(area: Rect, sizes: &[(usize, usize)], out: &mut Vec<(usize, Rect)>) {
    let [first, ..] = sizes else { return };
    if sizes.len() == 1 {
        out.push((first.0, area));
        return;
    }
    let all: usize = sizes.iter().map(|s| s.1).sum();
    let mut left = 0;
    let mut cut = 1;
    for (i, s) in sizes[..sizes.len() - 1].iter().enumerate() {
        left += s.1;
        cut = i + 1;
        if left * 2 >= all {
            break;
        }
    }
    let share = left as f32 / all.max(1) as f32;
    let (a, b) = if area.w >= area.h {
        let w = area.w * share;
        (
            Rect { w, ..area },
            Rect {
                x: area.x + w,
                w: area.w - w,
                ..area
            },
        )
    } else {
        let h = area.h * share;
        (
            Rect { h, ..area },
            Rect {
                y: area.y + h,
                h: area.h - h,
                ..area
            },
        )
    };
    split(a, &sizes[..cut], out);
    split(b, &sizes[cut..], out);
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn text(svg: &mut String, x: f32, y: f32, size: u32, weight: u32, fill: &str, body: &str) {
    let _ = writeln!(
        svg,
        r#"<text x="{x:.1}" y="{y:.1}" font-size="{size}" font-weight="{weight}" fill="{fill}">{}</text>"#,
        escape(body)
    );
}

/// `name`, cut to fit `width` points of the group label's font.
fn fitted(name: &str, width: f32) -> String {
    let room = (width / 6.8).max(1.0) as usize;
    if name.chars().count() <= room {
        return name.to_string();
    }
    let cut: String = name.chars().take(room.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// One group's cells, done first, in a grid that fills `area`.
fn draw_group(svg: &mut String, area: Rect, group: &Group) {
    let mut items = group.items.clone();
    items.sort_by_key(|s| match s {
        State::Done => 0,
        State::Partial => 1,
        State::Todo => 2,
    });
    let n = items.len();
    let cols = ((n as f32 * area.w / area.h).sqrt().ceil() as usize).clamp(1, n.max(1));
    let rows = n.div_ceil(cols);
    let h = area.h / rows as f32;
    for row in 0..rows {
        let cells = &items[row * cols..((row + 1) * cols).min(n)];
        let w = area.w / cells.len() as f32;
        for (c, state) in cells.iter().enumerate() {
            let fill = match state {
                State::Done => DONE,
                State::Partial => PARTIAL,
                State::Todo => TODO,
            };
            let _ = writeln!(
                svg,
                r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{fill}" stroke="#1f7a33" stroke-opacity=".55" stroke-width=".6"/>"##,
                area.x + c as f32 * w,
                area.y + row as f32 * h,
                w,
                h
            );
        }
    }
    let _ = writeln!(
        svg,
        r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="none" stroke="#10141a" stroke-width="2"/>"##,
        area.x, area.y, area.w, area.h
    );
    let _ = writeln!(
        svg,
        r##"<text x="{:.1}" y="{:.1}" font-size="12" font-weight="600" fill="#fff" stroke="#000" stroke-width="2.5" paint-order="stroke">{}</text>"##,
        area.x + 5.0,
        area.y + 14.0,
        escape(&fitted(&group.name, area.w - 10.0))
    );
}

fn draw_chart(svg: &mut String, area: Rect, title: &str, groups: &[Group]) {
    let done = counted(groups, State::Done);
    let line = format!(
        "{title}: {:.1}% ({done}/{})",
        percent(groups),
        total(groups)
    );
    text(svg, area.x, area.y - 10.0, 14, 500, "#e6edf3", &line);
    let mut sizes: Vec<(usize, usize)> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| (i, g.items.len()))
        .filter(|s| s.1 > 0)
        .collect();
    sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
    let mut places = Vec::new();
    split(area, &sizes, &mut places);
    for (i, place) in places {
        draw_group(svg, place, &groups[i]);
    }
}

fn badge(svg: &mut String, x: f32, label: &str, value: f32) -> f32 {
    let (lw, vw) = (label.len() as f32 * 7.2 + 18.0, 62.0);
    let color = if value >= 75.0 {
        DONE
    } else if value >= 40.0 {
        PARTIAL
    } else {
        "#c0392b"
    };
    let _ = writeln!(
        svg,
        r##"<rect x="{x:.1}" y="16" width="{lw:.1}" height="24" fill="#555b63"/><rect x="{:.1}" y="16" width="{vw}" height="24" fill="{color}"/>"##,
        x + lw
    );
    text(svg, x + 9.0, 33.0, 12, 600, "#fff", label);
    text(
        svg,
        x + lw + 9.0,
        33.0,
        12,
        600,
        "#fff",
        &format!("{value:.2}%"),
    );
    x + lw + vw
}

fn svg(menus: &[Group], roadmap: &[Group]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif">"##
    );
    let _ = writeln!(
        out,
        r##"<rect width="{WIDTH}" height="{HEIGHT}" rx="10" fill="#161b22"/>"##
    );
    let x = badge(&mut out, MARGIN, "menu entries", percent(menus));
    badge(&mut out, x + 8.0, "roadmap", percent(roadmap));
    let chart_w = (WIDTH - 3.0 * MARGIN) / 2.0;
    let chart_h = HEIGHT - CHART_TOP - 34.0;
    let left = Rect {
        x: MARGIN,
        y: CHART_TOP,
        w: chart_w,
        h: chart_h,
    };
    draw_chart(&mut out, left, "Menu entries", menus);
    let right = Rect {
        x: MARGIN * 2.0 + chart_w,
        ..left
    };
    draw_chart(&mut out, right, "Roadmap items", roadmap);
    let y = HEIGHT - 12.0;
    let mut x = MARGIN;
    for (fill, name) in [(DONE, "done"), (PARTIAL, "started"), (TODO, "to do")] {
        let _ = writeln!(
            out,
            r##"<rect x="{x:.1}" y="{:.1}" width="11" height="11" fill="{fill}"/>"##,
            y - 10.0
        );
        text(&mut out, x + 16.0, y, 12, 400, "#9da7b3", name);
        x += 72.0;
    }
    out.push_str("</svg>\n");
    out
}

/// The README block: the picture and the numbers in plain text.
fn block(menus: &[Group], roadmap: &[Group]) -> String {
    let row = |name: &str, g: &[Group]| {
        format!(
            "| {name} | {} | {} | {} | {:.2}% |\n",
            counted(g, State::Done),
            counted(g, State::Partial),
            total(g),
            percent(g)
        )
    };
    format!(
        "{START}\n![Pro Wave status: menu entries and roadmap items, done and to do]({SVG})\n\n\
         | | Done | Started | Total | Done % |\n| --- | ---: | ---: | ---: | ---: |\n{}{}{END}",
        row("Menu entries", menus),
        row("Roadmap items", roadmap),
    )
}

fn replace_block(readme: &str, block: &str) -> Option<String> {
    let start = readme.find(START)?;
    let end = readme.find(END)? + END.len();
    let block = if readme.contains("\r\n") {
        block.replace('\n', "\r\n")
    } else {
        block.to_string()
    };
    Some(format!("{}{block}{}", &readme[..start], &readme[end..]))
}

/// Writes the picture and the README block. Returns what went wrong, if anything.
pub fn run(root: &Path) -> Result<(), String> {
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .map(|t| t.replace("\r\n", "\n"))
            .map_err(|e| format!("{path}: {e}"))
    };
    let menus = menu_groups(
        &read("crates/prowave-app/src/menus.rs")?,
        &read("crates/prowave-app/src/file_commands.rs")?,
    );
    let roadmap = roadmap_groups(&read("ROADMAP.md")?);
    if total(&menus) == 0 || total(&roadmap) == 0 {
        return Err("no menu entries or roadmap items found".into());
    }
    std::fs::write(root.join(SVG), svg(&menus, &roadmap)).map_err(|e| format!("{SVG}: {e}"))?;
    let readme_path = root.join("README.md");
    let readme = std::fs::read_to_string(&readme_path).map_err(|e| format!("README.md: {e}"))?;
    let updated = replace_block(&readme, &block(&menus, &roadmap))
        .ok_or("README.md has no status markers")?;
    std::fs::write(&readme_path, updated).map_err(|e| format!("README.md: {e}"))?;
    for (name, g) in [("menu entries", &menus), ("roadmap items", &roadmap)] {
        println!(
            "{name}: {} done, {} started, {} total ({:.2}%)",
            counted(g, State::Done),
            counted(g, State::Partial),
            total(g),
            percent(g)
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MENUS: &str = r#"
const FILE: &[&str] = &[
    "New…",
    "Print",
    "-",
    "Open Recent",
    "Import",
];
const HELP: &[&str] = &[
    "Language",
    "About",
];
const SUBMENUS: &[(&str, &[&str])] = &[
    ("Import", &["MIDI File…", "Audio File…"]),
    ("Language", LANGUAGE),
];

/// What is in the submenu
pub(crate) const MENUS: [(&str, &[&str]); 2] = [
    ("File", FILE),
    ("Help", HELP),
];
pub(crate) fn enabled(label: &str) -> bool {
    match label {
        "MIDI File…" => true,
        _ => false,
    }
}
/// Start of every region
"#;
    const FILES: &str = r#"
pub fn from_menu(label: &str) { "New…" => Some(New), }
pub fn pressed()
"#;

    #[test]
    fn menus_count_what_is_enabled_and_open_their_submenus() {
        let groups = menu_groups(MENUS, FILES);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "File");
        assert_eq!(
            groups[0].items,
            [State::Done, State::Todo, State::Done, State::Todo],
            "New, Print, MIDI File, Audio File"
        );
        assert_eq!(
            groups[1].items,
            [State::Todo],
            "the language list is left out"
        );
    }

    #[test]
    fn the_roadmap_counts_the_checkboxes_of_the_phases_only() {
        let text = "## 1. Inventory\n- [ ] not a phase\n\
                    ### Phase 0: Foundation (Weeks 1-4)\n- [x] a\n  - [ ] detail\n- [~] b\n\
                    ### Phase 5: Instruments - Part 1 (Weeks 31-40)\n- [ ] c\n\
                    ## 4. Future\n- [ ] later\n";
        let groups = roadmap_groups(text);
        assert_eq!(groups[0].name, "0 Foundation");
        assert_eq!(groups[0].items, [State::Done, State::Partial]);
        assert_eq!(groups[1].name, "5 Instruments");
        assert_eq!(groups[1].items, [State::Todo]);
        assert_eq!(total(&groups), 3);
    }

    #[test]
    fn each_platform_is_a_group_of_the_roadmap() {
        let text = "### Phase 9: Polish (Weeks 1-2)\n- [x] a\n\
                    ### Platform: Linux\n- [x] b\n- [ ] c\n\
                    ### Platform: macOS\n- [~] d\n\
                    ## 3b. Controls\n- [ ] not counted\n";
        let groups = roadmap_groups(text);
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["9 Polish", "Linux", "macOS"]);
        assert_eq!(groups[1].items, [State::Done, State::Todo]);
        assert_eq!(groups[2].items, [State::Partial]);
    }

    #[test]
    fn the_pieces_fill_the_area_in_proportion() {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: 300.0,
            h: 100.0,
        };
        let mut out = Vec::new();
        split(area, &[(0, 6), (1, 3), (2, 3)], &mut out);
        assert_eq!(out.len(), 3);
        let size = |r: &Rect| r.w * r.h;
        let sum: f32 = out.iter().map(|(_, r)| size(r)).sum();
        assert!((sum - 30_000.0).abs() < 1.0);
        assert!((size(&out[0].1) - 15_000.0).abs() < 1.0);
    }

    #[test]
    fn the_readme_block_is_replaced_and_the_rest_kept() {
        let readme = format!("top\n{START}\nold\n{END}\nbottom\n");
        let done = replace_block(&readme, "new").unwrap();
        assert_eq!(done, "top\nnew\nbottom\n");
        assert_eq!(replace_block("no markers", "new"), None);
    }

    #[test]
    fn names_are_escaped_in_the_picture() {
        let groups = [Group {
            name: "MIDI & Sequencer".into(),
            items: vec![State::Done, State::Todo],
        }];
        let picture = svg(&groups, &groups);
        assert!(picture.contains("MIDI &amp; Sequencer"));
        assert!(!picture.contains("MIDI & Sequencer"));
    }

    #[test]
    fn long_names_are_cut_to_fit() {
        assert_eq!(fitted("Mix", 100.0), "Mix");
        assert_eq!(fitted("4 Mixer & Channel Strip", 60.0), "4 Mixer…");
    }

    #[test]
    fn nothing_to_count_is_not_a_division_by_zero() {
        assert_eq!(percent(&[]), 0.0);
    }
}
