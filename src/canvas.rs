//! Canvas — an infinite whiteboard of cards, text, to-do lists, images, links, workspace pages and free-hand ink, tied together by
//! connectors (Obsidian Canvas / Miro-lite).
//!
//! This module is the **data and logic** half: the serialisable model, undo history, geometry (connector routing, hit testing, fitting
//! the view), auto layout, templates and Obsidian-compatible JSON import / export. Nothing in here touches egui widgets, so all of it is
//! unit tested. The drawing and gesture half lives in `canvasui.rs`.
//!
//! Open design: every struct has `#[serde(default)]` fields only, so new properties / node kinds can be added later without breaking
//! saved workspaces. [`Kind`] is the extension point for new node types (embed, table, shape …): add a variant, render it in
//! `canvasui::draw_node`, and it is selectable / movable / connectable / exportable immediately.

use std::collections::{BTreeSet, HashMap};

use egui::{pos2, vec2, Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 4.0;
/// Grid size (world units) used when snapping is on.
pub const GRID: f32 = 8.0;
const HIST_MAX: usize = 80;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Kind {
    /// Card with lightweight markdown (headings, bullets, quotes, check boxes).
    #[default]
    Note,
    /// Frameless text: titles and labels floating on the board.
    Text,
    /// Check list card with a progress bar.
    Todo,
    /// Live link to a page / database of the workspace (icon, title, excerpt).
    Page,
    /// Picture from an address or an uploaded file.
    Image,
    /// Web bookmark.
    Link,
    /// Labelled frame: moves its contents with it, doubles as a slide in presentation mode.
    Group,
    /// Free-hand pen stroke.
    Ink,
}

impl Kind {
    pub fn default_size(self) -> Vec2 {
        match self {
            Kind::Note => vec2(240.0, 120.0),
            Kind::Text => vec2(260.0, 44.0),
            Kind::Todo => vec2(250.0, 150.0),
            Kind::Page => vec2(264.0, 96.0),
            Kind::Image => vec2(300.0, 0.0),
            Kind::Link => vec2(288.0, 84.0),
            Kind::Group => vec2(480.0, 320.0),
            Kind::Ink => vec2(10.0, 10.0),
        }
    }
    pub fn min_size(self) -> Vec2 {
        match self {
            Kind::Text => vec2(60.0, 28.0),
            Kind::Group => vec2(160.0, 100.0),
            Kind::Image => vec2(60.0, 40.0),
            Kind::Ink => vec2(4.0, 4.0),
            _ => vec2(120.0, 56.0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Side {
    #[default]
    Auto,
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Arrow {
    None,
    #[default]
    End,
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Shape {
    #[default]
    Curve,
    Straight,
    Elbow,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Grid {
    #[default]
    Dots,
    Lines,
    None,
}

fn yes() -> bool {
    true
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Node {
    pub id: u64,
    #[serde(default)]
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// 0 for an image that has not been measured yet.
    pub h: f32,
    /// 0 = neutral, 1‥7 = red, orange, yellow, green, blue, purple, pink.
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub text: String,
    /// Image source (`https://…` or `media:<id>`) or link address.
    #[serde(default)]
    pub url: String,
    /// Target page of a [`Kind::Page`] node.
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub locked: bool,
    /// Text size step (0 normal, 1 large, 2 huge) / pen width step.
    #[serde(default)]
    pub size: u8,
    /// Pen stroke points relative to the node's top-left corner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pts: Vec<[f32; 2]>,
}

impl Node {
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), vec2(self.w, self.h.max(0.0)))
    }
    pub fn center(&self) -> Pos2 {
        self.rect().center()
    }
    pub fn set_rect(&mut self, r: Rect) {
        self.x = r.min.x;
        self.y = r.min.y;
        self.w = r.width();
        self.h = r.height();
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Edge {
    pub id: u64,
    pub from: u64,
    pub to: u64,
    #[serde(default)]
    pub from_side: Side,
    #[serde(default)]
    pub to_side: Side,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub dashed: bool,
    #[serde(default)]
    pub arrow: Arrow,
    #[serde(default)]
    pub shape: Shape,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        View { x: -80.0, y: -80.0, zoom: 1.0 }
    }
}

impl View {
    pub fn to_screen(&self, origin: Pos2, p: Pos2) -> Pos2 {
        pos2(origin.x + (p.x - self.x) * self.zoom, origin.y + (p.y - self.y) * self.zoom)
    }
    pub fn to_world(&self, origin: Pos2, p: Pos2) -> Pos2 {
        pos2((p.x - origin.x) / self.zoom + self.x, (p.y - origin.y) / self.zoom + self.y)
    }
    pub fn rect_to_screen(&self, origin: Pos2, r: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(origin, r.min), self.to_screen(origin, r.max))
    }
    /// Zooms by `factor` keeping the world point under screen position `at` fixed.
    pub fn zoom_at(&mut self, origin: Pos2, at: Pos2, factor: f32) {
        let before = self.to_world(origin, at);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.to_world(origin, at);
        self.x += before.x - after.x;
        self.y += before.y - after.y;
    }
    pub fn sane(mut self) -> Self {
        if !self.zoom.is_finite() || self.zoom <= 0.0 {
            self.zoom = 1.0;
        }
        self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if !self.x.is_finite() {
            self.x = 0.0;
        }
        if !self.y.is_finite() {
            self.y = 0.0;
        }
        self
    }
}

#[derive(Clone, Default)]
struct Snap {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

#[derive(Clone, Default)]
struct Hist {
    undo: Vec<Snap>,
    redo: Vec<Snap>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Canvas {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub view: View,
    #[serde(default)]
    pub grid: Grid,
    #[serde(default = "yes")]
    pub snap: bool,
    #[serde(default = "yes")]
    pub minimap: bool,
    /// Bumped on every edit (cache key for derived data).
    #[serde(skip)]
    pub rev: u64,
    #[serde(skip)]
    hist: Hist,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    CenterH,
    Right,
    Top,
    CenterV,
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tpl {
    MindMap,
    Kanban,
    Swot,
    Flow,
    Week,
}

impl Tpl {
    pub const ALL: [Tpl; 5] = [Tpl::MindMap, Tpl::Kanban, Tpl::Swot, Tpl::Flow, Tpl::Week];
    pub fn name(self) -> &'static str {
        match self {
            Tpl::MindMap => "Mind map",
            Tpl::Kanban => "Kanban board",
            Tpl::Swot => "SWOT analysis",
            Tpl::Flow => "Flowchart",
            Tpl::Week => "Weekly plan",
        }
    }
}

// ───────────────────────────── colours ─────────────────────────────

/// Accent colour of a colour index (0 = neutral → `None`).
pub fn accent_rgb(c: u8) -> Option<[u8; 3]> {
    Some(match c {
        1 => [224, 62, 62],
        2 => [217, 115, 13],
        3 => [203, 145, 47],
        4 => [15, 123, 108],
        5 => [11, 110, 153],
        6 => [105, 64, 165],
        7 => [173, 26, 114],
        _ => return None,
    })
}
pub const COLOR_COUNT: u8 = 8;

// ───────────────────────────── text helpers ─────────────────────────────

/// One display line of a card: what it is and the text without its markdown marker.
#[derive(Clone, Debug, PartialEq)]
pub enum Line<'a> {
    H(u8, &'a str),
    Bullet(u8, &'a str),
    Check(bool, &'a str),
    Quote(&'a str),
    Rule,
    Plain(&'a str),
    Blank,
}

pub fn parse_line(s: &str) -> Line<'_> {
    let t = s.trim_start();
    let indent = ((s.len() - t.len()) / 2).min(4) as u8;
    if t.is_empty() {
        return Line::Blank;
    }
    if t == "---" || t == "***" {
        return Line::Rule;
    }
    for (p, lvl) in [("### ", 3u8), ("## ", 2), ("# ", 1)] {
        if let Some(r) = t.strip_prefix(p) {
            return Line::H(lvl, r);
        }
    }
    for p in ["[ ] ", "- [ ] ", "* [ ] "] {
        if let Some(r) = t.strip_prefix(p) {
            return Line::Check(false, r);
        }
    }
    for p in ["[x] ", "[X] ", "- [x] ", "- [X] ", "* [x] "] {
        if let Some(r) = t.strip_prefix(p) {
            return Line::Check(true, r);
        }
    }
    if t == "[ ]" {
        return Line::Check(false, "");
    }
    if let Some(r) = t.strip_prefix("> ") {
        return Line::Quote(r);
    }
    for p in ["- ", "* ", "• "] {
        if let Some(r) = t.strip_prefix(p) {
            return Line::Bullet(indent, r);
        }
    }
    Line::Plain(s)
}

/// (done, total) check boxes of a card's text.
pub fn progress(text: &str) -> (usize, usize) {
    let mut d = 0;
    let mut n = 0;
    for l in text.lines() {
        if let Line::Check(on, _) = parse_line(l) {
            n += 1;
            d += on as usize;
        }
    }
    (d, n)
}

/// Flips the check box on line `idx` of `text` (keeps every other character).
pub fn toggle_check(text: &str, idx: usize) -> String {
    let mut out: Vec<String> = text.lines().map(|s| s.to_owned()).collect();
    if let Some(l) = out.get_mut(idx) {
        if let Some(p) = l.find("[ ]") {
            l.replace_range(p..p + 3, "[x]");
        } else if let Some(p) = l.find("[x]").or_else(|| l.find("[X]")) {
            l.replace_range(p..p + 3, "[ ]");
        }
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

// ───────────────────────────── geometry ─────────────────────────────

pub fn side_point(r: Rect, s: Side) -> (Pos2, Vec2) {
    match s {
        Side::Top => (pos2(r.center().x, r.top()), vec2(0.0, -1.0)),
        Side::Bottom => (pos2(r.center().x, r.bottom()), vec2(0.0, 1.0)),
        Side::Left => (pos2(r.left(), r.center().y), vec2(-1.0, 0.0)),
        _ => (pos2(r.right(), r.center().y), vec2(1.0, 0.0)),
    }
}

/// Picks the sides that face each other (used for `Side::Auto`).
pub fn auto_sides(a: Rect, b: Rect) -> (Side, Side) {
    let d = b.center() - a.center();
    // compare in units of half-sizes so a wide card next to a tall one still picks the natural side
    let kx = d.x.abs() / ((a.width() + b.width()) * 0.5).max(1.0);
    let ky = d.y.abs() / ((a.height() + b.height()) * 0.5).max(1.0);
    if kx >= ky {
        if d.x >= 0.0 { (Side::Right, Side::Left) } else { (Side::Left, Side::Right) }
    } else if d.y >= 0.0 {
        (Side::Bottom, Side::Top)
    } else {
        (Side::Top, Side::Bottom)
    }
}

/// Route of a connector in world space: polyline points, the direction the path arrives with and the direction it leaves with
/// (both pointing *into* the node they end at, i.e. the direction an arrow head there should face).
pub fn route(a: Rect, b: Rect, fs: Side, ts: Side, shape: Shape) -> (Vec<Pos2>, Vec2, Vec2) {
    let (auto_f, auto_t) = auto_sides(a, b);
    let fs = if fs == Side::Auto { auto_f } else { fs };
    let ts = if ts == Side::Auto { auto_t } else { ts };
    let (p0, n0) = side_point(a, fs);
    let (p1, n1) = side_point(b, ts);
    match shape {
        Shape::Straight => {
            let d = (p1 - p0).normalized();
            (vec![p0, p1], d, -d)
        }
        Shape::Elbow => {
            let horiz0 = n0.x != 0.0;
            let horiz1 = n1.x != 0.0;
            let pts = if horiz0 && horiz1 {
                let mx = if (n0.x > 0.0) == (p1.x > p0.x) { (p0.x + p1.x) * 0.5 } else { p0.x + n0.x * 40.0 };
                vec![p0, pos2(mx, p0.y), pos2(mx, p1.y), p1]
            } else if !horiz0 && !horiz1 {
                let my = if (n0.y > 0.0) == (p1.y > p0.y) { (p0.y + p1.y) * 0.5 } else { p0.y + n0.y * 40.0 };
                vec![p0, pos2(p0.x, my), pos2(p1.x, my), p1]
            } else if horiz0 {
                vec![p0, pos2(p1.x, p0.y), p1]
            } else {
                vec![p0, pos2(p0.x, p1.y), p1]
            };
            let l = pts.len();
            let d1 = (pts[l - 1] - pts[l - 2]).normalized();
            let d0 = (pts[0] - pts[1]).normalized();
            (pts, d1, d0)
        }
        Shape::Curve => {
            let dist = (p1 - p0).length();
            let k = (dist * 0.45).clamp(40.0, 220.0);
            let c0 = p0 + n0 * k;
            let c1 = p1 + n1 * k;
            let n = 28;
            let mut pts = Vec::with_capacity(n + 1);
            for i in 0..=n {
                let t = i as f32 / n as f32;
                let u = 1.0 - t;
                let p = p0.to_vec2() * (u * u * u) + c0.to_vec2() * (3.0 * u * u * t) + c1.to_vec2() * (3.0 * u * t * t) + p1.to_vec2() * (t * t * t);
                pts.push(p.to_pos2());
            }
            (pts, -n1, -n0)
        }
    }
}

/// Distance from `p` to the polyline.
pub fn dist_to_path(pts: &[Pos2], p: Pos2) -> f32 {
    let mut best = f32::MAX;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ab = b - a;
        let l2 = ab.length_sq();
        let t = if l2 < 1e-6 { 0.0 } else { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) };
        best = best.min((a + ab * t - p).length());
    }
    best
}

pub fn path_mid(pts: &[Pos2]) -> Pos2 {
    let total: f32 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut left = total * 0.5;
    for w in pts.windows(2) {
        let l = (w[1] - w[0]).length();
        if left <= l && l > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / l);
        }
        left -= l;
    }
    pts.last().copied().unwrap_or(Pos2::ZERO)
}

pub fn snap_v(v: f32) -> f32 {
    (v / GRID).round() * GRID
}

// ───────────────────────────── the canvas ─────────────────────────────

impl Canvas {
    pub fn new() -> Self {
        Canvas { view: View::default(), snap: true, minimap: true, ..Default::default() }
    }

    pub fn new_id(&mut self) -> u64 {
        let max = self.nodes.iter().map(|n| n.id).chain(self.edges.iter().map(|e| e.id)).max().unwrap_or(0);
        self.next_id = self.next_id.max(max) + 1;
        self.next_id
    }

    pub fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }
    pub fn edge(&self, id: u64) -> Option<&Edge> {
        self.edges.iter().find(|e| e.id == id)
    }
    pub fn edge_mut(&mut self, id: u64) -> Option<&mut Edge> {
        self.edges.iter_mut().find(|e| e.id == id)
    }

    /// Adds a node whose centre is `at`. Returns its id.
    pub fn add_node(&mut self, kind: Kind, at: Pos2) -> u64 {
        let id = self.new_id();
        let s = kind.default_size();
        let mut n = Node { id, kind, x: at.x - s.x / 2.0, y: at.y - s.y.max(40.0) / 2.0, w: s.x, h: s.y, ..Default::default() };
        if self.snap {
            n.x = snap_v(n.x);
            n.y = snap_v(n.y);
        }
        match kind {
            Kind::Todo => n.text = "[ ] ".into(),
            Kind::Group => n.text = "Group".into(),
            _ => {}
        }
        if kind == Kind::Group {
            let at = self.nodes.iter().take_while(|n| n.kind == Kind::Group).count();
            self.nodes.insert(at, n);
        } else {
            self.nodes.push(n);
        }
        self.rev += 1;
        id
    }

    /// Adds a connector (ignored for duplicates / self loops). Returns the edge id.
    pub fn connect(&mut self, from: u64, to: u64, fs: Side, ts: Side) -> Option<u64> {
        if from == to || self.node(from).is_none() || self.node(to).is_none() {
            return None;
        }
        if self.edges.iter().any(|e| e.from == from && e.to == to && e.from_side == fs && e.to_side == ts) {
            return None;
        }
        let id = self.new_id();
        let color = self.node(from).map_or(0, |n| if n.kind == Kind::Group { 0 } else { n.color });
        self.edges.push(Edge { id, from, to, from_side: fs, to_side: ts, color, ..Default::default() });
        self.rev += 1;
        Some(id)
    }

    pub fn delete_nodes(&mut self, ids: &BTreeSet<u64>) {
        self.nodes.retain(|n| !ids.contains(&n.id));
        self.edges.retain(|e| !ids.contains(&e.from) && !ids.contains(&e.to));
        self.rev += 1;
    }

    pub fn delete_edge(&mut self, id: u64) {
        self.edges.retain(|e| e.id != id);
        self.rev += 1;
    }

    // ───────── undo / redo ─────────

    fn snap_now(&self) -> Snap {
        Snap { nodes: self.nodes.clone(), edges: self.edges.clone() }
    }

    /// Call right before a user action changes the canvas.
    pub fn checkpoint(&mut self) {
        let s = self.snap_now();
        self.hist.undo.push(s);
        if self.hist.undo.len() > HIST_MAX {
            self.hist.undo.remove(0);
        }
        self.hist.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.hist.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.hist.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.hist.undo.pop() else { return false };
        let cur = self.snap_now();
        self.hist.redo.push(cur);
        self.nodes = s.nodes;
        self.edges = s.edges;
        self.rev += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.hist.redo.pop() else { return false };
        let cur = self.snap_now();
        self.hist.undo.push(cur);
        self.nodes = s.nodes;
        self.edges = s.edges;
        self.rev += 1;
        true
    }

    // ───────── geometry queries ─────────

    pub fn bounds(&self, ids: Option<&BTreeSet<u64>>) -> Option<Rect> {
        let mut r: Option<Rect> = None;
        for n in &self.nodes {
            if ids.map_or(true, |s| s.contains(&n.id)) {
                let nr = n.rect();
                r = Some(r.map_or(nr, |a| a.union(nr)));
            }
        }
        r
    }

    /// View that shows `world` fully inside a screen of `size` with `margin` pixels around it.
    pub fn fit(world: Rect, size: Vec2, margin: f32) -> View {
        let w = world.width().max(40.0);
        let h = world.height().max(40.0);
        let zoom = ((size.x - margin * 2.0).max(40.0) / w).min((size.y - margin * 2.0).max(40.0) / h).clamp(MIN_ZOOM, 1.6);
        View { x: world.center().x - size.x / zoom / 2.0, y: world.center().y - size.y / zoom / 2.0, zoom }
    }

    /// Nodes (other than the group itself) whose centre lies inside the group's frame.
    pub fn group_members(&self, gid: u64) -> Vec<u64> {
        let Some(g) = self.node(gid) else { return vec![] };
        let gr = g.rect();
        self.nodes.iter().filter(|n| n.id != gid && !n.locked && gr.contains(n.center())).map(|n| n.id).collect()
    }

    /// Top-most node under `p` (world space). Groups only answer on their title strip.
    pub fn hit_node(&self, p: Pos2, slack: f32, strip: f32) -> Option<u64> {
        for n in self.nodes.iter().rev() {
            if n.kind == Kind::Group {
                continue;
            }
            if n.rect().expand(slack).contains(p) {
                return Some(n.id);
            }
        }
        for n in self.nodes.iter().rev() {
            if n.kind == Kind::Group {
                let r = n.rect();
                let head = Rect::from_min_size(r.min, vec2(r.width(), strip)).expand(slack);
                if head.contains(p) {
                    return Some(n.id);
                }
            }
        }
        None
    }

    pub fn edge_route(&self, e: &Edge) -> Option<(Vec<Pos2>, Vec2, Vec2)> {
        let a = self.node(e.from)?.rect();
        let b = self.node(e.to)?.rect();
        Some(route(a, b, e.from_side, e.to_side, e.shape))
    }

    pub fn hit_edge(&self, p: Pos2, tol: f32) -> Option<u64> {
        let mut best: Option<(f32, u64)> = None;
        for e in self.edges.iter().rev() {
            if let Some((pts, _, _)) = self.edge_route(e) {
                let d = dist_to_path(&pts, p);
                if d <= tol && best.map_or(true, |(bd, _)| d < bd) {
                    best = Some((d, e.id));
                }
            }
        }
        best.map(|b| b.1)
    }

    // ───────── editing helpers ─────────

    /// Copies `ids` (and the connectors between them) shifted by `by`. Returns the new node ids.
    pub fn duplicate(&mut self, ids: &BTreeSet<u64>, by: Vec2) -> Vec<u64> {
        let mut map: HashMap<u64, u64> = HashMap::new();
        let src: Vec<Node> = self.nodes.iter().filter(|n| ids.contains(&n.id)).cloned().collect();
        let mut out = vec![];
        for mut n in src {
            let old = n.id;
            n.id = self.new_id();
            n.x += by.x;
            n.y += by.y;
            n.locked = false;
            map.insert(old, n.id);
            out.push(n.id);
            self.nodes.push(n);
        }
        let es: Vec<Edge> = self.edges.iter().filter(|e| map.contains_key(&e.from) && map.contains_key(&e.to)).cloned().collect();
        for mut e in es {
            e.id = self.new_id();
            e.from = map[&e.from];
            e.to = map[&e.to];
            self.edges.push(e);
        }
        self.rev += 1;
        out
    }

    /// Wraps the selection in a new group frame (inserted *behind* everything).
    pub fn group_selection(&mut self, ids: &BTreeSet<u64>) -> Option<u64> {
        let b = self.bounds(Some(ids))?;
        let id = self.new_id();
        let r = b.expand2(vec2(28.0, 28.0));
        let g = Node { id, kind: Kind::Group, x: r.min.x, y: r.min.y - 22.0, w: r.width(), h: r.height() + 22.0, text: "Group".into(), ..Default::default() };
        self.nodes.insert(0, g);
        self.rev += 1;
        Some(id)
    }

    pub fn bring_to_front(&mut self, ids: &BTreeSet<u64>) {
        let (mut keep, top): (Vec<Node>, Vec<Node>) = self.nodes.drain(..).partition(|n| !ids.contains(&n.id));
        keep.extend(top);
        self.nodes = keep;
        self.rev += 1;
    }

    pub fn send_to_back(&mut self, ids: &BTreeSet<u64>) {
        let (keep, mut top): (Vec<Node>, Vec<Node>) = self.nodes.drain(..).partition(|n| !ids.contains(&n.id));
        top.extend(keep);
        self.nodes = top;
        self.rev += 1;
    }

    pub fn align(&mut self, ids: &BTreeSet<u64>, how: Align) {
        let Some(b) = self.bounds(Some(ids)) else { return };
        for n in self.nodes.iter_mut().filter(|n| ids.contains(&n.id) && !n.locked) {
            match how {
                Align::Left => n.x = b.left(),
                Align::Right => n.x = b.right() - n.w,
                Align::CenterH => n.x = b.center().x - n.w / 2.0,
                Align::Top => n.y = b.top(),
                Align::Bottom => n.y = b.bottom() - n.h,
                Align::CenterV => n.y = b.center().y - n.h / 2.0,
            }
        }
        self.rev += 1;
    }

    /// Equal gaps between the selected nodes along one axis.
    pub fn distribute(&mut self, ids: &BTreeSet<u64>, horizontal: bool) {
        let mut v: Vec<(u64, f32, f32)> = self.nodes.iter().filter(|n| ids.contains(&n.id)).map(|n| if horizontal { (n.id, n.x, n.w) } else { (n.id, n.y, n.h) }).collect();
        if v.len() < 3 {
            return;
        }
        v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let first = v[0].1;
        let last_end = v.iter().map(|a| a.1 + a.2).fold(f32::MIN, f32::max);
        let total: f32 = v.iter().map(|a| a.2).sum();
        let gap = (last_end - first - total) / (v.len() as f32 - 1.0);
        let mut cur = first;
        for (id, _, size) in v {
            if let Some(n) = self.node_mut(id) {
                if horizontal {
                    n.x = cur
                } else {
                    n.y = cur
                }
            }
            cur += size + gap;
        }
        self.rev += 1;
    }

    /// Arranges nodes into a tidy grid, keeping their rough reading order.
    pub fn tidy(&mut self, ids: &BTreeSet<u64>) {
        let Some(b) = self.bounds(Some(ids)) else { return };
        let mut v: Vec<(u64, f32, f32)> = self.nodes.iter().filter(|n| ids.contains(&n.id) && n.kind != Kind::Group && n.kind != Kind::Ink && !n.locked).map(|n| (n.id, n.y, n.x)).collect();
        if v.is_empty() {
            return;
        }
        let cols = (v.len() as f32).sqrt().ceil().max(1.0) as usize;
        v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut rows: Vec<Vec<(u64, f32, f32)>> = v.chunks(cols).map(|c| c.to_vec()).collect();
        for r in rows.iter_mut() {
            r.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        }
        let gap = 32.0;
        let mut y = b.top();
        for row in rows {
            let mut x = b.left();
            let mut rh: f32 = 0.0;
            for (id, _, _) in row {
                if let Some(n) = self.node_mut(id) {
                    n.x = x;
                    n.y = y;
                    x += n.w + gap;
                    rh = rh.max(n.h);
                }
            }
            y += rh + gap;
        }
        self.rev += 1;
    }

    /// Lays out the tree hanging off `root` (following connectors) from left to right.
    pub fn tree_layout(&mut self, root: u64) {
        let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        seen.insert(root);
        let mut queue = vec![root];
        while let Some(p) = queue.pop() {
            let mut c: Vec<u64> = vec![];
            for e in &self.edges {
                if e.from == p && !seen.contains(&e.to) && self.node(e.to).is_some() {
                    seen.insert(e.to);
                    c.push(e.to);
                    queue.push(e.to);
                }
            }
            // keep the current top-to-bottom order of siblings
            c.sort_by(|a, b| {
                let ya = self.node(*a).map_or(0.0, |n| n.y);
                let yb = self.node(*b).map_or(0.0, |n| n.y);
                ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal)
            });
            kids.insert(p, c);
        }
        fn height(cv: &Canvas, kids: &HashMap<u64, Vec<u64>>, id: u64, memo: &mut HashMap<u64, f32>) -> f32 {
            let own = cv.node(id).map_or(60.0, |n| n.h.max(40.0));
            let ch = kids.get(&id).cloned().unwrap_or_default();
            let h = if ch.is_empty() { own } else { (ch.iter().map(|c| height(cv, kids, *c, memo)).sum::<f32>() + 20.0 * (ch.len() as f32 - 1.0)).max(own) };
            memo.insert(id, h);
            h
        }
        let mut memo = HashMap::new();
        height(self, &kids, root, &mut memo);
        let Some(r) = self.node(root).map(|n| n.rect()) else { return };
        let mut pos: Vec<(u64, f32, f32)> = vec![];
        fn place(cv: &Canvas, kids: &HashMap<u64, Vec<u64>>, memo: &HashMap<u64, f32>, id: u64, x: f32, top: f32, out: &mut Vec<(u64, f32, f32)>) {
            let h = memo.get(&id).copied().unwrap_or(60.0);
            let nh = cv.node(id).map_or(60.0, |n| n.h.max(40.0));
            let nw = cv.node(id).map_or(200.0, |n| n.w);
            out.push((id, x, top + (h - nh) / 2.0));
            let mut y = top;
            for c in kids.get(&id).cloned().unwrap_or_default() {
                place(cv, kids, memo, c, x + nw + 96.0, y, out);
                y += memo.get(&c).copied().unwrap_or(60.0) + 20.0;
            }
        }
        let total = memo.get(&root).copied().unwrap_or(60.0);
        place(self, &kids, &memo, root, r.left(), r.center().y - total / 2.0, &mut pos);
        for (id, x, y) in pos {
            if let Some(n) = self.node_mut(id) {
                n.x = x;
                n.y = y;
            }
        }
        self.rev += 1;
    }

    /// Mind-map style: a new connected card to the right of `parent`. Returns its id.
    pub fn add_child(&mut self, parent: u64) -> Option<u64> {
        let p = self.node(parent)?.clone();
        let sibs: Vec<Rect> = self.edges.iter().filter(|e| e.from == parent).filter_map(|e| self.node(e.to)).map(|n| n.rect()).collect();
        let size = if p.kind == Kind::Group || p.kind == Kind::Image { Kind::Note.default_size() } else { vec2(p.w.clamp(160.0, 300.0), 64.0) };
        let x = p.x + p.w + 90.0;
        let y = sibs.iter().map(|r| r.bottom() + 20.0).fold(p.y, f32::max);
        let id = self.new_id();
        self.nodes.push(Node { id, kind: Kind::Note, x, y, w: size.x, h: size.y, color: p.color, ..Default::default() });
        self.connect(parent, id, Side::Right, Side::Left);
        self.rev += 1;
        Some(id)
    }

    /// New card under `node` that shares its parent (or a free card when it has none).
    pub fn add_sibling(&mut self, node: u64) -> Option<u64> {
        let parent = self.edges.iter().find(|e| e.to == node).map(|e| e.from);
        let n = self.node(node)?.clone();
        let id = self.new_id();
        self.nodes.push(Node { id, kind: Kind::Note, x: n.x, y: n.y + n.h.max(40.0) + 20.0, w: n.w.max(120.0), h: 64.0, color: n.color, ..Default::default() });
        if let Some(p) = parent {
            self.connect(p, id, Side::Right, Side::Left);
        }
        self.rev += 1;
        Some(id)
    }

    // ───────── text search ─────────

    pub fn find(&self, q: &str) -> Vec<u64> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        self.nodes.iter().filter(|n| n.text.to_lowercase().contains(&q) || n.url.to_lowercase().contains(&q)).map(|n| n.id).collect()
    }

    /// All text of the canvas (workspace search / indexing).
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for n in &self.nodes {
            if !n.text.is_empty() {
                s.push_str(&n.text);
                s.push('\n');
            }
        }
        s
    }

    /// Ink points → compact stroke node. `pts` are world points.
    pub fn add_ink(&mut self, pts: &[Pos2], color: u8, size: u8) -> Option<u64> {
        if pts.len() < 2 {
            return None;
        }
        // drop points that are closer than 1.5 units to the previous kept one
        let mut kept: Vec<Pos2> = vec![pts[0]];
        for p in &pts[1..] {
            if (*p - *kept.last().unwrap()).length() >= 1.5 {
                kept.push(*p);
            }
        }
        if kept.len() < 2 {
            kept.push(pts[pts.len() - 1] + vec2(0.5, 0.5));
        }
        let mut r = Rect::from_min_max(kept[0], kept[0]);
        for p in &kept {
            r.extend_with(*p);
        }
        let id = self.new_id();
        self.nodes.push(Node {
            id,
            kind: Kind::Ink,
            x: r.min.x,
            y: r.min.y,
            w: r.width().max(1.0),
            h: r.height().max(1.0),
            color,
            size,
            pts: kept.iter().map(|p| [p.x - r.min.x, p.y - r.min.y]).collect(),
            ..Default::default()
        });
        self.rev += 1;
        Some(id)
    }

    // ───────── Obsidian Canvas JSON ─────────

    /// Obsidian-compatible `.canvas` JSON (extra `lumen` fields keep Lumen-only kinds lossless).
    pub fn to_json(&self, ids: Option<&BTreeSet<u64>>) -> String {
        use serde_json::{json, Value};
        let sel = |id: u64| ids.map_or(true, |s| s.contains(&id));
        let mut nodes: Vec<Value> = vec![];
        for n in self.nodes.iter().filter(|n| sel(n.id) && n.kind != Kind::Ink) {
            let color = if n.color == 0 {
                Value::Null
            } else if n.color <= 6 {
                json!(n.color.to_string())
            } else {
                json!("#ad1a72")
            };
            let mut v = match n.kind {
                Kind::Group => json!({"id": n.id.to_string(), "type": "group", "label": n.text}),
                Kind::Link => json!({"id": n.id.to_string(), "type": "link", "url": n.url}),
                Kind::Image => json!({"id": n.id.to_string(), "type": "file", "file": n.url}),
                _ => json!({"id": n.id.to_string(), "type": "text", "text": n.text}),
            };
            let o = v.as_object_mut().unwrap();
            o.insert("x".into(), json!(n.x.round() as i64));
            o.insert("y".into(), json!(n.y.round() as i64));
            o.insert("width".into(), json!(n.w.round() as i64));
            o.insert("height".into(), json!(n.h.round().max(1.0) as i64));
            if !color.is_null() {
                o.insert("color".into(), color);
            }
            o.insert("lumen".into(), json!(format!("{:?}", n.kind)));
            if let Some(p) = n.page {
                o.insert("page".into(), json!(p));
            }
            nodes.push(v);
        }
        let mut edges: Vec<Value> = vec![];
        let side = |s: Side| match s {
            Side::Top => Some("top"),
            Side::Right => Some("right"),
            Side::Bottom => Some("bottom"),
            Side::Left => Some("left"),
            Side::Auto => None,
        };
        for e in self.edges.iter().filter(|e| sel(e.from) && sel(e.to)) {
            let (fa, ta) = (self.node(e.from), self.node(e.to));
            if fa.map_or(true, |n| n.kind == Kind::Ink) || ta.map_or(true, |n| n.kind == Kind::Ink) {
                continue;
            }
            let (af, at) = match (fa, ta) {
                (Some(a), Some(b)) => auto_sides(a.rect(), b.rect()),
                _ => (Side::Right, Side::Left),
            };
            let fs = if e.from_side == Side::Auto { af } else { e.from_side };
            let ts = if e.to_side == Side::Auto { at } else { e.to_side };
            let mut o = json!({"id": e.id.to_string(), "fromNode": e.from.to_string(), "toNode": e.to.to_string(),
                "fromSide": side(fs), "toSide": side(ts)});
            let m = o.as_object_mut().unwrap();
            if !e.label.is_empty() {
                m.insert("label".into(), json!(e.label));
            }
            if e.color > 0 && e.color <= 6 {
                m.insert("color".into(), json!(e.color.to_string()));
            }
            if e.arrow == Arrow::None {
                m.insert("toEnd".into(), json!("none"));
            }
            if e.arrow == Arrow::Both {
                m.insert("fromEnd".into(), json!("arrow"));
            }
            if e.dashed {
                m.insert("lumenDashed".into(), json!(true));
            }
            edges.push(o);
        }
        serde_json::to_string_pretty(&json!({"nodes": nodes, "edges": edges})).unwrap_or_default()
    }

    /// Imports Obsidian-style JSON; new nodes are centred on `at`. Returns the new node ids (empty when `text` is not canvas JSON).
    pub fn import_json(&mut self, text: &str, at: Pos2) -> Vec<u64> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else { return vec![] };
        let Some(nodes) = v.get("nodes").and_then(|n| n.as_array()) else { return vec![] };
        let mut r: Option<Rect> = None;
        for n in nodes {
            let (x, y) = (n["x"].as_f64().unwrap_or(0.0) as f32, n["y"].as_f64().unwrap_or(0.0) as f32);
            let (w, h) = (n["width"].as_f64().unwrap_or(240.0) as f32, n["height"].as_f64().unwrap_or(120.0) as f32);
            let nr = Rect::from_min_size(pos2(x, y), vec2(w, h));
            r = Some(r.map_or(nr, |a| a.union(nr)));
        }
        let Some(b) = r else { return vec![] };
        let shift = at - b.center();
        let mut map: HashMap<String, u64> = HashMap::new();
        let mut out = vec![];
        let color_of = |c: &serde_json::Value| -> u8 {
            match c.as_str() {
                Some("1") => 1,
                Some("2") => 2,
                Some("3") => 3,
                Some("4") => 4,
                Some("5") => 5,
                Some("6") => 6,
                Some("#ad1a72") => 7,
                _ => 0,
            }
        };
        for n in nodes {
            let ty = n["type"].as_str().unwrap_or("text");
            let kind = match n["lumen"].as_str() {
                Some("Text") => Kind::Text,
                Some("Todo") => Kind::Todo,
                Some("Page") => Kind::Page,
                Some("Image") => Kind::Image,
                Some("Link") => Kind::Link,
                Some("Group") => Kind::Group,
                Some("Note") => Kind::Note,
                _ => match ty {
                    "group" => Kind::Group,
                    "link" => Kind::Link,
                    "file" => {
                        let f = n["file"].as_str().unwrap_or("").to_lowercase();
                        if [".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg"].iter().any(|e| f.ends_with(e)) {
                            Kind::Image
                        } else {
                            Kind::Note
                        }
                    }
                    _ => Kind::Note,
                },
            };
            let id = self.new_id();
            map.insert(n["id"].as_str().map(|s| s.to_owned()).unwrap_or_else(|| id.to_string()), id);
            let mut node = Node {
                id,
                kind,
                x: n["x"].as_f64().unwrap_or(0.0) as f32 + shift.x,
                y: n["y"].as_f64().unwrap_or(0.0) as f32 + shift.y,
                w: n["width"].as_f64().unwrap_or(240.0) as f32,
                h: n["height"].as_f64().unwrap_or(120.0) as f32,
                color: color_of(&n["color"]),
                ..Default::default()
            };
            node.text = match ty {
                "group" => n["label"].as_str().unwrap_or("Group").to_owned(),
                "file" if kind != Kind::Image => n["file"].as_str().unwrap_or("").to_owned(),
                _ => n["text"].as_str().unwrap_or("").to_owned(),
            };
            node.url = n["url"].as_str().or_else(|| n["file"].as_str()).unwrap_or("").to_owned();
            if kind == Kind::Page {
                node.page = n["page"].as_u64();
            }
            if node.w < 20.0 || !node.w.is_finite() {
                node.w = 240.0;
            }
            if !node.h.is_finite() || node.h < 1.0 {
                node.h = 60.0;
            }
            if kind == Kind::Group {
                let at = self.nodes.iter().take_while(|n| n.kind == Kind::Group).count();
                self.nodes.insert(at, node);
            } else {
                self.nodes.push(node);
            }
            out.push(id);
        }
        if let Some(edges) = v.get("edges").and_then(|e| e.as_array()) {
            let side = |s: &serde_json::Value| match s.as_str() {
                Some("top") => Side::Top,
                Some("right") => Side::Right,
                Some("bottom") => Side::Bottom,
                Some("left") => Side::Left,
                _ => Side::Auto,
            };
            for e in edges {
                let (Some(f), Some(t)) = (e["fromNode"].as_str().and_then(|s| map.get(s)), e["toNode"].as_str().and_then(|s| map.get(s))) else { continue };
                let id = self.new_id();
                self.edges.push(Edge {
                    id,
                    from: *f,
                    to: *t,
                    from_side: side(&e["fromSide"]),
                    to_side: side(&e["toSide"]),
                    label: e["label"].as_str().unwrap_or("").to_owned(),
                    color: color_of(&e["color"]),
                    dashed: e["lumenDashed"].as_bool().unwrap_or(false),
                    arrow: if e["toEnd"].as_str() == Some("none") {
                        Arrow::None
                    } else if e["fromEnd"].as_str() == Some("arrow") {
                        Arrow::Both
                    } else {
                        Arrow::End
                    },
                    shape: Shape::Curve,
                });
            }
        }
        self.rev += 1;
        out
    }

    // ───────── templates ─────────

    /// Inserts a ready-made board centred on `at`. Returns the new node ids.
    pub fn insert_template(&mut self, t: Tpl, at: Pos2) -> Vec<u64> {
        let before: BTreeSet<u64> = self.nodes.iter().map(|n| n.id).collect();
        let mut ox = at.x;
        let mut oy = at.y;
        match t {
            Tpl::MindMap => {
                ox -= 420.0;
                oy -= 170.0;
                let c = self.tpl_node(Kind::Note, ox, oy + 120.0, 200.0, 80.0, 5, "# Central idea");
                let branches = [("Goals", 4, ["Why it matters", "Success looks like"]), ("People", 6, ["Who is involved", "Who decides"]), ("Risks", 1, ["What could go wrong", "Plan B"]), ("Next steps", 2, ["This week", "This month"])];
                let mut y = oy - 40.0;
                for (name, col, leaves) in branches {
                    let b = self.tpl_node(Kind::Note, ox + 300.0, y + 30.0, 170.0, 56.0, col, &format!("## {name}"));
                    self.connect(c, b, Side::Right, Side::Left);
                    for (i, l) in leaves.iter().enumerate() {
                        let n = self.tpl_node(Kind::Note, ox + 560.0, y + i as f32 * 62.0, 210.0, 52.0, 0, l);
                        self.connect(b, n, Side::Right, Side::Left);
                    }
                    y += 150.0;
                }
            }
            Tpl::Kanban => {
                ox -= 510.0;
                oy -= 280.0;
                let cols = [("To do", 0u8, vec!["[ ] Write the brief", "[ ] Collect references"]), ("In progress", 3, vec!["[ ] First draft"]), ("Done", 4, vec!["[x] Kick-off meeting"])];
                for (i, (name, col, cards)) in cols.into_iter().enumerate() {
                    let x = ox + i as f32 * 344.0;
                    self.tpl_node(Kind::Group, x, oy, 320.0, 560.0, col, name);
                    for (j, c) in cards.into_iter().enumerate() {
                        self.tpl_node(Kind::Todo, x + 24.0, oy + 64.0 + j as f32 * 120.0, 272.0, 96.0, 0, c);
                    }
                }
            }
            Tpl::Swot => {
                ox -= 340.0;
                oy -= 270.0;
                let q = [("Strengths", 4u8, "- What we do well\n- Unique resources"), ("Weaknesses", 1, "- What we lack\n- Where we are slow"), ("Opportunities", 5, "- Trends we can use\n- Gaps in the market"), ("Threats", 2, "- Competitors\n- Changes we cannot control")];
                for (i, (name, col, body)) in q.into_iter().enumerate() {
                    let x = ox + (i % 2) as f32 * 360.0;
                    let y = oy + (i / 2) as f32 * 280.0;
                    self.tpl_node(Kind::Group, x, y, 340.0, 260.0, col, name);
                    self.tpl_node(Kind::Note, x + 24.0, y + 64.0, 292.0, 150.0, 0, body);
                }
            }
            Tpl::Flow => {
                ox -= 480.0;
                oy -= 150.0;
                let a = self.tpl_node(Kind::Note, ox, oy + 100.0, 150.0, 56.0, 4, "Start");
                let b = self.tpl_node(Kind::Note, ox + 230.0, oy + 100.0, 170.0, 56.0, 0, "Do the work");
                let c = self.tpl_node(Kind::Note, ox + 480.0, oy + 88.0, 170.0, 80.0, 3, "Good enough?");
                let d = self.tpl_node(Kind::Note, ox + 750.0, oy + 100.0, 150.0, 56.0, 5, "Ship it");
                let e = self.tpl_node(Kind::Note, ox + 480.0, oy + 270.0, 170.0, 56.0, 1, "Improve");
                self.connect(a, b, Side::Right, Side::Left);
                self.connect(b, c, Side::Right, Side::Left);
                if let Some(id) = self.connect(c, d, Side::Right, Side::Left) {
                    if let Some(ed) = self.edge_mut(id) {
                        ed.label = "Yes".into();
                    }
                }
                if let Some(id) = self.connect(c, e, Side::Bottom, Side::Top) {
                    if let Some(ed) = self.edge_mut(id) {
                        ed.label = "No".into();
                    }
                }
                if let Some(id) = self.connect(e, b, Side::Left, Side::Bottom) {
                    if let Some(ed) = self.edge_mut(id) {
                        ed.dashed = true;
                        ed.shape = Shape::Elbow;
                    }
                }
            }
            Tpl::Week => {
                ox -= 660.0;
                oy -= 230.0;
                for (i, d) in ["Mon", "Tue", "Wed", "Thu", "Fri"].iter().enumerate() {
                    let x = ox + i as f32 * 264.0;
                    self.tpl_node(Kind::Group, x, oy, 248.0, 460.0, if i == 4 { 4 } else { 0 }, d);
                    self.tpl_node(Kind::Todo, x + 16.0, oy + 60.0, 216.0, 110.0, 0, "[ ] \n[ ] \n[ ] ");
                }
            }
        }
        self.rev += 1;
        self.nodes.iter().filter(|n| !before.contains(&n.id)).map(|n| n.id).collect()
    }

    fn tpl_node(&mut self, kind: Kind, x: f32, y: f32, w: f32, h: f32, color: u8, text: &str) -> u64 {
        let id = self.new_id();
        let n = Node { id, kind, x, y, w, h, color, text: text.to_owned(), ..Default::default() };
        // groups live behind everything else
        if kind == Kind::Group {
            let at = self.nodes.iter().take_while(|n| n.kind == Kind::Group).count();
            self.nodes.insert(at, n);
        } else {
            self.nodes.push(n);
        }
        id
    }

    /// The board a brand-new workspace opens with. `a` / `b` are workspace pages to link to.
    pub fn sample(a: Option<u64>, b: Option<u64>) -> Canvas {
        let mut c = Canvas::new();
        c.view = View { x: -120.0, y: -120.0, zoom: 0.8 };
        let t = c.tpl_node(Kind::Text, 0.0, 0.0, 520.0, 56.0, 0, "# Product launch");
        if let Some(n) = c.node_mut(t) {
            n.size = 2;
        }
        c.tpl_node(Kind::Group, 0.0, 90.0, 330.0, 400.0, 5, "Research");
        c.tpl_node(Kind::Group, 400.0, 90.0, 330.0, 400.0, 4, "Build");
        c.tpl_node(Kind::Group, 800.0, 90.0, 330.0, 400.0, 2, "Launch");
        let n1 = c.tpl_node(Kind::Note, 24.0, 150.0, 282.0, 120.0, 0, "## Who is it for?\nSolo founders and small teams who want one calm place for notes, plans and boards.");
        c.tpl_node(Kind::Todo, 24.0, 300.0, 282.0, 140.0, 0, "[x] Interview 5 customers\n[x] Map the competition\n[ ] Pick the positioning");
        let n3 = c.tpl_node(Kind::Note, 424.0, 150.0, 282.0, 120.0, 5, "## Scope v1\n- Canvas, pages, databases\n- Windows + web");
        c.tpl_node(Kind::Todo, 424.0, 300.0, 282.0, 140.0, 0, "[x] Prototype\n[ ] Polish onboarding\n[ ] Performance pass");
        let n5 = c.tpl_node(Kind::Note, 824.0, 150.0, 282.0, 120.0, 2, "## Announce\nBlog post, short demo video, community post.");
        c.tpl_node(Kind::Todo, 824.0, 300.0, 282.0, 140.0, 0, "[ ] Landing page\n[ ] Press kit\n[ ] Launch day checklist");
        c.connect(n1, n3, Side::Right, Side::Left);
        c.connect(n3, n5, Side::Right, Side::Left);
        let q = c.tpl_node(Kind::Note, 1200.0, 160.0, 230.0, 90.0, 3, "Open question\nWhat is the one-line pitch?");
        if let Some(id) = c.connect(n5, q, Side::Right, Side::Left) {
            if let Some(e) = c.edge_mut(id) {
                e.dashed = true;
                e.label = "needs".into();
            }
        }
        if let Some(pg) = a {
            let id = c.tpl_node(Kind::Page, 0.0, 540.0, 300.0, 96.0, 0, "");
            if let Some(n) = c.node_mut(id) {
                n.page = Some(pg);
            }
        }
        if let Some(pg) = b {
            let id = c.tpl_node(Kind::Page, 400.0, 540.0, 300.0, 96.0, 0, "");
            if let Some(n) = c.node_mut(id) {
                n.page = Some(pg);
            }
        }
        let l = c.tpl_node(Kind::Link, 800.0, 540.0, 330.0, 84.0, 0, "Rust + egui");
        if let Some(n) = c.node_mut(l) {
            n.url = "https://github.com/emilk/egui".into();
        }
        c.rev += 1;
        c
    }

    /// Fixes anything a damaged / hand-edited file could contain (NaN geometry, dangling connectors, duplicate ids).
    pub fn repair(&mut self) {
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        let mut fix: Vec<usize> = vec![];
        for (i, n) in self.nodes.iter().enumerate() {
            if !seen.insert(n.id) {
                fix.push(i);
            }
        }
        for i in fix {
            let id = self.new_id();
            self.nodes[i].id = id;
        }
        for n in self.nodes.iter_mut() {
            if !n.x.is_finite() {
                n.x = 0.0;
            }
            if !n.y.is_finite() {
                n.y = 0.0;
            }
            if !n.w.is_finite() || n.w < 1.0 {
                n.w = n.kind.default_size().x;
            }
            if !n.h.is_finite() || n.h < 0.0 {
                n.h = 0.0;
            }
            if n.color >= COLOR_COUNT {
                n.color = 0;
            }
        }
        let ids: BTreeSet<u64> = self.nodes.iter().map(|n| n.id).collect();
        self.edges.retain(|e| ids.contains(&e.from) && ids.contains(&e.to) && e.from != e.to);
        self.view = self.view.sane();
        let max = self.nodes.iter().map(|n| n.id).chain(self.edges.iter().map(|e| e.id)).max().unwrap_or(0);
        self.next_id = self.next_id.max(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[u64]) -> BTreeSet<u64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn nodes_edges_and_delete() {
        let mut c = Canvas::new();
        let a = c.add_node(Kind::Note, pos2(0.0, 0.0));
        let b = c.add_node(Kind::Todo, pos2(400.0, 0.0));
        let e = c.connect(a, b, Side::Auto, Side::Auto).unwrap();
        assert!(c.connect(a, b, Side::Auto, Side::Auto).is_none(), "no duplicate connector");
        assert!(c.connect(a, a, Side::Auto, Side::Auto).is_none(), "no self loop");
        assert!(c.edge(e).is_some());
        c.delete_nodes(&set(&[a]));
        assert!(c.edges.is_empty(), "connectors die with their node");
        assert_eq!(c.nodes.len(), 1);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut c = Canvas::new();
        c.checkpoint();
        let a = c.add_node(Kind::Note, pos2(0.0, 0.0));
        c.checkpoint();
        c.node_mut(a).unwrap().x = 500.0;
        assert!(c.undo());
        assert_eq!(c.node(a).unwrap().x, snap_v(-120.0));
        assert!(c.undo());
        assert!(c.nodes.is_empty());
        assert!(!c.undo());
        assert!(c.redo() && c.redo());
        assert_eq!(c.node(a).unwrap().x, 500.0);
        c.checkpoint();
        assert!(!c.can_redo(), "a new action clears redo");
    }

    #[test]
    fn view_zoom_keeps_anchor() {
        let mut v = View { x: 100.0, y: 50.0, zoom: 1.0 };
        let o = pos2(10.0, 20.0);
        let at = pos2(300.0, 200.0);
        let w0 = v.to_world(o, at);
        v.zoom_at(o, at, 1.7);
        let w1 = v.to_world(o, at);
        assert!((w0 - w1).length() < 1e-3);
        v.zoom_at(o, at, 1000.0);
        assert_eq!(v.zoom, MAX_ZOOM);
        let s = v.to_screen(o, w1);
        assert!((s - at).length() < 1.0);
    }

    #[test]
    fn fit_contains_bounds() {
        let b = Rect::from_min_size(pos2(-500.0, 200.0), vec2(2000.0, 800.0));
        let size = vec2(800.0, 600.0);
        let v = Canvas::fit(b, size, 40.0);
        let s = v.rect_to_screen(Pos2::ZERO, b);
        assert!(s.min.x >= 39.0 && s.max.x <= 761.0 && s.min.y >= 0.0 && s.max.y <= 600.0, "{s:?}");
    }

    #[test]
    fn auto_sides_face_each_other() {
        let a = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
        assert_eq!(auto_sides(a, a.translate(vec2(300.0, 0.0))), (Side::Right, Side::Left));
        assert_eq!(auto_sides(a, a.translate(vec2(-300.0, 0.0))), (Side::Left, Side::Right));
        assert_eq!(auto_sides(a, a.translate(vec2(0.0, 200.0))), (Side::Bottom, Side::Top));
        assert_eq!(auto_sides(a, a.translate(vec2(10.0, -200.0))), (Side::Top, Side::Bottom));
    }

    #[test]
    fn routes_and_hit_testing() {
        let mut c = Canvas::new();
        let a = c.add_node(Kind::Note, pos2(0.0, 0.0));
        let b = c.add_node(Kind::Note, pos2(500.0, 200.0));
        let e = c.connect(a, b, Side::Auto, Side::Auto).unwrap();
        for shape in [Shape::Curve, Shape::Straight, Shape::Elbow] {
            c.edge_mut(e).unwrap().shape = shape;
            let (pts, _, _) = c.edge_route(c.edge(e).unwrap()).unwrap();
            assert!(pts.len() >= 2);
            let mid = path_mid(&pts);
            assert_eq!(c.hit_edge(mid, 6.0), Some(e), "{shape:?}");
            assert_eq!(c.hit_edge(mid + vec2(0.0, 300.0), 6.0), None);
        }
    }

    #[test]
    fn hit_node_prefers_top_and_groups_only_by_title() {
        let mut c = Canvas::new();
        c.snap = false;
        let g = c.add_node(Kind::Group, pos2(200.0, 200.0));
        let a = c.add_node(Kind::Note, pos2(200.0, 200.0));
        assert_eq!(c.hit_node(pos2(200.0, 200.0), 0.0, 30.0), Some(a));
        // inside the group but outside the note: nothing (so a marquee can start)
        assert_eq!(c.hit_node(pos2(60.0, 260.0), 0.0, 30.0), None);
        let gn = c.node(g).unwrap();
        assert_eq!(c.hit_node(pos2(gn.x + 20.0, gn.y + 10.0), 0.0, 30.0), Some(g));
        assert_eq!(c.group_members(g), vec![a]);
    }

    #[test]
    fn markdown_lines() {
        assert_eq!(parse_line("## Title"), Line::H(2, "Title"));
        assert_eq!(parse_line("- item"), Line::Bullet(0, "item"));
        assert_eq!(parse_line("  - item"), Line::Bullet(1, "item"));
        assert_eq!(parse_line("[x] done"), Line::Check(true, "done"));
        assert_eq!(parse_line("- [ ] todo"), Line::Check(false, "todo"));
        assert_eq!(parse_line("> q"), Line::Quote("q"));
        assert_eq!(parse_line("---"), Line::Rule);
        assert_eq!(parse_line(""), Line::Blank);
        assert_eq!(progress("[x] a\n[ ] b\ntext\n[ ] c"), (1, 3));
        assert_eq!(toggle_check("[x] a\n[ ] b", 1), "[x] a\n[x] b");
        assert_eq!(toggle_check("[x] a\n[ ] b", 0), "[ ] a\n[ ] b");
    }

    #[test]
    fn align_distribute_tidy() {
        let mut c = Canvas::new();
        c.snap = false;
        let ids: Vec<u64> = (0..4).map(|i| c.add_node(Kind::Note, pos2(i as f32 * 333.0, i as f32 * 71.0))).collect();
        let s: BTreeSet<u64> = ids.iter().copied().collect();
        c.align(&s, Align::Left);
        let x0 = c.node(ids[0]).unwrap().x;
        assert!(ids.iter().all(|i| (c.node(*i).unwrap().x - x0).abs() < 0.01));
        c.align(&s, Align::Top);
        let y0 = c.node(ids[0]).unwrap().y;
        assert!(ids.iter().all(|i| (c.node(*i).unwrap().y - y0).abs() < 0.01));
        for (k, id) in ids.iter().enumerate() {
            c.node_mut(*id).unwrap().x = (k * k) as f32 * 200.0;
        }
        c.distribute(&s, true);
        let gaps: Vec<f32> = ids.windows(2).map(|w| c.node(w[1]).unwrap().x - c.node(w[0]).unwrap().rect().right()).collect();
        assert!((gaps[0] - gaps[1]).abs() < 0.01 && (gaps[1] - gaps[2]).abs() < 0.01, "{gaps:?}");
        c.tidy(&s);
        for i in 0..ids.len() {
            for j in i + 1..ids.len() {
                assert!(!c.node(ids[i]).unwrap().rect().intersects(c.node(ids[j]).unwrap().rect()));
            }
        }
    }

    #[test]
    fn tree_layout_and_mind_map_keys() {
        let mut c = Canvas::new();
        c.snap = false;
        let root = c.add_node(Kind::Note, pos2(0.0, 0.0));
        let k1 = c.add_child(root).unwrap();
        let k2 = c.add_child(root).unwrap();
        let g = c.add_child(k1).unwrap();
        let sib = c.add_sibling(k2).unwrap();
        assert_eq!(c.edges.iter().filter(|e| e.from == root).count(), 3);
        c.tree_layout(root);
        let r = |id| c.node(id).unwrap().rect();
        assert!(r(k1).left() > r(root).right() && r(g).left() > r(k1).right());
        for (a, b) in [(k1, k2), (k2, sib), (k1, sib), (g, k2)] {
            assert!(!r(a).intersects(r(b)), "{a} overlaps {b}");
        }
    }

    #[test]
    fn duplicate_keeps_internal_connectors() {
        let mut c = Canvas::new();
        let a = c.add_node(Kind::Note, pos2(0.0, 0.0));
        let b = c.add_node(Kind::Note, pos2(400.0, 0.0));
        let z = c.add_node(Kind::Note, pos2(800.0, 0.0));
        c.connect(a, b, Side::Auto, Side::Auto);
        c.connect(b, z, Side::Auto, Side::Auto);
        let new = c.duplicate(&set(&[a, b]), vec2(0.0, 300.0));
        assert_eq!(new.len(), 2);
        assert_eq!(c.edges.len(), 3, "only the connector inside the selection is copied");
        let mut ids: Vec<u64> = c.nodes.iter().map(|n| n.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn json_roundtrip_is_obsidian_shaped() {
        let mut c = Canvas::sample(Some(7), None);
        c.snap = false;
        let json = c.to_json(None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["nodes"].as_array().unwrap().iter().all(|n| n["id"].is_string() && n["x"].is_i64() && n["width"].is_i64() && n["type"].is_string()));
        assert!(v["edges"].as_array().unwrap().iter().all(|e| e["fromNode"].is_string() && e["toNode"].is_string()));
        let mut d = Canvas::new();
        let ids = d.import_json(&json, pos2(0.0, 0.0));
        assert_eq!(ids.len(), c.nodes.len());
        assert_eq!(d.edges.len(), c.edges.len());
        assert!(d.nodes.iter().any(|n| n.kind == Kind::Group && n.text == "Research"));
        assert!(d.nodes.iter().any(|n| n.kind == Kind::Page && n.page == Some(7)));
        // plain obsidian file (no lumen fields)
        let obs = r##"{"nodes":[{"id":"a","type":"text","text":"hi","x":0,"y":0,"width":200,"height":80,"color":"4"},{"id":"b","type":"file","file":"pic.PNG","x":300,"y":0,"width":200,"height":80},{"id":"g","type":"group","label":"G","x":-50,"y":-50,"width":700,"height":300}],"edges":[{"id":"e","fromNode":"a","toNode":"b","fromSide":"right","toSide":"left","label":"x"}]}"##;
        let mut o = Canvas::new();
        let ids = o.import_json(obs, pos2(0.0, 0.0));
        assert_eq!(ids.len(), 3);
        assert_eq!(o.nodes[0].kind, Kind::Group, "groups are sent behind");
        assert!(o.nodes.iter().any(|n| n.color == 4 && n.text == "hi"));
        assert!(o.nodes.iter().any(|n| n.kind == Kind::Image && n.url == "pic.PNG"));
        assert_eq!(o.edges[0].label, "x");
        assert_eq!(o.edges[0].from_side, Side::Right);
        assert!(o.import_json("not json", Pos2::ZERO).is_empty());
        assert!(o.import_json("{\"foo\":1}", Pos2::ZERO).is_empty());
    }

    #[test]
    fn serde_survives_missing_fields_and_repair() {
        let c = Canvas::sample(None, None);
        let s = serde_json::to_string(&c).unwrap();
        let back: Canvas = serde_json::from_str(&s).unwrap();
        assert_eq!(back.nodes.len(), c.nodes.len());
        let mut old: Canvas = serde_json::from_str(r#"{"nodes":[{"id":1,"x":0,"y":0,"w":100,"h":50},{"id":1,"x":5,"y":0,"w":-5,"h":50,"color":99}],"edges":[{"id":3,"from":1,"to":9}]}"#).unwrap();
        old.repair();
        let ids: BTreeSet<u64> = old.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids.len(), 2, "duplicate ids fixed");
        assert!(old.edges.is_empty(), "dangling connector removed");
        assert!(old.nodes.iter().all(|n| n.w >= 1.0 && n.color < COLOR_COUNT));
        assert!(old.snap && old.minimap, "defaults are on");
    }

    #[test]
    fn templates_build_connected_boards() {
        for t in Tpl::ALL {
            let mut c = Canvas::new();
            let ids = c.insert_template(t, pos2(0.0, 0.0));
            assert!(ids.len() >= 5, "{t:?}");
            let b = c.bounds(None).unwrap();
            assert!(b.center().to_vec2().length() < 120.0, "{t:?} is centred on the insertion point: {b:?}");
            assert!(c.edges.iter().all(|e| c.node(e.from).is_some() && c.node(e.to).is_some()));
            let first_non_group = c.nodes.iter().position(|n| n.kind != Kind::Group).unwrap_or(0);
            assert!(c.nodes[first_non_group..].iter().all(|n| n.kind != Kind::Group), "{t:?}");
        }
    }

    #[test]
    fn ink_and_search() {
        let mut c = Canvas::new();
        let pts: Vec<Pos2> = (0..50).map(|i| pos2(100.0 + i as f32 * 3.0, 50.0 + (i as f32 * 0.3).sin() * 20.0)).collect();
        let id = c.add_ink(&pts, 1, 1).unwrap();
        let n = c.node(id).unwrap();
        assert_eq!(n.kind, Kind::Ink);
        assert!(n.w > 100.0 && n.pts.iter().all(|p| p[0] >= 0.0 && p[1] >= 0.0));
        assert!(c.add_ink(&pts[..1], 0, 0).is_none());
        let a = c.add_node(Kind::Note, Pos2::ZERO);
        c.node_mut(a).unwrap().text = "Quarterly Roadmap".into();
        assert_eq!(c.find("road"), vec![a]);
        assert!(c.find("  ").is_empty());
        assert!(c.plain_text().contains("Quarterly"));
    }

    #[test]
    fn z_order_commands() {
        let mut c = Canvas::new();
        let a = c.add_node(Kind::Note, pos2(0.0, 0.0));
        let b = c.add_node(Kind::Note, pos2(10.0, 0.0));
        let d = c.add_node(Kind::Note, pos2(20.0, 0.0));
        c.bring_to_front(&set(&[a]));
        assert_eq!(c.nodes.last().unwrap().id, a);
        c.send_to_back(&set(&[d]));
        assert_eq!(c.nodes[0].id, d);
        let g = c.group_selection(&set(&[a, b])).unwrap();
        assert_eq!(c.nodes[0].id, g);
        assert!(c.node(g).unwrap().rect().contains_rect(c.node(a).unwrap().rect()));
    }
}
