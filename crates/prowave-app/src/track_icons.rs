//! Track icons: small shaded instrument illustrations in the style of Logic Pro's track icons
//! (drum kit, Drummer tile, electric guitar and bass, synth on an X stand, wooden vintage keys,
//! black piano, condenser microphone, audio waveform, violin, default note tile).
//!
//! Original vector drawings (no Apple artwork): every icon is designed on a 36Ã—36 box and
//! scaled to the target rect; gradients come from per-vertex mesh colours.
use crate::model::{Track, TrackKind};
use egui::{epaint::Mesh, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    Note,
    Waveform,
    DrumKit,
    DrummerTile,
    Piano,
    WoodKeys,
    Synth,
    Bass,
    Guitar,
    Mic,
    Strings,
    /// Auxiliary channel strip: lines running into a bus.
    Aux,
    /// Folder (track stack).
    Folder,
    /// VCA: three faders on a purple tile.
    Vca,
    /// External MIDI: a five-pin MIDI socket on an orange tile.
    ExternalMidi,
}

impl IconKind {
    /// Every icon, in the order the Assign Track Icon window shows them.
    pub const ALL: [IconKind; 15] = [
        IconKind::Note,
        IconKind::Waveform,
        IconKind::DrumKit,
        IconKind::DrummerTile,
        IconKind::Piano,
        IconKind::WoodKeys,
        IconKind::Synth,
        IconKind::Bass,
        IconKind::Guitar,
        IconKind::Mic,
        IconKind::Strings,
        IconKind::Aux,
        IconKind::Folder,
        IconKind::Vca,
        IconKind::ExternalMidi,
    ];

    /// The name the project file stores this icon under.
    pub fn name(self) -> &'static str {
        match self {
            IconKind::Note => "note",
            IconKind::Waveform => "waveform",
            IconKind::DrumKit => "drum_kit",
            IconKind::DrummerTile => "drummer",
            IconKind::Piano => "piano",
            IconKind::WoodKeys => "wood_keys",
            IconKind::Synth => "synth",
            IconKind::Bass => "bass",
            IconKind::Guitar => "guitar",
            IconKind::Mic => "mic",
            IconKind::Strings => "strings",
            IconKind::Aux => "aux",
            IconKind::Folder => "folder",
            IconKind::Vca => "vca",
            IconKind::ExternalMidi => "external_midi",
        }
    }

    /// The icon a project file's name stands for (`None` for an unknown name).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// Icon drawn for a track (header, Library, inspector). As in Logic, a software instrument
/// track shows the generic green note tile until a Library patch is loaded; other tracks use
/// [`icon_kind`]. (The sound is still chosen from the name, see `patches::synth_patch`.)
pub fn track_icon(t: &Track) -> IconKind {
    // An icon the user picked (Track > Assign Track Icon) wins over the automatic one.
    if let Some(icon) = t.icon {
        icon
    } else if t.kind == TrackKind::Aux {
        IconKind::Aux
    } else if t.kind == TrackKind::Folder {
        IconKind::Folder
    } else if t.kind == TrackKind::Vca {
        IconKind::Vca
    } else if t.kind == TrackKind::ExternalMidi {
        IconKind::ExternalMidi
    } else if t.kind == TrackKind::Instrument && t.category.is_none() {
        IconKind::Note
    } else {
        icon_kind(t)
    }
}

/// Icon suggested by the track's patch category and name.
pub fn icon_kind(t: &Track) -> IconKind {
    let n = format!("{} {}", t.category.unwrap_or(""), t.name).to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| n.contains(x));
    if has(&["vocal", "vox", "voice", "mic", "choir"]) && !has(&["tape choir"]) {
        IconKind::Mic
    } else if t.kind == TrackKind::Drummer {
        IconKind::DrummerTile
    } else if has(&[
        "drum", "kick", "snare", "hat", "perc", " kit", "808", "909", "beat",
    ]) {
        IconKind::DrumKit
    } else if has(&["bass"]) {
        IconKind::Bass
    } else if has(&[
        "guitar",
        "acoustic",
        "strat",
        "nylon",
        "steel string",
        "twelve string",
    ]) {
        IconKind::Guitar
    } else if has(&["string", "violin", "cello", "orchestral", "pizzicato"]) {
        IconKind::Strings
    } else if has(&[
        "organ",
        "b3",
        "clav",
        "electric piano",
        "e-piano",
        "tine",
        "reed",
        "suitcase",
        "wurli",
        "mellotron",
        "tape ",
    ]) {
        IconKind::WoodKeys
    } else if has(&["piano", "grand", "steinway", "upright"]) {
        IconKind::Piano
    } else if has(&[
        "pad",
        "synth",
        "lead",
        "keys",
        "keyboard",
        "arpegg",
        "arp",
        "cinematic",
        "legacy",
        "saw",
        "pulse",
        "pluck",
    ]) {
        IconKind::Synth
    } else if t.kind == TrackKind::Audio {
        IconKind::Waveform
    } else {
        IconKind::Note
    }
}

/// Drawing context: design coordinates are a 36Ã—36 box centred on `c`, scaled by `s`.
struct Pen<'a> {
    g: &'a Painter,
    c: Pos2,
    s: f32,
}

/// Width (points) of the hairline drawn around gradient meshes: egui does not anti-alias mesh
/// edges, so without it shapes look jagged; the feathered stroke smooths them.
const EDGE_AA: f32 = 0.75;

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.c + vec2(x, y) * self.s
    }

    /// Convex or star-shaped polygon with a vertical gradient.
    fn poly(&self, pts: &[(f32, f32)], top: Color32, bottom: Color32) {
        if pts.len() < 3 {
            return;
        }
        let (ymin, ymax) = pts
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let col = |y: f32| mix(top, bottom, (y - ymin) / (ymax - ymin).max(1e-3));
        let (cx, cy) = pts.iter().fold((0.0, 0.0), |(a, b), p| (a + p.0, b + p.1));
        let (cx, cy) = (cx / pts.len() as f32, cy / pts.len() as f32);
        let mut m = Mesh::default();
        m.colored_vertex(self.p(cx, cy), col(cy));
        for (x, y) in pts {
            m.colored_vertex(self.p(*x, *y), col(*y));
        }
        let n = pts.len() as u32;
        for i in 0..n {
            m.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        self.g.add(Shape::mesh(m));
        let outline: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        self.g.add(Shape::closed_line(
            outline,
            Stroke::new(EDGE_AA, mix(top, bottom, 0.5)),
        ));
    }

    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, top: Color32, bottom: Color32) {
        self.poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], top, bottom);
    }

    /// Horizontal gradient (left â†’ right), for metal and cylinders.
    #[allow(clippy::too_many_arguments)]
    fn rect_h(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        left: Color32,
        mid: Color32,
        right: Color32,
    ) {
        let mut m = Mesh::default();
        let xm = (x0 + x1) * 0.4 + x0 * 0.2;
        for (x, c) in [(x0, left), (xm, mid), (x1, right)] {
            m.colored_vertex(self.p(x, y0), c);
            m.colored_vertex(self.p(x, y1), c);
        }
        for i in 0..2u32 {
            let a = i * 2;
            m.add_triangle(a, a + 1, a + 2);
            m.add_triangle(a + 1, a + 3, a + 2);
        }
        self.g.add(Shape::mesh(m));
    }

    /// Ellipse with a radial gradient whose bright spot sits up and to the left.
    fn ellipse(&self, cx: f32, cy: f32, rx: f32, ry: f32, inner: Color32, outer: Color32) {
        let mut m = Mesh::default();
        m.colored_vertex(self.p(cx - rx * 0.25, cy - ry * 0.3), inner);
        let n = 28u32;
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            m.colored_vertex(self.p(cx + a.cos() * rx, cy + a.sin() * ry), outer);
        }
        for i in 0..n {
            m.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        self.g.add(Shape::mesh(m));
        let outline: Vec<Pos2> = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                self.p(cx + a.cos() * rx, cy + a.sin() * ry)
            })
            .collect();
        self.g
            .add(Shape::closed_line(outline, Stroke::new(EDGE_AA, outer)));
    }

    fn ellipse_stroke(&self, cx: f32, cy: f32, rx: f32, ry: f32, w: f32, col: Color32) {
        let pts: Vec<Pos2> = (0..=28)
            .map(|i| {
                let a = i as f32 / 28.0 * std::f32::consts::TAU;
                self.p(cx + a.cos() * rx, cy + a.sin() * ry)
            })
            .collect();
        self.g.add(Shape::line(pts, Stroke::new(w * self.s, col)));
    }

    fn line(&self, pts: &[(f32, f32)], w: f32, col: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        self.g.add(Shape::line(pts, Stroke::new(w * self.s, col)));
    }
}

/// Rotates design points around `pivot` by `deg` (clockwise on screen) and scales by `k`.
fn rot(pts: &[(f32, f32)], pivot: (f32, f32), deg: f32, k: f32) -> Vec<(f32, f32)> {
    let (s, c) = deg.to_radians().sin_cos();
    pts.iter()
        .map(|(x, y)| (pivot.0 + (x * c - y * s) * k, pivot.1 + (x * s + y * c) * k))
        .collect()
}

/// Draws a track icon scaled to `r`.
pub fn draw_icon(g: &Painter, r: Rect, kind: IconKind) {
    let pen = Pen {
        g,
        c: r.center(),
        s: (r.width().min(r.height()) / 36.0).max(0.2),
    };
    match kind {
        IconKind::Note => note_tile(&pen),
        IconKind::Waveform => waveform(&pen),
        IconKind::DrumKit => drum_kit(&pen),
        IconKind::DrummerTile => drummer_tile(&pen),
        IconKind::Piano => piano(&pen),
        IconKind::WoodKeys => wood_keys(&pen),
        IconKind::Synth => synth(&pen),
        IconKind::Bass => guitar(&pen, true),
        IconKind::Guitar => guitar(&pen, false),
        IconKind::Mic => mic(&pen),
        IconKind::Strings => violin(&pen),
        IconKind::Aux => aux(&pen),
        IconKind::Folder => folder(&pen),
        IconKind::Vca => vca(&pen),
        IconKind::ExternalMidi => external_midi(&pen),
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Logic's generic software-instrument icon: a flat dark green tile (rgb 44,60,47, a little
/// wider than tall) with a flat green (rgb 102,213,109) pair of beamed eighth notes. The
/// geometry was fitted to the pixels of a Logic Pro track icon (units: tile height = 26).
fn note_tile(p: &Pen) {
    let tile = Rect::from_center_size(p.c, vec2(34.0, 26.0) * p.s);
    p.g.rect_filled(tile, 1.6 * p.s, rgb(44, 60, 47));
    let ink = rgb(102, 213, 109);
    // Flat anti-aliased shapes (egui feathers convex polygons).
    let fill = |pts: &[(f32, f32)]| {
        let pts: Vec<Pos2> = pts.iter().map(|(x, y)| p.p(*x, *y)).collect();
        p.g.add(Shape::convex_polygon(pts, ink, Stroke::NONE));
    };
    // Two identical oval heads tilted 29.1°.
    for (hx, hy) in [(-6.082f32, 8.131f32), (3.958, 5.954)] {
        let oval: Vec<(f32, f32)> = (0..48)
            .map(|i| {
                let a = i as f32 / 48.0 * std::f32::consts::TAU;
                (a.cos() * 2.938, a.sin() * 2.44)
            })
            .collect();
        let head: Vec<(f32, f32)> = rot(&oval, (0.0, 0.0), -29.1, 1.0)
            .iter()
            .map(|(x, y)| (x + hx, y + hy))
            .collect();
        fill(&head);
    }
    // Stems on the heads' right edges, then the thick beam rising to the right.
    fill(&[
        (-4.532, -7.765),
        (-3.279, -7.765),
        (-3.279, 8.131),
        (-4.532, 8.131),
    ]);
    fill(&[
        (5.508, -10.33),
        (6.761, -10.33),
        (6.761, 5.954),
        (5.508, 5.954),
    ]);
    // The beam's top corners are slightly rounded, as in Logic.
    let (tl, tr) = ((-4.532f32, -8.229f32), (6.761f32, -10.795f32));
    let mut beam = Vec::new();
    for (corner, from, to) in [(tl, (-4.532, -3.256), tr), (tr, tl, (6.761, -5.56))] {
        let r = 1.0;
        let towards = |q: (f32, f32)| {
            let (dx, dy) = (q.0 - corner.0, q.1 - corner.1);
            let len = (dx * dx + dy * dy).sqrt();
            (corner.0 + dx / len * r, corner.1 + dy / len * r)
        };
        let (a, b) = (towards(from), towards(to));
        // Quadratic curve from `a` to `b` with the sharp corner as control point.
        for i in 0..=6 {
            let t = i as f32 / 6.0;
            let u = 1.0 - t;
            beam.push((
                u * u * a.0 + 2.0 * u * t * corner.0 + t * t * b.0,
                u * u * a.1 + 2.0 * u * t * corner.1 + t * t * b.1,
            ));
        }
    }
    beam.push((6.761, -5.56));
    beam.push((-4.532, -3.256));
    fill(&beam);
}

/// Aux: a dark teal tile with three bus lines that gather into one arrow.
fn aux(p: &Pen) {
    p.rect(
        -16.0,
        -12.0,
        16.0,
        12.0,
        rgb(0x2a, 0x55, 0x58),
        rgb(0x1a, 0x37, 0x3a),
    );
    let teal = rgb(0x4f, 0xd1, 0xd1);
    for y in [-6.0, 0.0, 6.0] {
        p.line(&[(-11.0, y), (3.0, y)], 1.6, teal);
    }
    p.line(&[(3.0, -6.0), (8.0, 0.0), (3.0, 6.0)], 1.6, teal);
    p.line(&[(8.0, 0.0), (13.0, 0.0)], 1.6, teal);
}

/// VCA: a dark purple tile with three faders at different heights.
fn vca(p: &Pen) {
    p.rect(
        -16.0,
        -12.0,
        16.0,
        12.0,
        rgb(0x4a, 0x38, 0x66),
        rgb(0x32, 0x24, 0x48),
    );
    let ink = rgb(0xc2, 0x9a, 0xf0);
    for (x, knob) in [(-8.0, -3.0), (0.0, 4.0), (8.0, -6.0)] {
        p.line(&[(x, -9.0), (x, 9.0)], 1.2, rgb(0x7a, 0x62, 0xa0));
        p.rect(
            x - 3.5,
            knob - 2.0,
            x + 3.5,
            knob + 2.0,
            ink,
            rgb(0x9c, 0x78, 0xd0),
        );
    }
}

/// External MIDI: a dark orange tile with a five-pin DIN socket.
fn external_midi(p: &Pen) {
    p.rect(
        -16.0,
        -12.0,
        16.0,
        12.0,
        rgb(0x66, 0x40, 0x22),
        rgb(0x48, 0x2c, 0x14),
    );
    let ink = rgb(0xf0, 0xa8, 0x5c);
    let ring: Vec<(f32, f32)> = (0..=24)
        .map(|k| {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            (9.0 * a.cos(), 9.0 * a.sin())
        })
        .collect();
    p.line(&ring, 1.4, ink);
    // Five pins on the upper half of the socket.
    for k in 0..5 {
        let a = std::f32::consts::PI * (1.0 + k as f32 / 4.0);
        let (x, y) = (5.2 * a.cos(), 5.2 * a.sin() + 1.5);
        p.rect(x - 0.9, y - 0.9, x + 0.9, y + 0.9, ink, ink);
    }
    p.line(&[(-3.0, 7.0), (3.0, 7.0)], 1.4, ink);
}

/// Folder: a grey-blue folder with a tab.
fn folder(p: &Pen) {
    p.poly(
        &[
            (-15.0, -5.0),
            (-15.0, -11.0),
            (-5.0, -11.0),
            (-2.0, -7.0),
            (15.0, -7.0),
            (15.0, -5.0),
        ],
        rgb(0xa9, 0xae, 0xbd),
        rgb(0x8a, 0x8f, 0x9f),
    );
    p.rect(
        -15.0,
        -6.0,
        15.0,
        11.0,
        rgb(0xc4, 0xc8, 0xd4),
        rgb(0x7f, 0x84, 0x94),
    );
}

fn waveform(p: &Pen) {
    let blue = rgb(0x3a, 0x80, 0xf2);
    let heights = [
        2.0f32, 4.0, 3.0, 8.0, 5.0, 13.0, 7.0, 16.0, 10.0, 6.0, 12.0, 4.0, 9.0, 3.0, 5.0, 2.0,
    ];
    for (i, h) in heights.iter().enumerate() {
        let x = -15.0 + i as f32 * 2.0;
        p.line(
            &[(x, -h), (x, *h)],
            1.4,
            mix(blue, rgb(0x8c, 0xbc, 0xff), (1.0 - h / 16.0) * 0.6),
        );
    }
    p.line(&[(-17.0, 0.0), (17.0, 0.0)], 0.8, blue);
}

fn drummer_tile(p: &Pen) {
    let yellow = rgb(0xec, 0xb8, 0x24);
    let r = Rect::from_center_size(p.c, vec2(28.0, 28.0) * p.s);
    p.g.rect_filled(r, 5.0 * p.s, rgb(0x2a, 0x26, 0x1c));
    p.g.rect_stroke(
        r,
        5.0 * p.s,
        Stroke::new(2.2 * p.s, yellow),
        egui::StrokeKind::Middle,
    );
    // Drummer's beat lane: hits of different heights on a centre line.
    p.line(&[(-10.0, 0.0), (10.0, 0.0)], 1.0, yellow);
    for (x, h) in [
        (-8.0f32, 3.0f32),
        (-5.0, 6.5),
        (-2.5, 2.5),
        (0.5, 8.0),
        (3.0, 3.0),
        (5.5, 6.0),
        (8.0, 2.5),
    ] {
        p.line(&[(x, -h), (x, h)], 1.6, yellow);
    }
}

fn drum_kit(p: &Pen) {
    let chrome = rgb(0x9a, 0x9a, 0x9e);
    // Cymbal stands, then cymbals (gold, seen slightly from above).
    p.line(&[(-12.0, -9.0), (-12.0, 9.0), (-15.0, 13.0)], 0.9, chrome);
    p.line(&[(12.5, -11.0), (12.5, 8.0), (15.5, 13.0)], 0.9, chrome);
    p.ellipse(
        -12.0,
        -9.5,
        7.5,
        1.9,
        rgb(0xff, 0xe2, 0x8a),
        rgb(0xb8, 0x8a, 0x26),
    );
    p.ellipse(
        12.5,
        -11.5,
        6.5,
        1.7,
        rgb(0xff, 0xe2, 0x8a),
        rgb(0xb8, 0x8a, 0x26),
    );
    // Rack toms.
    for x in [-4.5f32, 4.5] {
        p.rect_h(
            x - 4.0,
            -6.5,
            x + 4.0,
            -1.5,
            rgb(0x8a, 0x8a, 0x8e),
            rgb(0xf4, 0xf4, 0xf4),
            rgb(0x9e, 0x9e, 0xa2),
        );
        p.ellipse(x, -6.5, 4.0, 1.4, Color32::WHITE, rgb(0xcf, 0xcf, 0xd2));
        p.ellipse_stroke(x, -6.5, 4.0, 1.4, 0.6, rgb(0x55, 0x55, 0x58));
    }
    // Bass drum (front head facing us).
    p.ellipse(0.0, 5.5, 9.0, 8.5, Color32::WHITE, rgb(0xc4, 0xc4, 0xc8));
    p.ellipse_stroke(0.0, 5.5, 9.0, 8.5, 1.8, rgb(0x2c, 0x2c, 0x30));
    p.ellipse(
        0.0,
        5.5,
        3.2,
        3.0,
        rgb(0x55, 0x55, 0x5a),
        rgb(0x22, 0x22, 0x26),
    );
    // Snare (left) and floor tom (right), chrome shells.
    p.rect_h(
        -17.0,
        6.0,
        -9.5,
        10.5,
        rgb(0x7c, 0x7c, 0x80),
        rgb(0xf0, 0xf0, 0xf0),
        rgb(0x8c, 0x8c, 0x90),
    );
    p.ellipse(
        -13.25,
        6.0,
        3.75,
        1.3,
        Color32::WHITE,
        rgb(0xd4, 0xd4, 0xd8),
    );
    p.rect_h(
        9.5,
        3.5,
        17.0,
        11.5,
        rgb(0x7c, 0x7c, 0x80),
        rgb(0xf0, 0xf0, 0xf0),
        rgb(0x8c, 0x8c, 0x90),
    );
    p.ellipse(13.25, 3.5, 3.75, 1.3, Color32::WHITE, rgb(0xd4, 0xd4, 0xd8));
    p.line(&[(-15.5, 10.5), (-16.5, 14.0)], 0.9, chrome);
    p.line(&[(-11.0, 10.5), (-10.0, 14.0)], 0.9, chrome);
}

fn piano(p: &Pen) {
    // Black digital/upright piano seen slightly from the left.
    p.rect(
        -15.0,
        -10.0,
        15.0,
        3.0,
        rgb(0x55, 0x55, 0x58),
        rgb(0x16, 0x16, 0x18),
    );
    p.rect(
        -15.0,
        -11.5,
        15.0,
        -10.0,
        rgb(0x7a, 0x7a, 0x7e),
        rgb(0x44, 0x44, 0x48),
    );
    // Keyboard.
    p.rect(-14.0, 0.0, 14.0, 4.5, Color32::WHITE, rgb(0xd8, 0xd8, 0xd8));
    for i in 0..15 {
        let x = -14.0 + i as f32 * 1.87;
        p.line(&[(x, 0.0), (x, 4.5)], 0.4, rgb(0x90, 0x90, 0x90));
    }
    for i in [1, 2, 4, 5, 6, 8, 9, 11, 12, 13] {
        let x = -14.0 + i as f32 * 1.87 - 0.55;
        p.rect(
            x,
            0.0,
            x + 1.1,
            2.7,
            rgb(0x30, 0x30, 0x30),
            rgb(0x05, 0x05, 0x05),
        );
    }
    p.rect(
        -15.0,
        4.5,
        15.0,
        6.5,
        rgb(0x2c, 0x2c, 0x2e),
        rgb(0x10, 0x10, 0x12),
    );
    // Legs and pedals.
    p.rect(
        -14.5,
        6.5,
        -12.5,
        14.0,
        rgb(0x3a, 0x3a, 0x3c),
        rgb(0x12, 0x12, 0x14),
    );
    p.rect(
        12.5,
        6.5,
        14.5,
        14.0,
        rgb(0x3a, 0x3a, 0x3c),
        rgb(0x12, 0x12, 0x14),
    );
    p.line(&[(-1.5, 13.5), (1.5, 13.5)], 1.0, rgb(0xc8, 0xa8, 0x50));
}

fn wood_keys(p: &Pen) {
    // Vintage keys in a wooden cabinet (organ / electric piano).
    let (wood_hi, wood_lo) = (rgb(0xc4, 0x86, 0x4a), rgb(0x6a, 0x3c, 0x1c));
    p.poly(
        &[(-15.0, -9.0), (15.0, -9.0), (16.0, -6.0), (-16.0, -6.0)],
        rgb(0xd8, 0x9c, 0x5c),
        rgb(0xa4, 0x6a, 0x34),
    );
    p.rect(-16.0, -6.0, 16.0, 8.0, wood_hi, wood_lo);
    // Grain.
    for y in [-3.0f32, 4.5] {
        p.line(&[(-14.0, y), (14.0, y + 0.4)], 0.4, rgb(0x8a, 0x52, 0x26));
    }
    p.rect(
        -13.5,
        -2.0,
        13.5,
        2.5,
        Color32::WHITE,
        rgb(0xdc, 0xdc, 0xd8),
    );
    for i in 0..14 {
        let x = -13.5 + i as f32 * 1.93;
        p.line(&[(x, -2.0), (x, 2.5)], 0.4, rgb(0x96, 0x96, 0x90));
    }
    for i in [1, 2, 4, 5, 6, 8, 9, 11, 12, 13] {
        let x = -13.5 + i as f32 * 1.93 - 0.55;
        p.rect(
            x,
            -2.0,
            x + 1.1,
            0.6,
            rgb(0x2a, 0x2a, 0x2a),
            rgb(0x08, 0x08, 0x08),
        );
    }
    p.rect(-14.5, 8.0, -12.0, 14.0, wood_lo, rgb(0x3e, 0x22, 0x10));
    p.rect(12.0, 8.0, 14.5, 14.0, wood_lo, rgb(0x3e, 0x22, 0x10));
}

fn synth(p: &Pen) {
    // Chrome X stand.
    let chrome_a = rgb(0xe8, 0xe8, 0xec);
    p.line(&[(-10.0, 2.0), (10.0, 14.0)], 1.6, chrome_a);
    p.line(&[(10.0, 2.0), (-10.0, 14.0)], 1.6, rgb(0xb4, 0xb4, 0xb8));
    p.line(&[(-12.0, 14.0), (-8.0, 14.0)], 1.4, rgb(0x60, 0x60, 0x64));
    p.line(&[(8.0, 14.0), (12.0, 14.0)], 1.4, rgb(0x60, 0x60, 0x64));
    // Red body in slight perspective.
    p.poly(
        &[(-15.0, -7.0), (15.0, -7.0), (17.0, 2.5), (-17.0, 2.5)],
        rgb(0xf0, 0x55, 0x48),
        rgb(0x9a, 0x1e, 0x18),
    );
    p.rect(
        -14.0,
        -6.0,
        14.0,
        -4.2,
        rgb(0x3a, 0x12, 0x10),
        rgb(0x22, 0x0a, 0x08),
    );
    for i in 0..6 {
        p.ellipse(
            -12.0 + i as f32 * 2.2,
            -5.1,
            0.6,
            0.6,
            Color32::WHITE,
            rgb(0xb0, 0xb0, 0xb0),
        );
    }
    // Keybed.
    p.poly(
        &[(-14.5, -3.0), (14.5, -3.0), (15.6, 1.8), (-15.6, 1.8)],
        Color32::WHITE,
        rgb(0xd6, 0xd6, 0xd6),
    );
    for i in 0..16 {
        let t = i as f32 / 16.0;
        let (xt, xb) = (-14.5 + 29.0 * t, -15.6 + 31.2 * t);
        p.line(&[(xt, -3.0), (xb, 1.8)], 0.35, rgb(0x90, 0x90, 0x90));
    }
    for i in [1, 2, 4, 5, 6, 8, 9, 11, 12, 13, 15] {
        let t = i as f32 / 16.0;
        let x = -14.5 + 29.0 * t;
        p.poly(
            &[
                (x - 0.55, -3.0),
                (x + 0.55, -3.0),
                (x + 0.6, -0.4),
                (x - 0.6, -0.4),
            ],
            rgb(0x2c, 0x2c, 0x2c),
            rgb(0x06, 0x06, 0x06),
        );
    }
}

fn guitar(p: &Pen, bass: bool) {
    let pivot = (-5.0, 7.0);
    let k = 0.82;
    let (body_hi, body_lo) = if bass {
        (rgb(0xf0, 0xa0, 0x3c), rgb(0x7a, 0x2c, 0x0c))
    } else {
        (rgb(0xff, 0xd6, 0x4a), rgb(0xd4, 0x8a, 0x18))
    };
    let neck_len = if bass { 27.0 } else { 22.0 };
    let r = |pts: &[(f32, f32)]| rot(pts, pivot, 40.0, k);
    // Neck + headstock (behind the body).
    p.poly(
        &r(&[
            (-1.4, -3.0),
            (1.4, -3.0),
            (1.3, -neck_len),
            (-1.3, -neck_len),
        ]),
        rgb(0xf0, 0xc8, 0x82),
        rgb(0xc8, 0x94, 0x50),
    );
    for i in 1..9 {
        let y = -3.0 - i as f32 * (neck_len - 3.0) / 9.0;
        p.line(&r(&[(-1.3, y), (1.3, y)]), 0.35, rgb(0x9a, 0x9a, 0x9a));
    }
    p.poly(
        &r(&[
            (-1.6, -neck_len),
            (2.6, -neck_len - 1.0),
            (2.2, -neck_len - 6.0),
            (-0.8, -neck_len - 5.0),
        ]),
        rgb(0xf0, 0xc8, 0x82),
        rgb(0xb8, 0x84, 0x44),
    );
    // Body.
    let body = [
        (-7.0, 2.0),
        (-6.5, -3.0),
        (-5.0, -6.0),
        (-3.2, -5.2),
        (-2.0, -3.0),
        (2.0, -3.0),
        (3.5, -7.5),
        (5.2, -6.2),
        (6.2, -2.0),
        (7.2, 3.0),
        (6.2, 6.5),
        (3.0, 8.5),
        (-3.0, 8.5),
        (-6.3, 6.3),
    ];
    p.poly(&r(&body), body_hi, body_lo);
    // Pickguard, pickups, bridge.
    let guard_col = if bass {
        rgb(0x20, 0x20, 0x20)
    } else {
        rgb(0xf6, 0xf2, 0xe8)
    };
    p.poly(
        &r(&[
            (-4.8, 0.5),
            (-3.0, -3.0),
            (2.4, -3.0),
            (3.5, 1.0),
            (2.4, 5.5),
            (-3.2, 6.2),
        ]),
        guard_col,
        guard_col,
    );
    let pickups: &[f32] = if bass { &[0.5, 3.5] } else { &[-1.5, 1.0, 3.5] };
    for y in pickups {
        p.poly(
            &r(&[(-1.8, *y), (1.8, *y), (1.8, y + 0.9), (-1.8, y + 0.9)]),
            rgb(0x1a, 0x1a, 0x1a),
            rgb(0x40, 0x40, 0x40),
        );
    }
    p.poly(
        &r(&[(-1.7, 5.6), (1.7, 5.6), (1.7, 6.6), (-1.7, 6.6)]),
        rgb(0xd8, 0xd8, 0xdc),
        rgb(0x8a, 0x8a, 0x8e),
    );
    // Strings.
    for x in [-0.8f32, -0.3, 0.3, 0.8] {
        p.line(&r(&[(x, 6.0), (x, -neck_len)]), 0.25, rgb(0xe8, 0xe8, 0xe8));
    }
}

fn mic(p: &Pen) {
    // Large-diaphragm condenser on a stand.
    p.line(&[(0.0, 8.0), (0.0, 14.0)], 1.6, rgb(0x8c, 0x8c, 0x90));
    p.ellipse(
        0.0,
        15.0,
        6.0,
        1.2,
        rgb(0x9a, 0x9a, 0x9e),
        rgb(0x4a, 0x4a, 0x4e),
    );
    // Body.
    p.rect_h(
        -3.8,
        1.0,
        3.8,
        9.0,
        rgb(0x70, 0x70, 0x74),
        rgb(0xe6, 0xe6, 0xea),
        rgb(0x80, 0x80, 0x84),
    );
    // Grille capsule (rounded top).
    p.rect_h(
        -5.0,
        -10.0,
        5.0,
        1.5,
        rgb(0x8a, 0x8a, 0x8e),
        rgb(0xf6, 0xf6, 0xf8),
        rgb(0x96, 0x96, 0x9a),
    );
    p.ellipse(
        0.0,
        -10.0,
        5.0,
        4.5,
        rgb(0xff, 0xff, 0xff),
        rgb(0x9a, 0x9a, 0x9e),
    );
    for i in 0..6 {
        let y = -11.0 + i as f32 * 2.2;
        p.line(&[(-4.6, y), (4.6, y)], 0.45, rgb(0x6a, 0x6a, 0x6e));
    }
    p.rect_h(
        -5.2,
        1.0,
        5.2,
        2.6,
        rgb(0x5a, 0x5a, 0x5e),
        rgb(0xc8, 0xc8, 0xcc),
        rgb(0x60, 0x60, 0x64),
    );
}

fn violin(p: &Pen) {
    let pivot = (-2.0, 5.0);
    let r = |pts: &[(f32, f32)]| rot(pts, pivot, 25.0, 0.85);
    let (hi, lo) = (rgb(0xd8, 0x7a, 0x2c), rgb(0x6a, 0x26, 0x08));
    p.poly(
        &r(&[(-1.0, -5.0), (1.0, -5.0), (0.9, -19.0), (-0.9, -19.0)]),
        rgb(0x2a, 0x1a, 0x10),
        rgb(0x10, 0x08, 0x04),
    );
    p.poly(
        &r(&[
            (-1.2, -19.0),
            (1.2, -19.0),
            (1.6, -22.0),
            (0.0, -23.5),
            (-1.6, -22.0),
        ]),
        hi,
        lo,
    );
    // Upper bout, waist, lower bout.
    p.poly(
        &r(&[
            (-5.0, -6.0),
            (-3.0, -8.5),
            (3.0, -8.5),
            (5.0, -6.0),
            (4.5, -2.5),
            (2.5, -0.5),
            (-2.5, -0.5),
            (-4.5, -2.5),
        ]),
        hi,
        lo,
    );
    p.poly(
        &r(&[
            (-6.5, 2.5),
            (-4.5, -0.8),
            (4.5, -0.8),
            (6.5, 2.5),
            (6.0, 7.0),
            (3.0, 9.5),
            (-3.0, 9.5),
            (-6.0, 7.0),
        ]),
        hi,
        lo,
    );
    p.line(&r(&[(-2.2, 0.5), (-2.0, 4.5)]), 0.6, rgb(0x20, 0x10, 0x08));
    p.line(&r(&[(2.2, 0.5), (2.0, 4.5)]), 0.6, rgb(0x20, 0x10, 0x08));
    p.poly(
        &r(&[(-1.6, 5.5), (1.6, 5.5), (1.6, 6.3), (-1.6, 6.3)]),
        rgb(0xe0, 0xc8, 0x90),
        rgb(0xa0, 0x80, 0x50),
    );
    for x in [-0.6f32, -0.2, 0.2, 0.6] {
        p.line(&r(&[(x, 8.0), (x, -19.0)]), 0.25, rgb(0xe0, 0xe0, 0xe0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    #[test]
    fn demo_tracks_get_the_expected_icons() {
        let p = Project::demo();
        let kinds: Vec<IconKind> = p.tracks.iter().map(icon_kind).collect();
        assert_eq!(
            kinds,
            vec![
                IconKind::WoodKeys,
                IconKind::Piano,
                IconKind::Synth,
                IconKind::DrummerTile,
                IconKind::Bass,
                IconKind::Mic
            ]
        );
    }

    #[test]
    fn aux_and_folder_tracks_have_their_own_icons() {
        use crate::model::{Track, TrackKind};
        // Not the note tile, whatever the name or patch category says.
        let mut aux = Track::new("Piano Reverb", TrackKind::Aux);
        aux.category = Some("Piano");
        assert_eq!(track_icon(&aux), IconKind::Aux);
        assert_eq!(
            track_icon(&Track::new("Drums", TrackKind::Folder)),
            IconKind::Folder
        );
    }

    #[test]
    fn instrument_tracks_without_a_patch_show_the_note_tile() {
        let mut p = Project::demo();
        // No Library patch loaded: the generic tile, even with an instrument-like name.
        assert_eq!(track_icon(&p.tracks[0]), IconKind::Note);
        assert_eq!(track_icon(&p.tracks[1]), IconKind::Note);
        // Drummer and audio tracks keep their own icons.
        assert_eq!(track_icon(&p.tracks[3]), IconKind::DrummerTile);
        assert_eq!(track_icon(&p.tracks[5]), IconKind::Mic);
        // Loading a patch shows its instrument.
        p.tracks[1].category = Some("Piano");
        assert_eq!(track_icon(&p.tracks[1]), IconKind::Piano);
    }

    #[test]
    fn rotation_keeps_distances() {
        let pts = rot(&[(0.0, -10.0)], (0.0, 0.0), 90.0, 1.0);
        assert!(
            (pts[0].0 - 10.0).abs() < 1e-4 && pts[0].1.abs() < 1e-4,
            "{pts:?}"
        );
    }
}
