//! Dashboard widgets on the whiteboard (`canvas::Kind::Dash`).
//!
//! Every widget of the dashboard catalogue (charts, numbers, task lists, timers, calendars … 30+ types, see `dashwidgets`) can be placed
//! on a canvas. The node stores the id of a widget in the hidden "widgets inside pages" board (`Node::num`), so the whole widget
//! engine — data sources, filters, settings panel, drill-down — is reused unchanged.
//!
//! **Vector scaling.** A widget is laid out once in *design units* (the node's width × height at 100 %) and the resulting shapes are
//! scaled to the board's zoom (`Shape::transform` on the freshly painted range of the layer). Text, charts, paddings, strokes and icons
//! therefore keep their exact proportions at every zoom level: nothing is hidden, clipped or re-flowed when the board is zoomed out and
//! nothing overlaps. Widgets are drawn in place (correct z-order, selection handles stay on top).
//!
//! **Interaction.** A widget on the board is a display until it is *activated* (double-click, Enter, or "Interact" in the selection
//! toolbar): it then lives in its own transformed layer that receives pointer input (tick tasks, start timers, click chart bars …).
//! Clicking anywhere else, or Esc, hands the board back to the pointer.
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use egui::emath::TSTransform;
use egui::{pos2, vec2, Area, Id, LayerId, Order, Pos2, Rect, Sense, Ui, UiBuilder};

use crate::canvas::{Canvas, Kind};
use crate::dashboard::Nav;
use crate::inlinew;
use crate::model::Workspace;
use crate::theme::Palette;

/// Widgets are laid out far away from the board's own coordinates so that they never collide with real hit areas.
const FAR: f32 = -100_000.0;

thread_local! {
    static LIVE: Cell<u64> = const { Cell::new(0) };
    static LIVE_RECT: Cell<Rect> = const { Cell::new(Rect::NOTHING) };
    static PICK: RefCell<Vec<(Cmd, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Id of the node whose widget currently receives pointer input (0 = none).
pub fn live() -> u64 {
    LIVE.with(|l| l.get())
}

pub fn set_live(id: u64) {
    LIVE.with(|l| l.set(id));
    if id == 0 {
        LIVE_RECT.with(|r| r.set(Rect::NOTHING));
    }
}

/// True while `p` is over the activated widget: the board must not start a drag / selection there.
pub fn live_blocks(p: Pos2) -> bool {
    live() != 0 && LIVE_RECT.with(|r| r.get()).contains(p)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Cmd {
    /// Open the widget gallery for the node.
    Pick,
    /// Double-click / Enter: choose a widget when there is none, otherwise activate (or leave) the live widget.
    Open,
    /// Open the settings panel of the node's widget.
    Configure,
}

/// Queues a command for node `id`; it runs in the next draw, which has the workspace at hand.
pub fn request(cmd: Cmd, id: u64) {
    PICK.with(|p| p.borrow_mut().push((cmd, id)));
}

pub fn has_widget(ws: &Workspace, num: u32) -> bool {
    num != 0 && ws.dash.widget(num as u64).is_some()
}

/// Short label of the node's widget ("Bar chart", "Tasks" …) for search and export.
pub fn label(ws: &Workspace, num: u32) -> String {
    let Some(wd) = (num != 0).then(|| ws.dash.widget(num as u64)).flatten() else { return "Dashboard widget".into() };
    if !wd.title.is_empty() {
        return wd.title.clone();
    }
    crate::dashwidgets::def_for(wd).map(|d| d.name.to_string()).unwrap_or_else(|| "Dashboard widget".into())
}

/// Paints every dashboard widget of the board (call between the nodes and the selection handles).
#[allow(clippy::too_many_arguments)]
pub fn draw_all(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, c: &mut Canvas, sel: &[u64], origin: Pos2, clip: Rect, nav: &mut Nav) {
    let ctx = ui.ctx().clone();
    let ids: Vec<u64> = c.nodes.iter().filter(|n| n.kind == Kind::Dash).map(|n| n.id).collect();
    if ids.is_empty() {
        set_live(0);
        return;
    }
    // widget chosen in the gallery
    if let Some((node, wid)) = inlinew::take_canvas_assign() {
        if let Some(n) = c.node_mut(node) {
            n.num = wid as u32;
        }
    }
    for (cmd, id) in PICK.with(|p| std::mem::take(&mut *p.borrow_mut())) {
        let num = c.node(id).map_or(0, |n| n.num);
        match (cmd, has_widget(ws, num)) {
            (Cmd::Pick, _) | (Cmd::Open | Cmd::Configure, false) => inlinew::request_pick_canvas(ws, id),
            (Cmd::Open, true) => set_live(if live() == id { 0 } else { id }),
            (Cmd::Configure, true) => inlinew::inspect(num as u64),
        }
    }
    // a duplicated / pasted node must not share its widget with the original
    let mut seen: HashSet<u32> = HashSet::new();
    for id in &ids {
        let num = c.node(*id).map_or(0, |n| n.num);
        if num != 0 && !seen.insert(num) {
            if let Some(nw) = ws.dash.duplicate_widget(num as u64) {
                if let Some(n) = c.node_mut(*id) {
                    n.num = nw as u32;
                }
                seen.insert(nw as u32);
            }
        }
    }
    let z = c.view.zoom;
    let live_id = live();
    if live_id != 0 && !(sel.contains(&live_id) && ids.contains(&live_id)) {
        set_live(0);
    }
    if live() != 0 {
        let (pressed_at, esc) = ctx.input(|i| (if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }, i.key_pressed(egui::Key::Escape)));
        if esc || pressed_at.is_some_and(|p| !LIVE_RECT.with(|r| r.get()).contains(p)) {
            set_live(0);
        }
    }
    let far = vec2(FAR, FAR);
    for id in ids {
        let Some(n) = c.node(id) else { continue };
        let r = c.view.rect_to_screen(origin, n.rect());
        let vis = r.intersect(clip);
        if vis.width() <= 1.0 || vis.height() <= 1.0 || r.width() < 9.0 || z < 0.06 {
            continue;
        }
        let Some(wd) = (n.num != 0).then(|| ws.dash.widget(n.num as u64).cloned()).flatten() else { continue };
        let size = vec2(n.w, n.h.max(n.kind.min_size().y));
        let selected = sel.contains(&id);
        let live_now = live() == id;
        if live_now {
            // activated: an interactive layer with the same transform
            LIVE_RECT.with(|l| l.set(vis));
            let area_id = Id::new(("cnvdash", id));
            let layer = LayerId::new(Order::Middle, area_id);
            ctx.set_transform_layer(layer, TSTransform::new(r.min.to_vec2(), z));
            let clip_local = Rect::from_min_max(((vis.min - r.min) / z).to_pos2(), ((vis.max - r.min) / z).to_pos2());
            let local = Rect::from_min_size(Pos2::ZERO, size);
            Area::new(area_id).order(Order::Middle).fixed_pos(Pos2::ZERO).movable(false).constrain(false).interactable(true).show(&ctx, |ui| {
                ui.set_clip_rect(clip_local);
                ui.allocate_exact_size(size, Sense::hover());
                inlinew::paint_card(ui, ws, pal, &wd, id | (1 << 62), local, true, true, nav);
            });
            let tf = TSTransform::new(r.min.to_vec2(), z);
            inlinew::drill_to_screen(|p| tf * p);
        } else {
            let layer = ui.layer_id();
            let local = Rect::from_min_size(pos2(FAR, FAR), size);
            let clip_local = Rect::from_min_max(pos2(FAR, FAR) + (vis.min - r.min) / z, pos2(FAR, FAR) + (vis.max - r.min) / z);
            let start = ctx.graphics_mut(|g| g.entry(layer).next_idx());
            {
                let mut child = ui.new_child(UiBuilder::new().max_rect(local).layout(egui::Layout::top_down(egui::Align::Min)));
                child.set_clip_rect(clip_local);
                child.freeze();
                inlinew::paint_card(&mut child, ws, pal, &wd, id | (1 << 62), local, true, selected, nav);
            }
            let end = ctx.graphics_mut(|g| g.entry(layer).next_idx());
            let tf = TSTransform::new(r.min.to_vec2() - far * z, z);
            ctx.graphics_mut(|g| g.entry(layer).transform_range(start, end, tf));
        }
    }
}
