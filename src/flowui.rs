//! Flow editor — drawing and gestures for `flow.rs` (node-based data-flow graphs).
//!
//! Mobile first: everything that works with a mouse also works with fingers — drag the board to pan, pinch to zoom, **tap an output port and then an
//! input port** to wire them (or drag), tap a wire to select it, the inspector becomes a bottom sheet on phones, the node palette a full-width sheet.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use egui::epaint::CubicBezierShape;
use egui::{pos2, vec2, Align2, Color32, Context, CornerRadius, Event, Id, Key, Margin, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2};

use crate::automation::Output;
use crate::flow::{self, Cat, FNode, Flow, Json, Run, St as RunSt, Wire, DEFS, HEAD_H, PK, ROW_H};
use crate::icons;
use crate::lucide as l;
use crate::model::Workspace;
use crate::theme::{body, bold, Palette};
use crate::touch;
use crate::widgets::{text_fit, with_alpha, HOST_ORDER};

const MIN_Z: f32 = 0.2;
const MAX_Z: f32 = 2.5;
const HIST_MAX: usize = 60;

#[derive(Default)]
pub struct Out {
    pub toast: Option<String>,
    /// Notifications / changes produced by a live run — the app turns them into toasts and OS notifications.
    pub output: Option<Output>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pend {
    /// Started at an output port (node, port).
    From(u32, usize),
    /// Started at an input port (node, port).
    To(u32, usize),
}

struct Picker {
    pos: Pos2,
    pend: Option<Pend>,
    query: String,
    focus: bool,
}

#[derive(Default)]
enum Drag {
    #[default]
    None,
    Wire(Pend),
    Box(Pos2),
}

type Snap = (Vec<FNode>, Vec<Wire>);

#[derive(Default)]
struct St {
    page: u64,
    sel: BTreeSet<u32>,
    sel_wire: Option<Wire>,
    drag: Drag,
    tap: Option<Pend>,
    picker: Option<Picker>,
    run: Option<Run>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    edit_key: Option<(u32, String)>,
    menu: bool,
    report: bool,
    paste: String,
    var_name: String,
    var_value: String,
    want_run: Option<(Option<u32>, bool)>,
    last_auto: f64,
    inited: bool,
    toast: Option<String>,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

fn snap(f: &Flow) -> Snap {
    (f.nodes.clone(), f.wires.clone())
}

fn push_undo(st: &mut St, f: &Flow) {
    st.undo.push(snap(f));
    if st.undo.len() > HIST_MAX {
        st.undo.remove(0);
    }
    st.redo.clear();
}

// ───────────────────────────── public API ─────────────────────────────

pub fn show(ui: &mut Ui, ws: &mut Workspace, page: u64, pal: &Palette) -> Out {
    let mut out = Out::default();
    let Some(mut f) = ws.page_mut(page).map(|p| p.flow.take().unwrap_or_default()) else { return out };
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.page != page || !st.inited {
        st = St { page, inited: true, ..Default::default() };
        if touch::compact() && f.nodes.is_empty() {
            f.zoom = 0.85;
        }
    }
    let before = (f.nodes.len(), f.wires.len(), f.every, f.vars.len());
    let dirty = run_ui(ui, ws, page, pal, &mut f, &mut st, &mut out);
    if dirty || before != (f.nodes.len(), f.wires.len(), f.every, f.vars.len()) {
        ws.touch(page);
    }
    ST.with(|s| *s.borrow_mut() = st);
    if let Some(p) = ws.page_mut(page) {
        p.flow = Some(f);
    }
    out
}

pub fn can_undo_redo(ws: &Workspace, page: u64) -> (bool, bool) {
    let _ = ws;
    ST.with(|s| {
        let s = s.borrow();
        if s.page == page {
            (!s.undo.is_empty(), !s.redo.is_empty())
        } else {
            (false, false)
        }
    })
}

pub fn undo_redo(ws: &mut Workspace, page: u64, redo: bool) -> bool {
    let Some(f) = ws.page_mut(page).and_then(|p| p.flow.as_mut()) else { return false };
    ST.with(|s| {
        let mut s = s.borrow_mut();
        if s.page != page {
            return false;
        }
        let cur = snap(f);
        let prev = if redo { s.redo.pop() } else { s.undo.pop() };
        let Some((nodes, wires)) = prev else { return false };
        if redo {
            s.undo.push(cur);
        } else {
            s.redo.push(cur);
        }
        f.nodes = nodes;
        f.wires = wires;
        s.sel.retain(|id| f.nodes.iter().any(|n| n.id == *id));
        s.sel_wire = None;
        true
    })
    .then(|| ws.touch(page))
    .is_some()
}

/// Repairs damaged flows once per session.
pub fn fix_once(ws: &mut Workspace) {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !DONE.with(|d| d.replace(true)) {
        ws.fix_flows();
    }
}

// ───────────────────────────── geometry ─────────────────────────────

struct View {
    origin: Pos2,
    pan: Vec2,
    z: f32,
}

impl View {
    fn to_screen(&self, x: f32, y: f32) -> Pos2 {
        self.origin + self.pan + vec2(x, y) * self.z
    }
    fn to_world(&self, p: Pos2) -> Pos2 {
        let v = (p - self.origin - self.pan) / self.z;
        pos2(v.x, v.y)
    }
}

fn bez_pts(a: Pos2, b: Pos2, z: f32) -> [Pos2; 4] {
    let dx = ((b.x - a.x).abs() * 0.5).max(60.0 * z);
    [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b]
}

fn bez(p: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    pos2(a * p[0].x + b * p[1].x + c * p[2].x + d * p[3].x, a * p[0].y + b * p[1].y + c * p[2].y + d * p[3].y)
}

fn dist_to_wire(p: &[Pos2; 4], pt: Pos2) -> f32 {
    let mut best = f32::MAX;
    for i in 0..=24 {
        best = best.min(bez(p, i as f32 / 24.0).distance(pt));
    }
    best
}

fn cat_color(c: Cat) -> Color32 {
    let (r, g, b) = c.rgb();
    Color32::from_rgb(r, g, b)
}

fn dark(pal: &Palette) -> bool {
    (pal.bg.r() as u32 + pal.bg.g() as u32 + pal.bg.b() as u32) < 384
}

fn port_screen(v: &View, n: &FNode, idx: usize, out: bool) -> Pos2 {
    let (x, y) = n.port_pos(idx, out);
    v.to_screen(x, y)
}

// ───────────────────────────── main ─────────────────────────────

#[allow(clippy::too_many_lines)]
fn run_ui(ui: &mut Ui, ws: &mut Workspace, page: u64, pal: &Palette, f: &mut Flow, st: &mut St, out: &mut Out) -> bool {
    let ctx = ui.ctx().clone();
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let compact = touch::compact() || rect.width() < 640.0;
    let touchy = touch::touchy();
    let mut dirty = false;
    let now = crate::auth::now_ms();
    let shift = ctx.input(|i| i.modifiers.shift);

    // ── pan / zoom (wheel, trackpad, pinch, two-finger pan) ──
    let over_chrome = ctx.is_pointer_over_area();
    let hovered = ui.rect_contains_pointer(rect) && !over_chrome;
    let ptr = ctx.input(|i| i.pointer.hover_pos());
    let (zoom_d, scroll, mt) = ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.multi_touch()));
    if hovered || mt.is_some() {
        if (zoom_d - 1.0).abs() > 1e-4 {
            let nz = (f.zoom * zoom_d).clamp(MIN_Z, MAX_Z);
            let anchor = mt.map(|m| m.start_pos).or(ptr).unwrap_or(rect.center()) - rect.min;
            let pan = vec2(f.pan[0], f.pan[1]);
            let np = anchor - (anchor - pan) * (nz / f.zoom);
            f.pan = [np.x, np.y];
            f.zoom = nz;
        }
        let mut d = Vec2::ZERO;
        if scroll != Vec2::ZERO && mt.is_none() {
            d += scroll;
        }
        if let Some(m) = mt {
            d += m.translation_delta;
        }
        if d != Vec2::ZERO {
            f.pan[0] += d.x;
            f.pan[1] += d.y;
        }
    }
    let v = View { origin: rect.min, pan: vec2(f.pan[0], f.pan[1]), z: f.zoom };
    let p = ui.painter_at(rect);

    // ── background + dot grid ──
    p.rect_filled(rect, 0.0, pal.bg);
    {
        let step = 24.0 * v.z;
        if step > 7.0 {
            let dot = with_alpha(pal.muted, if dark(pal) { 0.28 } else { 0.35 });
            let ox = (v.origin.x + v.pan.x).rem_euclid(step);
            let oy = (v.origin.y + v.pan.y).rem_euclid(step);
            let mut x = rect.left() + ox;
            while x < rect.right() {
                let mut y = rect.top() + oy;
                while y < rect.bottom() {
                    p.circle_filled(pos2(x, y), 1.0, dot);
                    y += step;
                }
                x += step;
            }
        }
    }
    let bg = ui.interact(rect, Id::new(("flow_bg", page)), Sense::click_and_drag());

    // statuses of the last run
    let run = st.run.take();
    let status: HashMap<u32, RunSt> = run.as_ref().map(|r| r.status.clone()).unwrap_or_default();
    let wire_live = |w: &Wire| -> Option<bool> {
        let r = run.as_ref()?;
        match r.status.get(&w.from)? {
            RunSt::Done(_) => Some(r.values.get(&w.from).and_then(|o| o.get(w.fp as usize)).map_or(false, |x| x.is_some())),
            RunSt::Idle | RunSt::Running => None,
            _ => Some(false),
        }
    };

    // ── wires ──
    let connected: HashSet<(u32, usize, bool)> = f.wires.iter().flat_map(|w| [(w.from, w.fp as usize, true), (w.to, w.tp as usize, false)]).collect();
    let mut wire_hit: Option<Wire> = None;
    let mut delete_wire: Option<Wire> = None;
    for w in &f.wires {
        let (Some(a), Some(b)) = (f.node(w.from), f.node(w.to)) else { continue };
        let pa = port_screen(&v, a, w.fp as usize, true);
        let pb = port_screen(&v, b, w.tp as usize, false);
        if !rect.expand(400.0).intersects(Rect::from_two_pos(pa, pb)) {
            continue;
        }
        let pts = bez_pts(pa, pb, v.z);
        let col = cat_color(a.def().map_or(Cat::Output, |d| d.cat));
        let live = wire_live(w);
        let selected = st.sel_wire == Some(*w);
        let (c, width) = match (selected, live) {
            (true, _) => (pal.accent, 3.0),
            (_, Some(true)) => (col, 2.6),
            (_, Some(false)) => (with_alpha(pal.muted, 0.35), 1.6),
            _ => (with_alpha(col, 0.75), 2.0),
        };
        p.add(CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, Stroke::new(width * v.z.clamp(0.7, 1.4), c)));
        // direction marker
        let m = bez(&pts, 0.55);
        let m2 = bez(&pts, 0.58);
        let dir = (m2 - m).normalized();
        let nrm = vec2(-dir.y, dir.x);
        let s = 4.5 * v.z.clamp(0.7, 1.3);
        p.add(egui::Shape::convex_polygon(vec![m + dir * s, m - dir * s + nrm * s * 0.8, m - dir * s - nrm * s * 0.8], c, Stroke::NONE));
        if selected {
            let mid = bez(&pts, 0.5);
            let r = Rect::from_center_size(mid + vec2(0.0, -22.0), Vec2::splat(if touchy { 40.0 } else { 28.0 }));
            let resp = ui.interact(r, Id::new(("flow_wdel", page, w.from, w.fp, w.to, w.tp)), Sense::click());
            p.circle_filled(r.center(), r.width() * 0.5, if resp.hovered() { pal.danger } else { pal.popup });
            p.circle_stroke(r.center(), r.width() * 0.5, Stroke::new(1.0, pal.border));
            icons::glyph(&p, r.center(), 15.0, l::TRASH_2, if resp.hovered() { Color32::WHITE } else { pal.danger });
            if resp.clicked() {
                delete_wire = Some(*w);
            }
        }
        if bg.clicked() {
            if let Some(pt) = bg.interact_pointer_pos() {
                if dist_to_wire(&pts, pt) < if touchy { 16.0 } else { 9.0 } {
                    wire_hit = Some(*w);
                }
            }
        }
    }

    // ── nodes ──
    let hit_r = if touchy { 20.0 } else { 11.0 };
    let mut moves: Vec<Vec2> = vec![];
    let mut front: Option<u32> = None;
    let mut started_move = false;
    let mut new_drag: Option<Pend> = None;
    let mut released: Option<(Pend, Option<Pos2>)> = None;
    let mut connect_req: Option<(Pend, Pend)> = None;
    let pointer_now = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
    let order: Vec<u32> = f.nodes.iter().map(|n| n.id).collect();
    for id in order {
        let Some(n) = f.node(id) else { continue };
        let z = v.z;
        let nr = Rect::from_min_size(v.to_screen(n.x, n.y), vec2(n.width(), n.height()) * z);
        if !nr.expand(30.0).intersects(rect) {
            continue;
        }
        let def = n.def();
        let cat = def.map_or(Cat::Output, |d| d.cat);
        let sel = st.sel.contains(&id);
        let resp = ui.interact(nr, Id::new(("flow_node", page, id)), Sense::click_and_drag());
        if resp.drag_started() {
            if !sel {
                if !shift {
                    st.sel.clear();
                }
                st.sel.insert(id);
            }
            st.sel_wire = None;
            started_move = true;
        }
        if resp.dragged() && matches!(st.drag, Drag::None) && mt.is_none() {
            moves.push(resp.drag_delta() / z);
        }
        if resp.clicked() || resp.drag_started() {
            front = Some(id);
        }
        if resp.clicked() {
            if shift {
                if !st.sel.remove(&id) {
                    st.sel.insert(id);
                }
            } else {
                st.sel.clear();
                st.sel.insert(id);
            }
            st.sel_wire = None;
            st.tap = None;
        }
        // card
        let ns = status.get(&id);
        let err = matches!(ns, Some(RunSt::Error(_)));
        let r = (9.0 * z).max(3.0);
        let nr_c = if n.kind == "note" { 4.0 } else { r };
        p.rect_filled(nr.translate(vec2(0.0, 3.0 * z)).expand(1.0), nr_c + 1.0, Color32::from_black_alpha(if dark(pal) { 80 } else { 26 }));
        let fill = if n.kind == "note" { Color32::from_rgb(255, 243, 176).linear_multiply(if dark(pal) { 0.28 } else { 1.0 }) } else { pal.card };
        p.rect_filled(nr, nr_c, if n.disabled { with_alpha(fill, 0.6) } else { fill });
        let stroke = if err { Stroke::new(2.0, pal.danger) } else if sel { Stroke::new(2.2, pal.accent) } else if matches!(ns, Some(RunSt::Running)) { Stroke::new(2.0, with_alpha(pal.accent, 0.7)) } else { Stroke::new(1.0, pal.border) };
        if n.kind == "note" {
            let text = n.param("text");
            let galley_w = nr.width() - 16.0 * z;
            let fid = body((12.5 * z).max(5.0));
            let g = p.layout(text, fid, if dark(pal) { Color32::from_rgb(240, 230, 190) } else { Color32::from_rgb(90, 74, 20) }, galley_w);
            p.with_clip_rect(nr.shrink(2.0)).galley(nr.min + vec2(8.0, 8.0) * z, g, Color32::BLACK);
            p.rect_stroke(nr, nr_c, stroke, StrokeKind::Inside);
            continue;
        }
        let head = Rect::from_min_size(nr.min, vec2(nr.width(), HEAD_H * z));
        p.rect_filled(head, CornerRadius { nw: nr_c as u8, ne: nr_c as u8, sw: 0, se: 0 }, with_alpha(cat_color(cat), if dark(pal) { 0.28 } else { 0.2 }));
        p.rect_filled(Rect::from_min_size(nr.min + vec2(0.0, 2.0 * z), vec2(3.5 * z, (HEAD_H - 4.0) * z)), 2.0, cat_color(cat));
        p.rect_stroke(nr, nr_c, stroke, StrokeKind::Inside);
        let tcol = if n.disabled { pal.muted } else { pal.text };
        text_fit(&p, pos2(nr.left() + 12.0 * z, head.center().y), Align2::LEFT_CENTER, &n.title(), bold((12.5 * z).max(4.0)), tcol, nr.width() - 40.0 * z);
        // status glyph
        let sc = pos2(nr.right() - 14.0 * z, head.center().y);
        match ns {
            Some(RunSt::Done(_)) => icons::glyph(&p, sc, 14.0 * z, l::CHECK, Color32::from_rgb(46, 160, 110)),
            Some(RunSt::Error(_)) => icons::glyph(&p, sc, 14.0 * z, l::CIRCLE_X, pal.danger),
            Some(RunSt::Running) => icons::glyph(&p, sc, 14.0 * z, l::CLOCK, pal.accent),
            Some(RunSt::Skipped) | Some(RunSt::Blocked) => {
                p.text(sc, Align2::CENTER_CENTER, "–", bold(13.0 * z), pal.muted);
            }
            _ => {}
        }
        // ports
        if let Some(d) = def {
            let lod = z >= 0.5;
            let pr = (5.0 * z).clamp(3.2, 9.0);
            for (i, name) in d.ins.iter().enumerate() {
                let c = port_screen(&v, n, i, false);
                let used = connected.contains(&(id, i, false));
                port_dot(&p, pal, c, pr, cat_color(cat), used, false);
                if lod {
                    p.text(c + vec2(10.0 * z, 0.0), Align2::LEFT_CENTER, name, body(11.0 * z), pal.muted);
                }
                let pr_rect = Rect::from_center_size(c, Vec2::splat(hit_r * 2.0));
                let pr_resp = ui.interact(pr_rect, Id::new(("flow_in", page, id, i)), Sense::click_and_drag());
                if pr_resp.hovered() || matches!(st.tap, Some(Pend::From(..))) && pr_resp.hovered() {
                    p.circle_stroke(c, pr + 3.0, Stroke::new(1.5, pal.accent));
                }
                if pr_resp.drag_started() {
                    new_drag = Some(Pend::To(id, i));
                }
                if pr_resp.drag_stopped() {
                    released = Some((Pend::To(id, i), pointer_now));
                }
                if pr_resp.clicked() {
                    match st.tap {
                        Some(a @ Pend::From(..)) => connect_req = Some((a, Pend::To(id, i))),
                        _ => st.tap = Some(Pend::To(id, i)),
                    }
                }
            }
            for (i, name) in d.outs.iter().enumerate() {
                let c = port_screen(&v, n, i, true);
                let used = connected.contains(&(id, i, true));
                let live = run.as_ref().and_then(|r| r.values.get(&id)).map(|o| o.get(i).map_or(false, |x| x.is_some()));
                port_dot(&p, pal, c, pr, cat_color(cat), used || live == Some(true), live == Some(false));
                if lod {
                    p.text(c - vec2(10.0 * z, 0.0), Align2::RIGHT_CENTER, name, body(11.0 * z), if live == Some(false) { with_alpha(pal.muted, 0.5) } else { pal.muted });
                }
                let pr_rect = Rect::from_center_size(c, Vec2::splat(hit_r * 2.0));
                let pr_resp = ui.interact(pr_rect, Id::new(("flow_out", page, id, i)), Sense::click_and_drag());
                if pr_resp.hovered() {
                    p.circle_stroke(c, pr + 3.0, Stroke::new(1.5, pal.accent));
                }
                if pr_resp.drag_started() {
                    new_drag = Some(Pend::From(id, i));
                }
                if pr_resp.drag_stopped() {
                    released = Some((Pend::From(id, i), pointer_now));
                }
                if pr_resp.clicked() {
                    match st.tap {
                        Some(a @ Pend::To(..)) => connect_req = Some((Pend::From(id, i), a)),
                        _ => st.tap = Some(Pend::From(id, i)),
                    }
                }
            }
            if lod {
                // footer: error / value preview / main parameter
                let fy = nr.top() + (HEAD_H + ROW_H * d.ins.len().max(d.outs.len()).max(1) as f32 + 14.0) * z;
                let fpos = pos2(nr.left() + 12.0 * z, fy);
                let maxw = nr.width() - 24.0 * z;
                match ns {
                    Some(RunSt::Error(m)) => {
                        text_fit(&p, fpos, Align2::LEFT_CENTER, m, body(10.5 * z), pal.danger, maxw);
                    }
                    Some(RunSt::Done(ms)) if run.as_ref().and_then(|r| r.values.get(&id)).and_then(|o| o.iter().flatten().next()).is_some() => {
                        let val = run.as_ref().and_then(|r| r.values.get(&id)).and_then(|o| o.iter().flatten().next()).cloned().unwrap_or(Json::Null);
                        text_fit(&p, fpos, Align2::LEFT_CENTER, &format!("{}  · {}ms", flow::preview(&val, 60), ms), body(10.5 * z), Color32::from_rgb(46, 150, 104), maxw);
                    }
                    Some(RunSt::Skipped) => {
                        text_fit(&p, fpos, Align2::LEFT_CENTER, "skipped (inactive branch)", body(10.5 * z), pal.muted, maxw);
                    }
                    Some(RunSt::Blocked) => {
                        text_fit(&p, fpos, Align2::LEFT_CENTER, "blocked by an earlier error", body(10.5 * z), pal.muted, maxw);
                    }
                    _ => {
                        let s = if n.disabled { "disabled".to_owned() } else { summary(n) };
                        text_fit(&p, fpos, Align2::LEFT_CENTER, &s, body(10.5 * z), with_alpha(pal.muted, 0.9), maxw);
                    }
                }
            }
        }
    }

    // ── apply node interaction results ──
    if started_move {
        push_undo(st, f);
    }
    if !moves.is_empty() {
        let total = moves.iter().fold(Vec2::ZERO, |a, b| a + *b);
        for n in f.nodes.iter_mut().filter(|n| st.sel.contains(&n.id)) {
            n.x += total.x;
            n.y += total.y;
        }
        dirty = true;
    }
    if let Some(id) = front {
        if f.nodes.last().map(|n| n.id) != Some(id) {
            if let Some(i) = f.nodes.iter().position(|n| n.id == id) {
                let n = f.nodes.remove(i);
                f.nodes.push(n);
            }
        }
    }

    // ── wiring gestures ──
    if let Some(d) = new_drag {
        st.drag = Drag::Wire(d);
        st.tap = None;
    }
    if let Drag::Wire(from) = st.drag {
        if let Some(pt) = pointer_now {
            let anchor = match from {
                Pend::From(n, i) => f.node(n).map(|n| (port_screen(&v, n, i, true), true)),
                Pend::To(n, i) => f.node(n).map(|n| (port_screen(&v, n, i, false), false)),
            };
            if let Some((a, is_out)) = anchor {
                let pts = if is_out { bez_pts(a, pt, v.z) } else { bez_pts(pt, a, v.z) };
                p.add(CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, Stroke::new(2.4, pal.accent)));
            }
        }
    }
    if let Some((from, at)) = released {
        st.drag = Drag::None;
        if let Some(pt) = at {
            // nearest compatible port under the pointer
            let mut target: Option<Pend> = None;
            let mut best = hit_r * 1.6;
            for n in &f.nodes {
                let Some(d) = n.def() else { continue };
                match from {
                    Pend::From(a, _) if a != n.id => {
                        for i in 0..d.ins.len() {
                            let dd = port_screen(&v, n, i, false).distance(pt);
                            if dd < best {
                                best = dd;
                                target = Some(Pend::To(n.id, i));
                            }
                        }
                    }
                    Pend::To(a, _) if a != n.id => {
                        for i in 0..d.outs.len() {
                            let dd = port_screen(&v, n, i, true).distance(pt);
                            if dd < best {
                                best = dd;
                                target = Some(Pend::From(n.id, i));
                            }
                        }
                    }
                    _ => {}
                }
            }
            match target {
                Some(t) => connect_req = Some((from, t)),
                None => {
                    let over_node = f.nodes.iter().any(|n| Rect::from_min_size(v.to_screen(n.x, n.y), vec2(n.width(), n.height()) * v.z).contains(pt));
                    if !over_node && rect.contains(pt) && !ctx.is_pointer_over_area() {
                        let w = v.to_world(pt);
                        st.picker = Some(Picker { pos: w, pend: Some(from), query: String::new(), focus: true });
                    }
                }
            }
        }
    }
    if let Some((a, b)) = connect_req {
        let (fo, to) = match (a, b) {
            (Pend::From(n, i), Pend::To(m, j)) => ((n, i), (m, j)),
            _ => ((0, 0), (0, 0)),
        };
        if fo.0 != 0 {
            let snapshot = snap(f);
            match f.connect(fo.0, fo.1, to.0, to.1) {
                Ok(()) => {
                    st.undo.push(snapshot);
                    st.redo.clear();
                    dirty = true;
                }
                Err(e) => out.toast = Some(e.to_owned()),
            }
        }
        st.tap = None;
    }
    if let Some(pend) = st.tap {
        // tell touch users what to do next
        let msg = match pend {
            Pend::From(..) => "Now tap an input dot to connect",
            Pend::To(..) => "Now tap an output dot to connect",
        };
        p.text(pos2(rect.center().x, rect.bottom() - 78.0), Align2::CENTER_CENTER, msg, bold(13.0), pal.accent);
    }
    if let Some(w) = delete_wire {
        push_undo(st, f);
        f.wires.retain(|x| *x != w);
        st.sel_wire = None;
        dirty = true;
    }

    // ── background gestures ──
    if bg.drag_started() && shift {
        if let Some(pt) = bg.interact_pointer_pos() {
            st.drag = Drag::Box(pt);
        }
    }
    if bg.dragged() {
        match st.drag {
            Drag::Box(a) => {
                if let Some(pt) = pointer_now {
                    let b = Rect::from_two_pos(a, pt);
                    p.rect_filled(b, 2.0, with_alpha(pal.accent, 0.1));
                    p.rect_stroke(b, 2.0, Stroke::new(1.0, pal.accent), StrokeKind::Inside);
                }
            }
            Drag::None if mt.is_none() => {
                f.pan[0] += bg.drag_delta().x;
                f.pan[1] += bg.drag_delta().y;
            }
            _ => {}
        }
        if matches!(st.drag, Drag::None) {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    if bg.drag_stopped() {
        if let (Drag::Box(a), Some(pt)) = (&st.drag, pointer_now) {
            let b = Rect::from_two_pos(*a, pt);
            st.sel = f.nodes.iter().filter(|n| Rect::from_min_size(v.to_screen(n.x, n.y), vec2(n.width(), n.height()) * v.z).intersects(b)).map(|n| n.id).collect();
        }
        if matches!(st.drag, Drag::Box(_)) {
            st.drag = Drag::None;
        }
    }
    if bg.clicked() {
        st.sel.clear();
        st.tap = None;
        st.sel_wire = wire_hit;
        st.picker = None;
    }
    if bg.double_clicked() || bg.secondary_clicked() {
        if let Some(pt) = bg.interact_pointer_pos() {
            st.picker = Some(Picker { pos: v.to_world(pt), pend: None, query: String::new(), focus: true });
        }
    }
    if f.nodes.is_empty() && st.picker.is_none() {
        let msg = if touchy { "Tap + to add your first node" } else { "Double-click to add a node — or pick a template with the gear button" };
        p.text(rect.center() - vec2(0.0, 8.0), Align2::CENTER_CENTER, msg, body(15.0), with_alpha(pal.muted, 0.9));
    }

    // ── keyboard / clipboard ──
    if !ctx.wants_keyboard_input() {
        if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
            if let Some(w) = st.sel_wire.take() {
                push_undo(st, f);
                f.wires.retain(|x| *x != w);
                dirty = true;
            } else if !st.sel.is_empty() {
                push_undo(st, f);
                for id in std::mem::take(&mut st.sel) {
                    f.remove_node(id);
                }
                dirty = true;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) && !st.sel.is_empty() {
            push_undo(st, f);
            duplicate(f, st);
            dirty = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            st.sel = f.nodes.iter().map(|n| n.id).collect();
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            st.sel.clear();
            st.tap = None;
            st.picker = None;
            st.menu = false;
        }
        let (copy, paste) = ctx.input(|i| {
            let mut c = false;
            let mut pst = None;
            for e in &i.events {
                match e {
                    Event::Copy | Event::Cut => c = true,
                    Event::Paste(s) => pst = Some(s.clone()),
                    _ => {}
                }
            }
            (c, pst)
        });
        if copy && !st.sel.is_empty() {
            ctx.copy_text(selection_json(f, &st.sel));
            out.toast = Some(format!("Copied {} node{}", st.sel.len(), if st.sel.len() == 1 { "" } else { "s" }));
        }
        if let Some(s) = paste {
            if let Some(other) = Flow::from_json(&s) {
                push_undo(st, f);
                let at = v.to_world(rect.center());
                let (bx, by) = other.bounds().map_or((0.0, 0.0), |b| (b.0, b.1));
                let ids = f.merge_in(&other, at.x - bx - 100.0, at.y - by - 60.0);
                st.sel = ids.into_iter().collect();
                dirty = true;
            }
        }
    }

    // ── run engine ──
    st.run = run;
    if let Some((only, dry)) = st.want_run.take() {
        if let Some(r) = st.run.as_mut() {
            r.cancel();
        }
        let problems = f.problems();
        if let Some(pr) = problems.first() {
            out.toast = Some(pr.clone());
        } else {
            let name = ws.page(page).map(|pg| pg.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Flow".into());
            st.run = Some(Run::start(f, &name, only, dry, ws, now));
            st.report = false;
            st.last_auto = now;
        }
    }
    if f.every > 0 && st.run.as_ref().map_or(true, |r| r.finished) && now - st.last_auto >= f.every as f64 * 1000.0 && f.problems().is_empty() {
        let name = ws.page(page).map(|pg| pg.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Flow".into());
        st.run = Some(Run::start(f, &name, None, false, ws, now));
        st.last_auto = now;
    }
    if f.every > 0 {
        ctx.request_repaint_after(std::time::Duration::from_millis(1000));
    }
    let mut finished_now = false;
    if let Some(r) = st.run.as_mut() {
        if !r.finished {
            r.step(ws, now);
            ctx.request_repaint_after(std::time::Duration::from_millis(if r.waiting() { 40 } else { 0 }));
            if r.finished {
                finished_now = true;
            }
        }
    }
    if finished_now {
        if let Some(r) = st.run.as_mut() {
            let o = std::mem::take(&mut r.output);
            if !r.dry {
                if o.changed || !o.notices.is_empty() {
                    out.output = Some(o);
                }
                if let Some(t) = r.copy.take() {
                    ctx.copy_text(t);
                }
            }
            let errs = r.errors();
            out.toast = Some(if errs > 0 {
                format!("{} finished with {} error{}", if r.dry { "Test run" } else { "Flow" }, errs, if errs == 1 { "" } else { "s" })
            } else {
                format!("{} finished · {} nodes · {} ms", if r.dry { "Test run" } else { "Flow" }, r.count_done(), r.elapsed_ms)
            });
            if !r.results.is_empty() || !r.log.is_empty() || errs > 0 {
                st.report = true;
            }
        }
        dirty = true;
    }

    // ── chrome ──
    top_bar(&ctx, ws, page, pal, f, st, rect, compact);
    inspector(&ctx, pal, f, st, rect, compact, &mut dirty);
    status_bar(&ctx, pal, f, st, rect, compact);
    picker(&ctx, pal, f, st, rect, compact, &mut dirty);
    if st.menu {
        menu(&ctx, ws, page, pal, f, st, rect, compact, out, &mut dirty);
    }
    if let Some(t) = st.toast.take() {
        out.toast = Some(t);
    }
    dirty
}

fn port_dot(p: &egui::Painter, pal: &Palette, c: Pos2, r: f32, col: Color32, filled: bool, dim: bool) {
    let col = if dim { with_alpha(col, 0.35) } else { col };
    if filled {
        p.circle_filled(c, r, col);
        p.circle_stroke(c, r, Stroke::new(1.0, pal.card));
    } else {
        p.circle_filled(c, r, pal.card);
        p.circle_stroke(c, r, Stroke::new(1.6, col));
    }
}

/// The most informative parameter of a node, one line.
fn summary(n: &FNode) -> String {
    let Some(d) = n.def() else { return String::new() };
    for pp in d.params {
        let v = n.param(pp.key);
        if !v.trim().is_empty() && !matches!(pp.kind, PK::Bool) {
            let one = v.lines().next().unwrap_or("").to_owned();
            return match pp.kind {
                PK::Choice(_) => one,
                _ => one,
            };
        }
    }
    d.doc.split('.').next().unwrap_or("").to_owned()
}

fn duplicate(f: &mut Flow, st: &mut St) {
    let sel: Vec<u32> = st.sel.iter().copied().collect();
    let mut sub = Flow::default();
    sub.nodes = f.nodes.iter().filter(|n| sel.contains(&n.id)).cloned().collect();
    sub.wires = f.wires.iter().filter(|w| sel.contains(&w.from) && sel.contains(&w.to)).copied().collect();
    let ids = f.merge_in(&sub, 36.0, 36.0);
    st.sel = ids.into_iter().collect();
}

fn selection_json(f: &Flow, sel: &BTreeSet<u32>) -> String {
    let mut sub = Flow::default();
    sub.nodes = f.nodes.iter().filter(|n| sel.contains(&n.id)).cloned().collect();
    sub.wires = f.wires.iter().filter(|w| sel.contains(&w.from) && sel.contains(&w.to)).copied().collect();
    sub.to_json()
}

fn fit(f: &mut Flow, rect: Rect) {
    let Some((x0, y0, x1, y1)) = f.bounds() else { return };
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let z = ((rect.width() - 80.0) / w).min((rect.height() - 190.0) / h).clamp(MIN_Z, 1.2);
    f.zoom = z;
    f.pan = [(rect.width() - w * z) * 0.5 - x0 * z, (rect.height() - h * z) * 0.5 - y0 * z + 20.0];
}

// ───────────────────────────── chrome ─────────────────────────────

fn frame(pal: &Palette) -> egui::Frame {
    egui::Frame::NONE.fill(pal.popup).stroke(Stroke::new(1.0, pal.border)).corner_radius(12.0).inner_margin(Margin::same(8)).shadow(egui::Shadow { offset: [0, 3], blur: 12, spread: 0, color: Color32::from_black_alpha(if dark(pal) { 90 } else { 30 }) })
}

fn btn(ui: &mut Ui, pal: &Palette, glyph: &str, label: &str, primary: bool, enabled: bool, tip: &str) -> egui::Response {
    let touchy = touch::touchy();
    let h = if touchy { 38.0 } else { 30.0 };
    let font = bold(13.0);
    let tw = if label.is_empty() { 0.0 } else { ui.painter().layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x };
    let w = 14.0 + 16.0 + if label.is_empty() { 0.0 } else { 6.0 + tw } + if label.is_empty() { 0.0 } else { 2.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(w.max(h), h), if enabled { Sense::click() } else { Sense::hover() });
    let hov = resp.hovered() && enabled;
    let bgc = if primary { if hov { pal.accent.linear_multiply(0.88) } else { pal.accent } } else if hov || resp.is_pointer_button_down_on() { pal.hover } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(r, 8.0, bgc);
    let fg = if !enabled { with_alpha(pal.muted, 0.5) } else if primary { Color32::WHITE } else { pal.text };
    if label.is_empty() {
        icons::glyph(ui.painter(), r.center(), 16.0, glyph, fg);
    } else {
        icons::glyph(ui.painter(), pos2(r.left() + 14.0 + 1.0, r.center().y), 15.0, glyph, fg);
        ui.painter().text(pos2(r.left() + 14.0 + 16.0 + 4.0, r.center().y), Align2::LEFT_CENTER, label, font, fg);
    }
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

#[allow(clippy::too_many_arguments)]
fn top_bar(ctx: &Context, ws: &mut Workspace, page: u64, pal: &Palette, f: &mut Flow, st: &mut St, rect: Rect, compact: bool) {
    let m = if compact { 6.0 } else { 14.0 };
    let running = st.run.as_ref().map_or(false, |r| !r.finished);
    egui::Area::new(Id::new(("flow_top", page))).order(HOST_ORDER).fixed_pos(rect.min + vec2(m, m)).show(ctx, |ui| {
        frame(pal).show(ui, |ui| {
            ui.set_max_width(rect.width() - 2.0 * m - 16.0);
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(p) = ws.page_mut(page) {
                    icons::glyph(ui.painter(), ui.cursor().min + vec2(10.0, 15.0), 17.0, l::ZAP, pal.accent);
                    ui.add_space(24.0);
                    let before = p.title.clone();
                    ui.add(TextEdit::singleline(&mut p.title).id(Id::new(("title", page))).frame(false).font(bold(15.0)).text_color(pal.text).hint_text("Untitled flow").desired_width(if compact { 110.0 } else { 170.0 }).margin(Margin::symmetric(0, 6)));
                    if p.title != before {
                        p.updated = crate::auth::now_ms();
                    }
                }
                ui.add_space(4.0);
                if btn(ui, pal, l::PLAY, "Run", true, !running, "Run the flow for real  (changes your workspace)").clicked() {
                    st.want_run = Some((None, false));
                }
                if btn(ui, pal, l::SHIELD_CHECK, if compact { "" } else { "Test" }, false, !running, "Dry run on a copy of your workspace — nothing is saved").clicked() {
                    st.want_run = Some((None, true));
                }
                if btn(ui, pal, l::PLUS, if compact { "" } else { "Add node" }, false, true, "Add a node").clicked() {
                    let c = View { origin: rect.min, pan: vec2(f.pan[0], f.pan[1]), z: f.zoom }.to_world(rect.center() - vec2(0.0, 40.0));
                    let jitter = (f.nodes.len() % 5) as f32 * 18.0;
                    st.picker = Some(Picker { pos: pos2(c.x - 100.0 + jitter, c.y - 40.0 + jitter), pend: None, query: String::new(), focus: true });
                }
                if btn(ui, pal, l::MAXIMIZE, "", false, !f.nodes.is_empty(), "Fit all nodes").clicked() {
                    fit(f, rect);
                }
                if btn(ui, pal, l::SETTINGS, "", st.menu, true, "Templates, variables, schedule, import / export").clicked() {
                    st.menu = !st.menu;
                }
            });
        });
    });
}

fn status_bar(ctx: &Context, pal: &Palette, f: &Flow, st: &mut St, rect: Rect, compact: bool) {
    let _ = rect;
    let Some(r) = st.run.as_ref() else {
        return;
    };
    let total = r.status.len();
    let done = r.status.values().filter(|s| !matches!(s, RunSt::Idle | RunSt::Running)).count();
    let errs = r.errors();
    let msg = if !r.finished {
        format!("{} {done}/{total} nodes…", if r.dry { "Testing" } else { "Running" })
    } else if errs > 0 {
        format!("{} error{} · {} ms", errs, if errs == 1 { "" } else { "s" }, r.elapsed_ms)
    } else {
        format!("{} · {total} nodes · {} ms", if r.dry { "Test passed" } else { "Done" }, r.elapsed_ms)
    };
    let finished = r.finished;
    let mut cancel = false;
    let mut toggle = false;
    let mut close = false;
    let report: Vec<String> = r.log.clone();
    let results: Vec<(String, Json)> = r.results.clone();
    let dry = r.dry;
    let _ = f;
    egui::Area::new(Id::new("flow_status")).order(HOST_ORDER).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -12.0)).show(ctx, |ui| {
        frame(pal).show(ui, |ui| {
            ui.set_max_width(if compact { ctx.screen_rect().width() - 24.0 } else { 520.0 });
            ui.horizontal(|ui| {
                let col = if errs > 0 { pal.danger } else if finished { Color32::from_rgb(46, 160, 110) } else { pal.accent };
                ui.label(egui::RichText::new(msg).color(col).font(bold(13.0)));
                if dry {
                    ui.label(egui::RichText::new("dry run").color(pal.muted).small());
                }
                if !finished && ui.small_button("Cancel").clicked() {
                    cancel = true;
                }
                if finished {
                    let label = if st.report { "Hide report" } else { "Report" };
                    if ui.small_button(label).clicked() {
                        toggle = true;
                    }
                    if ui.small_button("Close").clicked() {
                        close = true;
                    }
                }
            });
            if st.report && finished {
                egui::ScrollArea::vertical().max_height(if compact { 170.0 } else { 240.0 }).id_salt("flow_report").show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for (label, v) in &results {
                        ui.label(egui::RichText::new(if label.is_empty() { "Result" } else { label }).strong());
                        let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
                        ui.add(TextEdit::multiline(&mut s).font(egui::TextStyle::Monospace).desired_rows(1).desired_width(f32::INFINITY).interactive(true));
                    }
                    for line in &report {
                        let c = if line.starts_with('✖') { pal.danger } else { pal.muted };
                        ui.label(egui::RichText::new(line).color(c).small());
                    }
                    if results.is_empty() && report.is_empty() {
                        ui.label(egui::RichText::new("Nothing to report. Add a Result or Inspect node to see values here.").color(pal.muted).small());
                    }
                });
            }
        });
    });
    if cancel {
        if let Some(r) = st.run.as_mut() {
            r.cancel();
        }
    }
    if toggle {
        st.report = !st.report;
    }
    if close {
        st.run = None;
        st.report = false;
    }
}

fn inspector(ctx: &Context, pal: &Palette, f: &mut Flow, st: &mut St, rect: Rect, compact: bool, dirty: &mut bool) {
    if st.sel.len() != 1 || st.picker.is_some() || st.menu {
        return;
    }
    let id = *st.sel.iter().next().unwrap_or(&0);
    let Some(idx) = f.nodes.iter().position(|n| n.id == id) else { return };
    let before = snap(f);
    let node_vals: Vec<Option<Json>> = st.run.as_ref().and_then(|r| r.values.get(&id).cloned()).unwrap_or_default();
    let node_err = match st.run.as_ref().and_then(|r| r.status.get(&id)) {
        Some(RunSt::Error(m)) => Some(m.clone()),
        _ => None,
    };
    let def = f.nodes[idx].def();
    let w = if compact { ctx.screen_rect().width() - 16.0 } else { 310.0 };
    let max_h = if compact { ctx.screen_rect().height() * 0.46 } else { (rect.height() - 150.0).max(240.0) };
    let area = egui::Area::new(Id::new("flow_inspector")).order(HOST_ORDER);
    let area = if compact { area.anchor(Align2::CENTER_BOTTOM, vec2(0.0, -62.0)) } else { area.anchor(Align2::RIGHT_TOP, vec2(-12.0, rect.top() + 66.0)) };
    let mut action: Option<&'static str> = None;
    let mut changed_key: Option<String> = None;
    area.show(ctx, |ui| {
        frame(pal).show(ui, |ui| {
            ui.set_width(w - 16.0);
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            egui::ScrollArea::vertical().max_height(max_h).id_salt("flow_insp_scroll").auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(w - 32.0);
                let n = &mut f.nodes[idx];
                ui.horizontal(|ui| {
                    if let Some(d) = def {
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 5.0, cat_color(d.cat));
                        ui.label(egui::RichText::new(d.cat.label()).small().color(pal.muted));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Delete").clicked() {
                            action = Some("delete");
                        }
                        if ui.small_button("Duplicate").clicked() {
                            action = Some("dup");
                        }
                    });
                });
                if n.kind != "note" {
                    let before = n.label.clone();
                    ui.add(TextEdit::singleline(&mut n.label).hint_text(def.map_or("Title", |d| d.label)).font(bold(15.0)).desired_width(f32::INFINITY));
                    if n.label != before {
                        changed_key = Some("label".into());
                    }
                }
                if let Some(d) = def {
                    ui.label(egui::RichText::new(d.doc).small().color(pal.muted));
                    ui.add_space(2.0);
                    for pp in d.params {
                        let mut val = n.param(pp.key);
                        let old = val.clone();
                        ui.label(egui::RichText::new(pp.label).strong().small());
                        match pp.kind {
                            PK::Text | PK::Num => {
                                ui.add(TextEdit::singleline(&mut val).desired_width(f32::INFINITY).hint_text(pp.hint));
                            }
                            PK::Long => {
                                ui.add(TextEdit::multiline(&mut val).desired_width(f32::INFINITY).desired_rows(3).hint_text(pp.hint));
                            }
                            PK::Code => {
                                ui.add(TextEdit::multiline(&mut val).font(egui::TextStyle::Monospace).code_editor().desired_width(f32::INFINITY).desired_rows(if d.key == "code" { 8 } else { 3 }).hint_text(pp.hint));
                                if d.key == "code" {
                                    if let Err(e) = crate::script::check(&val) {
                                        ui.label(egui::RichText::new(e.to_string()).color(pal.danger).small());
                                    }
                                }
                            }
                            PK::Bool => {
                                let mut b = val == "true";
                                if ui.checkbox(&mut b, "").changed() {
                                    val = if b { "true".into() } else { "false".into() };
                                }
                            }
                            PK::Choice(opts) => {
                                egui::ComboBox::from_id_salt(("flow_choice", id, pp.key)).selected_text(val.clone()).width(ui.available_width() - 8.0).show_ui(ui, |ui| {
                                    for o in opts {
                                        ui.selectable_value(&mut val, (*o).to_owned(), *o);
                                    }
                                });
                            }
                        }
                        if !pp.hint.is_empty() && !matches!(pp.kind, PK::Text | PK::Num | PK::Long | PK::Code) {
                            ui.label(egui::RichText::new(pp.hint).small().color(pal.muted));
                        }
                        if val != old {
                            n.params.insert(pp.key.to_owned(), val);
                            changed_key = Some(pp.key.to_owned());
                        }
                    }
                    if d.key != "note" {
                        ui.horizontal(|ui| {
                            let mut dis = n.disabled;
                            if ui.checkbox(&mut dis, "Disabled (skipped when running)").changed() {
                                n.disabled = dis;
                                changed_key = Some("disabled".into());
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Run up to here").clicked() {
                                action = Some("run_to");
                            }
                            if ui.button("Test up to here").clicked() {
                                action = Some("test_to");
                            }
                        });
                    }
                    if let Some(e) = &node_err {
                        ui.label(egui::RichText::new(e).color(pal.danger));
                    }
                    for (i, o) in d.outs.iter().enumerate() {
                        if let Some(Some(v)) = node_vals.get(i) {
                            ui.label(egui::RichText::new(format!("Output “{o}”")).strong().small());
                            let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
                            if s.len() > 6000 {
                                s.truncate(6000);
                                s.push_str("\n…");
                            }
                            ui.add(TextEdit::multiline(&mut s).font(egui::TextStyle::Monospace).desired_rows(2).desired_width(f32::INFINITY));
                        }
                    }
                }
            });
        });
    });
    if let Some(k) = changed_key {
        // one undo step per edited field, not per keystroke
        let key = (id, k);
        if st.edit_key.as_ref() != Some(&key) {
            st.undo.push(before);
            if st.undo.len() > HIST_MAX {
                st.undo.remove(0);
            }
            st.redo.clear();
            st.edit_key = Some(key);
        }
        *dirty = true;
    } else if ctx.input(|i| i.pointer.any_pressed()) {
        st.edit_key = None;
    }
    match action {
        Some("delete") => {
            push_undo(st, f);
            f.remove_node(id);
            st.sel.clear();
            *dirty = true;
        }
        Some("dup") => {
            push_undo(st, f);
            duplicate(f, st);
            *dirty = true;
        }
        Some("run_to") => st.want_run = Some((Some(id), false)),
        Some("test_to") => st.want_run = Some((Some(id), true)),
        _ => {}
    }
}

fn picker(ctx: &Context, pal: &Palette, f: &mut Flow, st: &mut St, rect: Rect, compact: bool, dirty: &mut bool) {
    let Some(pk) = st.picker.as_mut() else { return };
    let w = if compact { ctx.screen_rect().width() - 16.0 } else { 340.0 };
    let area = egui::Area::new(Id::new("flow_picker")).order(HOST_ORDER);
    let area = if compact { area.anchor(Align2::CENTER_BOTTOM, vec2(0.0, -8.0)) } else { area.anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)) };
    let mut chosen: Option<&'static str> = None;
    let mut close = false;
    let max_h = if compact { ctx.screen_rect().height() * 0.7 } else { (rect.height() - 120.0).clamp(240.0, 520.0) };
    area.show(ctx, |ui| {
        frame(pal).show(ui, |ui| {
            ui.set_width(w - 16.0);
            ui.horizontal(|ui| {
                let r = ui.add(TextEdit::singleline(&mut pk.query).hint_text("Search nodes…").desired_width(w - 80.0).id(Id::new("flow_pick_q")));
                if pk.focus {
                    r.request_focus();
                    pk.focus = false;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if let Some(pend) = pk.pend {
                ui.label(egui::RichText::new(match pend {
                    Pend::From(..) => "The new node will be connected to this output.",
                    Pend::To(..) => "The new node will feed this input.",
                }).small().color(pal.muted));
            }
            let q = pk.query.to_lowercase();
            egui::ScrollArea::vertical().max_height(max_h).id_salt("flow_pick_scroll").show(ui, |ui| {
                for cat in Cat::ALL {
                    let items: Vec<&'static flow::Def> = DEFS.iter().filter(|d| d.cat == cat && (q.is_empty() || d.label.to_lowercase().contains(&q) || d.doc.to_lowercase().contains(&q) || d.key.contains(&q))).collect();
                    if items.is_empty() {
                        continue;
                    }
                    ui.label(egui::RichText::new(cat.label()).small().strong().color(cat_color(cat)));
                    for d in items {
                        let h = if touch::touchy() { 46.0 } else { 38.0 };
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                        if resp.hovered() || resp.is_pointer_button_down_on() {
                            ui.painter().rect_filled(r, 8.0, pal.hover);
                        }
                        ui.painter().circle_filled(pos2(r.left() + 12.0, r.center().y), 5.0, cat_color(cat));
                        ui.painter().text(pos2(r.left() + 26.0, r.top() + 11.0), Align2::LEFT_CENTER, d.label, bold(13.5), pal.text);
                        text_fit(ui.painter(), pos2(r.left() + 26.0, r.bottom() - 11.0), Align2::LEFT_CENTER, d.doc.split(['.', '\n']).next().unwrap_or(""), body(11.0), pal.muted, r.width() - 34.0);
                        if resp.clicked() {
                            chosen = Some(d.key);
                        }
                    }
                }
            });
        });
    });
    if close {
        st.picker = None;
        return;
    }
    if let Some(kind) = chosen {
        let Some(pk) = st.picker.take() else { return };
        push_undo(st, f);
        if let Some(id) = f.add_node(kind, pk.pos.x, pk.pos.y) {
            match pk.pend {
                Some(Pend::From(a, i)) => {
                    // first free input, else the first one
                    let ins = def_ins(f, id);
                    let free = (0..ins).find(|j| !f.wires.iter().any(|w| w.to == id && w.tp as usize == *j)).unwrap_or(0);
                    let _ = f.connect(a, i, id, free);
                    let siblings = f.wires.iter().filter(|w| w.from == a).count().saturating_sub(1) as f32;
                    if let Some(src) = f.node(a) {
                        let (sx, sy) = (src.x + src.width() + 70.0, src.y);
                        if let Some(nn) = f.node_mut(id) {
                            nn.x = sx;
                            nn.y = sy + siblings * 120.0;
                        }
                    }
                }
                Some(Pend::To(b, j)) => {
                    let _ = f.connect(id, 0, b, j);
                    if let Some(dst) = f.node(b) {
                        let (dx, dy) = (dst.x - flow::NODE_W - 70.0, dst.y);
                        if let Some(nn) = f.node_mut(id) {
                            nn.x = dx;
                            nn.y = dy;
                        }
                    }
                }
                None => {}
            }
            st.sel = std::iter::once(id).collect();
            *dirty = true;
        }
    }
}

fn def_ins(f: &Flow, id: u32) -> usize {
    f.node(id).and_then(|n| n.def()).map_or(0, |d| d.ins.len())
}

#[allow(clippy::too_many_arguments)]
fn menu(ctx: &Context, ws: &mut Workspace, page: u64, pal: &Palette, f: &mut Flow, st: &mut St, rect: Rect, compact: bool, out: &mut Out, dirty: &mut bool) {
    let _ = (ws, page);
    let w = if compact { ctx.screen_rect().width() - 16.0 } else { 420.0 };
    let max_h = if compact { ctx.screen_rect().height() * 0.78 } else { (rect.height() - 100.0).clamp(260.0, 640.0) };
    let area = egui::Area::new(Id::new("flow_menu")).order(HOST_ORDER);
    let area = if compact { area.anchor(Align2::CENTER_BOTTOM, vec2(0.0, -8.0)) } else { area.anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)) };
    let mut close = false;
    let mut tpl: Option<usize> = None;
    let mut layout = false;
    let mut import: Option<Flow> = None;
    let mut remove_var: Option<String> = None;
    let mut add_var = false;
    let mut export = false;
    let mut snapshot = None;
    area.show(ctx, |ui| {
        frame(pal).show(ui, |ui| {
            ui.set_width(w - 16.0);
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Flow settings").font(bold(15.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        close = true;
                    }
                });
            });
            egui::ScrollArea::vertical().max_height(max_h).id_salt("flow_menu_scroll").show(ui, |ui| {
                ui.set_width(w - 32.0);
                ui.label(egui::RichText::new("Templates").strong());
                for (i, t) in flow::TEMPLATES.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            tpl = Some(i);
                        }
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(t.name).strong());
                            ui.label(egui::RichText::new(t.desc).small().color(pal.muted));
                        });
                    });
                }
                ui.separator();
                ui.label(egui::RichText::new("Variables").strong());
                ui.label(egui::RichText::new("Use {{vars.name}} in any text field, or vars.name in scripts. Handy for base URLs and tokens.").small().color(pal.muted));
                let keys: Vec<String> = f.vars.keys().cloned().collect();
                for k in keys {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&k).monospace());
                        if let Some(v) = f.vars.get_mut(&k) {
                            ui.add(TextEdit::singleline(v).desired_width(ui.available_width() - 60.0));
                        }
                        if ui.small_button("Remove").clicked() {
                            remove_var = Some(k.clone());
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut st.var_name).hint_text("name").desired_width(90.0));
                    ui.add(TextEdit::singleline(&mut st.var_value).hint_text("value").desired_width(ui.available_width() - 56.0));
                    if ui.button("Add").clicked() {
                        add_var = true;
                    }
                });
                ui.separator();
                ui.label(egui::RichText::new("Schedule").strong());
                ui.horizontal(|ui| {
                    ui.label("Run every");
                    ui.add(egui::DragValue::new(&mut f.every).range(0..=86_400).suffix(" s"));
                    ui.label(egui::RichText::new(if f.every == 0 { "off" } else { "while this page is open" }).small().color(pal.muted));
                });
                ui.label(egui::RichText::new("Scheduled runs are live runs. Lumen has to be open: browsers and phones do not run apps in the background.").small().color(pal.muted));
                ui.separator();
                ui.label(egui::RichText::new("Layout").strong());
                if ui.button("Arrange left → right").clicked() {
                    layout = true;
                }
                ui.separator();
                ui.label(egui::RichText::new("Share").strong());
                ui.horizontal(|ui| {
                    if ui.button("Copy flow as JSON").clicked() {
                        export = true;
                    }
                    ui.label(egui::RichText::new("Paste JSON below (or Ctrl+V on the board) to import.").small().color(pal.muted));
                });
                ui.add(TextEdit::multiline(&mut st.paste).hint_text("Paste a flow here…").font(egui::TextStyle::Monospace).desired_rows(3).desired_width(f32::INFINITY));
                if !st.paste.trim().is_empty() && ui.button("Import pasted flow").clicked() {
                    match Flow::from_json(&st.paste) {
                        Some(o) => import = Some(o),
                        None => st.toast = Some("That text is not a Lumen flow".into()),
                    }
                }
            });
        });
    });
    let origin_y = f.bounds().map_or(0.0, |b| b.3 + 80.0);
    if let Some(i) = tpl {
        snapshot = Some(snap(f));
        let t = (flow::TEMPLATES[i].build)();
        let ids = f.merge_in(&t, 0.0, origin_y);
        st.sel = ids.into_iter().collect();
        fit(f, rect);
        *dirty = true;
        close = true;
    }
    if layout {
        snapshot = Some(snap(f));
        f.auto_layout();
        fit(f, rect);
        *dirty = true;
    }
    if let Some(o) = import {
        snapshot = Some(snap(f));
        let ids = f.merge_in(&o, 0.0, origin_y);
        st.sel = ids.into_iter().collect();
        st.paste.clear();
        fit(f, rect);
        *dirty = true;
        close = true;
    }
    if let Some(s) = snapshot {
        st.undo.push(s);
        st.redo.clear();
    }
    if let Some(k) = remove_var {
        f.vars.remove(&k);
    }
    if add_var && !st.var_name.trim().is_empty() {
        let k: String = st.var_name.trim().chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
        f.vars.insert(k, std::mem::take(&mut st.var_value));
        st.var_name.clear();
    }
    if export {
        ctx.copy_text(f.to_json());
        out.toast = Some("Flow copied as JSON".into());
    }
    if close {
        st.menu = false;
    }
}
