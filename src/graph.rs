//! Graph view — an Obsidian-style map of the whole workspace.
//!
//! Nodes are pages, databases and database rows; edges are page links, `[[wiki links]]` typed in any text, database relations and
//! (optionally) the page hierarchy. A force-directed layout (repulsion + springs + gravity, cooled with an alpha like d3-force)
//! arranges them; it wakes up when you drag a node and sleeps when settled so an idle graph costs nothing.
//!
//! * wheel / pinch zoom around the pointer, drag the background to pan (a swipe pans on touch), drag a node to move it
//! * hover lights up a node's neighbourhood, click selects (info card with links and backlinks), double-click opens the page
//! * search highlights matches, **Local** shows only the neighbourhood of the open page up to a chosen depth
//! * toggles for hierarchy edges, database rows and orphan pages; *Fit* frames everything

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};

use egui::{pos2, vec2, Align2, Color32, Context, CursorIcon, Id, Pos2, Rect, Sense, Stroke, TextEdit, Ui, Vec2};

use crate::model::{BlockKind, PropKind, Val, Workspace};
use crate::theme::{bold, body, tag_color, Palette};
use crate::widgets::{text_fit, with_alpha};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Doc,
    Db,
    Row,
}

struct Node {
    id: u64,
    title: String,
    kind: Kind,
    deg: usize,
}

#[derive(Clone, Copy)]
struct Edge {
    a: usize,
    b: usize,
    hier: bool,
}

/// What the view asks the app to do.
#[derive(Default)]
pub struct Out {
    pub open: Option<u64>,
    pub close: bool,
}

pub struct Graph {
    sig: u64,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<u64, usize>,
    adj: Vec<Vec<usize>>,
    pos: Vec<Vec2>,
    vel: Vec<Vec2>,
    alpha: f32,
    cam: Vec2,
    zoom: f32,
    selected: Option<usize>,
    drag: Option<usize>,
    query: String,
    show_hier: bool,
    show_rows: bool,
    show_orphans: bool,
    local: bool,
    depth: usize,
    fit_req: u32,
}

impl Default for Graph {
    fn default() -> Self {
        Graph {
            sig: 0,
            nodes: vec![],
            edges: vec![],
            index: HashMap::new(),
            adj: vec![],
            pos: vec![],
            vel: vec![],
            alpha: 1.0,
            cam: Vec2::ZERO,
            zoom: 1.0,
            selected: None,
            drag: None,
            query: String::new(),
            show_hier: true,
            show_rows: false,
            show_orphans: true,
            local: false,
            depth: 2,
            fit_req: 2,
        }
    }
}

thread_local! {
    static G: RefCell<Graph> = RefCell::new(Graph::default());
}

fn hash_id(id: u64) -> u64 {
    let mut x = id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// `[[Title]]` mentions in a text.
pub fn wiki_links(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(a) = rest.find("[[") {
        let tail = &rest[a + 2..];
        match tail.find("]]") {
            Some(b) => {
                let t = tail[..b].split('|').next().unwrap_or("").trim();
                if !t.is_empty() {
                    out.push(t.to_owned());
                }
                rest = &tail[b + 2..];
            }
            None => break,
        }
    }
    out
}

impl Graph {
    fn signature(&self, ws: &Workspace) -> u64 {
        let mut h: u64 = 1469598103934665603;
        let mut mix = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(1099511628211);
        };
        for p in &ws.pages {
            mix(p.id);
            mix(p.updated.to_bits());
            mix(p.blocks.len() as u64);
            mix(p.trashed as u64);
            mix(p.parent.unwrap_or(0));
        }
        mix(self.show_hier as u64 | (self.show_rows as u64) << 1);
        h
    }

    fn rebuild(&mut self, ws: &Workspace) {
        let old: HashMap<u64, (Vec2, Vec2)> = self.nodes.iter().enumerate().map(|(i, n)| (n.id, (self.pos[i], self.vel[i]))).collect();
        let selected_id = self.selected.map(|i| self.nodes[i].id);
        self.nodes.clear();
        self.edges.clear();
        self.index.clear();
        for p in ws.pages.iter().filter(|p| !p.trashed && (self.show_rows || !p.row)) {
            let kind = if p.row {
                Kind::Row
            } else if p.db.is_some() {
                Kind::Db
            } else {
                Kind::Doc
            };
            self.index.insert(p.id, self.nodes.len());
            self.nodes.push(Node { id: p.id, title: if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }, kind, deg: 0 });
        }
        let mut by_title: HashMap<String, usize> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            by_title.entry(n.title.to_lowercase()).or_insert(i);
        }
        let mut seen: HashSet<(usize, usize, bool)> = HashSet::new();
        let mut add = |edges: &mut Vec<Edge>, a: usize, b: usize, hier: bool| {
            if a != b && seen.insert((a.min(b), a.max(b), hier)) {
                edges.push(Edge { a, b, hier });
            }
        };
        for p in ws.pages.iter().filter(|p| !p.trashed) {
            let Some(&a) = self.index.get(&p.id) else { continue };
            if self.show_hier {
                if let Some(&par) = p.parent.and_then(|pa| self.index.get(&pa)) {
                    add(&mut self.edges, par, a, true);
                }
            }
            for b in &p.blocks {
                if b.kind == BlockKind::PageLink {
                    if let Some(&t) = b.link.and_then(|l| self.index.get(&l)) {
                        add(&mut self.edges, a, t, false);
                    }
                }
                if b.text.contains("[[") {
                    for t in wiki_links(&b.text) {
                        if let Some(&to) = by_title.get(&t.to_lowercase()) {
                            add(&mut self.edges, a, to, false);
                        }
                    }
                }
            }
            if p.row {
                if let Some(db) = p.parent.and_then(|pa| ws.db(pa)) {
                    for (prop, v) in &p.props {
                        if let (PropKind::Relation, Val::Multi(ids)) = (db.kind_of(*prop), v) {
                            for t in ids {
                                if let Some(&to) = self.index.get(t) {
                                    add(&mut self.edges, a, to, false);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.adj = vec![vec![]; self.nodes.len()];
        for e in &self.edges {
            self.adj[e.a].push(e.b);
            self.adj[e.b].push(e.a);
        }
        for (i, n) in self.nodes.iter_mut().enumerate() {
            n.deg = self.adj[i].len();
        }
        // keep known positions; new nodes appear next to a neighbour (or on a ring)
        let n = self.nodes.len();
        self.pos = vec![Vec2::ZERO; n];
        self.vel = vec![Vec2::ZERO; n];
        let mut fresh = 0;
        for i in 0..n {
            if let Some((p, v)) = old.get(&self.nodes[i].id) {
                self.pos[i] = *p;
                self.vel[i] = *v;
            } else {
                fresh += 1;
                let h = hash_id(self.nodes[i].id);
                let ang = (h % 6283) as f32 / 1000.0;
                let rad = 60.0 + (h >> 20) as f32 % 160.0;
                self.pos[i] = vec2(ang.cos(), ang.sin()) * rad;
            }
        }
        for i in 0..n {
            if !old.contains_key(&self.nodes[i].id) {
                if let Some(&nb) = self.adj[i].iter().find(|&&j| old.contains_key(&self.nodes[j].id)) {
                    let h = hash_id(self.nodes[i].id ^ 77);
                    self.pos[i] = self.pos[nb] + vec2(((h % 100) as f32 - 50.0) * 0.6, (((h >> 8) % 100) as f32 - 50.0) * 0.6);
                }
            }
        }
        self.selected = selected_id.and_then(|id| self.index.get(&id).copied());
        self.drag = None;
        if fresh > 0 {
            self.alpha = self.alpha.max(if old.is_empty() { 1.0 } else { 0.5 });
            if old.is_empty() {
                self.fit_req = 90;
            }
        }
    }

    /// Nodes that are drawn: Local mode keeps the neighbourhood of `centre`; orphans can be hidden.
    fn visible(&self, centre: Option<usize>) -> Vec<bool> {
        let n = self.nodes.len();
        let mut vis = vec![true; n];
        if self.local {
            if let Some(c) = centre {
                vis = vec![false; n];
                let mut q = VecDeque::new();
                vis[c] = true;
                q.push_back((c, 0usize));
                while let Some((u, d)) = q.pop_front() {
                    if d >= self.depth {
                        continue;
                    }
                    for &v in &self.adj[u] {
                        if !vis[v] {
                            vis[v] = true;
                            q.push_back((v, d + 1));
                        }
                    }
                }
            }
        }
        if !self.show_orphans {
            for i in 0..n {
                if self.nodes[i].deg == 0 {
                    vis[i] = false;
                }
            }
        }
        vis
    }

    /// One layout iteration over the visible nodes.
    fn step(&mut self, vis: &[bool]) {
        let n = self.nodes.len();
        let ids: Vec<usize> = (0..n).filter(|&i| vis[i]).collect();
        let mut force = vec![Vec2::ZERO; n];
        let cap = 420.0_f32;
        for (k, &i) in ids.iter().enumerate() {
            for &j in &ids[k + 1..] {
                let d = self.pos[i] - self.pos[j];
                let d2 = d.length_sq();
                if d2 > cap * cap {
                    continue;
                }
                let d2 = d2.max(36.0);
                let dist = d2.sqrt();
                let f = 2400.0 / d2;
                let dir = if dist > 0.0 { d / dist } else { vec2(1.0, 0.0) };
                force[i] += dir * f;
                force[j] -= dir * f;
            }
        }
        for e in &self.edges {
            if !(vis[e.a] && vis[e.b]) {
                continue;
            }
            let d = self.pos[e.b] - self.pos[e.a];
            let len = d.length().max(0.01);
            let rest = if e.hier { 52.0 } else { 78.0 };
            let f = (len - rest) * if e.hier { 0.030 } else { 0.022 };
            let dir = d / len;
            force[e.a] += dir * f;
            force[e.b] -= dir * f;
        }
        let k = self.alpha.clamp(0.02, 1.0);
        for &i in &ids {
            if self.drag == Some(i) {
                self.vel[i] = Vec2::ZERO;
                continue;
            }
            let f = force[i] - self.pos[i] * 0.0045;
            self.vel[i] = (self.vel[i] + f * 0.9) * 0.80;
            let v = self.vel[i].clamp(vec2(-40.0, -40.0), vec2(40.0, 40.0));
            self.pos[i] += v * k;
        }
        self.alpha *= 0.992;
    }

    fn fit(&mut self, vis: &[bool], area: Rect) {
        let mut b: Option<Rect> = None;
        for (i, p) in self.pos.iter().enumerate() {
            if vis[i] {
                let r = Rect::from_center_size(pos2(p.x, p.y), vec2(1.0, 1.0));
                b = Some(b.map_or(r, |x| x.union(r)));
            }
        }
        if let Some(b) = b {
            let pad = 70.0;
            let zx = (area.width() - pad) / b.width().max(80.0);
            let zy = (area.height() - pad) / b.height().max(80.0);
            self.zoom = zx.min(zy).clamp(0.12, 2.2);
            self.cam = vec2(b.center().x, b.center().y);
        }
    }
}

fn node_radius(n: &Node) -> f32 {
    4.5 + (n.deg as f32).sqrt() * 2.3
}

fn node_color(n: &Node, pal: &Palette) -> Color32 {
    if n.deg == 0 {
        return pal.muted;
    }
    match n.kind {
        Kind::Doc => pal.accent,
        Kind::Db => tag_color(3),
        Kind::Row => tag_color(2),
    }
}

fn chip(ui: &mut Ui, pal: &Palette, text: &str, on: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body(13.0), if on { Color32::WHITE } else { pal.text });
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 20.0, 28.0), Sense::click());
    let bg = if on { pal.accent } else if resp.hovered() { pal.active } else { pal.hover };
    ui.painter().rect_filled(rect, 14.0, bg);
    ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, pal.text);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Draws the graph view in the central area.
pub fn show(ui: &mut Ui, ws: &Workspace, pal: &Palette) -> Out {
    G.with(|g| show_inner(ui, ws, pal, &mut g.borrow_mut()))
}

fn show_inner(ui: &mut Ui, ws: &Workspace, pal: &Palette, g: &mut Graph) -> Out {
    let ctx: Context = ui.ctx().clone();
    let mut out = Out::default();
    let sig = g.signature(ws);
    if sig != g.sig {
        g.sig = sig;
        g.rebuild(ws);
    }
    let narrow = ui.available_width() < 640.0;

    // ── controls ──
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Graph view").font(bold(18.0)));
        ui.label(egui::RichText::new(format!("{} notes · {} links", g.nodes.len(), g.edges.len())).font(body(12.5)).color(pal.muted));
        let w = if narrow { (ui.available_width() - 150.0).max(110.0) } else { 220.0 };
        let r = ui.add(TextEdit::singleline(&mut g.query).hint_text("Search nodes…").desired_width(w).font(body(14.0)));
        if r.changed() {
            g.alpha = g.alpha.max(0.05);
        }
        if chip(ui, pal, "Fit", false).clicked() {
            g.fit_req = 1;
        }
        if chip(ui, pal, "Close", false).clicked() {
            out.close = true;
        }
    });
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(8.0);
        let mut rebuild = false;
        if chip(ui, pal, "Hierarchy", g.show_hier).clicked() {
            g.show_hier = !g.show_hier;
            rebuild = true;
        }
        if chip(ui, pal, "Database rows", g.show_rows).clicked() {
            g.show_rows = !g.show_rows;
            rebuild = true;
        }
        if chip(ui, pal, "Orphans", g.show_orphans).clicked() {
            g.show_orphans = !g.show_orphans;
            g.alpha = g.alpha.max(0.4);
            g.fit_req = 1;
        }
        if chip(ui, pal, "Local", g.local).clicked() {
            g.local = !g.local;
            g.alpha = g.alpha.max(0.5);
            g.fit_req = 30;
        }
        if g.local {
            if chip(ui, pal, "−", false).clicked() && g.depth > 1 {
                g.depth -= 1;
                g.fit_req = 20;
            }
            ui.label(egui::RichText::new(format!("depth {}", g.depth)).font(body(12.5)).color(pal.muted));
            if chip(ui, pal, "+", false).clicked() && g.depth < 5 {
                g.depth += 1;
                g.fit_req = 20;
                g.alpha = g.alpha.max(0.5);
            }
        }
        if rebuild {
            g.sig = 0;
        }
    });
    ui.add_space(4.0);

    // ── canvas ──
    let canvas = ui.available_rect_before_wrap();
    let (rect, resp) = ui.allocate_exact_size(canvas.size(), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, pal.bg);
    let centre = ws.current.and_then(|c| g.index.get(&c).copied());
    let vis = g.visible(centre.or(g.selected));
    let to_screen = |g: &Graph, w: Vec2| rect.center() + (w - g.cam) * g.zoom;
    let to_world = |g: &Graph, s: Pos2| g.cam + (s - rect.center()) / g.zoom;
    let pointer = resp.hover_pos();

    if g.fit_req > 0 {
        g.fit(&vis, rect);
        g.fit_req -= 1;
    }

    // wheel / pinch / touch pan
    if resp.hovered() || resp.dragged() {
        let (scroll, zd) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        let at = pointer.unwrap_or(rect.center());
        let before = to_world(g, at);
        let mut changed = false;
        if crate::touch::direct(&ctx) {
            if scroll != Vec2::ZERO {
                g.cam -= scroll / g.zoom;
                changed = true;
            }
        } else if scroll.y != 0.0 {
            g.zoom = (g.zoom * (scroll.y * 0.0016).exp()).clamp(0.08, 6.0);
            changed = true;
        }
        if (zd - 1.0).abs() > 1e-4 {
            g.zoom = (g.zoom * zd).clamp(0.08, 6.0);
            changed = true;
        }
        if changed && !crate::touch::direct(&ctx) {
            g.cam = before - (at - rect.center()) / g.zoom;
        }
        if changed {
            g.fit_req = 0;
        }
    }

    // hover / drag / click
    let hit = |g: &Graph, pos: Pos2| -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for i in 0..g.nodes.len() {
            if !vis[i] {
                continue;
            }
            let c = to_screen(g, g.pos[i]);
            let r = (node_radius(&g.nodes[i]) * g.zoom.sqrt()).max(4.0) + 6.0;
            let d = (c - pos).length();
            if d <= r && best.map_or(true, |b| d < b.0) {
                best = Some((d, i));
            }
        }
        best.map(|b| b.1)
    };
    let hover = pointer.and_then(|pt| hit(g, pt));
    if resp.drag_started() {
        let o = ctx.input(|i| i.pointer.press_origin());
        g.drag = o.and_then(|o| hit(g, o));
        if g.drag.is_some() {
            g.alpha = g.alpha.max(0.45);
            g.fit_req = 0;
        }
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        match g.drag {
            Some(i) => {
                g.pos[i] += d / g.zoom;
                g.alpha = g.alpha.max(0.3);
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
            None => {
                g.cam -= d / g.zoom;
                g.fit_req = 0;
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
        }
    }
    if resp.drag_stopped() {
        g.drag = None;
    }
    if hover.is_some() && !resp.dragged() {
        ctx.set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        g.selected = hover;
    }
    if resp.double_clicked() {
        if let Some(i) = hover {
            out.open = Some(g.nodes[i].id);
        }
    }

    // physics
    if g.alpha > 0.021 || g.drag.is_some() {
        for _ in 0..2 {
            g.step(&vis);
        }
        ctx.request_repaint();
    }

    // ── paint ──
    let focus = hover.or(g.selected);
    let nb: HashSet<usize> = focus.map(|f| g.adj[f].iter().copied().chain(std::iter::once(f)).collect()).unwrap_or_default();
    let q = g.query.trim().to_lowercase();
    let matches = |n: &Node| q.is_empty() || n.title.to_lowercase().contains(&q);
    let view = rect.expand(40.0);
    for e in &g.edges {
        if !(vis[e.a] && vis[e.b]) {
            continue;
        }
        let (a, b) = (to_screen(g, g.pos[e.a]), to_screen(g, g.pos[e.b]));
        if !view.contains(a) && !view.contains(b) {
            continue;
        }
        let lit = focus.map_or(false, |f| e.a == f || e.b == f);
        let (col, w) = if lit {
            (pal.accent, 2.0)
        } else if focus.is_some() {
            (with_alpha(pal.muted, 0.10), 1.0)
        } else if e.hier {
            (with_alpha(pal.muted, 0.28), 1.0)
        } else {
            (with_alpha(pal.accent, 0.55), 1.3)
        };
        p.line_segment([a, b], Stroke::new(w, col));
    }
    let mut labels: Vec<(f32, usize)> = vec![];
    for i in 0..g.nodes.len() {
        if !vis[i] {
            continue;
        }
        let n = &g.nodes[i];
        let c = to_screen(g, g.pos[i]);
        if !view.contains(c) {
            continue;
        }
        let r = node_radius(n) * g.zoom.sqrt().clamp(0.6, 2.0);
        let dim = (focus.is_some() && !nb.contains(&i)) || !matches(n);
        let mut col = node_color(n, pal);
        if dim {
            col = with_alpha(col, 0.22);
        }
        p.circle_filled(c, r, col);
        if g.selected == Some(i) {
            p.circle_stroke(c, r + 4.0, Stroke::new(2.0_f32, pal.accent));
        }
        if hover == Some(i) {
            p.circle_stroke(c, r + 3.0, Stroke::new(1.5_f32, pal.text));
        }
        if !q.is_empty() && matches(n) {
            p.circle_stroke(c, r + 6.0, Stroke::new(1.5_f32, tag_color(6)));
        }
        let show = hover == Some(i) || g.selected == Some(i) || (!q.is_empty() && matches(n)) || (focus.is_some() && nb.contains(&i)) || g.zoom > 1.1 || (g.zoom > 0.55 && n.deg >= 3) || (g.zoom > 0.35 && n.deg >= 7);
        if show {
            labels.push((r, i));
        }
    }
    for (r, i) in labels {
        let n = &g.nodes[i];
        let c = to_screen(g, g.pos[i]);
        let dim = (focus.is_some() && !nb.contains(&i)) || !matches(n);
        let col = if dim { with_alpha(pal.text, 0.3) } else { pal.text };
        let size = (12.0 * g.zoom.sqrt()).clamp(10.5, 15.0);
        text_fit(&p, pos2(c.x, c.y + r + 4.0), Align2::CENTER_TOP, &n.title, body(size), col, 190.0);
    }
    if g.nodes.is_empty() || vis.iter().all(|v| !v) {
        p.text(rect.center(), Align2::CENTER_CENTER, "Nothing to show yet — link pages together (page-link blocks or [[Title]]) and they appear here.", body(14.0), pal.muted);
    }

    // legend
    let ly = rect.bottom() - 14.0;
    let mut lx = rect.right() - 12.0;
    for (name, col) in [("Orphan", pal.muted), ("Row", tag_color(2)), ("Database", tag_color(3)), ("Page", pal.accent)] {
        let gl = p.layout_no_wrap(name.to_owned(), body(11.5), pal.muted);
        lx -= gl.size().x;
        p.galley(pos2(lx, ly - gl.size().y / 2.0), gl, pal.muted);
        lx -= 12.0;
        p.circle_filled(pos2(lx, ly), 4.0, col);
        lx -= 14.0;
    }

    // ── info card ──
    if let Some(si) = g.selected.filter(|i| *i < g.nodes.len()) {
        let w = (rect.width() - 24.0).min(330.0);
        let neigh: Vec<usize> = g.adj[si].iter().copied().take(5).collect();
        let h = 96.0 + neigh.len() as f32 * 24.0;
        let card = Rect::from_min_size(pos2(rect.left() + 12.0, rect.bottom() - h - 34.0), vec2(w, h));
        p.rect_filled(card.translate(vec2(0.0, 3.0)), 12.0, Color32::from_black_alpha(40));
        p.rect_filled(card, 12.0, pal.popup);
        p.rect_stroke(card, 12.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
        let n = &g.nodes[si];
        text_fit(&p, pos2(card.left() + 14.0, card.top() + 20.0), Align2::LEFT_CENTER, &n.title, bold(15.0), pal.text, w - 28.0);
        let backs = ws.backlinks(n.id).len();
        p.text(pos2(card.left() + 14.0, card.top() + 42.0), Align2::LEFT_CENTER, format!("{} connections · {} backlinks", n.deg, backs), body(12.5), pal.muted);
        let open = Rect::from_min_size(pos2(card.right() - 92.0, card.top() + 10.0), vec2(80.0, 26.0));
        let oresp = ui.interact(open, Id::new("graph_open"), Sense::click());
        p.rect_filled(open, 13.0, if oresp.hovered() { with_alpha(pal.accent, 0.85) } else { pal.accent });
        p.text(open.center(), Align2::CENTER_CENTER, "Open", bold(12.5), Color32::WHITE);
        if oresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            out.open = Some(n.id);
        }
        let mut go: Option<usize> = None;
        for (k, &j) in neigh.iter().enumerate() {
            let row = Rect::from_min_size(pos2(card.left() + 8.0, card.top() + 58.0 + k as f32 * 24.0), vec2(w - 16.0, 24.0));
            let r = ui.interact(row, Id::new(("graph_nb", j)), Sense::click());
            if r.hovered() {
                p.rect_filled(row, 6.0, pal.hover);
            }
            p.circle_filled(pos2(row.left() + 10.0, row.center().y), 4.0, node_color(&g.nodes[j], pal));
            text_fit(&p, pos2(row.left() + 24.0, row.center().y), Align2::LEFT_CENTER, &g.nodes[j].title, body(13.0), pal.text, row.width() - 34.0);
            if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                go = Some(j);
            }
        }
        if g.adj[si].len() > neigh.len() {
            p.text(pos2(card.left() + 14.0, card.bottom() - 12.0), Align2::LEFT_CENTER, format!("+ {} more", g.adj[si].len() - neigh.len()), body(12.0), pal.muted);
        }
        if let Some(j) = go {
            g.selected = Some(j);
            g.cam = vec2(g.pos[j].x, g.pos[j].y);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_links_are_found() {
        assert_eq!(wiki_links("see [[Alpha]] and [[Beta|the second]], [[ ]] [[open"), vec!["Alpha".to_string(), "Beta".to_string()]);
        assert!(wiki_links("no links").is_empty());
    }

    #[test]
    fn layout_separates_nodes_and_pulls_links_together() {
        let mut g = Graph::default();
        for i in 0..6u64 {
            g.nodes.push(Node { id: i + 1, title: format!("n{i}"), kind: Kind::Doc, deg: 0 });
            g.pos.push(vec2(i as f32, 0.5 * i as f32));
            g.vel.push(Vec2::ZERO);
        }
        g.edges = vec![Edge { a: 0, b: 1, hier: false }, Edge { a: 1, b: 2, hier: false }];
        g.adj = vec![vec![1], vec![0, 2], vec![1], vec![], vec![], vec![]];
        let vis = vec![true; 6];
        for _ in 0..400 {
            g.step(&vis);
        }
        let d01 = (g.pos[0] - g.pos[1]).length();
        let d03 = (g.pos[0] - g.pos[3]).length();
        assert!(d01 > 20.0 && d01 < 160.0, "linked nodes settle at a sensible distance: {d01}");
        assert!(d03 > d01 * 0.5, "unlinked nodes are pushed apart: {d03}");
        assert!(g.pos.iter().all(|p| p.x.is_finite() && p.length() < 2000.0));
        assert!(g.alpha < 0.1, "the layout cools down and sleeps");
    }

    #[test]
    fn local_mode_limits_depth() {
        let mut g = Graph::default();
        for i in 0..5u64 {
            g.nodes.push(Node { id: i, title: String::new(), kind: Kind::Doc, deg: 1 });
        }
        g.adj = vec![vec![1], vec![0, 2], vec![1, 3], vec![2, 4], vec![3]];
        g.local = true;
        g.depth = 2;
        let v = g.visible(Some(0));
        assert_eq!(v, vec![true, true, true, false, false]);
    }
}
