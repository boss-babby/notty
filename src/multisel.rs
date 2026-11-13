//! Multi-block selection: drag a rectangle on the empty page area (marquee), then act on all selected blocks at once with a
//! floating toolbar or the keyboard (Delete, Ctrl+C/X/D, Esc, Ctrl+A, Ctrl+Z / Ctrl+Shift+Z).
//! Also hosts the block-level undo / redo history.

use crate::widgets::LayerRaise;
use egui::{pos2, vec2, Align2, Context, CursorIcon, Event, Id, Key, Margin, Modifiers, Order, Pos2, Rect, Response, Sense, Stroke, Ui};

use crate::editor::{gutter, swatches, EditorState};
use crate::icons::Icon;
use crate::lucide as l;
use crate::model::{Block, BlockKind, Page};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{self, with_alpha};

// ───────────────────────────── undo / redo ─────────────────────────────

/// Undo steps are recorded centrally by `history::track` (it sees every change, whatever caused it), so the
/// per-operation hook is a no-op kept for call-site clarity.
pub fn push_undo(_ed: &mut EditorState, _page: &Page) {}

pub fn undo(ed: &mut EditorState, page: &mut Page) -> bool {
    crate::history::undo(ed, page)
}

pub fn redo(ed: &mut EditorState, page: &mut Page) -> bool {
    crate::history::redo(ed, page)
}

// ───────────────────────────── operations on the selection ─────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum Op {
    Turn(BlockKind),
    Color(u8, bool),
    Indent(i8),
    Up,
    Down,
    Dup,
    Del,
    Copy,
    Cut,
    /// Toggle an inline format (`**`, `*`, `__`, `~~`, `` ` ``, `==`) on the whole text of every selected block.
    Fmt(&'static str),
}

fn md_line(b: &Block) -> String {
    let pad = "  ".repeat(b.indent as usize);
    match b.kind {
        BlockKind::H1 => format!("{pad}# {}", b.text),
        BlockKind::H2 => format!("{pad}## {}", b.text),
        BlockKind::H3 => format!("{pad}### {}", b.text),
        BlockKind::Todo => format!("{pad}- [{}] {}", if b.checked { "x" } else { " " }, b.text),
        BlockKind::Bullet | BlockKind::Toggle => format!("{pad}- {}", b.text),
        BlockKind::Numbered => format!("{pad}1. {}", b.text),
        BlockKind::Quote => format!("{pad}> {}", b.text),
        BlockKind::Callout => format!("{pad}> 💡 {}", b.text),
        BlockKind::Code => format!("{pad}```\n{}\n{pad}```", b.text),
        BlockKind::Equation => format!("{pad}$$\n{}\n{pad}$$", b.text),
        BlockKind::Divider => format!("{pad}---"),
        BlockKind::Image => format!("{pad}![{}]({})", b.text, b.src()),
        BlockKind::Embed | BlockKind::Bookmark | BlockKind::File => format!("{pad}[{}]({})", if b.text.is_empty() { &b.url } else { &b.text }, b.url),
        BlockKind::Table => crate::mdtable::to_markdown(b).lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n"),
        _ => format!("{pad}{}", b.text),
    }
}

/// Applies `op` to the selected blocks. Returns true when the page changed.
pub fn apply_op(ctx: &Context, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, op: Op) -> bool {
    let idxs: Vec<usize> = page.blocks.iter().enumerate().filter(|(_, b)| ed.sel.contains(&b.id)).map(|(i, _)| i).collect();
    if idxs.is_empty() {
        return false;
    }
    if matches!(op, Op::Copy | Op::Cut) {
        // children travel with their parent; the *exact* blocks are kept in the in-app clipboard
        let mut all: Vec<usize> = vec![];
        for &i in &idxs {
            let end = (i + 1..page.blocks.len()).find(|&j| page.blocks[j].indent <= page.blocks[i].indent).unwrap_or(page.blocks.len());
            all.extend(i..end);
        }
        all.sort_unstable();
        all.dedup();
        let text = all.iter().map(|&i| md_line(&page.blocks[i])).collect::<Vec<_>>().join("\n");
        ctx.copy_text(text.clone());
        crate::clip::set_blocks(text, all.iter().map(|&i| page.blocks[i].clone()).collect());
        if op == Op::Copy {
            return false;
        }
    }
    push_undo(ed, page);
    match op {
        Op::Turn(k) => {
            for &i in &idxs {
                let b = &mut page.blocks[i];
                if b.kind.has_text() {
                    b.kind = k;
                }
            }
        }
        Op::Color(c, is_bg) => {
            for &i in &idxs {
                if is_bg {
                    page.blocks[i].bg = c;
                } else {
                    page.blocks[i].color = c;
                }
            }
        }
        Op::Indent(d) => {
            for &i in &idxs {
                let max = if i == 0 { 0 } else { (page.blocks[i - 1].indent as i8 + 1).min(6) };
                let cur = page.blocks[i].indent as i8;
                page.blocks[i].indent = (cur + d).clamp(0, max.max(0)) as u8;
            }
        }
        Op::Up => {
            for i in 1..page.blocks.len() {
                if ed.sel.contains(&page.blocks[i].id) && !ed.sel.contains(&page.blocks[i - 1].id) {
                    page.blocks.swap(i - 1, i);
                }
            }
        }
        Op::Down => {
            for i in (0..page.blocks.len().saturating_sub(1)).rev() {
                if ed.sel.contains(&page.blocks[i].id) && !ed.sel.contains(&page.blocks[i + 1].id) {
                    page.blocks.swap(i, i + 1);
                }
            }
        }
        Op::Dup => {
            let last = *idxs.last().unwrap();
            let mut copies: Vec<Block> = idxs.iter().map(|&i| page.blocks[i].clone()).collect();
            for c in copies.iter_mut() {
                *next_id += 1;
                c.id = *next_id;
            }
            ed.sel = copies.iter().map(|c| c.id).collect();
            for (k, c) in copies.into_iter().enumerate() {
                page.blocks.insert(last + 1 + k, c);
            }
        }
        Op::Del | Op::Cut => {
            page.blocks.retain(|b| !ed.sel.contains(&b.id));
            if page.blocks.is_empty() {
                *next_id += 1;
                page.blocks.push(Block::new(*next_id, BlockKind::Text, "", 0));
            }
            ed.sel.clear();
        }
        Op::Fmt(mark) => {
            let flag = crate::rich::flag_for(mark);
            let mut rts: Vec<(usize, crate::rich::Rich)> = vec![];
            for &i in &idxs {
                let b = &page.blocks[i];
                if b.kind.has_text() && !b.kind.raw() && !b.text.is_empty() {
                    rts.push((i, crate::rich::Rich::parse(&b.text)));
                }
            }
            // every block already has it → remove it everywhere; otherwise add it where it is missing
            let all = rts.iter().all(|(_, r)| r.len() == 0 || r.all_have(0, r.len(), flag));
            for (i, mut r) in rts {
                let n = r.len();
                if n > 0 && (all || !r.all_have(0, n, flag)) {
                    r.toggle(0, n, flag);
                    page.blocks[i].text = r.markup();
                }
            }
        }
        Op::Copy => {}
    }
    true
}

/// Right-click / long-press menu for a block selection: every entry acts on *all* selected blocks.
pub fn menu(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette, pos: Pos2) -> bool {
    use crate::widgets::CtxItem;
    let ctx = ui.ctx().clone();
    let n = page.blocks.iter().filter(|b| ed.sel.contains(&b.id)).count();
    if n == 0 {
        ed.menu = None;
        return false;
    }
    let first = page.blocks.iter().position(|b| ed.sel.contains(&b.id)).unwrap_or(0);
    let texty = page.blocks.iter().any(|b| ed.sel.contains(&b.id) && b.kind.has_text() && !b.kind.raw());
    #[derive(Clone, Copy)]
    enum Cmd {
        Op(Op),
        Turn,
        Colour,
        Clear,
        Undo,
    }
    let mut list: Vec<(CtxItem, Cmd)> = vec![
        (CtxItem::new(l::COPY, if n == 1 { "Copy block" } else { "Copy blocks" }).hint("Ctrl+C"), Cmd::Op(Op::Copy)),
        (CtxItem::new(l::SCISSORS, if n == 1 { "Cut block" } else { "Cut blocks" }).hint("Ctrl+X"), Cmd::Op(Op::Cut)),
        (CtxItem::new(l::COPY_PLUS, "Duplicate").hint("Ctrl+D"), Cmd::Op(Op::Dup)),
        (CtxItem::new(l::REPLACE, "Turn into…").sep(), Cmd::Turn),
        (CtxItem::new(l::PALETTE, "Colour & background…"), Cmd::Colour),
    ];
    if texty {
        list.push((CtxItem::new(l::BOLD, "Bold").hint("Ctrl+B").sep(), Cmd::Op(Op::Fmt("**"))));
        list.push((CtxItem::new(l::ITALIC, "Italic").hint("Ctrl+I"), Cmd::Op(Op::Fmt("*"))));
        list.push((CtxItem::new(l::UNDERLINE, "Underline").hint("Ctrl+U"), Cmd::Op(Op::Fmt("__"))));
        list.push((CtxItem::new(l::STRIKETHROUGH, "Strikethrough").hint("Ctrl+Shift+S"), Cmd::Op(Op::Fmt("~~"))));
        list.push((CtxItem::new(l::CODE, "Inline code").hint("Ctrl+E"), Cmd::Op(Op::Fmt("`"))));
        list.push((CtxItem::new(l::HIGHLIGHTER, "Highlight").hint("Ctrl+Shift+H"), Cmd::Op(Op::Fmt("=="))));
    }
    list.push((CtxItem::new(l::INDENT_INCREASE, "Indent").hint("Tab").sep().enabled(first > 0), Cmd::Op(Op::Indent(1))));
    list.push((CtxItem::new(l::INDENT_DECREASE, "Outdent").hint("Shift+Tab"), Cmd::Op(Op::Indent(-1))));
    list.push((CtxItem::new(l::ARROW_UP, "Move up"), Cmd::Op(Op::Up)));
    list.push((CtxItem::new(l::ARROW_DOWN, "Move down"), Cmd::Op(Op::Down)));
    list.push((CtxItem::new(l::UNDO_2, "Undo").hint("Ctrl+Z").enabled(ed.undo.iter().any(|(p, _)| *p == page.id)), Cmd::Undo));
    list.push((CtxItem::new(l::X, "Clear selection").hint("Esc").sep(), Cmd::Clear));
    list.push((CtxItem::new(l::TRASH_2, if n == 1 { "Delete block" } else { "Delete blocks" }).hint("Del").danger(), Cmd::Op(Op::Del)));
    let items: Vec<CtxItem> = list.iter().map(|x| x.0).collect();
    let (open, pick) = widgets::context_menu(&ctx, Id::new("ed_menu_multi"), pos, pal, &mut ed.menu_fresh, &items);
    let mut changed = false;
    if let Some(pi) = pick {
        match list[pi].1 {
            Cmd::Op(op) => changed |= apply_op(&ctx, page, next_id, ed, op),
            Cmd::Turn => ed.sel_menu = 1,
            Cmd::Colour => ed.sel_menu = 2,
            Cmd::Clear => ed.sel.clear(),
            Cmd::Undo => ed.want_undo = true,
        }
    }
    if !open {
        ed.menu = None;
    }
    changed
}

// ───────────────────────────── marquee + keyboard ─────────────────────────────

/// Handles drag selection on the page background, paints the selection and processes keyboard commands.
/// `rects` are the laid-out block rows, `origin` is the content-space anchor used to keep the marquee glued to the page while scrolling.
#[allow(clippy::too_many_arguments)]
pub fn handle(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette, bg: &Response, rects: &[(usize, Rect)], origin: Pos2) -> bool {
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let pointer = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
    let mods = ctx.input(|i| i.modifiers);

    // Touch: tap-and-hold, then drag, is what starts a selection (marquee on the page, text selection inside a block). The hold itself
    // is consumed here; holding *without* dragging opens the context menu instead (touch.rs → right-click), whose "Select block"
    // entry starts block selection. A plain swipe only scrolls.
    let _ = crate::touch::take_long_press(&ctx);

    // Touch: while blocks are selected a plain tap on another block adds / removes it (no keyboard, no second long press).
    let mut toggled = false;
    if ed.touch_sel && !ed.sel.is_empty() && crate::touch::touchy() {
        let tap = ctx.input(|i| if i.pointer.any_click() { i.pointer.interact_pos() } else { None });
        if let Some(p) = tap {
            let on_page = ctx.layer_id_at(p).map_or(true, |l| l.order == Order::Background);
            if let (true, Some((i, _))) = (on_page, rects.iter().find(|(_, r)| r.expand2(vec2(60.0, 3.0)).contains(p))) {
                let id = page.blocks[*i].id;
                if let Some(k) = ed.sel.iter().position(|b| *b == id) {
                    ed.sel.remove(k);
                } else {
                    ed.sel.push(id);
                }
                ed.touch_sel = !ed.sel.is_empty();
                ed.focus = None;
                ed.active = None;
                toggled = true;
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f);
                    }
                });
            }
        }
    }

    if bg.drag_started() {
        // egui lets a drag-sensing widget below receive drags that began on click-only widgets above it (toolbars, popups…):
        // only start a selection when the press was really on the page itself.
        let on_page = ctx.input(|i| i.pointer.press_origin()).map_or(false, |p| ctx.layer_id_at(p).map_or(true, |l| l.order == Order::Background));
        if let (true, Some(p)) = (on_page, ctx.input(|i| i.pointer.press_origin())) {
            ed.marquee = Some(p - origin);
            let additive = mods.shift || mods.command || (ed.touch_sel && !ed.sel.is_empty());
            ed.sel_base = if additive { ed.sel.clone() } else { vec![] };
            if !additive {
                ed.sel.clear();
            }
            ed.sel_menu = 0;
            ed.fmt_bar = None;
            ed.focus = None;
            ctx.memory_mut(|m| {
                if let Some(f) = m.focused() {
                    m.surrender_focus(f);
                }
            });
        }
    }
    if let (Some(start), true) = (ed.marquee, bg.dragged()) {
        if let Some(cur) = pointer {
            let r = Rect::from_two_pos(origin + start, cur);
            let mut sel = ed.sel_base.clone();
            for (i, row) in rects {
                if row.intersects(r) {
                    let id = page.blocks[*i].id;
                    if !sel.contains(&id) {
                        sel.push(id);
                    }
                }
            }
            ed.sel = sel;
            ui.painter().rect_filled(r, 2.0, with_alpha(pal.accent, 0.10));
            ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.75)), egui::StrokeKind::Inside);
            let clip = ui.clip_rect();
            if cur.y < clip.top() + 36.0 {
                ui.scroll_with_delta(vec2(0.0, 16.0));
            } else if cur.y > clip.bottom() - 36.0 {
                ui.scroll_with_delta(vec2(0.0, -16.0));
            }
            ctx.request_repaint();
        }
    }
    if bg.drag_stopped() || !ctx.input(|i| i.pointer.primary_down()) {
        ed.marquee = None;
    }
    if bg.clicked() && !toggled {
        ed.sel.clear();
    }
    let text_focus = ctx.memory(|m| m.focused().is_some());
    if text_focus && ed.marquee.is_none() && !ed.sel.is_empty() && !toggled {
        ed.sel.clear();
    }
    if ed.sel.is_empty() {
        ed.touch_sel = false;
    }

    // highlight
    if !ed.sel.is_empty() {
        for (i, row) in rects {
            if ed.sel.contains(&page.blocks[*i].id) {
                let r = Rect::from_min_max(pos2(row.left() + gutter() - 8.0 + page.blocks[*i].indent as f32 * 28.0, row.top() - 1.0), pos2(row.right() + 6.0, row.bottom() + 1.0));
                ui.painter().rect_filled(r, 5.0, with_alpha(pal.accent, 0.16));
            }
        }
    }

    // keyboard (only while no text field is being edited)
    if !text_focus {
        let (copy, cut) = ctx.input(|i| (i.events.iter().any(|e| matches!(e, Event::Copy)), i.events.iter().any(|e| matches!(e, Event::Cut))));
        if !ed.sel.is_empty() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                ed.sel.clear();
            } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
                changed |= apply_op(&ctx, page, next_id, ed, Op::Del);
            } else if cut {
                changed |= apply_op(&ctx, page, next_id, ed, Op::Cut);
            } else if copy {
                apply_op(&ctx, page, next_id, ed, Op::Copy);
            } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) {
                changed |= apply_op(&ctx, page, next_id, ed, Op::Dup);
            } else {
                // inline-format shortcuts act on every selected block
                let marks: [(Modifiers, Key, &'static str); 6] = [
                    (Modifiers::COMMAND | Modifiers::SHIFT, Key::S, "~~"),
                    (Modifiers::COMMAND | Modifiers::SHIFT, Key::H, "=="),
                    (Modifiers::COMMAND, Key::B, "**"),
                    (Modifiers::COMMAND, Key::I, "*"),
                    (Modifiers::COMMAND, Key::E, "`"),
                    (Modifiers::COMMAND, Key::U, "__"),
                ];
                for (m, k, mark) in marks {
                    if ctx.input_mut(|i| i.consume_key(m, k)) {
                        changed |= apply_op(&ctx, page, next_id, ed, Op::Fmt(mark));
                        break;
                    }
                }
            }
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            ed.sel = page.blocks.iter().map(|b| b.id).collect();
        }
        // paste: exact blocks (or Markdown → blocks) after the selection / at the end of the page
        let pasted = ctx.input(|i| i.events.iter().find_map(|e| if let Event::Paste(t) = e { Some(t.clone()) } else { None }));
        if let Some(t) = pasted {
            if let Some(blocks) = crate::clip::blocks_for(&t).or_else(|| crate::clip::markdown_blocks(&t)) {
                push_undo(ed, page);
                let after = page.blocks.iter().rposition(|b| ed.sel.contains(&b.id));
                let at = match after {
                    Some(i) => (i + 1..page.blocks.len()).find(|&j| page.blocks[j].indent <= page.blocks[i].indent).unwrap_or(page.blocks.len()),
                    None => page.blocks.len(),
                };
                let base = after.map_or(0, |i| page.blocks[i].indent);
                let mut ids = vec![];
                for (k, mut nb) in blocks.into_iter().enumerate() {
                    *next_id += 1;
                    nb.id = *next_id;
                    nb.indent = (nb.indent + base).min(6);
                    ids.push(nb.id);
                    page.blocks.insert(at + k, nb);
                }
                ed.sel = ids;
                changed = true;
            }
        }
        let redo_k = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y));
        if redo_k {
            changed |= redo(ed, page);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            changed |= undo(ed, page);
        }
    }
    changed
}

// ───────────────────────────── floating toolbar ─────────────────────────────

fn pill(ui: &mut Ui, pal: &Palette, glyph: &str, label: &str, active: bool) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), body(13.0), pal.text);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 40.0, 28.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(r, 6.0, with_alpha(if active { pal.active } else { pal.hover }, if active { 1.0 } else { t }));
    ui.painter().text(pos2(r.left() + 16.0, r.center().y), Align2::CENTER_CENTER, glyph, crate::icons::font(14.0), pal.muted);
    ui.painter().text(pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, label, body(13.0), pal.text);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// The toolbar that appears while blocks are selected.
pub fn bar(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette) -> bool {
    let ctx = ui.ctx().clone();
    if ed.sel.is_empty() || ed.marquee.is_some() {
        return false;
    }
    let n = page.blocks.iter().filter(|b| ed.sel.contains(&b.id)).count();
    if n == 0 {
        ed.sel.clear();
        return false;
    }
    let clip = ui.clip_rect();
    // phones: the bar is as wide as the screen, wraps onto a second row and sits at the bottom (thumb reach, never off-screen)
    let narrow = clip.width() < 520.0;
    let width = if narrow { (clip.width() - 16.0).max(200.0) } else { 468.0 };
    let pos = pos2((clip.center().x - width / 2.0).max(clip.left() + 8.0), clip.top() + 12.0);
    let mut op: Option<Op> = None;
    let mut menu = ed.sel_menu;
    let mut close = false;
    let (cur_c, cur_bg) = page.blocks.iter().find(|b| ed.sel.contains(&b.id)).map(|b| (b.color, b.bg)).unwrap_or((0, 0));
    let can_undo = ed.undo.iter().any(|(p, _)| *p == page.id);
    let area_b = egui::Area::new(Id::new("sel_bar")).order(Order::Foreground);
    let area_b = if narrow { area_b.anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -10.0)) } else { area_b.fixed_pos(pos) };
    let area = area_b.show(&ctx, |ui| {
        widgets::popup_frame(pal).inner_margin(Margin::same(5)).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if narrow {
                ui.set_width(width - 10.0);
            }
            let row = |ui: &mut Ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                if narrow {
                    ui.set_max_width(width - 10.0);
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("{n} selected")).font(bold(13.0)).color(pal.text));
                ui.add_space(8.0);
                let sep = |ui: &mut Ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(9.0, 26.0), Sense::hover());
                    ui.painter().vline(r.center().x, r.y_range().shrink(4.0), Stroke::new(1.0_f32, pal.border));
                };
                sep(ui);
                if widgets::icon_button(ui, pal, Icon::Type, 15.0, "Turn into").clicked() {
                    menu = if menu == 1 { 0 } else { 1 };
                }
                if widgets::icon_button(ui, pal, Icon::Palette, 15.0, "Colour & background").clicked() {
                    menu = if menu == 2 { 0 } else { 2 };
                }
                sep(ui);
                if widgets::icon_button(ui, pal, Icon::Outdent, 15.0, "Outdent").clicked() {
                    op = Some(Op::Indent(-1));
                }
                if widgets::icon_button(ui, pal, Icon::Indent, 15.0, "Indent").clicked() {
                    op = Some(Op::Indent(1));
                }
                if widgets::icon_button(ui, pal, Icon::MoveUp, 15.0, "Move up").clicked() {
                    op = Some(Op::Up);
                }
                if widgets::icon_button(ui, pal, Icon::MoveDown, 15.0, "Move down").clicked() {
                    op = Some(Op::Down);
                }
                sep(ui);
                if widgets::icon_button(ui, pal, Icon::Duplicate, 15.0, "Duplicate  Ctrl+D").clicked() {
                    op = Some(Op::Dup);
                }
                if widgets::icon_button(ui, pal, Icon::Copy, 15.0, "Copy as Markdown  Ctrl+C").clicked() {
                    op = Some(Op::Copy);
                }
                if ui.add_enabled_ui(can_undo, |ui| widgets::icon_button(ui, pal, Icon::Undo, 15.0, "Undo  Ctrl+Z")).inner.clicked() {
                    ed.want_undo = true;
                }
                if widgets::icon_button(ui, pal, Icon::Trash, 15.0, "Delete  Del").clicked() {
                    op = Some(Op::Del);
                }
                if widgets::icon_button(ui, pal, Icon::Close, 15.0, "Clear selection  Esc").clicked() {
                    close = true;
                }
            };
            if narrow {
                ui.horizontal_wrapped(row);
            } else {
                ui.horizontal(row);
            }
            if menu == 1 {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    ui.set_max_width(width - 20.0);
                    for k in BlockKind::TEXTY {
                        if pill(ui, pal, k.glyph(), k.label(), false).clicked() {
                            op = Some(Op::Turn(k));
                        }
                    }
                });
            } else if menu == 2 {
                ui.add_space(6.0);
                ui.set_max_width(width - 20.0);
                widgets::section_label(ui, pal, "TEXT COLOUR");
                let mut pick = None;
                swatches(ui, pal, cur_c, false, &mut pick);
                if let Some(c) = pick {
                    op = Some(Op::Color(c, false));
                }
                widgets::section_label(ui, pal, "BACKGROUND");
                let mut pick = None;
                swatches(ui, pal, cur_bg, true, &mut pick);
                if let Some(c) = pick {
                    op = Some(Op::Color(c, true));
                }
            }
        });
    });
    ctx.raise_layer(area.response.layer_id);
    ed.sel_menu = menu;
    let mut changed = false;
    if let Some(o) = op {
        changed |= apply_op(&ctx, page, next_id, ed, o);
        if matches!(o, Op::Turn(_)) {
            ed.sel_menu = 0;
        }
    }
    if ed.want_undo {
        ed.want_undo = false;
        changed |= undo(ed, page);
    }
    if close {
        ed.sel.clear();
    }
    let _ = (lerp_color, l::X);
    changed
}
