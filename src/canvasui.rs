//! Canvas interaction: pan / zoom, selection, drag, resize, connectors, pen, text editing, shortcuts, floating chrome and menus.
//!
//! Input model (desktop and phone share one code path):
//! * wheel / two-finger swipe pans, Ctrl+wheel / trackpad pinch / two-finger pinch zooms around the pointer;
//! * the main widget is a single click-and-drag surface; hit testing is done by hand (`Canvas::hit_node`), so hundreds of cards cost
//!   nothing per frame;
//! * on touch screens `touch.rs` turns a one-finger swipe into scrolling (= panning here) and only lets a finger drag
//!   immediately inside the rectangles registered with `touch::claim` (cards, handles) — so a finger on empty board pans and a finger
//!   on a card moves it; hold + drag draws a selection box; hold + lift opens the context menu.

use std::cell::RefCell;
use std::collections::BTreeSet;

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{pos2, vec2, Align2, Color32, Context, CursorIcon, Event, Id, Key, Margin, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2};

use crate::canvas::{self as cv, Align, Arrow, Canvas, Grid, Kind, Shape, Side, Tpl, View};
use crate::canvasdraw::{self as draw, DrawOut, Frame, NodeSt};
use crate::icons;
use crate::lucide as l;
use crate::model::{PageKind, Workspace};
use crate::platform::{self, PickTarget};
use crate::theme::{bold, body, Palette};
use crate::touch;
use crate::uploader::{self, Spec};
use crate::widgets::{self, context_menu, with_alpha, CtxItem};

// ───────────────────────────── state ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Tool {
    #[default]
    Select,
    Hand,
    Pen,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Edit {
    Node(u64),
    Edge(u64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pop {
    Image(u64),
    Link(u64),
    Page(u64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Empty,
    Node(u64),
    Edge(u64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MenuKind {
    Context(Target),
    Add,
    Templates,
    Canvas,
}

#[derive(Clone, Copy)]
struct MenuState {
    kind: MenuKind,
    pos: Pos2,
    world: Pos2,
}

#[derive(Clone, Default)]
enum Drag {
    #[default]
    None,
    Pan,
    Marquee { from: Pos2, base: BTreeSet<u64> },
    Move { start: Pos2, orig: Vec<(u64, Pos2)>, moved: bool },
    Resize { id: u64, corner: u8, orig: Rect, pts: Vec<[f32; 2]>, changed: bool },
    Connect { from: u64, side: Side, to: Pos2 },
    Ink { pts: Vec<Pos2> },
}

#[derive(Default)]
struct St {
    page: u64,
    sel: BTreeSet<u64>,
    sel_edge: Option<u64>,
    tool: Tool,
    drag: Drag,
    edit: Option<Edit>,
    focus_edit: bool,
    menu: Option<MenuState>,
    fresh: bool,
    pop: Option<Pop>,
    pop_fresh: bool,
    page_query: String,
    anim: Option<View>,
    guides: Vec<(bool, f32)>,
    find: Option<String>,
    find_idx: usize,
    focus_find: bool,
    present: Option<usize>,
    space: bool,
    pen_color: u8,
    pen_size: u8,
    color_open: bool,
    rect: Option<Rect>,
    hover: Option<u64>,
    checks: Vec<(Rect, u64, usize)>,
    opens: Vec<(Rect, u64)>,
    spawn_n: u32,
    paste_n: u32,
    want_paste: bool,
    checked_visible: bool,
    last_tap: f64,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

#[derive(Default)]
pub struct Out {
    /// Page to open (page cards).
    pub open: Option<u64>,
    pub toast: Option<String>,
}

const PEN_COLORS: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

// ───────────────────────────── public API ─────────────────────────────

pub fn show(ui: &mut Ui, ws: &mut Workspace, page: u64, pal: &Palette) -> Out {
    let mut out = Out::default();
    let Some(mut c) = ws.page_mut(page).map(|p| p.canvas.take().unwrap_or_else(Canvas::new)) else { return out };
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.page != page {
        st = St { page, pen_size: 1, ..Default::default() };
    }
    let rev0 = c.rev;
    run(ui, ws, page, pal, &mut c, &mut st, &mut out);
    if c.rev != rev0 {
        ws.touch(page);
    }
    ST.with(|s| *s.borrow_mut() = st);
    if let Some(p) = ws.page_mut(page) {
        p.canvas = Some(c);
    }
    out
}

/// Top-bar undo / redo for canvas pages.
pub fn undo_redo(ws: &mut Workspace, page: u64, redo: bool) -> bool {
    let Some(c) = ws.page_mut(page).and_then(|p| p.canvas.as_mut()) else { return false };
    let ok = if redo { c.redo() } else { c.undo() };
    if ok {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.edit = None;
            s.drag = Drag::None;
        });
        ws.touch(page);
    }
    ok
}

pub fn can_undo_redo(ws: &Workspace, page: u64) -> (bool, bool) {
    ws.page(page).and_then(|p| p.canvas.as_ref()).map_or((false, false), |c| (c.can_undo(), c.can_redo()))
}

/// Closes the image / link / page popover (an upload finished).
pub fn close_popup() {
    ST.with(|s| s.borrow_mut().pop = None);
}

/// Repairs damaged canvases once per session (after loading a workspace).
pub fn fix_once(ws: &mut Workspace) {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !DONE.with(|d| d.replace(true)) {
        ws.fix_canvases();
    }
}

/// A new empty image card in the middle of the visible board (dropped / uploaded files). Returns the node id.
pub fn new_image_node(ws: &mut Workspace, page: u64) -> Option<u64> {
    let c = ws.page_mut(page)?.canvas.as_mut()?;
    let at = ST.with(|s| {
        let s = s.borrow();
        s.rect.map(|r| c.view.to_world(r.min, r.center())).unwrap_or_else(|| pos2(c.view.x + 300.0, c.view.y + 200.0))
    });
    c.checkpoint();
    let id = c.add_node(Kind::Image, at);
    ws.touch(page);
    Some(id)
}

// ───────────────────────────── helpers ─────────────────────────────

fn center_world(st: &St, c: &Canvas) -> Pos2 {
    match st.rect {
        Some(r) => c.view.to_world(r.min, r.center()),
        None => pos2(c.view.x + 300.0, c.view.y + 200.0),
    }
}

fn is_text_kind(k: Kind) -> bool {
    matches!(k, Kind::Note | Kind::Todo | Kind::Text)
}

fn animate_to(st: &mut St, v: View) {
    st.anim = Some(v.sane());
}

fn fit_ids(st: &mut St, c: &Canvas, ids: Option<&BTreeSet<u64>>) {
    if let (Some(r), Some(b)) = (st.rect, c.bounds(ids)) {
        animate_to(st, Canvas::fit(b, r.size(), if touch::compact() { 28.0 } else { 70.0 }));
    }
}

fn slides(c: &Canvas) -> Vec<u64> {
    let mut g: Vec<(i32, f32, u64)> = c.nodes.iter().filter(|n| n.kind == Kind::Group).map(|n| ((n.y / 160.0).round() as i32, n.x, n.id)).collect();
    g.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
    g.into_iter().map(|x| x.2).collect()
}

fn select_only(st: &mut St, ids: impl IntoIterator<Item = u64>) {
    st.sel = ids.into_iter().collect();
    st.sel_edge = None;
}

fn end_edit(ctx: &Context, st: &mut St, c: &mut Canvas) {
    if let Some(e) = st.edit.take() {
        ctx.memory_mut(|m| {
            m.surrender_focus(Id::new(("cnv_edit", match e {
                Edit::Node(i) => i,
                Edit::Edge(i) => i + (1 << 40),
            })))
        });
        if let Edit::Node(id) = e {
            let empty_text = c.node(id).map_or(false, |n| n.kind == Kind::Text && n.text.trim().is_empty());
            if empty_text {
                let mut s = BTreeSet::new();
                s.insert(id);
                c.delete_nodes(&s);
                st.sel.remove(&id);
            }
        }
    }
}

fn begin_edit(st: &mut St, c: &mut Canvas, e: Edit) {
    c.checkpoint();
    st.edit = Some(e);
    st.focus_edit = true;
    st.pop = None;
}

fn add_at(st: &mut St, c: &mut Canvas, kind: Kind, at: Pos2) -> u64 {
    c.checkpoint();
    let id = c.add_node(kind, at);
    select_only(st, [id]);
    st.tool = Tool::Select;
    match kind {
        Kind::Note | Kind::Text | Kind::Todo | Kind::Group => {
            st.edit = Some(Edit::Node(id));
            st.focus_edit = true;
        }
        Kind::Image => {
            st.pop = Some(Pop::Image(id));
            st.pop_fresh = true;
        }
        Kind::Link => {
            st.pop = Some(Pop::Link(id));
            st.pop_fresh = true;
        }
        Kind::Page => {
            st.pop = Some(Pop::Page(id));
            st.pop_fresh = true;
            st.page_query.clear();
        }
        Kind::Ink => {}
    }
    id
}

/// Next free spot near the middle of the screen (cards added from the dock cascade instead of stacking).
fn spawn_point(st: &mut St, c: &Canvas) -> Pos2 {
    let p = center_world(st, c);
    let k = (st.spawn_n % 7) as f32;
    st.spawn_n += 1;
    p + vec2(k * 28.0, k * 28.0) - vec2(84.0, 84.0)
}

fn hit_slack(st: &St, c: &Canvas) -> f32 {
    let _ = st;
    (if touch::touchy() { 8.0 } else { 3.0 }) / c.view.zoom
}

fn sel_bounds(c: &Canvas, st: &St) -> Option<Rect> {
    if st.sel.is_empty() { None } else { c.bounds(Some(&st.sel)) }
}

// ───────────────────────────── actions (menus, shortcuts, toolbar) ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    AddNote,
    AddText,
    AddTodo,
    AddPage,
    AddImage,
    AddLink,
    AddGroup,
    Templates,
    Tpl(Tpl),
    Paste,
    Edit,
    OpenPage,
    OpenLink,
    ReplaceSrc,
    Duplicate,
    Copy,
    Cut,
    Delete,
    Front,
    Back,
    Lock,
    GroupSel,
    AddChild,
    AddSibling,
    TreeLayout,
    AlignL,
    AlignT,
    DistH,
    DistV,
    Tidy,
    SelectAll,
    Fit,
    FitSel,
    Reset,
    GridCycle,
    SnapToggle,
    MapToggle,
    Present,
    Find,
    CopyJson,
    Undo,
    Redo,
    EdgeDash,
    EdgeArrow,
    EdgeShape,
    EdgeLabel,
    EdgeReverse,
}

fn act(a: Act, st: &mut St, c: &mut Canvas, ws: &mut Workspace, ctx: &Context, page: u64, out: &mut Out, at: Pos2) {
    let sel = st.sel.clone();
    match a {
        Act::AddNote => {
            add_at(st, c, Kind::Note, at);
        }
        Act::AddText => {
            add_at(st, c, Kind::Text, at);
        }
        Act::AddTodo => {
            add_at(st, c, Kind::Todo, at);
        }
        Act::AddPage => {
            add_at(st, c, Kind::Page, at);
        }
        Act::AddImage => {
            add_at(st, c, Kind::Image, at);
        }
        Act::AddLink => {
            add_at(st, c, Kind::Link, at);
        }
        Act::AddGroup => {
            add_at(st, c, Kind::Group, at);
        }
        Act::Templates => {
            let pos = st.rect.map_or(Pos2::ZERO, |r| r.center() - vec2(110.0, 120.0));
            st.menu = Some(MenuState { kind: MenuKind::Templates, pos, world: at });
            st.fresh = true;
        }
        Act::Tpl(t) => {
            c.checkpoint();
            let ids = c.insert_template(t, at);
            select_only(st, ids.iter().copied());
            let set: BTreeSet<u64> = ids.into_iter().collect();
            fit_ids(st, c, Some(&set));
        }
        Act::Paste => {
            platform::request_paste(ctx);
            st.want_paste = true;
        }
        Act::Edit => {
            if let Some(id) = sel.iter().next().copied().filter(|_| sel.len() == 1) {
                if let Some(n) = c.node(id).cloned() {
                    match n.kind {
                        k if is_text_kind(k) || k == Kind::Group => begin_edit(st, c, Edit::Node(id)),
                        Kind::Image => {
                            st.pop = Some(Pop::Image(id));
                            st.pop_fresh = true;
                        }
                        Kind::Link => {
                            st.pop = Some(Pop::Link(id));
                            st.pop_fresh = true;
                        }
                        Kind::Page => {
                            st.pop = Some(Pop::Page(id));
                            st.pop_fresh = true;
                            st.page_query.clear();
                        }
                        Kind::Ink => {}
                        _ => {}
                    }
                }
            }
        }
        Act::OpenPage => {
            if let Some(p) = sel.iter().filter_map(|i| c.node(*i)).find_map(|n| n.page) {
                out.open = Some(p);
            }
        }
        Act::OpenLink => {
            if let Some(n) = sel.iter().filter_map(|i| c.node(*i)).find(|n| n.kind == Kind::Link && !n.url.is_empty()) {
                platform::open_url(ctx, &crate::media::normalize_url(&n.url));
            }
        }
        Act::ReplaceSrc => {
            act(Act::Edit, st, c, ws, ctx, page, out, at);
        }
        Act::Duplicate => {
            if !sel.is_empty() {
                c.checkpoint();
                let ids = c.duplicate(&sel, vec2(28.0, 28.0));
                select_only(st, ids);
            }
        }
        Act::Copy | Act::Cut => {
            if !sel.is_empty() {
                ctx.copy_text(c.to_json(Some(&sel)));
                out.toast = Some(format!("Copied {} item{}", sel.len(), if sel.len() == 1 { "" } else { "s" }));
                if a == Act::Cut {
                    act(Act::Delete, st, c, ws, ctx, page, out, at);
                }
            }
        }
        Act::Delete => {
            if let Some(e) = st.sel_edge.take() {
                c.checkpoint();
                c.delete_edge(e);
            } else if !sel.is_empty() {
                c.checkpoint();
                c.delete_nodes(&sel);
                st.sel.clear();
            }
        }
        Act::Front => {
            if !sel.is_empty() {
                c.checkpoint();
                c.bring_to_front(&sel);
            }
        }
        Act::Back => {
            if !sel.is_empty() {
                c.checkpoint();
                c.send_to_back(&sel);
            }
        }
        Act::Lock => {
            if !sel.is_empty() {
                c.checkpoint();
                let lock = !sel.iter().filter_map(|i| c.node(*i)).all(|n| n.locked);
                for n in c.nodes.iter_mut().filter(|n| sel.contains(&n.id)) {
                    n.locked = lock;
                }
                c.rev += 1;
            }
        }
        Act::GroupSel => {
            if !sel.is_empty() {
                c.checkpoint();
                if let Some(g) = c.group_selection(&sel) {
                    select_only(st, [g]);
                }
            }
        }
        Act::AddChild => {
            if let Some(id) = sel.iter().next().copied().filter(|_| sel.len() == 1) {
                c.checkpoint();
                if let Some(n) = c.add_child(id) {
                    select_only(st, [n]);
                    st.edit = Some(Edit::Node(n));
                    st.focus_edit = true;
                }
            }
        }
        Act::AddSibling => {
            if let Some(id) = sel.iter().next().copied().filter(|_| sel.len() == 1) {
                c.checkpoint();
                if let Some(n) = c.add_sibling(id) {
                    select_only(st, [n]);
                    st.edit = Some(Edit::Node(n));
                    st.focus_edit = true;
                }
            }
        }
        Act::TreeLayout => {
            if let Some(id) = sel.iter().next().copied().filter(|_| sel.len() == 1) {
                c.checkpoint();
                c.tree_layout(id);
            }
        }
        Act::AlignL | Act::AlignT => {
            c.checkpoint();
            c.align(&sel, if a == Act::AlignL { Align::Left } else { Align::Top });
        }
        Act::DistH | Act::DistV => {
            c.checkpoint();
            c.distribute(&sel, a == Act::DistH);
        }
        Act::Tidy => {
            c.checkpoint();
            let ids: BTreeSet<u64> = if sel.len() > 1 { sel } else { c.nodes.iter().map(|n| n.id).collect() };
            c.tidy(&ids);
        }
        Act::SelectAll => {
            select_only(st, c.nodes.iter().map(|n| n.id));
        }
        Act::Fit => {
            if c.nodes.is_empty() {
                animate_to(st, View::default());
            } else {
                fit_ids(st, c, None);
            }
        }
        Act::FitSel => {
            if st.sel.is_empty() {
                fit_ids(st, c, None);
            } else {
                fit_ids(st, c, Some(&sel));
            }
        }
        Act::Reset => {
            if let Some(r) = st.rect {
                let mut v = c.view;
                v.zoom_at(r.min, r.center(), 1.0 / v.zoom);
                animate_to(st, v);
            }
        }
        Act::GridCycle => {
            c.grid = match c.grid {
                Grid::Dots => Grid::Lines,
                Grid::Lines => Grid::None,
                Grid::None => Grid::Dots,
            };
        }
        Act::SnapToggle => c.snap = !c.snap,
        Act::MapToggle => c.minimap = !c.minimap,
        Act::Present => {
            if slides(c).is_empty() {
                out.toast = Some("Add a group to present — each group is one slide".into());
            } else {
                st.present = Some(0);
                st.edit = None;
                st.menu = None;
                select_only(st, []);
                present_goto(st, c, 0);
            }
        }
        Act::Find => {
            st.find = Some(st.find.take().unwrap_or_default());
            st.focus_find = true;
        }
        Act::CopyJson => {
            ctx.copy_text(c.to_json(None));
            out.toast = Some("Canvas copied as Obsidian-compatible JSON".into());
        }
        Act::Undo => {
            c.undo();
            st.edit = None;
        }
        Act::Redo => {
            c.redo();
            st.edit = None;
        }
        Act::EdgeDash | Act::EdgeArrow | Act::EdgeShape | Act::EdgeLabel | Act::EdgeReverse => {
            let Some(eid) = st.sel_edge else { return };
            if a == Act::EdgeLabel {
                begin_edit(st, c, Edit::Edge(eid));
                return;
            }
            c.checkpoint();
            if let Some(e) = c.edge_mut(eid) {
                match a {
                    Act::EdgeDash => e.dashed = !e.dashed,
                    Act::EdgeArrow => {
                        e.arrow = match e.arrow {
                            Arrow::End => Arrow::Both,
                            Arrow::Both => Arrow::None,
                            Arrow::None => Arrow::End,
                        }
                    }
                    Act::EdgeShape => {
                        e.shape = match e.shape {
                            Shape::Curve => Shape::Straight,
                            Shape::Straight => Shape::Elbow,
                            Shape::Elbow => Shape::Curve,
                        }
                    }
                    _ => {
                        std::mem::swap(&mut e.from, &mut e.to);
                        std::mem::swap(&mut e.from_side, &mut e.to_side);
                    }
                }
            }
            c.rev += 1;
        }
    }
}

fn present_goto(st: &mut St, c: &Canvas, i: usize) {
    let s = slides(c);
    if s.is_empty() {
        return;
    }
    let i = i.min(s.len() - 1);
    st.present = Some(i);
    if let (Some(n), Some(r)) = (c.node(s[i]), st.rect) {
        animate_to(st, Canvas::fit(n.rect(), r.size(), 40.0));
    }
}

// ───────────────────────────── main ─────────────────────────────

fn run(ui: &mut Ui, ws: &mut Workspace, page: u64, pal: &Palette, c: &mut Canvas, st: &mut St, out: &mut Out) {
    let ctx = ui.ctx().clone();
    let rect = ui.available_rect_before_wrap();
    ui.set_clip_rect(rect);
    st.rect = Some(rect);
    let origin = rect.min;
    let compact = touch::compact() || rect.width() < 560.0;
    let touchy = touch::touchy();
    let now = ctx.input(|i| i.time);
    let dt = ctx.input(|i| i.stable_dt).min(0.05);

    // sanitise (undo can remove what the selection / editor points at)
    st.sel.retain(|id| c.node(*id).is_some());
    if st.sel_edge.map_or(false, |e| c.edge(e).is_none()) {
        st.sel_edge = None;
    }
    match st.edit {
        Some(Edit::Node(id)) if c.node(id).is_none() => st.edit = None,
        Some(Edit::Edge(id)) if c.edge(id).is_none() => st.edit = None,
        _ => {}
    }
    c.view = c.view.sane();

    // first frame: make sure something is visible
    if !st.checked_visible {
        st.checked_visible = true;
        let vw = Rect::from_min_max(c.view.to_world(origin, rect.min), c.view.to_world(origin, rect.max));
        if c.nodes.iter().all(|n| !n.rect().intersects(vw)) && !c.nodes.is_empty() {
            fit_ids(st, c, None);
        }
    }

    // animated view changes
    if let Some(t) = st.anim {
        let k = 1.0 - (-dt * 11.0).exp();
        let v = &mut c.view;
        v.x += (t.x - v.x) * k;
        v.y += (t.y - v.y) * k;
        v.zoom *= (t.zoom / v.zoom).powf(k);
        if (t.x - v.x).abs() < 0.2 / v.zoom && (t.y - v.y).abs() < 0.2 / v.zoom && (t.zoom / v.zoom - 1.0).abs() < 0.002 {
            *v = t;
            st.anim = None;
        }
        ctx.request_repaint();
    }

    let resp = ui.interact(rect, Id::new(("cnv_surface", page)), Sense::click_and_drag());
    let over_canvas = ctx.input(|i| i.pointer.hover_pos()).map_or(false, |p| rect.contains(p) && ctx.layer_id_at(p).map_or(true, |lid| lid == ui.layer_id()));
    let multi = ctx.input(|i| i.multi_touch().is_some());
    let editing = st.edit.is_some();

    // ── pan & zoom ──
    if (over_canvas || multi) && !st.present.is_some() {
        let (scroll, zoom_d, mt, hover) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.multi_touch(), i.pointer.hover_pos()));
        let mut moved = false;
        if scroll != Vec2::ZERO && mt.is_none() {
            c.view.x -= scroll.x / c.view.zoom;
            c.view.y -= scroll.y / c.view.zoom;
            moved = true;
        }
        if let Some(m) = mt {
            c.view.x -= m.translation_delta.x / c.view.zoom;
            c.view.y -= m.translation_delta.y / c.view.zoom;
            if (m.zoom_delta - 1.0).abs() > 1e-4 {
                c.view.zoom_at(origin, m.center_pos, m.zoom_delta);
            }
            moved = true;
        } else if (zoom_d - 1.0).abs() > 1e-4 {
            c.view.zoom_at(origin, hover.unwrap_or(rect.center()), zoom_d);
            moved = true;
        }
        if moved {
            st.anim = None;
            ctx.request_repaint();
        }
    }
    if multi {
        st.drag = Drag::None;
    }

    // ── keyboard ──
    let typing = ctx.wants_keyboard_input() || editing;
    handle_keys(&ctx, ui, ws, page, c, st, out, typing, over_canvas, origin);

    // ── pointer ──
    let hover_pos = resp.hover_pos();
    let wpos = |p: Pos2| c.view.to_world(origin, p);
    st.hover = None;
    if let (Some(hp), false) = (hover_pos, touchy) {
        if matches!(st.drag, Drag::None) && st.tool == Tool::Select && !editing {
            st.hover = c.hit_node(wpos(hp), hit_slack(st, c), 34.0);
        }
    }
    if !multi && !st.present.is_some() {
        pointer(&ctx, ui, ws, page, &resp, c, st, out, origin, over_canvas, touchy, pal);
    }

    // ── draw ──
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, draw::canvas_bg(pal));
    let mut dout = DrawOut::default();
    {
        let f = Frame { p: &painter, ui: &*ui, pal, view: c.view, origin, clip: rect };
        draw::grid(&f, c.grid);
        let found: Vec<u64> = st.find.as_ref().map(|q| c.find(q)).unwrap_or_default();
        let nst = |id: u64| NodeSt { selected: st.sel.contains(&id), editing: st.edit == Some(Edit::Node(id)), hovered: st.hover == Some(id), found: found.contains(&id) };
        for n in c.nodes.iter().filter(|n| n.kind == Kind::Group) {
            draw::draw_node(&f, ws, n, &nst(n.id), &mut dout);
        }
        for e in &c.edges {
            draw::draw_edge(&f, c, e, st.sel_edge == Some(e.id), false);
        }
        for n in c.nodes.iter().filter(|n| n.kind != Kind::Group) {
            draw::draw_node(&f, ws, n, &nst(n.id), &mut dout);
        }
        draw_overlays_world(&f, c, st, &ctx);
    }
    // auto-grow
    for (id, h) in dout.grow {
        if let Some(n) = c.node_mut(id) {
            let editing_it = st.edit == Some(Edit::Node(id));
            match n.kind {
                Kind::Image => {
                    if n.h <= 0.5 {
                        n.h = h;
                    }
                }
                _ => {
                    if h > n.h + 0.5 && !(editing_it && matches!(st.drag, Drag::Resize { .. })) {
                        n.h = h.ceil();
                    }
                }
            }
        }
    }
    st.checks = dout.checks;
    st.opens = dout.opens;
    if touchy {
        for n in &c.nodes {
            let r = c.view.rect_to_screen(origin, if n.kind == Kind::Group { Rect::from_min_size(n.rect().min, vec2(n.w, 34.0)) } else { n.rect() });
            if r.intersects(rect) && !n.locked {
                touch::claim(r.expand(2.0));
            }
        }
    }
    if st.tool == Tool::Pen && !st.present.is_some() {
        touch::set_passthrough(true);
    }

    // ── cursor ──
    if over_canvas && !multi {
        let cur = match (&st.drag, st.tool) {
            (Drag::Pan, _) => CursorIcon::Grabbing,
            (Drag::Move { .. }, _) => CursorIcon::Grabbing,
            (Drag::Connect { .. }, _) => CursorIcon::Crosshair,
            (Drag::Ink { .. }, _) | (_, Tool::Pen) => CursorIcon::Crosshair,
            (_, Tool::Hand) => CursorIcon::Grab,
            _ if st.space => CursorIcon::Grab,
            _ => {
                if st.hover.is_some() {
                    CursorIcon::PointingHand
                } else {
                    CursorIcon::Default
                }
            }
        };
        if !editing && hover_pos.is_some() {
            ctx.set_cursor_icon(cur);
        }
    }

    // empty-board hint
    if c.nodes.is_empty() && !editing && st.present.is_none() {
        let msg = if touchy { "Double-tap to add a card" } else { "Double-click to add a card" };
        ui.painter().text(rect.center() + vec2(0.0, -8.0), Align2::CENTER_CENTER, msg, body(15.0), with_alpha(pal.muted, 0.9));
    }

    // ── chrome & popovers ──
    edit_overlay(ui, &ctx, c, st, pal);
    if st.present.is_none() {
        selection_toolbar(ui, &ctx, ws, page, c, st, out, pal, rect, compact);
        title_chip(ui, ws, page, pal, rect, compact);
        top_right(ui, &ctx, ws, page, c, st, out, pal, rect, compact);
        if st.find.is_some() {
            find_bar(ui, &ctx, c, st, pal, rect);
        }
        dock(ui, &ctx, ws, page, c, st, out, pal, rect, compact);
        if !compact && c.minimap && !c.nodes.is_empty() {
            minimap(ui, c, st, pal, rect);
        }
    } else {
        present_bar(ui, c, st, pal, rect);
    }
    popovers(ui, &ctx, ws, page, c, st, pal, rect);
    menus(ui, &ctx, ws, page, c, st, out, pal);

    // pasted text requested from the menu
    if st.want_paste {
        if let Some(t) = platform::take_paste() {
            st.want_paste = false;
            let at = center_world(st, c);
            paste_text(&ctx, c, st, &t, at, out);
        }
    }
    let _ = now;
}

// ───────────────────────────── keyboard ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_keys(ctx: &Context, ui: &Ui, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, out: &mut Out, typing: bool, over_canvas: bool, origin: Pos2) {
    let _ = origin;
    st.space = !typing && ctx.input(|i| i.key_down(Key::Space));
    // Escape always works
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        if st.menu.is_some() {
            st.menu = None;
        } else if st.pop.is_some() {
            st.pop = None;
        } else if st.present.is_some() {
            st.present = None;
        } else if st.find.is_some() {
            st.find = None;
        } else if st.edit.is_some() {
            end_edit(ctx, st, c);
        } else if !matches!(st.drag, Drag::None) {
            st.drag = Drag::None;
        } else if st.tool != Tool::Select {
            st.tool = Tool::Select;
        } else {
            select_only(st, []);
        }
    }
    if st.present.is_some() {
        let right = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::PageDown) || i.consume_key(Modifiers::NONE, Key::Space));
        let left = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::NONE, Key::PageUp));
        let cur = st.present.unwrap_or(0);
        if right {
            present_goto(st, c, cur + 1);
        }
        if left {
            present_goto(st, c, cur.saturating_sub(1));
        }
        return;
    }
    // find shortcut works everywhere on the page
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
        act(Act::Find, st, c, ws, ctx, page, out, Pos2::ZERO);
    }
    if typing {
        // Cmd+Enter finishes a card edit
        if st.edit.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            end_edit(ctx, st, c);
        }
        return;
    }
    let at = center_world(st, c);
    let cmd = Modifiers::COMMAND;
    let mut run_act = |a: Act, st: &mut St, c: &mut Canvas, ws: &mut Workspace, out: &mut Out| act(a, st, c, ws, ctx, page, out, at);
    macro_rules! key {
        ($m:expr, $k:expr, $a:expr) => {
            if ctx.input_mut(|i| i.consume_key($m, $k)) {
                run_act($a, st, c, ws, out);
            }
        };
    }
    key!(cmd, Key::A, Act::SelectAll);
    key!(cmd, Key::D, Act::Duplicate);
    key!(cmd, Key::G, Act::GroupSel);
    key!(cmd | Modifiers::SHIFT, Key::Z, Act::Redo);
    key!(cmd, Key::Y, Act::Redo);
    key!(cmd, Key::Z, Act::Undo);
    key!(Modifiers::NONE, Key::Delete, Act::Delete);
    key!(Modifiers::NONE, Key::Backspace, Act::Delete);
    key!(Modifiers::NONE, Key::Tab, Act::AddChild);
    key!(cmd, Key::Enter, Act::AddSibling);
    key!(Modifiers::NONE, Key::Enter, Act::Edit);
    key!(Modifiers::SHIFT, Key::Num1, Act::Fit);
    key!(Modifiers::SHIFT, Key::Num2, Act::FitSel);
    key!(cmd, Key::Num0, Act::Reset);
    let mut tool_key = |k: Key, t: Tool| {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k)) {
            st.tool = t;
        }
    };
    tool_key(Key::V, Tool::Select);
    tool_key(Key::H, Tool::Hand);
    tool_key(Key::P, Tool::Pen);
    if over_canvas {
        let spawn = |st: &mut St, c: &mut Canvas, k: Kind, ctx: &Context| {
            let at = ctx.input(|i| i.pointer.hover_pos()).map(|p| c.view.to_world(st.rect.map_or(Pos2::ZERO, |r| r.min), p)).unwrap_or_else(|| center_world(st, c));
            add_at(st, c, k, at);
        };
        for (k, kind) in [(Key::N, Kind::Note), (Key::T, Kind::Text), (Key::L, Kind::Todo), (Key::G, Kind::Group)] {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k)) {
                spawn(st, c, kind, ctx);
            }
        }
    }
    // zoom keys
    if let Some(r) = st.rect {
        for (m, k, f) in [(Modifiers::NONE, Key::Plus, 1.25), (Modifiers::NONE, Key::Equals, 1.25), (Modifiers::NONE, Key::Minus, 0.8), (cmd, Key::Equals, 1.25), (cmd, Key::Minus, 0.8)] {
            if ctx.input_mut(|i| i.consume_key(m, k)) {
                let mut v = st.anim.unwrap_or(c.view);
                v.zoom_at(r.min, r.center(), f);
                animate_to(st, v);
            }
        }
    }
    // nudge
    let snap_on = c.snap;
    let step = move |big: bool| if big { 40.0 } else if snap_on { cv::GRID } else { 4.0 };
    for (m, big) in [(Modifiers::NONE, false), (Modifiers::SHIFT, true)] {
        for (k, d) in [(Key::ArrowLeft, vec2(-1.0, 0.0)), (Key::ArrowRight, vec2(1.0, 0.0)), (Key::ArrowUp, vec2(0.0, -1.0)), (Key::ArrowDown, vec2(0.0, 1.0))] {
            if !st.sel.is_empty() && ctx.input_mut(|i| i.consume_key(m, k)) {
                c.checkpoint();
                let by = d * step(big);
                let sel = st.sel.clone();
                for n in c.nodes.iter_mut().filter(|n| sel.contains(&n.id) && !n.locked) {
                    n.x += by.x;
                    n.y += by.y;
                }
                c.rev += 1;
            }
        }
    }
    // clipboard events
    let events: Vec<Event> = ctx.input(|i| i.events.clone());
    for e in events {
        match e {
            Event::Copy => run_act(Act::Copy, st, c, ws, out),
            Event::Cut => run_act(Act::Cut, st, c, ws, out),
            Event::Paste(t) => {
                let tail = ctx.input(|i| i.pointer.hover_pos()).filter(|p| st.rect.map_or(false, |r| r.contains(*p)));
                let at = tail.map(|p| c.view.to_world(st.rect.map_or(Pos2::ZERO, |r| r.min), p)).unwrap_or(at);
                paste_text(ctx, c, st, &t, at, out);
            }
            _ => {}
        }
    }
    let _ = ui;
}

fn looks_like_image(s: &str) -> bool {
    let l = s.to_lowercase();
    let base = l.split(['?', '#']).next().unwrap_or("");
    s.starts_with("http") && [".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".avif"].iter().any(|e| base.ends_with(e))
}

fn paste_text(ctx: &Context, c: &mut Canvas, st: &mut St, text: &str, at: Pos2, out: &mut Out) {
    let _ = ctx;
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    st.paste_n += 1;
    let off = vec2(24.0, 24.0) * ((st.paste_n % 6) as f32);
    c.checkpoint();
    let ids = c.import_json(t, at + off);
    if !ids.is_empty() {
        out.toast = Some(format!("Pasted {} item{}", ids.len(), if ids.len() == 1 { "" } else { "s" }));
        select_only(st, ids);
        return;
    }
    if looks_like_image(t) {
        let id = c.add_node(Kind::Image, at + off);
        if let Some(n) = c.node_mut(id) {
            n.url = t.to_owned();
        }
        select_only(st, [id]);
    } else if crate::media::looks_like_url(t) && !t.contains(char::is_whitespace) {
        let id = c.add_node(Kind::Link, at + off);
        if let Some(n) = c.node_mut(id) {
            n.url = crate::media::normalize_url(t);
        }
        select_only(st, [id]);
    } else {
        let id = c.add_node(Kind::Note, at + off);
        if let Some(n) = c.node_mut(id) {
            n.text = t.to_owned();
            n.w = if t.len() > 140 { 320.0 } else { 240.0 };
        }
        select_only(st, [id]);
    }
}

// ───────────────────────────── pointer ─────────────────────────────

fn corner_pts(r: Rect) -> [Pos2; 4] {
    [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()]
}

fn handle_sides(r: Rect) -> [(Side, Pos2); 4] {
    let o = 15.0;
    [(Side::Top, pos2(r.center().x, r.top() - o)), (Side::Right, pos2(r.right() + o, r.center().y)), (Side::Bottom, pos2(r.center().x, r.bottom() + o)), (Side::Left, pos2(r.left() - o, r.center().y))]
}

/// The node whose connector / resize handles are visible.
fn handle_node(st: &St, c: &Canvas) -> Option<u64> {
    if st.tool != Tool::Select || st.present.is_some() {
        return None;
    }
    if st.sel.len() == 1 && st.sel_edge.is_none() {
        return st.sel.iter().next().copied().filter(|id| c.node(*id).map_or(false, |n| n.kind != Kind::Ink));
    }
    st.hover.filter(|id| c.node(*id).map_or(false, |n| n.kind != Kind::Ink && !n.locked))
}

#[allow(clippy::too_many_arguments)]
fn pointer(ctx: &Context, ui: &Ui, ws: &mut Workspace, page: u64, resp: &egui::Response, c: &mut Canvas, st: &mut St, out: &mut Out, origin: Pos2, over_canvas: bool, touchy: bool, pal: &Palette) {
    let _ = pal;
    let z = c.view.zoom;
    let to_w = |v: &View, p: Pos2| v.to_world(origin, p);
    let (mods, press, cur, delta_ptr) = ctx.input(|i| (i.modifiers, i.pointer.press_origin(), i.pointer.interact_pos(), i.pointer.delta()));
    let _ = delta_ptr;
    let middle = ctx.input(|i| i.pointer.button_down(PointerButton::Middle));

    // click outside the active text editor ends the edit
    if st.edit.is_some() && ctx.input(|i| i.pointer.any_pressed()) {
        if let (Some(p), Some(e)) = (cur, st.edit) {
            let er = edit_rect(c, e, origin);
            if !er.expand(4.0).contains(p) && resp.hovered() {
                end_edit(ctx, st, c);
            }
        }
    }

    // ── press ──
    if resp.drag_started() || resp.drag_started_by(PointerButton::Middle) {
        let p0 = press.or(cur).unwrap_or(origin);
        let w0 = to_w(&c.view, p0);
        st.pop = None;
        st.menu = None;
        st.color_open = false;
        let pan_mode = st.tool == Tool::Hand || st.space || middle || resp.drag_started_by(PointerButton::Middle);
        if st.tool == Tool::Pen && !pan_mode {
            st.drag = Drag::Ink { pts: vec![w0] };
        } else if pan_mode {
            st.drag = Drag::Pan;
            st.anim = None;
        } else {
            let hn = handle_node(st, c);
            let mut started = false;
            if let Some(id) = hn {
                if let Some(n) = c.node(id).cloned() {
                    let sr = c.view.rect_to_screen(origin, n.rect());
                    let rr = if touchy { 22.0 } else { 11.0 };
                    // resize corners (single selected, unlocked)
                    if st.sel.len() == 1 && st.sel.contains(&id) && !n.locked {
                        for (i, cp) in corner_pts(sr).iter().enumerate() {
                            if (*cp - p0).length() <= rr {
                                let pts = n.pts.clone();
                                st.drag = Drag::Resize { id, corner: i as u8, orig: n.rect(), pts, changed: false };
                                started = true;
                                break;
                            }
                        }
                    }
                    if !started {
                        let hr = if touchy { 24.0 } else { 13.0 };
                        for (side, hp) in handle_sides(sr) {
                            if (hp - p0).length() <= hr {
                                st.drag = Drag::Connect { from: id, side, to: w0 };
                                started = true;
                                break;
                            }
                        }
                    }
                }
            }
            if !started {
                let slack = hit_slack(st, c);
                match c.hit_node(w0, slack, 34.0) {
                    Some(id) => {
                        if !st.sel.contains(&id) {
                            if mods.shift {
                                st.sel.insert(id);
                            } else {
                                select_only(st, [id]);
                            }
                        }
                        st.sel_edge = None;
                        let locked = c.node(id).map_or(false, |n| n.locked);
                        if locked {
                            st.drag = Drag::Pan;
                        } else {
                            let mut ids: BTreeSet<u64> = st.sel.clone();
                            for sid in st.sel.clone() {
                                if c.node(sid).map_or(false, |n| n.kind == Kind::Group) && !mods.alt {
                                    ids.extend(c.group_members(sid));
                                }
                            }
                            let orig = c.nodes.iter().filter(|n| ids.contains(&n.id) && !n.locked).map(|n| (n.id, pos2(n.x, n.y))).collect();
                            st.drag = Drag::Move { start: w0, orig, moved: false };
                        }
                    }
                    None => {
                        let base = if mods.shift { st.sel.clone() } else { BTreeSet::new() };
                        if !mods.shift {
                            select_only(st, []);
                        }
                        st.drag = Drag::Marquee { from: p0, base };
                    }
                }
            }
        }
    }

    // ── drag ──
    if resp.dragged() {
        let d = resp.drag_delta();
        let drag = std::mem::take(&mut st.drag);
        st.drag = match drag {
            Drag::None => Drag::None,
            Drag::Pan => {
                c.view.x -= d.x / z;
                c.view.y -= d.y / z;
                Drag::Pan
            }
            Drag::Marquee { from, base } => {
                if let Some(p) = cur {
                    let sr = Rect::from_two_pos(from, p);
                    let wr = Rect::from_min_max(to_w(&c.view, sr.min), to_w(&c.view, sr.max));
                    let mut sel = base.clone();
                    for n in &c.nodes {
                        let hit = if n.kind == Kind::Group { wr.contains_rect(n.rect()) } else { wr.intersects(n.rect()) };
                        if hit {
                            sel.insert(n.id);
                        }
                    }
                    st.sel = sel;
                }
                Drag::Marquee { from, base }
            }
            Drag::Move { start, orig, mut moved } => {
                if let Some(p) = cur {
                    let w = to_w(&c.view, p);
                    let mut dw = w - start;
                    if !moved && (dw.length() * z) < 3.0 {
                        st.drag = Drag::Move { start, orig, moved };
                        return finish_pointer(ctx, ui, ws, page, resp, c, st, out, origin, mods, cur);
                    }
                    if !moved {
                        c.checkpoint();
                        moved = true;
                    }
                    // bounding box of everything that moves
                    let mut bb: Option<Rect> = None;
                    for (id, o) in &orig {
                        if let Some(n) = c.node(*id) {
                            let r = Rect::from_min_size(*o, vec2(n.w, n.h));
                            bb = Some(bb.map_or(r, |b| b.union(r)));
                        }
                    }
                    st.guides.clear();
                    if let (Some(b), false) = (bb, mods.alt) {
                        let moved_bb = b.translate(dw);
                        let moving: BTreeSet<u64> = orig.iter().map(|o| o.0).collect();
                        let (adj, guides) = snap_to_others(c, &moving, moved_bb, 7.0 / z);
                        dw += adj;
                        st.guides = guides;
                        if c.snap {
                            let nb = b.translate(dw);
                            if !st.guides.iter().any(|g| g.0) {
                                dw.x += cv::snap_v(nb.min.x) - nb.min.x;
                            }
                            if !st.guides.iter().any(|g| !g.0) {
                                dw.y += cv::snap_v(nb.min.y) - nb.min.y;
                            }
                        }
                    }
                    for (id, o) in &orig {
                        if let Some(n) = c.node_mut(*id) {
                            n.x = o.x + dw.x;
                            n.y = o.y + dw.y;
                        }
                    }
                    c.rev += 1;
                }
                Drag::Move { start, orig, moved }
            }
            Drag::Resize { id, corner, orig, pts, mut changed } => {
                if let Some(p) = cur {
                    let mut w = to_w(&c.view, p);
                    if c.snap && !mods.alt {
                        w = pos2(cv::snap_v(w.x), cv::snap_v(w.y));
                    }
                    let kind = c.node(id).map_or(Kind::Note, |n| n.kind);
                    let min = kind.min_size();
                    let (fx, fy) = ((corner & 1) as f32, (corner >> 1) as f32); // 0 = left/top corner, 1 = right/bottom
                    let anchor = pos2(if fx > 0.5 { orig.left() } else { orig.right() }, if fy > 0.5 { orig.top() } else { orig.bottom() });
                    let mut nw = ((w.x - anchor.x) * if fx > 0.5 { 1.0 } else { -1.0 }).max(min.x);
                    let mut nh = ((w.y - anchor.y) * if fy > 0.5 { 1.0 } else { -1.0 }).max(min.y);
                    if kind == Kind::Image && orig.height() > 1.0 {
                        let ratio = orig.height() / orig.width();
                        nh = nw * ratio;
                    }
                    if kind == Kind::Ink || (mods.shift && orig.height() > 1.0) {
                        let ratio = orig.height() / orig.width().max(1.0);
                        if nw * ratio >= nh {
                            nh = nw * ratio;
                        } else {
                            nw = nh / ratio.max(1e-3);
                        }
                    }
                    if !changed {
                        c.checkpoint();
                        changed = true;
                    }
                    let x = if fx > 0.5 { anchor.x } else { anchor.x - nw };
                    let y = if fy > 0.5 { anchor.y } else { anchor.y - nh };
                    if let Some(n) = c.node_mut(id) {
                        n.x = x;
                        n.y = y;
                        n.w = nw;
                        n.h = nh;
                        if kind == Kind::Ink && !pts.is_empty() {
                            let (sx, sy) = (nw / orig.width().max(1e-3), nh / orig.height().max(1e-3));
                            n.pts = pts.iter().map(|p| [p[0] * sx, p[1] * sy]).collect();
                        }
                    }
                    c.rev += 1;
                }
                Drag::Resize { id, corner, orig, pts, changed }
            }
            Drag::Connect { from, side, .. } => Drag::Connect { from, side, to: cur.map(|p| to_w(&c.view, p)).unwrap_or(Pos2::ZERO) },
            Drag::Ink { mut pts } => {
                if let Some(p) = cur {
                    let w = to_w(&c.view, p);
                    if pts.last().map_or(true, |l| (*l - w).length() * z > 1.2) {
                        pts.push(w);
                    }
                }
                Drag::Ink { pts }
            }
        };
        ctx.request_repaint();
    }
    finish_pointer(ctx, ui, ws, page, resp, c, st, out, origin, mods, cur);
    let _ = over_canvas;
}

/// Releases, clicks, double clicks and the context menu.
#[allow(clippy::too_many_arguments)]
fn finish_pointer(ctx: &Context, ui: &Ui, ws: &mut Workspace, page: u64, resp: &egui::Response, c: &mut Canvas, st: &mut St, out: &mut Out, origin: Pos2, mods: Modifiers, cur: Option<Pos2>) {
    let view = c.view;
    let to_w = move |p: Pos2| view.to_world(origin, p);
    // ── release ──
    if resp.drag_stopped() || (!matches!(st.drag, Drag::None) && !ctx.input(|i| i.pointer.any_down())) {
        let drag = std::mem::take(&mut st.drag);
        st.guides.clear();
        match drag {
            Drag::Connect { from, side, to } => {
                let target = c.hit_node(to, 4.0 / c.view.zoom, 34.0).filter(|t| *t != from);
                let far = c.node(from).map_or(false, |n| !n.rect().expand(30.0 / c.view.zoom).contains(to));
                match target {
                    Some(t) => {
                        c.checkpoint();
                        if let Some(e) = c.connect(from, t, side, Side::Auto) {
                            st.sel_edge = Some(e);
                            st.sel.clear();
                        }
                    }
                    None if far => {
                        c.checkpoint();
                        let id = c.add_node(Kind::Note, to + vec2(60.0, 0.0) * if side == Side::Left { -1.0 } else { 1.0 });
                        let from_color = c.node(from).map_or(0, |f| f.color);
                        if let Some(n) = c.node_mut(id) {
                            n.h = 64.0;
                            n.w = 200.0;
                            n.x = to.x - if side == Side::Left { 200.0 } else { 0.0 };
                            n.y = to.y - 32.0;
                            n.color = from_color;
                        }
                        let ts = match side {
                            Side::Left => Side::Right,
                            Side::Top => Side::Bottom,
                            Side::Bottom => Side::Top,
                            _ => Side::Left,
                        };
                        c.connect(from, id, side, ts);
                        select_only(st, [id]);
                        st.edit = Some(Edit::Node(id));
                        st.focus_edit = true;
                    }
                    None => {}
                }
            }
            Drag::Ink { pts } => {
                if pts.len() >= 2 {
                    c.checkpoint();
                    c.add_ink(&pts, st.pen_color, st.pen_size);
                }
            }
            Drag::Move { moved: true, orig, .. } => {
                // a card dropped inside a group stays selected; nothing else to do — connectors follow automatically
                let _ = orig;
            }
            _ => {}
        }
    }

    // ── context menu (right click / press-and-hold) ──
    let rc = if resp.secondary_clicked() { resp.interact_pointer_pos() } else { widgets::right_clicked(ui, resp.rect) };
    if let Some(p) = rc {
        let w = to_w(p);
        let target = if let Some(id) = c.hit_node(w, hit_slack(st, c), 34.0) {
            if !st.sel.contains(&id) {
                select_only(st, [id]);
            }
            Target::Node(id)
        } else if let Some(e) = c.hit_edge(w, 8.0 / c.view.zoom) {
            st.sel.clear();
            st.sel_edge = Some(e);
            Target::Edge(e)
        } else {
            Target::Empty
        };
        end_edit(ctx, st, c);
        st.menu = Some(MenuState { kind: MenuKind::Context(target), pos: p, world: w });
        st.fresh = true;
        return;
    }

    // ── double click ──
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos().or(cur) {
            let w = to_w(p);
            if let Some(id) = c.hit_node(w, hit_slack(st, c), 34.0) {
                select_only(st, [id]);
                let n = c.node(id).cloned();
                if let Some(n) = n {
                    match n.kind {
                        Kind::Page => {
                            if let Some(pg) = n.page {
                                out.open = Some(pg);
                            } else {
                                st.pop = Some(Pop::Page(id));
                                st.pop_fresh = true;
                            }
                        }
                        Kind::Link => {
                            if n.url.is_empty() {
                                st.pop = Some(Pop::Link(id));
                                st.pop_fresh = true;
                            } else {
                                platform::open_url(ctx, &crate::media::normalize_url(&n.url));
                            }
                        }
                        Kind::Image => {
                            st.pop = Some(Pop::Image(id));
                            st.pop_fresh = true;
                        }
                        Kind::Ink => {}
                        _ => begin_edit(st, c, Edit::Node(id)),
                    }
                }
            } else if let Some(e) = c.hit_edge(w, 8.0 / c.view.zoom) {
                st.sel_edge = Some(e);
                st.sel.clear();
                begin_edit(st, c, Edit::Edge(e));
            } else if st.tool == Tool::Select {
                add_at(st, c, Kind::Note, w);
            }
        }
        return;
    }

    // ── click ──
    if resp.clicked() && matches!(st.drag, Drag::None) {
        let Some(p) = resp.interact_pointer_pos().or(cur) else { return };
        let w = to_w(p);
        // check boxes first
        if let Some((_, id, line)) = st.checks.iter().rev().find(|(r, _, _)| r.contains(p)).copied() {
            if c.node(id).map_or(false, |n| !n.locked) {
                c.checkpoint();
                if let Some(n) = c.node_mut(id) {
                    n.text = cv::toggle_check(&n.text, line);
                }
                c.rev += 1;
                select_only(st, [id]);
                return;
            }
        }
        if let Some((_, id)) = st.opens.iter().rev().find(|(r, _)| r.contains(p)).copied() {
            if let Some(n) = c.node(id).cloned() {
                match n.kind {
                    Kind::Page => out.open = n.page,
                    Kind::Link => platform::open_url(ctx, &crate::media::normalize_url(&n.url)),
                    _ => {}
                }
                return;
            }
        }
        match c.hit_node(w, hit_slack(st, c), 34.0) {
            Some(id) => {
                if mods.shift {
                    if !st.sel.remove(&id) {
                        st.sel.insert(id);
                    }
                } else {
                    select_only(st, [id]);
                }
                st.sel_edge = None;
                // an empty picture / link / page card asks for its content on a tap
                if let Some(n) = c.node(id) {
                    let empty_pop = match n.kind {
                        Kind::Image if n.url.is_empty() => Some(Pop::Image(id)),
                        Kind::Link if n.url.is_empty() => Some(Pop::Link(id)),
                        Kind::Page if n.page.is_none() => Some(Pop::Page(id)),
                        _ => None,
                    };
                    if let Some(pp) = empty_pop {
                        st.pop = Some(pp);
                        st.pop_fresh = true;
                        st.page_query.clear();
                    }
                }
            }
            None => {
                if let Some(e) = c.hit_edge(w, 9.0 / c.view.zoom) {
                    st.sel_edge = Some(e);
                    st.sel.clear();
                } else if !mods.shift {
                    select_only(st, []);
                }
                end_edit(ctx, st, c);
            }
        }
    }
    let _ = (ws, page);
}

/// Snap a moving box to the edges / centres of other cards. Returns the correction and the guide lines (vertical?, coordinate).
fn snap_to_others(c: &Canvas, moving: &BTreeSet<u64>, bb: Rect, thr: f32) -> (Vec2, Vec<(bool, f32)>) {
    let xs = [bb.left(), bb.center().x, bb.right()];
    let ys = [bb.top(), bb.center().y, bb.bottom()];
    let mut best_x: Option<f32> = None;
    let mut best_y: Option<f32> = None;
    let mut gx = vec![];
    let mut gy = vec![];
    for n in c.nodes.iter().filter(|n| !moving.contains(&n.id) && n.kind != Kind::Group && n.kind != Kind::Ink) {
        let r = n.rect();
        for tx in [r.left(), r.center().x, r.right()] {
            for mx in xs {
                let d = tx - mx;
                if d.abs() < thr && best_x.map_or(true, |b| d.abs() < b.abs()) {
                    best_x = Some(d);
                    gx = vec![(true, tx)];
                }
            }
        }
        for ty in [r.top(), r.center().y, r.bottom()] {
            for my in ys {
                let d = ty - my;
                if d.abs() < thr && best_y.map_or(true, |b| d.abs() < b.abs()) {
                    best_y = Some(d);
                    gy = vec![(false, ty)];
                }
            }
        }
    }
    gx.extend(gy);
    (vec2(best_x.unwrap_or(0.0), best_y.unwrap_or(0.0)), gx)
}

// ───────────────────────────── world-space overlays ─────────────────────────────

fn draw_overlays_world(f: &Frame, c: &Canvas, st: &St, ctx: &Context) {
    let pal = f.pal;
    let touchy = touch::touchy();
    // guides
    for (vertical, v) in &st.guides {
        let col = with_alpha(Color32::from_rgb(235, 87, 87), 0.8);
        if *vertical {
            let x = f.view.to_screen(f.origin, pos2(*v, 0.0)).x;
            f.p.line_segment([pos2(x, f.clip.top()), pos2(x, f.clip.bottom())], Stroke::new(1.0_f32, col));
        } else {
            let y = f.view.to_screen(f.origin, pos2(0.0, *v)).y;
            f.p.line_segment([pos2(f.clip.left(), y), pos2(f.clip.right(), y)], Stroke::new(1.0_f32, col));
        }
    }
    // selection outlines
    let multi = st.sel.len() > 1;
    for id in &st.sel {
        let Some(n) = c.node(*id) else { continue };
        let r = f.s(n.rect());
        if n.kind == Kind::Ink {
            f.p.rect_stroke(r.expand(4.0), 4.0, Stroke::new(1.2_f32, pal.accent), StrokeKind::Outside);
        } else {
            let rad = (10.0 * f.z()).clamp(2.0, 14.0);
            f.p.rect_stroke(r.expand(1.0), rad + 1.0, Stroke::new(if multi { 1.4_f32 } else { 2.0_f32 }, pal.accent), StrokeKind::Outside);
        }
    }
    if multi {
        if let Some(b) = sel_bounds(c, st) {
            let r = f.s(b).expand(6.0);
            for s in egui::Shape::dashed_line(&[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()], Stroke::new(1.0_f32, with_alpha(pal.accent, 0.7)), 5.0, 4.0) {
                f.p.add(s);
            }
        }
    }
    // handles
    if let Some(id) = handle_node(st, c) {
        if let Some(n) = c.node(id) {
            let r = f.s(n.rect());
            let connecting = matches!(st.drag, Drag::Connect { .. });
            if !matches!(st.drag, Drag::Move { moved: true, .. } | Drag::Pan) {
                let rad = if touchy { 8.5 } else { 5.5 };
                for (side, hp) in handle_sides(r) {
                    let active = matches!(st.drag, Drag::Connect { from, side: s2, .. } if from == id && s2 == side);
                    f.p.circle_filled(hp, rad + 1.0, with_alpha(Color32::BLACK, 0.10));
                    f.p.circle_filled(hp, rad, if active { pal.accent } else { pal.card });
                    f.p.circle_stroke(hp, rad, Stroke::new(1.5_f32, pal.accent));
                }
                if st.sel.len() == 1 && st.sel.contains(&id) && !n.locked && !connecting {
                    let hs = if touchy { 11.0 } else { 7.5 };
                    for cp in corner_pts(r) {
                        let hr = Rect::from_center_size(cp, vec2(hs, hs));
                        f.p.rect_filled(hr, 2.5, pal.card);
                        f.p.rect_stroke(hr, 2.5, Stroke::new(1.5_f32, pal.accent), StrokeKind::Inside);
                    }
                }
            }
        }
    }
    // live gestures
    match &st.drag {
        Drag::Marquee { from, .. } => {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                let r = Rect::from_two_pos(*from, p);
                f.p.rect_filled(r, 3.0, with_alpha(pal.accent, 0.08));
                f.p.rect_stroke(r, 3.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.8)), StrokeKind::Inside);
            }
        }
        Drag::Connect { from, side, to } => {
            if let Some(n) = c.node(*from) {
                let target = c.hit_node(*to, 4.0 / f.z(), 34.0).filter(|t| t != from);
                let end_rect = match target.and_then(|t| c.node(t)) {
                    Some(t) => t.rect(),
                    None => Rect::from_center_size(*to, vec2(1.0, 1.0)),
                };
                let (pts, d, _) = cv::route(n.rect(), end_rect, *side, Side::Auto, Shape::Curve);
                let spts: Vec<Pos2> = pts.iter().map(|p| f.pt(*p)).collect();
                for s in egui::Shape::dashed_line(&spts, Stroke::new(2.0_f32, pal.accent), 7.0, 5.0) {
                    f.p.add(s);
                }
                if let Some(e) = spts.last() {
                    f.p.circle_filled(*e, 4.0, pal.accent);
                    let _ = d;
                }
                if let Some(t) = target.and_then(|t| c.node(t)) {
                    f.p.rect_stroke(f.s(t.rect()).expand(3.0), 12.0, Stroke::new(2.5_f32, with_alpha(pal.accent, 0.8)), StrokeKind::Outside);
                }
            }
        }
        Drag::Ink { pts } => {
            if pts.len() >= 2 {
                let sp: Vec<Pos2> = pts.iter().map(|p| f.pt(*p)).collect();
                let w = ((2.2 + st.pen_size as f32 * 1.9) * f.z()).clamp(1.0, 14.0);
                f.p.add(egui::Shape::line(sp, Stroke::new(w, draw::accent(pal, st.pen_color).unwrap_or(pal.text))));
            }
        }
        _ => {}
    }
}

// ───────────────────────────── text editing ─────────────────────────────

fn edit_rect(c: &Canvas, e: Edit, origin: Pos2) -> Rect {
    match e {
        Edit::Node(id) => match c.node(id) {
            Some(n) => {
                let r = c.view.rect_to_screen(origin, n.rect());
                match n.kind {
                    Kind::Group => Rect::from_min_size(r.min + vec2(10.0, 8.0) * c.view.zoom.max(0.8), vec2((r.width() - 20.0).clamp(80.0, 260.0), 28.0 * c.view.zoom.max(0.8))),
                    _ => r,
                }
            }
            None => Rect::NOTHING,
        },
        Edit::Edge(id) => match c.edge(id).and_then(|e| c.edge_route(e)) {
            Some((pts, _, _)) => Rect::from_center_size(c.view.to_screen(origin, cv::path_mid(&pts)), vec2(150.0, 30.0)),
            None => Rect::NOTHING,
        },
    }
}

/// Enter inside a list line continues the list (`- `, `[ ] `, `1. `); an empty item ends it. Returns true when handled.
fn list_continue(ctx: &Context, eid: Id, text: &mut String) -> bool {
    let Some(mut s) = TextEditState::load(ctx, eid) else { return false };
    let Some(rng) = s.cursor.char_range() else { return false };
    if rng.primary != rng.secondary {
        return false;
    }
    let idx = rng.primary.index;
    let byte = text.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(text.len());
    let line_start = text[..byte].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = text[byte..].find('\n').map(|p| byte + p).unwrap_or(text.len());
    let line = text[line_start..line_end].to_owned();
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let (marker, rest): (String, &str) = if let Some(r) = trimmed.strip_prefix("- [ ] ").or_else(|| trimmed.strip_prefix("- [x] ")).or_else(|| trimmed.strip_prefix("- [X] ")) {
        ("- [ ] ".into(), r)
    } else if let Some(r) = trimmed.strip_prefix("[ ] ").or_else(|| trimmed.strip_prefix("[x] ")).or_else(|| trimmed.strip_prefix("[X] ")) {
        ("[ ] ".into(), r)
    } else if trimmed == "[ ]" || trimmed == "[x]" {
        ("[ ] ".into(), "")
    } else if let Some(r) = trimmed.strip_prefix("- ") {
        ("- ".into(), r)
    } else if let Some(r) = trimmed.strip_prefix("* ") {
        ("* ".into(), r)
    } else if let Some(r) = trimmed.strip_prefix("> ") {
        ("> ".into(), r)
    } else if let Some((num, r)) = trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() < 4 && n.chars().all(|ch| ch.is_ascii_digit())) {
        (format!("{}. ", num.parse::<u32>().unwrap_or(0) + 1), r)
    } else {
        return false;
    };
    if rest.trim().is_empty() {
        // empty item: leave the list
        text.replace_range(line_start..line_end, "");
        s.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(text[..line_start].chars().count()))));
    } else {
        let ins = format!("\n{indent}{marker}");
        text.insert_str(byte, &ins);
        s.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(idx + ins.chars().count()))));
    }
    s.store(ctx, eid);
    true
}

fn edit_overlay(ui: &mut Ui, ctx: &Context, c: &mut Canvas, st: &mut St, pal: &Palette) {
    let Some(e) = st.edit else { return };
    let origin = st.rect.map_or(Pos2::ZERO, |r| r.min);
    let z = c.view.zoom;
    let eid = Id::new(("cnv_edit", match e {
        Edit::Node(i) => i,
        Edit::Edge(i) => i + (1 << 40),
    }));
    let focus_now = std::mem::take(&mut st.focus_edit);
    let r = edit_rect(c, e, origin);
    let mut done = false;
    match e {
        Edit::Node(id) => {
            let Some(n) = c.node(id).cloned() else { return };
            let mut text = n.text.clone();
            let multiline = is_text_kind(n.kind);
            let px = 14.5 * z;
            let (font, inner) = match n.kind {
                Kind::Text => (bold(px * [1.25, 1.9, 2.8][n.size.min(2) as usize]), r.shrink2(vec2(4.0 * z, 4.0 * z))),
                Kind::Group => (bold(13.5 * z.max(0.8)), r.shrink2(vec2(6.0, 2.0))),
                _ => (body(px), r.shrink2(vec2(14.0 * z, 12.0 * z))),
            };
            if n.kind == Kind::Group {
                ui.painter().rect_filled(r, r.height() / 2.0, pal.card);
                ui.painter().rect_stroke(r, r.height() / 2.0, Stroke::new(1.5_f32, pal.accent), StrokeKind::Inside);
            }
            if multiline && ctx.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift && !i.modifiers.command) && ctx.memory(|m| m.has_focus(eid)) && list_continue(ctx, eid, &mut text) {
                ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
            }
            let (resp_lost, galley_h) = {
                let inner_resp = ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                    let te = if multiline { TextEdit::multiline(&mut text) } else { TextEdit::singleline(&mut text) };
                    te.id(eid).frame(false).font(font).text_color(pal.text).desired_width(inner.width()).margin(Margin::ZERO).lock_focus(multiline).show(ui)
                });
                let o = inner_resp.inner;
                if focus_now {
                    o.response.request_focus();
                    if let Some(mut s) = TextEditState::load(ctx, eid) {
                        s.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(text.chars().count()))));
                        s.store(ctx, eid);
                    }
                }
                (o.response.lost_focus() && !focus_now, o.galley.size().y)
            };
            if text != n.text {
                if let Some(nn) = c.node_mut(id) {
                    nn.text = text;
                }
                c.rev += 1;
            }
            if multiline {
                let pad = if n.kind == Kind::Text { 10.0 } else if n.kind == Kind::Todo { 34.0 } else { 24.0 };
                let need = galley_h / z + pad;
                if let Some(nn) = c.node_mut(id) {
                    if need > nn.h + 0.5 {
                        nn.h = need.ceil();
                    }
                }
            }
            if resp_lost {
                done = true;
            }
        }
        Edit::Edge(id) => {
            let Some(ed) = c.edge(id).cloned() else { return };
            let mut text = ed.label.clone();
            ui.painter().rect_filled(r, 15.0, pal.card);
            ui.painter().rect_stroke(r, 15.0, Stroke::new(1.5_f32, pal.accent), StrokeKind::Inside);
            let lost = {
                let inner_resp = ui.scope_builder(UiBuilder::new().max_rect(r.shrink2(vec2(12.0, 6.0))), |ui| TextEdit::singleline(&mut text).id(eid).frame(false).font(body(13.0)).hint_text("Label").desired_width(r.width() - 24.0).margin(Margin::ZERO).show(ui));
                if focus_now {
                    inner_resp.inner.response.request_focus();
                }
                inner_resp.inner.response.lost_focus() && !focus_now
            };
            if text != ed.label {
                if let Some(e2) = c.edge_mut(id) {
                    e2.label = text;
                }
                c.rev += 1;
            }
            if lost {
                done = true;
            }
        }
    }
    if done {
        end_edit(ctx, st, c);
    }
}

// ───────────────────────────── chrome ─────────────────────────────

fn pill(ui: &Ui, pal: &Palette, r: Rect, rad: f32) {
    let p = ui.painter();
    p.rect_filled(r.translate(vec2(0.0, 3.0)).expand(1.0), rad + 1.0, Color32::from_black_alpha(if draw::is_dark(pal) { 70 } else { 22 }));
    p.rect_filled(r, rad, pal.popup);
    p.rect_stroke(r, rad, Stroke::new(1.0_f32, pal.border), StrokeKind::Inside);
}

#[allow(clippy::too_many_arguments)]
fn tbtn(ui: &mut Ui, pal: &Palette, rect: Rect, id: Id, glyph: &str, active: bool, tip: &str, enabled: bool) -> bool {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let hov = resp.hovered() && enabled;
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 9.0, pal.accent_soft);
    } else if hov || (resp.is_pointer_button_down_on() && enabled) {
        p.rect_filled(rect, 9.0, pal.hover);
    }
    let col = if !enabled { with_alpha(pal.muted, 0.5) } else if active { pal.accent } else { pal.text };
    icons::glyph(p, rect.center(), 18.0, glyph, col);
    if !tip.is_empty() && !touch::touchy() {
        resp.clone().on_hover_text(tip);
    }
    enabled && resp.clicked()
}

fn title_chip(ui: &mut Ui, ws: &mut Workspace, page: u64, pal: &Palette, rect: Rect, compact: bool) {
    let m = if compact { 8.0 } else { 14.0 };
    let right_w = if compact { 104.0 } else { 230.0 };
    let w = (rect.width() - right_w - m * 3.0).clamp(0.0, 340.0);
    if w < 90.0 {
        return;
    }
    let r = Rect::from_min_size(rect.min + vec2(m, m), vec2(w, 38.0));
    pill(ui, pal, r, 12.0);
    let Some(p) = ws.page_mut(page) else { return };
    let icon = if p.icon.is_empty() { l::NETWORK.to_owned() } else { p.icon.clone() };
    let ic = pos2(r.left() + 20.0, r.center().y);
    if icon.starts_with(|ch: char| (ch as u32) >= 0xE000 && (ch as u32) <= 0xF8FF) {
        icons::glyph(ui.painter(), ic, 17.0, &icon, pal.muted);
    } else {
        icons::paint_page_icon(ui, ic, &icon, 18.0, pal.text);
    }
    let te = Rect::from_min_max(pos2(r.left() + 38.0, r.top() + 4.0), pos2(r.right() - 10.0, r.bottom() - 4.0));
    let before = p.title.clone();
    ui.scope_builder(UiBuilder::new().max_rect(te), |ui| {
        ui.add(TextEdit::singleline(&mut p.title).id(Id::new(("title", page))).frame(false).font(bold(15.0)).text_color(pal.text).hint_text("Untitled canvas").desired_width(te.width()).vertical_align(egui::Align::Center).margin(Margin::symmetric(0, 6)));
    });
    if p.title != before {
        p.updated = crate::auth::now_ms();
    }
}

#[allow(clippy::too_many_arguments)]
fn top_right(ui: &mut Ui, ctx: &Context, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, out: &mut Out, pal: &Palette, rect: Rect, compact: bool) {
    let m = if compact { 8.0 } else { 14.0 };
    let bs = if touch::touchy() { 40.0 } else { 34.0 };
    let n = if compact { 2 } else { 5 };
    let pct_w = if compact { 0.0 } else { 52.0 };
    let w = bs * (n as f32 - if compact { 0.0 } else { 1.0 }) + pct_w + 10.0;
    let r = Rect::from_min_size(pos2(rect.right() - m - w, rect.top() + m), vec2(w, bs + 4.0));
    pill(ui, pal, r, 12.0);
    let mut x = r.left() + 5.0;
    let y = r.top() + 2.0;
    let mut next = |wd: f32| {
        let b = Rect::from_min_size(pos2(x, y), vec2(wd, bs));
        x += wd;
        b
    };
    let at = center_world(st, c);
    let mut run = |a: Act, st: &mut St, c: &mut Canvas, ws: &mut Workspace, out: &mut Out| act(a, st, c, ws, ctx, page, out, at);
    if !compact {
        let b = next(bs);
        if tbtn(ui, pal, b, Id::new("cnv_zo"), l::MINUS, false, "Zoom out  (−)", true) {
            if let Some(sr) = st.rect {
                let mut v = st.anim.unwrap_or(c.view);
                v.zoom_at(sr.min, sr.center(), 0.8);
                animate_to(st, v);
            }
        }
        let b = next(pct_w);
        let resp = ui.interact(b, Id::new("cnv_pct"), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(b, 9.0, pal.hover);
        }
        ui.painter().text(b.center(), Align2::CENTER_CENTER, format!("{}%", (c.view.zoom * 100.0).round() as i32), body(13.0), pal.text);
        if resp.on_hover_text("Reset to 100%").clicked() {
            run(Act::Reset, st, c, ws, out);
        }
        let b = next(bs);
        if tbtn(ui, pal, b, Id::new("cnv_zi"), l::PLUS, false, "Zoom in  (+)", true) {
            if let Some(sr) = st.rect {
                let mut v = st.anim.unwrap_or(c.view);
                v.zoom_at(sr.min, sr.center(), 1.25);
                animate_to(st, v);
            }
        }
    }
    let b = next(bs);
    if tbtn(ui, pal, b, Id::new("cnv_fit"), l::MAXIMIZE, false, "Fit to content  (Shift+1)", true) {
        run(Act::Fit, st, c, ws, out);
    }
    let b = next(bs);
    if tbtn(ui, pal, b, Id::new("cnv_more"), l::ELLIPSIS, matches!(st.menu, Some(MenuState { kind: MenuKind::Canvas, .. })), "Canvas options", true) {
        st.menu = Some(MenuState { kind: MenuKind::Canvas, pos: pos2(b.right(), b.bottom() + 6.0), world: at });
        st.fresh = true;
    }
}

const KINDS: [(Kind, &str, &str, &str); 7] = [
    (Kind::Note, l::STICKY_NOTE, "Card  (N)", "Card"),
    (Kind::Text, l::TYPE, "Text  (T)", "Text"),
    (Kind::Todo, l::LIST_TODO, "To-do list  (L)", "To-do list"),
    (Kind::Page, l::FILE_TEXT, "Page", "Page"),
    (Kind::Image, l::IMAGE, "Image", "Image"),
    (Kind::Link, l::LINK, "Link", "Link"),
    (Kind::Group, l::FOCUS, "Group  (G)", "Group"),
];

#[allow(clippy::too_many_arguments)]
fn dock(ui: &mut Ui, ctx: &Context, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, out: &mut Out, pal: &Palette, rect: Rect, compact: bool) {
    let _ = (ws, page, out, ctx);
    let bs = if touch::touchy() { 44.0 } else { 38.0 };
    let tools = [(Tool::Select, l::MOUSE_POINTER_2, "Select  (V)"), (Tool::Hand, l::MOVE, "Hand — drag to pan  (H)"), (Tool::Pen, l::PEN_LINE, "Pen  (P)")];
    let n_add = if compact { 1 } else { KINDS.len() };
    let items = tools.len() + n_add;
    let sep = 12.0;
    let w = items as f32 * bs + sep + 12.0;
    let m = if compact { 10.0 } else { 18.0 };
    let r = Rect::from_min_size(pos2(rect.center().x - w / 2.0, rect.bottom() - m - bs - 8.0), vec2(w, bs + 8.0));
    // pen options above the dock
    if st.tool == Tool::Pen {
        let cw = 26.0;
        let pw = (PEN_COLORS.len() + 3) as f32 * cw + 28.0;
        let pr = Rect::from_min_size(pos2(rect.center().x - pw / 2.0, r.top() - 50.0), vec2(pw, 40.0));
        pill(ui, pal, pr, 12.0);
        let mut x = pr.left() + 10.0;
        for col in PEN_COLORS {
            let cc = pos2(x + cw / 2.0, pr.center().y);
            let hit = Rect::from_center_size(cc, vec2(cw, 34.0));
            let resp = ui.interact(hit, Id::new(("cnv_pc", col)), Sense::click());
            let color = draw::accent(pal, col).unwrap_or(pal.text);
            ui.painter().circle_filled(cc, 8.5, color);
            if st.pen_color == col {
                ui.painter().circle_stroke(cc, 12.0, Stroke::new(1.8_f32, color));
            }
            if resp.clicked() {
                st.pen_color = col;
                // recolour the stroke that is selected
                if let Some(id) = st.sel.iter().next().copied().filter(|_| st.sel.len() == 1) {
                    if c.node(id).map_or(false, |n| n.kind == Kind::Ink) {
                        c.checkpoint();
                        if let Some(n) = c.node_mut(id) {
                            n.color = col;
                        }
                        c.rev += 1;
                    }
                }
            }
            x += cw;
        }
        x += 8.0;
        for s in 0..3u8 {
            let cc = pos2(x + cw / 2.0, pr.center().y);
            let hit = Rect::from_center_size(cc, vec2(cw, 34.0));
            let resp = ui.interact(hit, Id::new(("cnv_ps", s)), Sense::click());
            ui.painter().circle_filled(cc, 2.5 + s as f32 * 2.0, if st.pen_size == s { pal.accent } else { pal.muted });
            if resp.clicked() {
                st.pen_size = s;
            }
            x += cw;
        }
    }
    pill(ui, pal, r, 16.0);
    let mut x = r.left() + 6.0;
    let y = r.top() + 4.0;
    for (t, g, tip) in tools {
        let b = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
        if tbtn(ui, pal, b, Id::new(("cnv_tool", t as u8)), g, st.tool == t, tip, true) {
            st.tool = if st.tool == t && t != Tool::Select { Tool::Select } else { t };
        }
        x += bs;
    }
    ui.painter().vline(x + sep / 2.0, (r.top() + 10.0)..=(r.bottom() - 10.0), Stroke::new(1.0_f32, pal.border));
    x += sep;
    if compact {
        let b = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
        if tbtn(ui, pal, b, Id::new("cnv_plus"), l::PLUS, matches!(st.menu, Some(MenuState { kind: MenuKind::Add, .. })), "", true) {
            st.menu = Some(MenuState { kind: MenuKind::Add, pos: pos2(b.left() - 60.0, b.top() - 6.0), world: center_world(st, c) });
            st.fresh = true;
        }
    } else {
        for (kind, g, tip, _) in KINDS {
            let b = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
            if tbtn(ui, pal, b, Id::new(("cnv_add", kind as u8)), g, false, tip, true) {
                let at = spawn_point(st, c);
                add_at(st, c, kind, at);
            }
            x += bs;
        }
    }
}

fn find_bar(ui: &mut Ui, ctx: &Context, c: &mut Canvas, st: &mut St, pal: &Palette, rect: Rect) {
    let w = 320.0_f32.min(rect.width() - 24.0);
    let r = Rect::from_min_size(pos2(rect.center().x - w / 2.0, rect.top() + if touch::compact() { 54.0 } else { 14.0 }), vec2(w, 40.0));
    pill(ui, pal, r, 12.0);
    let mut q = st.find.clone().unwrap_or_default();
    let eid = Id::new("cnv_find");
    let te = Rect::from_min_max(pos2(r.left() + 38.0, r.top() + 5.0), pos2(r.right() - 96.0, r.bottom() - 5.0));
    icons::glyph(ui.painter(), pos2(r.left() + 20.0, r.center().y), 16.0, l::SEARCH, pal.muted);
    let resp = ui.scope_builder(UiBuilder::new().max_rect(te), |ui| TextEdit::singleline(&mut q).id(eid).frame(false).font(body(14.0)).hint_text("Find on canvas").desired_width(te.width()).margin(Margin::symmetric(0, 6)).show(ui)).inner.response;
    if st.focus_find {
        resp.request_focus();
        st.focus_find = false;
    }
    let matches = c.find(&q);
    if q != st.find.clone().unwrap_or_default() {
        st.find_idx = 0;
    }
    st.find = Some(q);
    let label = if matches.is_empty() { "0".to_owned() } else { format!("{}/{}", st.find_idx % matches.len() + 1, matches.len()) };
    ui.painter().text(pos2(r.right() - 58.0, r.center().y), Align2::RIGHT_CENTER, label, body(12.5), pal.muted);
    let go = (resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter))) || tbtn(ui, pal, Rect::from_center_size(pos2(r.right() - 44.0, r.center().y), vec2(28.0, 28.0)), Id::new("cnv_find_next"), l::CHEVRON_DOWN, false, "Next match", !matches.is_empty());
    if go && !matches.is_empty() {
        st.find_idx = (st.find_idx + 1) % matches.len();
        let id = matches[st.find_idx];
        if let (Some(n), Some(sr)) = (c.node(id), st.rect) {
            let mut v = c.view;
            let target = n.center();
            let z = v.zoom.max(0.7);
            v.zoom = z;
            v.x = target.x - sr.width() / z / 2.0;
            v.y = target.y - sr.height() / z / 2.0;
            animate_to(st, v);
            select_only(st, [id]);
        }
        resp.request_focus();
    }
    if tbtn(ui, pal, Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(28.0, 28.0)), Id::new("cnv_find_x"), l::X, false, "Close", true) {
        st.find = None;
    }
}

fn present_bar(ui: &mut Ui, c: &mut Canvas, st: &mut St, pal: &Palette, rect: Rect) {
    let s = slides(c);
    let cur = st.present.unwrap_or(0).min(s.len().saturating_sub(1));
    let r = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 40.0), vec2(176.0, 40.0));
    pill(ui, pal, r, 20.0);
    if tbtn(ui, pal, Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(34.0, 34.0)), Id::new("cnv_pr_prev"), l::CHEVRON_LEFT, false, "Previous", cur > 0) {
        present_goto(st, c, cur - 1);
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, format!("{} / {}", cur + 1, s.len()), bold(13.5), pal.text);
    if tbtn(ui, pal, Rect::from_center_size(pos2(r.right() - 58.0, r.center().y), vec2(34.0, 34.0)), Id::new("cnv_pr_next"), l::CHEVRON_RIGHT, false, "Next", cur + 1 < s.len()) {
        present_goto(st, c, cur + 1);
    }
    if tbtn(ui, pal, Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), vec2(34.0, 34.0)), Id::new("cnv_pr_x"), l::X, false, "Exit", true) {
        st.present = None;
    }
}

fn minimap(ui: &mut Ui, c: &mut Canvas, st: &mut St, pal: &Palette, rect: Rect) {
    let mr = Rect::from_min_size(pos2(rect.right() - 14.0 - 176.0, rect.bottom() - 14.0 - 116.0), vec2(176.0, 116.0));
    let resp = ui.interact(mr, Id::new("cnv_minimap"), Sense::click_and_drag());
    let origin = rect.min;
    let vw = Rect::from_min_max(c.view.to_world(origin, rect.min), c.view.to_world(origin, rect.max));
    let painter = ui.painter().clone();
    let (s, off) = {
        let f = Frame { p: &painter, ui: &*ui, pal, view: c.view, origin, clip: rect };
        draw::minimap(&f, c, mr, vw)
    };
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let w = pos2((p.x - off.x) / s, (p.y - off.y) / s);
            c.view.x = w.x - rect.width() / c.view.zoom / 2.0;
            c.view.y = w.y - rect.height() / c.view.zoom / 2.0;
            st.anim = None;
        }
    }
}

// ───────────────────────────── selection toolbar ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn selection_toolbar(ui: &mut Ui, ctx: &Context, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, out: &mut Out, pal: &Palette, rect: Rect, compact: bool) {
    if st.edit.is_some() || st.pop.is_some() || !matches!(st.drag, Drag::None) {
        return;
    }
    if st.tool == Tool::Pen && st.sel.is_empty() {
        return;
    }
    let origin = rect.min;
    let at = center_world(st, c);
    // which things are selected
    let (anchor, mode): (Rect, u8) = if let Some(e) = st.sel_edge {
        match c.edge(e).and_then(|e| c.edge_route(e)) {
            Some((pts, _, _)) => (Rect::from_center_size(c.view.to_screen(origin, cv::path_mid(&pts)), vec2(2.0, 2.0)), 2),
            None => return,
        }
    } else if let Some(b) = sel_bounds(c, st) {
        (c.view.rect_to_screen(origin, b), 1)
    } else {
        return;
    };
    if st.edit.is_some() {
        return;
    }
    let bs = if touch::touchy() { 40.0 } else { 34.0 };
    // buttons: (id, glyph, tip, act, active)
    #[derive(Clone)]
    enum B {
        Color,
        Act(Act, &'static str, &'static str, bool),
        Size,
        Dash(bool),
        Sep,
        More,
    }
    let single = st.sel.len() == 1;
    let first = st.sel.iter().next().and_then(|id| c.node(*id)).cloned();
    let mut btns: Vec<B> = vec![B::Color];
    if mode == 2 {
        let e = st.sel_edge.and_then(|e| c.edge(e)).cloned().unwrap_or_default();
        btns.push(B::Act(Act::EdgeShape, l::CHART_SPLINE, "Line shape", false));
        btns.push(B::Dash(e.dashed));
        btns.push(B::Act(Act::EdgeArrow, if e.arrow == Arrow::Both { l::ARROW_RIGHT_LEFT } else if e.arrow == Arrow::None { l::MINUS } else { l::ARROW_RIGHT }, "Arrow heads", false));
        btns.push(B::Act(Act::EdgeLabel, l::TEXT_CURSOR_INPUT, "Label", false));
        btns.push(B::Act(Act::EdgeReverse, l::ARROW_UP_DOWN, "Reverse direction", false));
        btns.push(B::Sep);
        btns.push(B::Act(Act::Delete, l::TRASH_2, "Delete", false));
    } else {
        if let (true, Some(n)) = (single, first.as_ref()) {
            match n.kind {
                Kind::Text | Kind::Ink => btns.push(B::Size),
                Kind::Page => btns.push(B::Act(Act::OpenPage, l::ARROW_UP_RIGHT, "Open page", false)),
                Kind::Link => btns.push(B::Act(Act::OpenLink, l::EXTERNAL_LINK, "Open link", false)),
                _ => {}
            }
            if matches!(n.kind, Kind::Image | Kind::Link | Kind::Page) {
                btns.push(B::Act(Act::ReplaceSrc, l::PENCIL, "Change", false));
            }
            if n.kind != Kind::Ink {
                btns.push(B::Act(Act::AddChild, l::CIRCLE_PLUS, "Add connected card  (Tab)", false));
            }
        }
        btns.push(B::Act(Act::Duplicate, l::COPY, "Duplicate  (Ctrl+D)", false));
        let all_locked = st.sel.iter().filter_map(|i| c.node(*i)).all(|n| n.locked);
        btns.push(B::Act(Act::Lock, if all_locked { l::LOCK } else { l::LOCK_OPEN }, if all_locked { "Unlock" } else { "Lock in place" }, all_locked));
        btns.push(B::Sep);
        btns.push(B::Act(Act::Delete, l::TRASH_2, "Delete  (Del)", false));
        btns.push(B::More);
    }
    let sep_w = 10.0;
    let w: f32 = btns.iter().map(|b| if matches!(b, B::Sep) { sep_w } else { bs }).sum::<f32>() + 10.0;
    let h = bs + 8.0;
    let mut x = (anchor.center().x - w / 2.0).clamp(rect.left() + 8.0, (rect.right() - w - 8.0).max(rect.left() + 8.0));
    let top_room = anchor.top() - rect.top();
    let ty = if top_room > h + 64.0 { anchor.top() - h - 18.0 } else { (anchor.bottom() + 18.0).min(rect.bottom() - h - if compact { 70.0 } else { 90.0 }) };
    let ty = ty.max(rect.top() + 56.0);
    let r = Rect::from_min_size(pos2(x, ty), vec2(w, h));
    pill(ui, pal, r, 12.0);
    x = r.left() + 5.0;
    let y = r.top() + 4.0;
    let mut color_btn: Option<Rect> = None;
    for (i, b) in btns.iter().enumerate() {
        match b {
            B::Sep => {
                ui.painter().vline(x + sep_w / 2.0, (r.top() + 10.0)..=(r.bottom() - 10.0), Stroke::new(1.0_f32, pal.border));
                x += sep_w;
            }
            B::Color => {
                let br = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
                let cur = if mode == 2 { st.sel_edge.and_then(|e| c.edge(e)).map_or(0, |e| e.color) } else { first.as_ref().map_or(0, |n| n.color) };
                let resp = ui.interact(br, Id::new(("cnv_tb", i)), Sense::click());
                if resp.hovered() || st.color_open {
                    ui.painter().rect_filled(br, 9.0, pal.hover);
                }
                let col = draw::accent(pal, cur).unwrap_or(pal.muted);
                ui.painter().circle_filled(br.center(), 8.0, col);
                ui.painter().circle_stroke(br.center(), 10.5, Stroke::new(1.2_f32, with_alpha(col, 0.5)));
                color_btn = Some(br);
                if resp.clicked() {
                    st.color_open = !st.color_open;
                }
                x += bs;
            }
            B::Act(a, g, tip, active) => {
                let br = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
                if tbtn(ui, pal, br, Id::new(("cnv_tb", i)), g, *active, tip, true) {
                    act(*a, st, c, ws, ctx, page, out, at);
                }
                x += bs;
            }
            B::Dash(on) => {
                let br = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
                let resp = ui.interact(br, Id::new(("cnv_tb", i)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(br, 9.0, pal.hover);
                }
                let st_ = Stroke::new(2.0_f32, if *on { pal.accent } else { pal.text });
                if *on {
                    for k in 0..3 {
                        let x0 = br.center().x - 9.0 + k as f32 * 7.0;
                        ui.painter().line_segment([pos2(x0, br.center().y), pos2(x0 + 4.0, br.center().y)], st_);
                    }
                } else {
                    ui.painter().line_segment([pos2(br.center().x - 9.0, br.center().y), pos2(br.center().x + 9.0, br.center().y)], st_);
                }
                if resp.on_hover_text("Dashed").clicked() {
                    act(Act::EdgeDash, st, c, ws, ctx, page, out, at);
                }
                x += bs;
            }
            B::Size => {
                let br = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
                if tbtn(ui, pal, br, Id::new(("cnv_tb", i)), l::TYPE, false, "Size", true) {
                    c.checkpoint();
                    let sel = st.sel.clone();
                    for n in c.nodes.iter_mut().filter(|n| sel.contains(&n.id)) {
                        n.size = (n.size + 1) % 3;
                    }
                    c.rev += 1;
                }
                x += bs;
            }
            B::More => {
                let br = Rect::from_min_size(pos2(x, y), vec2(bs, bs));
                if tbtn(ui, pal, br, Id::new(("cnv_tb", i)), l::ELLIPSIS, false, "More", true) {
                    let t = if single { first.as_ref().map(|n| Target::Node(n.id)).unwrap_or(Target::Empty) } else { Target::Node(*st.sel.iter().next().unwrap_or(&0)) };
                    st.menu = Some(MenuState { kind: MenuKind::Context(t), pos: pos2(br.left(), br.bottom() + 6.0), world: at });
                    st.fresh = true;
                }
                x += bs;
            }
        }
    }
    // colour row
    if st.color_open {
        if let Some(cb) = color_btn {
            let cw = 30.0;
            let pr = Rect::from_min_size(pos2(cb.left(), r.bottom() + 8.0), vec2(cw * cv::COLOR_COUNT as f32 + 12.0, 40.0));
            let pr = pr.translate(vec2((rect.right() - 8.0 - pr.right()).min(0.0), 0.0));
            pill(ui, pal, pr, 12.0);
            for col in 0..cv::COLOR_COUNT {
                let cc = pos2(pr.left() + 6.0 + cw * col as f32 + cw / 2.0, pr.center().y);
                let hit = Rect::from_center_size(cc, vec2(cw, 36.0));
                let resp = ui.interact(hit, Id::new(("cnv_col", col)), Sense::click());
                let color = draw::accent(pal, col).unwrap_or(pal.muted);
                if col == 0 {
                    ui.painter().circle_stroke(cc, 9.0, Stroke::new(1.6_f32, pal.muted));
                    ui.painter().line_segment([cc + vec2(-5.0, 5.0), cc + vec2(5.0, -5.0)], Stroke::new(1.4_f32, pal.muted));
                } else {
                    ui.painter().circle_filled(cc, 9.0, color);
                }
                if resp.hovered() {
                    ui.painter().circle_stroke(cc, 12.5, Stroke::new(1.5_f32, with_alpha(color, 0.6)));
                }
                if resp.clicked() {
                    c.checkpoint();
                    if let Some(e) = st.sel_edge {
                        if let Some(ed) = c.edge_mut(e) {
                            ed.color = col;
                        }
                    } else {
                        let sel = st.sel.clone();
                        for n in c.nodes.iter_mut().filter(|n| sel.contains(&n.id)) {
                            n.color = col;
                        }
                    }
                    c.rev += 1;
                    st.color_open = false;
                }
            }
        }
    }
}

// ───────────────────────────── popovers (page picker, image / link) ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn popovers(ui: &mut Ui, ctx: &Context, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, pal: &Palette, rect: Rect) {
    let Some(pop) = st.pop else { return };
    let id = match pop {
        Pop::Image(i) | Pop::Link(i) | Pop::Page(i) => i,
    };
    let Some(n) = c.node(id).cloned() else {
        st.pop = None;
        return;
    };
    let sr = c.view.rect_to_screen(rect.min, n.rect());
    let width = 340.0_f32.min(rect.width() - 20.0);
    let pos = pos2((sr.center().x - width / 2.0).clamp(rect.left() + 10.0, (rect.right() - width - 10.0).max(rect.left() + 10.0)), (sr.bottom() + 12.0).clamp(rect.top() + 60.0, (rect.bottom() - 260.0).max(rect.top() + 60.0)));
    let mut fresh = st.pop_fresh;
    let fresh_now = st.pop_fresh;
    let mut result: Option<String> = None;
    let mut chosen_page: Option<Option<u64>> = None;
    let pid = Id::new(("cnv_pop", id));
    let open = match pop {
        Pop::Image(_) | Pop::Link(_) => {
            const CHIPS: &[&str] = &["PNG", "JPG", "GIF", "WEBP"];
            let spec = if matches!(pop, Pop::Image(_)) {
                Spec { icon: l::IMAGE, title: "Add an image", subtitle: "Upload a photo or paste an image address.", upload: Some((true, CHIPS)), link_hint: "Paste an image link…", link_button: "Add image", providers: &[] }
            } else {
                Spec { icon: l::LINK, title: "Add a link", subtitle: "Paste any web address.", upload: None, link_hint: "Paste a link…", link_button: "Add link", providers: &[] }
            };
            let target = Some(PickTarget::CanvasImage { page, node: id });
            widgets::popup_area(ctx, pid, pos, pal, &mut fresh, width, |ui| {
                result = uploader::popover_body(ui, pal, pid.with("body"), &spec, if matches!(pop, Pop::Image(_)) { target } else { None }, width - 12.0);
            })
        }
        Pop::Page(_) => widgets::popup_area(ctx, pid, pos, pal, &mut fresh, width, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let te = ui.add(TextEdit::singleline(&mut st.page_query).hint_text("Search pages…").desired_width(f32::INFINITY).margin(Margin::symmetric(8, 7)));
            if fresh_now {
                te.request_focus();
            }
            ui.add_space(4.0);
            let q = st.page_query.to_lowercase();
            // new sub-page
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 7.0, pal.hover);
            }
            icons::glyph(ui.painter(), pos2(r.left() + 18.0, r.center().y), 16.0, l::FILE_PLUS, pal.accent);
            ui.painter().text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, "New page", bold(14.0), pal.accent);
            if resp.clicked() {
                chosen_page = Some(None);
            }
            let mut shown = 0;
            for p in ws.pages.iter().filter(|p| !p.trashed && !p.row && p.id != page) {
                let title = if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
                if !q.is_empty() && !title.to_lowercase().contains(&q) {
                    continue;
                }
                shown += 1;
                if shown > 60 {
                    break;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 7.0, pal.hover);
                }
                let ic = pos2(r.left() + 18.0, r.center().y);
                if p.icon.is_empty() {
                    icons::glyph(ui.painter(), ic, 16.0, if p.kind == PageKind::Database { l::TABLE } else if p.kind == PageKind::Canvas { l::NETWORK } else { l::FILE_TEXT }, pal.muted);
                } else {
                    icons::paint_page_icon(ui, ic, &p.icon, 17.0, pal.text);
                }
                widgets::text_fit(ui.painter(), pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, &title, body(14.0), pal.text, r.width() - 48.0);
                if resp.clicked() {
                    chosen_page = Some(Some(p.id));
                }
            }
        }),
    };
    st.pop_fresh = false;
    if let Some(url) = result {
        c.checkpoint();
        if let Some(nn) = c.node_mut(id) {
            nn.url = url;
            if nn.kind == Kind::Image {
                nn.h = 0.0;
            }
        }
        c.rev += 1;
        st.pop = None;
        return;
    }
    if let Some(ch) = chosen_page {
        let target = match ch {
            Some(p) => p,
            None => {
                let nid = ws.add_page(Some(page), PageKind::Doc);
                ws.current = Some(page);
                nid
            }
        };
        c.checkpoint();
        if let Some(nn) = c.node_mut(id) {
            nn.page = Some(target);
        }
        c.rev += 1;
        st.pop = None;
        return;
    }
    if !open {
        st.pop = None;
    }
}

// ───────────────────────────── menus ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn menus(ui: &mut Ui, ctx: &Context, ws: &mut Workspace, page: u64, c: &mut Canvas, st: &mut St, out: &mut Out, pal: &Palette) {
    let _ = ui;
    let Some(m) = st.menu.take() else { return };
    let mut items: Vec<(CtxItem, Act)> = vec![];
    let sel_n = st.sel.len();
    let first = st.sel.iter().next().and_then(|i| c.node(*i)).cloned();
    match m.kind {
        MenuKind::Add => {
            items.push((CtxItem::new(l::STICKY_NOTE, "Card"), Act::AddNote));
            items.push((CtxItem::new(l::TYPE, "Text"), Act::AddText));
            items.push((CtxItem::new(l::LIST_TODO, "To-do list"), Act::AddTodo));
            items.push((CtxItem::new(l::FILE_TEXT, "Page…"), Act::AddPage));
            items.push((CtxItem::new(l::IMAGE, "Image…"), Act::AddImage));
            items.push((CtxItem::new(l::LINK, "Link…"), Act::AddLink));
            items.push((CtxItem::new(l::FOCUS, "Group"), Act::AddGroup));
            items.push((CtxItem::new(l::LAYOUT_TEMPLATE, "Templates…").sep(), Act::Templates));
        }
        MenuKind::Templates => {
            for t in Tpl::ALL {
                items.push((CtxItem::new(match t {
                    Tpl::MindMap => l::NETWORK,
                    Tpl::Kanban => l::KANBAN,
                    Tpl::Swot => l::GRID_2X2,
                    Tpl::Flow => l::GIT_BRANCH,
                    Tpl::Week => l::CALENDAR_DAYS,
                }, t.name()), Act::Tpl(t)));
            }
        }
        MenuKind::Canvas => {
            items.push((CtxItem::new(l::MAXIMIZE, "Fit to content").hint("Shift+1"), Act::Fit));
            items.push((CtxItem::new(l::SCAN_SEARCH, "Zoom to selection").hint("Shift+2").enabled(sel_n > 0), Act::FitSel));
            items.push((CtxItem::new(l::SEARCH, "Find on canvas").hint("Ctrl+F"), Act::Find));
            items.push((CtxItem::new(l::MONITOR_PLAY, "Present groups as slides"), Act::Present));
            items.push((CtxItem::new(l::LAYOUT_TEMPLATE, "Templates…").sep(), Act::Templates));
            items.push((CtxItem::new(l::LAYOUT_GRID, "Tidy up cards"), Act::Tidy));
            items.push((CtxItem::new(l::GRID_3X3, match c.grid {
                Grid::Dots => "Grid: dots",
                Grid::Lines => "Grid: lines",
                Grid::None => "Grid: off",
            }).sep(), Act::GridCycle));
            items.push((CtxItem::new(l::TARGET, if c.snap { "Snap to grid: on" } else { "Snap to grid: off" }), Act::SnapToggle));
            items.push((CtxItem::new(l::MAP, if c.minimap { "Minimap: on" } else { "Minimap: off" }), Act::MapToggle));
            items.push((CtxItem::new(l::CLIPBOARD_PASTE, "Paste").sep().hint("Ctrl+V"), Act::Paste));
            items.push((CtxItem::new(l::FILE_JSON, "Copy as Obsidian JSON"), Act::CopyJson));
            items.push((CtxItem::new(l::UNDO_2, "Undo").hint("Ctrl+Z").enabled(c.can_undo()).sep(), Act::Undo));
            items.push((CtxItem::new(l::REDO_2, "Redo").hint("Ctrl+Shift+Z").enabled(c.can_redo()), Act::Redo));
        }
        MenuKind::Context(Target::Empty) => {
            items.push((CtxItem::new(l::STICKY_NOTE, "New card"), Act::AddNote));
            items.push((CtxItem::new(l::TYPE, "New text"), Act::AddText));
            items.push((CtxItem::new(l::LIST_TODO, "New to-do list"), Act::AddTodo));
            items.push((CtxItem::new(l::FILE_TEXT, "Add page…"), Act::AddPage));
            items.push((CtxItem::new(l::IMAGE, "Add image…"), Act::AddImage));
            items.push((CtxItem::new(l::LINK, "Add link…"), Act::AddLink));
            items.push((CtxItem::new(l::FOCUS, "New group"), Act::AddGroup));
            items.push((CtxItem::new(l::LAYOUT_TEMPLATE, "Templates…").sep(), Act::Templates));
            items.push((CtxItem::new(l::CLIPBOARD_PASTE, "Paste").hint("Ctrl+V"), Act::Paste));
            items.push((CtxItem::new(l::MAXIMIZE, "Fit to content").sep().hint("Shift+1"), Act::Fit));
        }
        MenuKind::Context(Target::Edge(_)) => {
            items.push((CtxItem::new(l::TEXT_CURSOR_INPUT, "Edit label"), Act::EdgeLabel));
            items.push((CtxItem::new(l::CHART_SPLINE, "Change line shape"), Act::EdgeShape));
            items.push((CtxItem::new(l::ELLIPSIS, "Dashed / solid"), Act::EdgeDash));
            items.push((CtxItem::new(l::ARROW_RIGHT_LEFT, "Arrow heads"), Act::EdgeArrow));
            items.push((CtxItem::new(l::ARROW_UP_DOWN, "Reverse direction"), Act::EdgeReverse));
            items.push((CtxItem::new(l::TRASH_2, "Delete").danger().sep().hint("Del"), Act::Delete));
        }
        MenuKind::Context(Target::Node(_)) => {
            let kind = first.as_ref().map(|n| n.kind);
            match kind {
                Some(Kind::Page) => items.push((CtxItem::new(l::ARROW_UP_RIGHT, "Open page"), Act::OpenPage)),
                Some(Kind::Link) => items.push((CtxItem::new(l::EXTERNAL_LINK, "Open link"), Act::OpenLink)),
                _ => {}
            }
            if sel_n == 1 && kind != Some(Kind::Ink) {
                items.push((CtxItem::new(l::PENCIL, "Edit").hint("Enter"), Act::Edit));
                items.push((CtxItem::new(l::CIRCLE_PLUS, "Add connected card").hint("Tab"), Act::AddChild));
                if c.edges.iter().any(|e| st.sel.contains(&e.from)) {
                    items.push((CtxItem::new(l::NETWORK, "Arrange branches as a tree"), Act::TreeLayout));
                }
            }
            items.push((CtxItem::new(l::COPY, "Duplicate").hint("Ctrl+D").sep(), Act::Duplicate));
            items.push((CtxItem::new(l::CLIPBOARD_COPY, "Copy").hint("Ctrl+C"), Act::Copy));
            items.push((CtxItem::new(l::SCISSORS, "Cut").hint("Ctrl+X"), Act::Cut));
            items.push((CtxItem::new(l::FOCUS, "Group selection").hint("Ctrl+G"), Act::GroupSel));
            items.push((CtxItem::new(l::LAYERS, "Bring to front").sep(), Act::Front));
            items.push((CtxItem::new(l::LAYERS, "Send to back"), Act::Back));
            let locked = first.as_ref().map_or(false, |n| n.locked);
            items.push((CtxItem::new(if locked { l::LOCK_OPEN } else { l::LOCK }, if locked { "Unlock" } else { "Lock in place" }), Act::Lock));
            if sel_n > 1 {
                items.push((CtxItem::new(l::ALIGN_LEFT, "Align left").sep(), Act::AlignL));
                items.push((CtxItem::new(l::ALIGN_LEFT, "Align top"), Act::AlignT));
                items.push((CtxItem::new(l::MOVE_HORIZONTAL, "Distribute horizontally").enabled(sel_n > 2), Act::DistH));
                items.push((CtxItem::new(l::MOVE_VERTICAL, "Distribute vertically").enabled(sel_n > 2), Act::DistV));
                items.push((CtxItem::new(l::LAYOUT_GRID, "Tidy into a grid"), Act::Tidy));
            }
            items.push((CtxItem::new(l::TRASH_2, "Delete").danger().sep().hint("Del"), Act::Delete));
        }
    }
    let key = match m.kind {
        MenuKind::Add => 1,
        MenuKind::Templates => 2,
        MenuKind::Canvas => 3,
        MenuKind::Context(Target::Empty) => 4,
        MenuKind::Context(Target::Node(_)) => 5,
        MenuKind::Context(Target::Edge(_)) => 6,
    };
    let list: Vec<CtxItem> = items.iter().map(|i| i.0).collect();
    let mut fresh = st.fresh;
    let (open, pick) = context_menu(ctx, Id::new(("cnv_menu", key)), m.pos, pal, &mut fresh, &list);
    st.fresh = false;
    if open {
        st.menu = Some(m);
    }
    if let Some(i) = pick {
        st.menu = None;
        let a = items[i].1;
        let at = match m.kind {
            MenuKind::Context(Target::Empty) | MenuKind::Templates | MenuKind::Add => m.world,
            _ => center_world(st, c),
        };
        act(a, st, c, ws, ctx, page, out, at);
    }
}
