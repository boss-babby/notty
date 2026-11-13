//! Page outline rail — the slim "table of contents at the side" of Notion.
//!
//! A column of short bars sits on the right edge of the page (one bar per heading, longer bars for higher levels, the heading that is
//! currently in view is highlighted). Hovering the rail (or tapping it on a touch screen) opens a card with the full outline; a click on
//! an entry scrolls the page to that heading. The card keeps the current section in view and closes shortly after the pointer leaves.

use egui::{pos2, vec2, Align, Align2, Area, Color32, CornerRadius, Frame, Id, Margin, Order, Rect, ScrollArea, Sense, Stroke, Ui};

use crate::editor::EditorState;
use crate::model::{Block, BlockKind};
use crate::theme::{body, bold, lerp_color, Palette};
use crate::widgets::with_alpha;

/// Headings of the page in reading order, and which one is "current" (the last one whose top edge has scrolled past the top of the view).
pub struct RailData {
    pub items: Vec<(u64, u8, String)>,
    pub active: usize,
}

/// Collects the outline from the blocks laid out this frame. `rects` = (block index, screen rect); `view` = the visible part of the page.
pub fn collect(blocks: &[Block], rects: &[(usize, Rect)], view: Rect) -> Option<RailData> {
    let mut items: Vec<(u64, u8, String)> = vec![];
    let mut tops: Vec<f32> = vec![];
    for (i, b) in blocks.iter().enumerate() {
        let lvl = match b.kind {
            BlockKind::H1 => 1,
            BlockKind::H2 => 2,
            BlockKind::H3 => 3,
            _ => continue,
        };
        let Some((_, r)) = rects.iter().find(|(k, _)| *k == i) else { continue };
        items.push((b.id, lvl, crate::rich::strip(&b.text)));
        tops.push(r.top());
    }
    if items.is_empty() {
        return None;
    }
    // a heading becomes current a little before it reaches the very top
    let line = view.top() + (view.height() * 0.2).clamp(40.0, 140.0);
    let active = tops.iter().rposition(|t| *t <= line).unwrap_or(0);
    Some(RailData { items, active })
}

fn bar_len(level: u8, compact: bool) -> f32 {
    let l = match level {
        1 => 18.0,
        2 => 13.0,
        _ => 9.0,
    };
    if compact {
        l * 0.8
    } else {
        l
    }
}

/// Draws the rail (and its card). Call inside the page's scroll area, after the blocks were laid out.
pub fn show(ui: &mut Ui, ed: &mut EditorState, pal: &Palette) {
    let Some(data) = ed.rail.take() else { return };
    if data.items.len() < 2 {
        return;
    }
    let ctx = ui.ctx().clone();
    let view = ui.clip_rect();
    if view.width() < 260.0 || view.height() < 200.0 {
        return;
    }
    let touchy = crate::touch::touchy();
    let compact = view.width() < 640.0;
    let n = data.items.len();
    let now = ctx.input(|i| i.time);

    // geometry: never taller than 60 % of the view; with very many headings the bars get closer together
    let max_h = (view.height() * 0.6).min(440.0);
    let base_step: f32 = if touchy { 11.0 } else { 9.0 };
    let step = base_step.min((max_h - 16.0) / n as f32).max(3.5);
    let h = step * n as f32 + 16.0;
    let w = if touchy { 34.0 } else { 26.0 };
    let right = view.right() - if compact { 2.0 } else { 20.0 };
    let rail = Rect::from_min_size(pos2(right - w, (view.center().y - h / 2.0).max(view.top() + 8.0)), vec2(w, h));

    let open_id = Id::new("outline_rail_open");
    let seen_id = Id::new("outline_rail_seen");
    let card_id = Id::new("outline_rail_card");
    let mut open: bool = ctx.data(|d| d.get_temp(open_id)).unwrap_or(false);
    let mut seen: f64 = ctx.data(|d| d.get_temp(seen_id)).unwrap_or(0.0);
    let card_prev: Option<Rect> = ctx.data(|d| d.get_temp(card_id));
    let ptr = ctx.pointer_hover_pos();
    let in_rail = ptr.map_or(false, |p| rail.expand2(vec2(8.0, 6.0)).contains(p));
    let in_card = open && ptr.zip(card_prev).map_or(false, |(p, c)| c.expand(10.0).contains(p));

    if touchy {
        // a finger cannot hover: tap = open / close, a tap anywhere else closes
        if open && ctx.input(|i| i.pointer.any_pressed()) && !in_rail && !in_card {
            open = false;
        }
    } else if in_rail || in_card {
        open = true;
        seen = now;
    } else if open {
        if now - seen > 0.3 {
            open = false;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    // fade the rail on phones while it is not in use so that it never distracts from the text
    let rest = if compact && !open { 0.6 } else { 1.0 };

    // ── the rail ──
    let hover_idx = ptr.filter(|_| in_rail).map(|p| (((p.y - rail.top() - 8.0) / step).floor() as isize).clamp(0, n as isize - 1) as usize);
    let mut tapped = false;
    let mut jump: Option<u64> = None;
    Area::new(Id::new("outline_rail")).order(Order::Middle).fixed_pos(rail.min).constrain(false).interactable(true).show(&ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(rail.size(), Sense::click());
        let t = ctx.animate_bool_with_time(Id::new("outline_rail_t"), open || in_rail, 0.12);
        // soft backing plate while the rail is in use (keeps the bars readable above text)
        if t > 0.0 {
            ui.painter().rect_filled(r, CornerRadius::same(9), with_alpha(pal.popup, 0.8 * t));
        }
        for (i, (_, lvl, _)) in data.items.iter().enumerate() {
            let y = r.top() + 8.0 + step * (i as f32 + 0.5);
            let is_active = i == data.active;
            let is_hover = hover_idx == Some(i);
            let len = bar_len(*lvl, compact) + if is_active { 5.0 } else { 0.0 } + if is_hover { 3.0 } else { 0.0 };
            let col = if is_active {
                pal.text
            } else if is_hover {
                lerp_color(pal.muted, pal.text, 0.6)
            } else {
                pal.muted
            };
            let alpha = if is_active { 1.0 } else if is_hover { 0.95 } else { 0.5 + 0.3 * t };
            let th = if is_active || is_hover { 2.4 } else { 1.8 };
            let br = Rect::from_min_max(pos2(r.right() - 6.0 - len, y - th / 2.0), pos2(r.right() - 6.0, y + th / 2.0));
            ui.painter().rect_filled(br, CornerRadius::same(2), col.gamma_multiply(alpha * rest));
        }
        if resp.clicked() {
            if touchy {
                tapped = true;
            } else if let Some(i) = hover_idx {
                jump = Some(data.items[i].0);
            }
        }
    });
    if tapped {
        open = !open;
        seen = now;
    }
    if jump.is_some() {
        ed.scroll_to = jump;
    }

    // ── the card with the full outline ──
    if open {
        let card_w = (if compact { 250.0_f32 } else { 280.0 }).min(view.width() - w - 24.0).max(180.0);
        let row_h = if touchy { 34.0 } else { 28.0 };
        let list_h = (row_h * n as f32).min(max_h.max(160.0) - 12.0).min(view.height() * 0.62);
        let card_h = list_h + 20.0;
        let x = (rail.left() - card_w - 6.0).max(view.left() + 6.0);
        let y = (rail.center().y - card_h / 2.0).clamp(view.top() + 8.0, (view.bottom() - card_h - 8.0).max(view.top() + 8.0));
        let was_open: bool = ctx.data(|d| d.get_temp(Id::new("outline_card_was"))).unwrap_or(false);
        let min_lvl = data.items.iter().map(|h| h.1).min().unwrap_or(1);
        let shadow = egui::epaint::Shadow { offset: [0, 6], blur: 28, spread: 0, color: Color32::from_black_alpha(if pal.bg.r() < 128 { 110 } else { 46 }) };
        let mut close = false;
        let out = Area::new(Id::new("outline_rail_card_area")).order(Order::Foreground).fixed_pos(pos2(x, y)).constrain(false).interactable(true).show(&ctx, |ui| {
            Frame::new().fill(pal.popup).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(12)).inner_margin(Margin::symmetric(6, 10)).shadow(shadow).show(ui, |ui| {
                ui.set_width(card_w - 12.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                ScrollArea::vertical().id_salt("outline_card_scroll").max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, (id, lvl, text)) in data.items.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                        let t = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.08);
                        let is_active = i == data.active;
                        if t > 0.0 {
                            ui.painter().rect_filled(r, CornerRadius::same(6), with_alpha(pal.hover, t));
                        }
                        let indent = (*lvl - min_lvl) as f32 * 14.0;
                        let label = if text.trim().is_empty() { "Untitled" } else { text.trim() };
                        let col = if is_active { pal.accent } else { lerp_color(pal.muted, pal.text, 0.35 + 0.65 * t) };
                        let font = if is_active { bold(13.5) } else { body(13.5) };
                        crate::widgets::text_fit(ui.painter(), pos2(r.left() + 10.0 + indent, r.center().y), Align2::LEFT_CENTER, label, font, col, r.width() - 20.0 - indent);
                        if is_active && !was_open {
                            resp.scroll_to_me(Some(Align::Center));
                        }
                        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            ed.scroll_to = Some(*id);
                            close = touchy;
                        }
                    }
                });
            });
        });
        ctx.data_mut(|d| {
            d.insert_temp(card_id, out.response.rect);
            d.insert_temp(Id::new("outline_card_was"), true);
        });
        if close {
            open = false;
        }
    } else {
        ctx.data_mut(|d| d.insert_temp(Id::new("outline_card_was"), false));
    }
    ctx.data_mut(|d| {
        d.insert_temp(open_id, open);
        d.insert_temp(seen_id, seen);
    });
}
