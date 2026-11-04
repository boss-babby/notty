//! The Notion-style block editor and kanban board.

use egui::{
    pos2,
    text::{CCursor, CCursorRange},
    vec2, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Modifiers, Order, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::icons::Icon;
use crate::model::{Block, BlockKind, Card, Column, Page, PageKind, ICONS};
use crate::theme::{bold, body, lerp_color, tag_color, Palette, COVERS, TAG_NAMES};
use crate::widgets::{
    appear, gradient_rect, icon_button, menu_item, popup_area, reset_appear, section_label, with_alpha,
};

const GUTTER: f32 = 56.0;

#[derive(Default)]
pub struct EditorState {
    pub focus: Option<(u64, usize)>,
    pub slash_sel: usize,
    pub slash_moved: bool,
    pub block_menu: Option<(u64, Pos2)>,
    pub dragging: Option<u64>,
    pub icon_picker: Option<Pos2>,
    pub cover_picker: Option<Pos2>,
    pub fresh: bool,
}

enum Act {
    Split(usize, usize),
    Merge(usize),
    Remove(usize),
    Convert(usize, BlockKind, bool),
    Strip(usize, BlockKind, usize),
    MoveTo(usize, usize),
    Dup(usize),
    InsertAfter(usize, BlockKind, String),
    FocusPrev(usize),
    FocusNext(usize),
}

fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

fn slash_items(query: &str) -> Vec<BlockKind> {
    let q = query.trim().to_lowercase();
    BlockKind::ALL
        .iter()
        .copied()
        .filter(|k| q.is_empty() || k.label().to_lowercase().contains(&q) || k.keywords().contains(&q))
        .collect()
}

fn md_shortcut(text: &str, cursor: usize) -> Option<(BlockKind, usize)> {
    const RULES: [(&str, BlockKind); 9] = [
        ("### ", BlockKind::H3),
        ("## ", BlockKind::H2),
        ("# ", BlockKind::H1),
        ("- ", BlockKind::Bullet),
        ("* ", BlockKind::Bullet),
        ("[] ", BlockKind::Todo),
        ("1. ", BlockKind::Numbered),
        ("> ", BlockKind::Quote),
        ("```", BlockKind::Code),
    ];
    if text == "---" {
        return Some((BlockKind::Divider, 3));
    }
    if text.starts_with("[ ] ") && cursor == 4 {
        return Some((BlockKind::Todo, 4));
    }
    for (prefix, kind) in RULES {
        let n = prefix.chars().count();
        if text.starts_with(prefix) && cursor == n {
            return Some((kind, n));
        }
    }
    None
}

/// Renders the page body. Returns `true` if anything changed.
pub fn show_page(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette) -> bool {
    let mut changed = false;
    let avail = ui.available_width();

    // ---- cover ----
    if let Some(ci) = page.cover {
        let (rect, _) = ui.allocate_exact_size(vec2(avail, 200.0), Sense::hover());
        let (a, b) = COVERS[ci % COVERS.len()];
        gradient_rect(ui, rect, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
        let hovered = ui.rect_contains_pointer(rect);
        let t = ui.ctx().animate_bool_with_time(Id::new(("cover_h", page.id)), hovered, 0.15);
        if t > 0.01 {
            let r = Rect::from_min_size(pos2(rect.right() - 262.0, rect.bottom() - 42.0), vec2(120.0, 28.0));
            let r2 = Rect::from_min_size(pos2(rect.right() - 134.0, rect.bottom() - 42.0), vec2(110.0, 28.0));
            for (rr, label, id) in [(r, "Change cover", "cc"), (r2, "Remove", "cr")] {
                let resp = ui.interact(rr, Id::new((id, page.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(rr, 6.0, with_alpha(lerp_color(pal.bg, pal.hover, h), t * 0.92));
                ui.painter().text(rr.center(), Align2::CENTER_CENTER, label, body(12.5), with_alpha(pal.text, t));
                if resp.clicked() {
                    if id == "cc" {
                        ed.cover_picker = Some(rr.left_bottom() + vec2(0.0, 6.0));
                        ed.fresh = true;
                    } else {
                        page.cover = None;
                        changed = true;
                    }
                }
            }
        }
    }

    let col_w = (avail - 80.0).clamp(300.0, if page.kind == PageKind::Board { 4000.0 } else { 900.0 });
    let left = ((avail - col_w) / 2.0).max(24.0);
    let content_w = (col_w - GUTTER).max(200.0);

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(if page.kind == PageKind::Board { 48.0 } else { left });
        ui.vertical(|ui| {
            ui.set_width(if page.kind == PageKind::Board { avail - 96.0 } else { col_w });
            ui.spacing_mut().item_spacing.y = 0.0;
            changed |= header(ui, page, ed, pal, content_w);
            match page.kind {
                PageKind::Doc => changed |= blocks(ui, page, next_id, ed, pal, content_w),
                PageKind::Board => changed |= board(ui, page, next_id, pal),
            }
        });
    });

    ui.add_space(80.0);
    changed |= popups(ui, page, next_id, ed, pal);
    changed
}

fn header(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette, content_w: f32) -> bool {
    let mut changed = false;
    let has_cover = page.cover.is_some();
    ui.add_space(if has_cover { 0.0 } else { 40.0 });

    // icon
    let icon_h = 76.0;
    let (rect, _) = ui.allocate_exact_size(vec2(content_w + GUTTER, if has_cover { 44.0 } else { icon_h }), Sense::hover());
    let hdr_top = rect.top();
    if !page.icon.is_empty() {
        let size = 68.0;
        let top = if has_cover { hdr_top - 34.0 } else { hdr_top };
        let ir = Rect::from_min_size(pos2(rect.left() + GUTTER - 8.0, top), vec2(size + 16.0, size + 8.0));
        let resp = ui.interact(ir, Id::new(("icon", page.id)), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(ir, 12.0, with_alpha(pal.hover, t));
        ui.painter().text(
            pos2(ir.center().x, ir.center().y + 2.0),
            Align2::CENTER_CENTER,
            &page.icon,
            body(54.0),
            pal.text,
        );
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ed.icon_picker = Some(ir.left_bottom() + vec2(0.0, 6.0));
            ed.fresh = true;
        }
    }

    // add icon / cover (revealed on hover)
    let row_h = 30.0;
    let (row, _) = ui.allocate_exact_size(vec2(content_w + GUTTER, row_h), Sense::hover());
    let hov = ui.rect_contains_pointer(Rect::from_min_max(pos2(row.left(), hdr_top - 10.0), pos2(row.right(), row.bottom() + 56.0)));
    let t = ui.ctx().animate_bool_with_time(Id::new(("hdr_h", page.id)), hov, 0.15);
    let mut x = row.left() + GUTTER;
    let mut buttons: Vec<(&str, &str)> = vec![];
    if page.icon.is_empty() {
        buttons.push(("☺  Add icon", "ai"));
    }
    if page.cover.is_none() {
        buttons.push(("▭  Add cover", "ac"));
    }
    for (label, key) in buttons {
        let w = 100.0 + if key == "ac" { 6.0 } else { 0.0 };
        let r = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(w, 26.0));
        x += w + 6.0;
        if t < 0.02 {
            continue;
        }
        let resp = ui.interact(r, Id::new((key, page.id)), Sense::click());
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, h * t));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, body(13.0), with_alpha(lerp_color(pal.muted, pal.text, h), t));
        if resp.clicked() {
            if key == "ai" {
                page.icon = "📄".into();
                changed = true;
            } else {
                page.cover = Some((page.id as usize) % COVERS.len());
                changed = true;
            }
        }
    }

    // title
    ui.horizontal(|ui| {
        ui.add_space(GUTTER);
        let tid = Id::new(("title", page.id));
        let title_focused = ui.memory(|m| m.has_focus(tid));
        let enter = title_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let r = ui.add(
            TextEdit::singleline(&mut page.title)
                .id(tid)
                .frame(false)
                .font(bold(40.0))
                .hint_text(RichText::new("Untitled").font(bold(40.0)).color(with_alpha(pal.muted, 0.55)))
                .desired_width(content_w)
                .margin(Margin::symmetric(0, 4)),
        );
        changed |= r.changed();
        if enter && page.kind == PageKind::Doc {
            if let Some(b) = page.blocks.first() {
                ed.focus = Some((b.id, 0));
            }
        }
    });
    ui.add_space(8.0);
    changed
}

fn font_for(kind: BlockKind) -> egui::FontId {
    match kind {
        BlockKind::H1 => bold(30.0),
        BlockKind::H2 => bold(24.0),
        BlockKind::H3 => bold(19.0),
        BlockKind::Code => egui::FontId::monospace(13.5),
        BlockKind::Quote => body(17.0),
        _ => body(16.0),
    }
}

fn blocks(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let mut changed = false;
    let n = page.blocks.len();
    let mut act: Option<Act> = None;
    let mut rects: Vec<Rect> = Vec::with_capacity(n);
    let mut num = 0usize;
    let pointer = ui.ctx().pointer_hover_pos();

    for i in 0..n {
        let kind = page.blocks[i].kind;
        let bid = page.blocks[i].id;
        num = if kind == BlockKind::Numbered { num + 1 } else { 0 };
        if i > 0 {
            ui.add_space(match kind {
                BlockKind::H1 => 18.0,
                BlockKind::H2 => 12.0,
                BlockKind::H3 => 8.0,
                BlockKind::Callout | BlockKind::Code | BlockKind::Quote => 6.0,
                _ => 1.0,
            });
        }
        let row = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.add_space(GUTTER);
            ui.vertical(|ui| {
                ui.set_width(w);
                changed |= draw_block(ui, &mut page.blocks[i], i, n, num, ed, pal, w, &mut act);
            });
        });
        let rect = row.response.rect;
        rects.push(rect);

        // gutter controls
        let hover_zone = Rect::from_min_max(pos2(rect.left(), rect.top() - 1.0), pos2(rect.right(), rect.bottom() + 1.0));
        let hovering = pointer.map_or(false, |p| hover_zone.contains(p)) && ed.dragging.is_none()
            || ed.dragging == Some(bid)
            || ed.block_menu.map_or(false, |(b, _)| b == bid);
        let t = ui.ctx().animate_bool_with_time(Id::new(("gutter", bid)), hovering, 0.12);
        if t > 0.02 {
            let y = rect.top() + match kind {
                BlockKind::H1 => 8.0,
                BlockKind::H2 => 5.0,
                BlockKind::H3 => 3.0,
                BlockKind::Callout => 11.0,
                BlockKind::Code => 14.0,
                _ => 1.0,
            };
            let plus = Rect::from_min_size(pos2(rect.left() + 6.0, y), vec2(22.0, 24.0));
            let grip = Rect::from_min_size(pos2(rect.left() + 29.0, y), vec2(20.0, 24.0));
            let pr = ui.interact(plus, Id::new(("plus", bid)), Sense::click());
            let gr = ui.interact(grip, Id::new(("grip", bid)), Sense::click_and_drag());
            for (r, resp, icon) in [(plus, &pr, Icon::Plus), (grip, &gr, Icon::Grip)] {
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(r, 5.0, with_alpha(pal.hover, h * t));
                crate::icons::paint(ui.painter(), r.center(), 15.0, icon, with_alpha(lerp_color(pal.muted, pal.text, h), t));
            }
            if pr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                act = Some(Act::InsertAfter(i, BlockKind::Text, "/".into()));
            }
            if gr.drag_started() {
                ed.dragging = Some(bid);
            }
            if gr.clicked() {
                ed.block_menu = Some((bid, grip.left_bottom() + vec2(-4.0, 6.0)));
                ed.fresh = true;
            }
            if gr.hovered() || gr.dragged() {
                ui.ctx().set_cursor_icon(if gr.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
        }
    }

    // drag & drop reorder
    if let Some(did) = ed.dragging {
        ui.ctx().request_repaint();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let (Some(p), Some(from)) = (pointer, page.blocks.iter().position(|b| b.id == did)) {
            let target = rects.iter().position(|r| p.y < r.center().y).unwrap_or(n);
            let y = if target < n { rects[target].top() - 1.0 } else { rects[n - 1].bottom() + 1.0 };
            let x0 = rects[0].left() + GUTTER;
            ui.painter().rect_filled(
                Rect::from_min_size(pos2(x0, y - 1.5), vec2(w, 3.0)),
                2.0,
                pal.accent,
            );
            if !ui.input(|i| i.pointer.primary_down()) {
                if target != from && target != from + 1 {
                    act = Some(Act::MoveTo(from, target));
                }
                ed.dragging = None;
            }
        } else if !ui.input(|i| i.pointer.primary_down()) {
            ed.dragging = None;
        }
    }

    // click below the last block to append
    let (rest, resp) = ui.allocate_exact_size(vec2(w + GUTTER, 150.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(Id::new(("tail", page.id)), resp.hovered(), 0.15);
    ui.painter().text(
        pos2(rest.left() + GUTTER, rest.top() + 16.0),
        Align2::LEFT_CENTER,
        "Click to add a block",
        body(14.0),
        with_alpha(pal.muted, t * 0.8),
    );
    if resp.clicked() && n > 0 {
        let last = &page.blocks[n - 1];
        if last.kind == BlockKind::Text && last.text.is_empty() {
            ed.focus = Some((last.id, 0));
        } else {
            act = Some(Act::InsertAfter(n - 1, BlockKind::Text, String::new()));
        }
    }

    if let Some(a) = act {
        apply(page, a, next_id, ed);
        changed = true;
        ui.ctx().request_repaint();
    }
    if let Some((fid, _)) = ed.focus {
        if !page.blocks.iter().any(|b| b.id == fid) && page.kind == PageKind::Doc {
            ed.focus = None;
        }
    }
    changed
}

fn new_block(next_id: &mut u64, kind: BlockKind, text: String) -> Block {
    *next_id += 1;
    Block { id: *next_id, kind, text, checked: false }
}

fn apply(page: &mut Page, act: Act, next_id: &mut u64, ed: &mut EditorState) {
    let bl = &mut page.blocks;
    match act {
        Act::Split(i, at) => {
            let kind = bl[i].kind;
            let cut = byte_at(&bl[i].text, at);
            let right = bl[i].text.split_off(cut);
            let nk = match kind {
                BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered => kind,
                _ => BlockKind::Text,
            };
            let nb = new_block(next_id, nk, right);
            ed.focus = Some((nb.id, 0));
            bl.insert(i + 1, nb);
        }
        Act::Merge(i) => {
            if i == 0 {
                return;
            }
            if bl[i - 1].kind == BlockKind::Divider {
                bl.remove(i - 1);
                ed.focus = Some((bl[i - 1].id, 0));
            } else {
                let b = bl.remove(i);
                let prev = &mut bl[i - 1];
                let pos = prev.text.chars().count();
                prev.text.push_str(&b.text);
                ed.focus = Some((prev.id, pos));
            }
        }
        Act::Remove(i) => {
            bl.remove(i);
            if bl.is_empty() {
                bl.push(new_block(next_id, BlockKind::Text, String::new()));
            }
            let j = i.saturating_sub(1).min(bl.len() - 1);
            ed.focus = Some((bl[j].id, usize::MAX));
        }
        Act::Convert(i, kind, clear) => {
            if clear {
                bl[i].text.clear();
            }
            bl[i].kind = kind;
            if kind == BlockKind::Divider {
                bl[i].text.clear();
                let nb = new_block(next_id, BlockKind::Text, String::new());
                ed.focus = Some((nb.id, 0));
                bl.insert(i + 1, nb);
            } else {
                ed.focus = Some((bl[i].id, usize::MAX));
            }
        }
        Act::Strip(i, kind, n) => {
            let cut = byte_at(&bl[i].text, n);
            bl[i].text = bl[i].text[cut..].to_owned();
            bl[i].kind = kind;
            if kind == BlockKind::Divider {
                bl[i].text.clear();
                let nb = new_block(next_id, BlockKind::Text, String::new());
                ed.focus = Some((nb.id, 0));
                bl.insert(i + 1, nb);
            } else {
                ed.focus = Some((bl[i].id, 0));
            }
        }
        Act::MoveTo(from, to) => {
            let b = bl.remove(from);
            let to = if to > from { to - 1 } else { to };
            bl.insert(to.min(bl.len()), b);
        }
        Act::Dup(i) => {
            let mut c = bl[i].clone();
            *next_id += 1;
            c.id = *next_id;
            ed.focus = Some((c.id, usize::MAX));
            bl.insert(i + 1, c);
        }
        Act::InsertAfter(i, kind, text) => {
            let nb = new_block(next_id, kind, text);
            ed.focus = Some((nb.id, usize::MAX));
            bl.insert(i + 1, nb);
        }
        Act::FocusPrev(i) => {
            let mut j = i;
            while j > 0 {
                j -= 1;
                if bl[j].kind != BlockKind::Divider {
                    ed.focus = Some((bl[j].id, usize::MAX));
                    break;
                }
            }
        }
        Act::FocusNext(i) => {
            let mut j = i + 1;
            while j < bl.len() {
                if bl[j].kind != BlockKind::Divider {
                    ed.focus = Some((bl[j].id, 0));
                    break;
                }
                j += 1;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_block(
    ui: &mut Ui,
    b: &mut Block,
    idx: usize,
    n: usize,
    num: usize,
    ed: &mut EditorState,
    pal: &Palette,
    w: f32,
    act: &mut Option<Act>,
) -> bool {
    let mut changed = false;
    let font = font_for(b.kind);
    match b.kind {
        BlockKind::Text | BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => {
            let hint = match b.kind {
                BlockKind::H1 => "Heading 1",
                BlockKind::H2 => "Heading 2",
                BlockKind::H3 => "Heading 3",
                _ => "Type '/' for commands…",
            };
            changed |= text_edit(ui, b, idx, n, ed, pal, font, pal.text, hint, w, act);
        }
        BlockKind::Todo => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
                let c = pos2(r.left() + 10.0, r.top() + 13.5);
                let t = ui.ctx().animate_bool_with_time(Id::new(("chk", b.id)), b.checked, 0.18);
                let box_r = Rect::from_center_size(c, vec2(17.0, 17.0));
                let fill = with_alpha(pal.accent, t);
                ui.painter().rect_filled(box_r, 4.0, fill);
                ui.painter().rect_stroke(
                    box_r,
                    4.0,
                    Stroke::new(1.6_f32, lerp_color(pal.text.gamma_multiply(0.8), pal.accent, t)),
                    egui::StrokeKind::Inside,
                );
                if t > 0.02 {
                    let s = t.min(1.0);
                    let p1 = pos2(c.x - 4.2, c.y + 0.2);
                    let p2 = pos2(c.x - 1.2, c.y + 3.2);
                    let p3 = pos2(c.x + 4.4, c.y - 3.2);
                    let mid = p1 + (p2 - p1) * (s * 2.0).min(1.0);
                    let mut pts = vec![p1, mid];
                    if s > 0.5 {
                        pts.push(p2 + (p3 - p2) * ((s - 0.5) * 2.0).min(1.0));
                    }
                    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0_f32, Color32::WHITE)));
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    b.checked = !b.checked;
                    changed = true;
                }
                let col = lerp_color(pal.text, pal.muted, t);
                changed |= text_edit(ui, b, idx, n, ed, pal, font, col, "To-do", w - 28.0, act);
            });
        }
        BlockKind::Bullet | BlockKind::Numbered => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                if b.kind == BlockKind::Bullet {
                    ui.painter().circle_filled(pos2(r.left() + 10.0, r.top() + 13.5), 3.0, pal.text);
                } else {
                    ui.painter().text(
                        pos2(r.left() + 17.0, r.top() + 13.5),
                        Align2::RIGHT_CENTER,
                        format!("{num}."),
                        body(16.0),
                        pal.text,
                    );
                }
                let hint = if b.kind == BlockKind::Bullet { "List" } else { "List item" };
                changed |= text_edit(ui, b, idx, n, ed, pal, font, pal.text, hint, w - 26.0, act);
            });
        }
        BlockKind::Quote => {
            let r = ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(14.0);
                changed |= text_edit(ui, b, idx, n, ed, pal, font, pal.text, "Empty quote", w - 14.0, act);
            });
            let rr = r.response.rect;
            ui.painter().rect_filled(Rect::from_min_size(rr.min + vec2(0.0, 3.0), vec2(3.0, rr.height() - 6.0)), 2.0, pal.text);
        }
        BlockKind::Callout => {
            Frame::new()
                .fill(pal.callout)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(w - 28.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let (r, _) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::hover());
                        ui.painter().text(r.left_center() + vec2(2.0, 0.0), Align2::LEFT_CENTER, "💡", body(20.0), pal.text);
                        changed |= text_edit(ui, b, idx, n, ed, pal, font, pal.text, "Type something…", w - 28.0 - 34.0, act);
                    });
                });
        }
        BlockKind::Code => {
            Frame::new()
                .fill(pal.code_bg)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.set_width(w - 32.0);
                    changed |= text_edit(ui, b, idx, n, ed, pal, font, pal.text, "Write some code…", w - 32.0, act);
                });
        }
        BlockKind::Divider => {
            let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
            ui.painter().line_segment(
                [pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)],
                Stroke::new(1.0_f32, pal.border.gamma_multiply(1.4)),
            );
        }
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn text_edit(
    ui: &mut Ui,
    b: &mut Block,
    idx: usize,
    n: usize,
    ed: &mut EditorState,
    pal: &Palette,
    font: egui::FontId,
    color: Color32,
    hint: &str,
    width: f32,
    act: &mut Option<Act>,
) -> bool {
    let ctx = ui.ctx().clone();
    let id = Id::new(("blk", b.id));
    let nchars = b.text.chars().count();

    if let Some((fid, pos)) = ed.focus {
        if fid == b.id {
            ctx.memory_mut(|m| m.request_focus(id));
            let mut st = TextEdit::load_state(&ctx, id).unwrap_or_default();
            st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(pos.min(nchars)))));
            st.store(&ctx, id);
            ed.focus = None;
            ed.slash_sel = 0;
        }
    }

    let focused = ctx.memory(|m| m.has_focus(id));
    let items = if focused && b.kind == BlockKind::Text && b.text.starts_with('/') && !b.text.contains('\n') {
        slash_items(&b.text[1..])
    } else {
        vec![]
    };
    let slash_active = !items.is_empty();
    if slash_active {
        ed.slash_sel = ed.slash_sel.min(items.len() - 1);
    }

    if focused {
        let range = TextEdit::load_state(&ctx, id).and_then(|s| s.cursor.char_range());
        let (cursor, collapsed) = range.map_or((nchars, true), |r| (r.primary.index, r.primary.index == r.secondary.index));
        if slash_active {
            let len = items.len();
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                ed.slash_sel = (ed.slash_sel + 1) % len;
                ed.slash_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                ed.slash_sel = (ed.slash_sel + len - 1) % len;
                ed.slash_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab)) {
                *act = Some(Act::Convert(idx, items[ed.slash_sel], true));
            }
        } else {
            if b.kind != BlockKind::Code {
                let shift = ui.input(|i| i.modifiers.shift);
                if !shift && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    let listy = matches!(
                        b.kind,
                        BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered | BlockKind::Quote | BlockKind::Callout
                    );
                    if b.text.is_empty() && listy {
                        *act = Some(Act::Convert(idx, BlockKind::Text, false));
                    } else {
                        *act = Some(Act::Split(idx, cursor));
                    }
                }
                ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
            }
            if collapsed && cursor == 0 && ui.input(|i| i.key_pressed(Key::Backspace)) {
                ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace));
                if b.kind != BlockKind::Text {
                    *act = Some(Act::Convert(idx, BlockKind::Text, false));
                } else if idx > 0 {
                    *act = Some(Act::Merge(idx));
                }
            }
            if b.kind != BlockKind::Code {
                let (row, rows) = ctx.data(|d| d.get_temp::<(usize, usize)>(id.with("rows"))).unwrap_or((1, 1));
                if row == 0 && idx > 0 && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    *act = Some(Act::FocusPrev(idx));
                }
                if row + 1 >= rows && idx + 1 < n && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    *act = Some(Act::FocusNext(idx));
                }
            }
        }
    }

    let hint_txt = if b.kind == BlockKind::Text && !focused { String::new() } else { hint.to_owned() };
    let out = TextEdit::multiline(&mut b.text)
        .id(id)
        .frame(false)
        .font(font.clone())
        .text_color(color)
        .desired_width(width)
        .desired_rows(1)
        .lock_focus(true)
        .margin(Margin::symmetric(0, 3))
        .hint_text(RichText::new(hint_txt).font(font).color(with_alpha(pal.muted, 0.75)))
        .show(ui);

    let mut cursor_idx = 0;
    if let Some(cr) = out.cursor_range {
        cursor_idx = cr.primary.ccursor.index;
        let row = cr.primary.rcursor.row;
        let rows = out.galley.rows.len();
        ctx.data_mut(|d| d.insert_temp(id.with("rows"), (row, rows)));
    }

    if out.response.changed() && b.kind == BlockKind::Text && act.is_none() {
        if let Some((kind, n)) = md_shortcut(&b.text, cursor_idx) {
            *act = Some(Act::Strip(idx, kind, n));
        }
    }

    // slash command popup
    if slash_active {
        let pos = out.response.rect.left_bottom() + vec2(0.0, 6.0);
        let t = appear(&ctx, Id::new("slash_appear"), 0.14);
        let moved = std::mem::take(&mut ed.slash_moved);
        let sel = ed.slash_sel;
        let mut pick: Option<BlockKind> = None;
        let screen = ctx.screen_rect();
        let y = pos.y.min(screen.bottom() - 340.0).max(10.0);
        egui::Area::new(Id::new("slash_menu"))
            .order(Order::Foreground)
            .fixed_pos(pos2(pos.x, y + (1.0 - t) * -5.0))
            .show(&ctx, |ui| {
                ui.set_opacity(t);
                crate::widgets::popup_frame(pal).show(ui, |ui| {
                    ui.set_width(300.0);
                    section_label(ui, pal, "BASIC BLOCKS");
                    ScrollArea::vertical().max_height(290.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (i, k) in items.iter().enumerate() {
                            let r = menu_item(ui, pal, k.glyph(), k.label(), Some(k.hint()), i == sel, false);
                            if i == sel && moved {
                                r.scroll_to_me(None);
                            }
                            if r.hovered() {
                                ed.slash_sel = i;
                            }
                            if r.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
                                pick = Some(*k);
                            }
                        }
                    });
                });
            });
        if let Some(k) = pick {
            *act = Some(Act::Convert(idx, k, true));
        }
    } else if focused || !ctx.memory(|m| m.focused().is_some()) {
        reset_appear(&ctx, Id::new("slash_appear"));
    }

    out.response.changed()
}

fn popups(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette) -> bool {
    let ctx = ui.ctx().clone();
    let mut changed = false;

    // block menu
    if let Some((bid, pos)) = ed.block_menu {
        let mut action: Option<Act> = None;
        let idx = page.blocks.iter().position(|b| b.id == bid);
        if let Some(idx) = idx {
            let cur_kind = page.blocks[idx].kind;
            let y = pos.y.min(ctx.screen_rect().bottom() - 460.0).max(10.0);
            let open = popup_area(&ctx, Id::new("block_menu"), pos2(pos.x, y), pal, &mut ed.fresh, 230.0, |ui| {
                section_label(ui, pal, "TURN INTO");
                for k in BlockKind::ALL.iter().filter(|k| **k != BlockKind::Divider) {
                    if menu_item(ui, pal, k.glyph(), k.label(), None, *k == cur_kind, false).clicked() {
                        action = Some(Act::Convert(idx, *k, false));
                    }
                }
                ui.add_space(4.0);
                ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                ui.add_space(5.0);
                if menu_item(ui, pal, "⧉", "Duplicate", None, false, false).clicked() {
                    action = Some(Act::Dup(idx));
                }
                if menu_item(ui, pal, "✕", "Delete", None, false, true).clicked() {
                    action = Some(Act::Remove(idx));
                }
            });
            if !open || action.is_some() {
                ed.block_menu = None;
            }
        } else {
            ed.block_menu = None;
        }
        if let Some(a) = action {
            apply(page, a, next_id, ed);
            changed = true;
        }
    }

    // icon picker
    if let Some(pos) = ed.icon_picker {
        let mut chosen: Option<String> = None;
        let mut remove = false;
        let open = popup_area(&ctx, Id::new("icon_picker"), pos, pal, &mut ed.fresh, 268.0, |ui| {
            section_label(ui, pal, "ICON");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                for e in ICONS {
                    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    ui.painter().rect_filled(r, 8.0, with_alpha(pal.hover, t));
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, e, body(22.0 + 3.0 * t), pal.text);
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        chosen = Some(e.to_owned());
                    }
                }
            });
            ui.add_space(4.0);
            if menu_item(ui, pal, "✕", "Remove icon", None, false, false).clicked() {
                remove = true;
            }
        });
        let chosen_flag = chosen.is_some();
        if let Some(e) = chosen {
            page.icon = e;
            changed = true;
        }
        if remove {
            page.icon.clear();
            changed = true;
        }
        if !open || chosen_flag || remove {
            ed.icon_picker = None;
        }
    }

    // cover picker
    if let Some(pos) = ed.cover_picker {
        let mut chosen: Option<usize> = None;
        let open = popup_area(&ctx, Id::new("cover_picker"), pos, pal, &mut ed.fresh, 268.0, |ui| {
            section_label(ui, pal, "GRADIENTS");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (i, (a, b)) in COVERS.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(60.0, 38.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    let rr = r.expand(t * 2.0);
                    gradient_rect(ui, rr, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
                    if page.cover == Some(i) {
                        ui.painter().rect_stroke(rr, 2.0, Stroke::new(2.0_f32, pal.accent), egui::StrokeKind::Outside);
                    }
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        chosen = Some(i);
                    }
                }
            });
        });
        if let Some(i) = chosen {
            page.cover = Some(i);
            changed = true;
        }
        if !open || chosen.is_some() {
            ed.cover_picker = None;
        }
    }
    changed
}

// ───────────────────────────── Board ─────────────────────────────

enum BAct {
    AddCard(usize),
    DelCard(usize, usize),
    MoveCard(usize, usize, i32),
    AddCol,
    DelCol(usize),
    CycleColor(usize),
}

fn board(ui: &mut Ui, page: &mut Page, next_id: &mut u64, pal: &Palette) -> bool {
    let mut changed = false;
    let mut act: Option<BAct> = None;
    ui.add_space(6.0);
    let page_id = page.id;
    ScrollArea::horizontal().id_salt(("board", page_id)).auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(14.0, 0.0);
            let ncols = page.columns.len();
            for ci in 0..ncols {
                let col = &mut page.columns[ci];
                Frame::new()
                    .fill(with_alpha(pal.sidebar, 1.0))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(268.0);
                        ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
                        // header
                        ui.horizontal(|ui| {
                            let (r, resp) = ui.allocate_exact_size(vec2(18.0, 24.0), Sense::click());
                            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                            ui.painter().circle_filled(r.center(), 5.0 + t * 1.5, tag_color(col.color));
                            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(TAG_NAMES[col.color % 6]).clicked() {
                                act = Some(BAct::CycleColor(ci));
                            }
                            let r = ui.add(
                                TextEdit::singleline(&mut col.title)
                                    .frame(false)
                                    .font(bold(14.0))
                                    .desired_width(150.0)
                                    .id(Id::new(("coltitle", col.id))),
                            );
                            changed |= r.changed();
                            ui.label(RichText::new(col.cards.len().to_string()).color(pal.muted).font(body(13.0)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if icon_button(ui, pal, Icon::Close, 12.0, "Delete group").clicked() {
                                    act = Some(BAct::DelCol(ci));
                                }
                            });
                        });
                        let ncards = col.cards.len();
                        for k in 0..ncards {
                            let card_id = col.cards[k].id;
                            let prev_hover = ui.ctx().data(|d| d.get_temp::<bool>(Id::new(("card_h", card_id)))).unwrap_or(false);
                            let t = ui.ctx().animate_bool_with_time(Id::new(("card_t", card_id)), prev_hover, 0.14);
                            let fr = Frame::new()
                                .fill(pal.card)
                                .stroke(Stroke::new(1.0_f32, lerp_color(pal.border, pal.accent.gamma_multiply(0.7), t * 0.6)))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 8))
                                .shadow(egui::Shadow {
                                    offset: [0, 2],
                                    blur: (2.0 + 8.0 * t) as u8,
                                    spread: 0,
                                    color: Color32::from_black_alpha((14.0 + 20.0 * t) as u8),
                                })
                                .show(ui, |ui| {
                                    ui.set_width(246.0);
                                    let r = ui.add(
                                        TextEdit::multiline(&mut col.cards[k].text)
                                            .frame(false)
                                            .font(body(14.5))
                                            .desired_width(246.0)
                                            .desired_rows(1)
                                            .hint_text(RichText::new("Untitled").color(with_alpha(pal.muted, 0.7)))
                                            .id(Id::new(("card", card_id))),
                                    );
                                    changed |= r.changed();
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 2.0;
                                        let mini = |ui: &mut Ui, icon: Icon, tip: &str| -> bool {
                                            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 20.0), Sense::click());
                                            if t > 0.05 {
                                                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                                                ui.painter().rect_filled(r, 5.0, with_alpha(pal.hover, h * t));
                                                crate::icons::paint(ui.painter(), r.center(), 13.0, icon, with_alpha(lerp_color(pal.muted, pal.text, h), t));
                                                resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked()
                                            } else {
                                                false
                                            }
                                        };
                                        if ci > 0 && mini(ui, Icon::ChevronLeft, "Move left") {
                                            act = Some(BAct::MoveCard(ci, k, -1));
                                        }
                                        if ci + 1 < ncols && mini(ui, Icon::ChevronRight, "Move right") {
                                            act = Some(BAct::MoveCard(ci, k, 1));
                                        }
                                        if mini(ui, Icon::Trash, "Delete card") {
                                            act = Some(BAct::DelCard(ci, k));
                                        }
                                    });
                                });
                            let hov = ui.rect_contains_pointer(fr.response.rect);
                            ui.ctx().data_mut(|d| d.insert_temp(Id::new(("card_h", card_id)), hov));
                        }
                        // + New
                        let (r, resp) = ui.allocate_exact_size(vec2(246.0, 30.0), Sense::click());
                        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                        ui.painter().rect_filled(r, 6.0, with_alpha(pal.active, h * 0.8));
                        crate::icons::paint(ui.painter(), pos2(r.left() + 16.0, r.center().y), 13.0, Icon::Plus, lerp_color(pal.muted, pal.text, h));
                        ui.painter().text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, "New", body(13.5), lerp_color(pal.muted, pal.text, h));
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            act = Some(BAct::AddCard(ci));
                        }
                        });
                    });
            }
            // add group
            let (r, resp) = ui.allocate_exact_size(vec2(200.0, 40.0), Sense::click());
            let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(r, 10.0, with_alpha(pal.hover, h));
            crate::icons::paint(ui.painter(), pos2(r.left() + 20.0, r.center().y), 14.0, Icon::Plus, lerp_color(pal.muted, pal.text, h));
            ui.painter().text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, "Add a group", body(14.0), lerp_color(pal.muted, pal.text, h));
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                act = Some(BAct::AddCol);
            }
        });
        ui.add_space(24.0);
    });

    if let Some(a) = act {
        changed = true;
        match a {
            BAct::AddCard(c) => {
                *next_id += 1;
                page.columns[c].cards.push(Card { id: *next_id, text: String::new() });
            }
            BAct::DelCard(c, k) => {
                page.columns[c].cards.remove(k);
            }
            BAct::MoveCard(c, k, d) => {
                let card = page.columns[c].cards.remove(k);
                let to = (c as i32 + d) as usize;
                page.columns[to].cards.push(card);
            }
            BAct::AddCol => {
                *next_id += 1;
                page.columns.push(Column {
                    id: *next_id,
                    title: "New group".into(),
                    color: page.columns.len() % 6,
                    cards: vec![],
                });
            }
            BAct::DelCol(c) => {
                page.columns.remove(c);
            }
            BAct::CycleColor(c) => {
                page.columns[c].color = (page.columns[c].color + 1) % 6;
            }
        }
    }
    changed
}
