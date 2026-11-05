//! Small reusable, animated widgets.

use egui::{
    emath::easing, pos2, text::{CCursor, CCursorRange}, vec2, Align2, Area, Color32, Context, CornerRadius, CursorIcon,
    Event, Frame, Id, Key, Margin, Modifiers, Order, PointerButton, Pos2, RawInput, Rect, Response, Sense, Shadow,
    Stroke, TextEdit, Ui, Vec2,
};

use crate::icons::{self, Icon};
use crate::theme::{bold, body, lerp_color, Palette};

pub fn ease(t: f32) -> f32 {
    easing::cubic_out(t.clamp(0.0, 1.0))
}

/// 0→1 animation that starts the first time it is called for `id`. Call [`reset_appear`] when hidden.
pub fn appear(ctx: &Context, id: Id, dur: f32) -> f32 {
    let now = ctx.input(|i| i.time);
    let t0 = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with::<f64>(id, || now));
    let t = ((now - t0) / dur as f64) as f32;
    if t < 1.0 {
        ctx.request_repaint();
    }
    ease(t)
}

pub fn reset_appear(ctx: &Context, id: Id) {
    ctx.data_mut(|d| d.remove::<f64>(id));
}

pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

pub fn popup_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.popup)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(6))
        .shadow(Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(60) })
}

/// Floating popup anchored at `pos`. Returns `false` when it should close (click outside / Esc).
pub fn popup_area(
    ctx: &Context,
    id: Id,
    pos: Pos2,
    pal: &Palette,
    fresh: &mut bool,
    width: f32,
    add: impl FnOnce(&mut Ui),
) -> bool {
    let t = appear(ctx, id.with("appear"), 0.16);
    let screen = ctx.screen_rect();
    // phones: a popup can never be wider than the screen
    let width = width.min((screen.width() - 16.0).max(180.0));
    let x = pos.x.min(screen.right() - width - 12.0).max(8.0);
    // Keep the whole popup on screen: use the size measured on the previous frame.
    let h = ctx.data(|d| d.get_temp::<f32>(id.with("h"))).unwrap_or(0.0);
    let y = if h > 0.0 { pos.y.min(screen.bottom() - h - 10.0).max(8.0) } else { pos.y };
    let resp = Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(pos2(x, y + (1.0 - t) * -6.0))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            popup_frame(pal).show(ui, |ui| {
                ui.set_width(width);
                // a menu taller than the screen (phones, landscape) scrolls instead of running off it
                let max_h = (screen.height() - 28.0).max(140.0);
                egui::ScrollArea::vertical().id_salt(id.with("popup_scroll")).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    add(ui);
                });
            });
        });
    // Always draw popups above everything else (modal backdrops included).
    ctx.move_to_top(resp.response.layer_id);
    let rect = resp.response.rect;
    ctx.data_mut(|d| d.insert_temp(id.with("h"), rect.height()));
    let mut open = true;
    if ctx.input(|i| i.pointer.any_pressed()) && !*fresh {
        if let Some(p) = ctx.pointer_interact_pos() {
            if !rect.contains(p) {
                open = false;
            }
        }
    }
    if let (Some(p), false) = (rclick(ctx), *fresh) {
        if !rect.contains(p) {
            open = false;
        }
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    *fresh = false;
    if !open {
        reset_appear(ctx, id.with("appear"));
    }
    open
}

pub fn icon_button(ui: &mut Ui, pal: &Palette, icon: Icon, size: f32, tip: &str) -> Response {
    let side = if crate::touch::touchy() { (size + 12.0).max(38.0) } else { size + 12.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    paint_icon_button(ui, pal, rect, &resp, icon, size);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

pub fn paint_icon_button(ui: &Ui, pal: &Palette, rect: Rect, resp: &Response, icon: Icon, size: f32) {
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    if t > 0.0 {
        let fill = if resp.is_pointer_button_down_on() { pal.active } else { pal.hover };
        ui.painter().rect_filled(rect, 6.0, with_alpha(fill, t));
    }
    icons::paint(ui.painter(), rect.center(), size, icon, lerp_color(pal.muted, pal.text, t));
}

pub fn primary_button(ui: &mut Ui, pal: &Palette, label: &str, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 38.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
    let down = resp.is_pointer_button_down_on();
    let base = pal.accent;
    let fill = lerp_color(base, Color32::from_rgb(20, 100, 190), t * 0.5);
    let r = if down { rect.shrink(1.0) } else { rect };
    ui.painter().rect_filled(r, 8.0, fill);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, bold(14.5), Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn ghost_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), body(13.5), pal.muted);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(18.0, 10.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
    let col = lerp_color(pal.muted, pal.text, t);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, body(13.5), col);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn section_label(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(text).font(bold(11.5)).color(pal.muted));
    });
    ui.add_space(2.0);
}

/// Full-width menu row with optional leading glyph and sub-label.
/// Paints single-line text that is truncated with an ellipsis instead of overflowing its box. Returns the painted width.
pub fn text_fit(p: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> f32 {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(10.0));
    let g = p.layout_job(job);
    let rect = anchor.anchor_size(pos, g.size());
    let w = g.size().x;
    p.galley(rect.min, g, color);
    w
}

pub fn menu_item(
    ui: &mut Ui,
    pal: &Palette,
    lead: &str,
    label: &str,
    sub: Option<&str>,
    selected: bool,
    danger: bool,
) -> Response {
    let tt = crate::touch::touchy();
    let h = if sub.is_some() { if tt { 54.0 } else { 46.0 } } else if tt { 40.0 } else { 30.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || selected, 0.1);
    let p = ui.painter();
    if t > 0.0 {
        p.rect_filled(rect, 6.0, with_alpha(pal.hover, t));
    }
    let col = if danger { pal.danger } else { pal.text };
    let mut x = rect.left() + 8.0;
    if !lead.is_empty() {
        let is_icon = lead.chars().next().map_or(false, |c| (0xE000..=0xF8FF).contains(&(c as u32)));
        if sub.is_some() {
            let b = Rect::from_center_size(pos2(x + 17.0, rect.center().y), vec2(34.0, 34.0));
            p.rect_filled(b, 6.0, pal.bg);
            p.rect_stroke(b, 6.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
            let f = if is_icon { icons::font(17.0) } else { bold(13.0) };
            p.text(b.center(), Align2::CENTER_CENTER, lead, f, lerp_color(col, pal.accent, t * 0.6));
            x += 44.0;
        } else {
            let f = if is_icon { icons::font(15.5) } else { bold(12.5) };
            let ic = if danger { pal.danger } else { lerp_color(pal.muted, pal.text, t) };
            p.text(pos2(x + 9.0, rect.center().y), Align2::CENTER_CENTER, lead, f, ic);
            x += 28.0;
        }
    }
    match sub {
        Some(s) => {
            let mw = rect.right() - x - 8.0;
            text_fit(p, pos2(x, rect.center().y - 8.0), Align2::LEFT_CENTER, label, body(14.0), col, mw);
            text_fit(p, pos2(x, rect.center().y + 9.0), Align2::LEFT_CENTER, s, body(12.0), pal.muted, mw);
        }
        None => {
            text_fit(p, pos2(x, rect.center().y), Align2::LEFT_CENTER, label, body(14.0), col, rect.right() - x - 8.0);
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Axis-aligned 4-corner gradient quad.
pub fn gradient_rect(ui: &Ui, rect: Rect, a: Color32, b: Color32) {
    let mut mesh = egui::Mesh::default();
    let mid = lerp_color(a, b, 0.5);
    mesh.colored_vertex(rect.left_top(), a);
    mesh.colored_vertex(rect.right_top(), mid);
    mesh.colored_vertex(rect.right_bottom(), b);
    mesh.colored_vertex(rect.left_bottom(), mid);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Soft radial glow (fan mesh from an opaque-ish centre to a transparent rim).
pub fn radial_glow(p: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let n = 48;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(pos2(center.x + a.cos() * radius, center.y + a.sin() * radius), Color32::TRANSPARENT);
    }
    for i in 1..=n {
        mesh.add_triangle(0, i, i + 1);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Rounded text field with an animated focus ring.
pub fn field(
    ui: &mut Ui,
    pal: &Palette,
    fill: Color32,
    text: &mut String,
    hint: &str,
    password: bool,
    width: f32,
    id: Id,
) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    let t = ui.ctx().animate_bool_with_time(id.with("focus"), focused, 0.15);
    let stroke = Stroke::new(1.0_f32 + t * 0.7, lerp_color(pal.border.gamma_multiply(1.6), pal.accent, t));
    Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .id(id)
                    .password(password)
                    .frame(false)
                    .hint_text(egui::RichText::new(hint).color(with_alpha(pal.muted, 0.8)))
                    .desired_width(width - 24.0)
                    .font(body(15.0)),
            )
        })
        .inner
}


// ───────────────────────────── right-click support ─────────────────────────────

const RC_KEY: &str = "lumen_rclick";

/// Hook for `App::raw_input_hook`: strips right-button events so egui's text fields never collapse or move the
/// selection when you right-click, and remembers where the click happened (see [`rclick`]).
pub fn filter_raw_input(ctx: &Context, raw: &mut RawInput) {
    let mut found = None;
    raw.events.retain(|e| match e {
        Event::PointerButton { pos, button: PointerButton::Secondary, pressed, .. } => {
            if *pressed {
                found = Some(*pos);
            }
            false
        }
        _ => true,
    });
    if let Some(p) = found {
        ctx.data_mut(|d| d.insert_temp(Id::new(RC_KEY), p));
        ctx.request_repaint();
    }
}

/// A press-and-hold on a touch screen counts as a right-click at `p` (see `touch.rs`): every context menu that reacts to
/// [`right_clicked`] opens, exactly as for a mouse.
pub fn inject_rclick(ctx: &Context, p: Pos2) {
    ctx.data_mut(|d| d.insert_temp(Id::new(RC_KEY), p));
    ctx.request_repaint();
}

/// Position of a right-click that happened this frame.
pub fn rclick(ctx: &Context) -> Option<Pos2> {
    ctx.data(|d| d.get_temp::<Pos2>(Id::new(RC_KEY)))
}

/// Must be called once at the end of every frame.
pub fn end_frame(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<Pos2>(Id::new(RC_KEY)));
}

/// A right-click landed on `rect` (and that part of the screen is not covered by a popup / other layer).
pub fn right_clicked(ui: &Ui, rect: Rect) -> Option<Pos2> {
    let p = rclick(ui.ctx())?;
    if rect.contains(p) && ui.clip_rect().contains(p) && ui.ctx().layer_id_at(p).map_or(true, |l| l == ui.layer_id()) {
        Some(p)
    } else {
        None
    }
}

#[derive(Clone, Copy)]
pub struct CtxItem {
    pub icon: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub danger: bool,
    pub enabled: bool,
    pub sep: bool,
}

impl CtxItem {
    pub fn new(icon: &'static str, label: &'static str) -> Self {
        Self { icon, label, hint: "", danger: false, enabled: true, sep: false }
    }
    pub fn hint(mut self, h: &'static str) -> Self {
        self.hint = h;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    /// Draw a separator line above this item.
    pub fn sep(mut self) -> Self {
        self.sep = true;
        self
    }
}

/// Animated right-click menu. Returns `(still_open, picked_index)`.
/// Flips to stay on screen, supports ↑/↓/Enter, closes on Esc or when clicking elsewhere.
pub fn context_menu(ctx: &Context, id: Id, pos: Pos2, pal: &Palette, fresh: &mut bool, items: &[CtxItem]) -> (bool, Option<usize>) {
    const W: f32 = 236.0;
    let t = appear(ctx, id.with("appear"), 0.14);
    let screen = ctx.screen_rect();
    let size = ctx
        .data(|d| d.get_temp::<Vec2>(id.with("size")))
        .unwrap_or_else(|| vec2(W + 14.0, 14.0 + items.len() as f32 * 30.0));
    let mut x = pos.x;
    let mut y = pos.y;
    if x + size.x > screen.right() - 8.0 {
        x = (pos.x - size.x).max(8.0);
    }
    let flip = y + size.y > screen.bottom() - 8.0;
    if flip {
        y = (pos.y - size.y).max(8.0);
    }
    let slide = (1.0 - t) * 7.0 * if flip { 1.0 } else { -1.0 };

    let sel_id = id.with("sel");
    let mut sel: Option<usize> = ctx.data(|d| d.get_temp(sel_id));
    let n = items.len();
    let mut picked: Option<usize> = None;
    if n > 0 {
        let step = |from: Option<usize>, dir: i32| -> Option<usize> {
            let mut i = from.map_or(if dir > 0 { -1 } else { 0 }, |v| v as i32);
            for _ in 0..n {
                i = (i + dir).rem_euclid(n as i32);
                if items[i as usize].enabled {
                    return Some(i as usize);
                }
            }
            None
        };
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            sel = step(sel, 1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
            sel = step(sel, -1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            if let Some(i) = sel {
                picked = Some(i);
            }
        }
    }

    let resp = Area::new(id).order(Order::Foreground).fixed_pos(pos2(x, y + slide)).show(ctx, |ui| {
        ui.set_opacity(t);
        popup_frame(pal).inner_margin(Margin::same(6)).show(ui, |ui| {
            ui.set_width(W);
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, it) in items.iter().enumerate() {
                if it.sep && i > 0 {
                    ui.add_space(3.0);
                    let r = ui.available_rect_before_wrap();
                    ui.painter().hline(r.x_range().shrink(6.0), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                    ui.add_space(4.0);
                }
                let (rect, r) = ui.allocate_exact_size(vec2(W, 30.0), if it.enabled { Sense::click() } else { Sense::hover() });
                if r.hovered() && it.enabled {
                    sel = Some(i);
                }
                let active = sel == Some(i) && it.enabled;
                let h = ui.ctx().animate_bool_with_time(r.id, active, 0.09);
                let p = ui.painter();
                if h > 0.0 {
                    let base = if it.danger { pal.danger.gamma_multiply(0.14) } else { pal.hover };
                    p.rect_filled(rect, 6.0, with_alpha(base, h));
                }
                let dim = if it.enabled { 1.0 } else { 0.38 };
                let col = if it.danger { pal.danger } else { pal.text };
                let ic = if it.danger { pal.danger } else { lerp_color(pal.muted, pal.text, h) };
                p.text(pos2(rect.left() + 17.0, rect.center().y), Align2::CENTER_CENTER, it.icon, icons::font(15.5), with_alpha(ic, dim));
                let mut hint_w = 0.0;
                if !it.hint.is_empty() {
                    hint_w = text_fit(p, pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, it.hint, body(11.5), with_alpha(pal.muted, 0.9 * dim), 90.0) + 12.0;
                }
                text_fit(p, pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, it.label, body(14.0), with_alpha(col, dim), W - 46.0 - hint_w);
                if r.on_hover_cursor(if it.enabled { CursorIcon::PointingHand } else { CursorIcon::Default }).clicked() && it.enabled {
                    picked = Some(i);
                }
            }
        });
    });
    ctx.move_to_top(resp.response.layer_id);
    let rect = resp.response.rect;
    ctx.data_mut(|d| {
        d.insert_temp(id.with("size"), rect.size());
        match sel {
            Some(s) => d.insert_temp(sel_id, s),
            None => {
                d.remove::<usize>(sel_id);
            }
        }
    });

    let mut open = true;
    if !*fresh {
        if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.pointer_interact_pos() {
                open &= rect.contains(p);
            }
        }
        if let Some(p) = rclick(ctx) {
            open &= rect.contains(p);
        }
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    *fresh = false;
    if picked.is_some() {
        open = false;
    }
    if !open {
        reset_appear(ctx, id.with("appear"));
        ctx.data_mut(|d| {
            d.remove::<Vec2>(id.with("size"));
            d.remove::<usize>(sel_id);
        });
    }
    (open, picked)
}

// ───────────────────────────── text helpers ─────────────────────────────

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

/// Text of the char range `a..b`.
pub fn slice_chars(s: &str, a: usize, b: usize) -> String {
    let (a, b) = (a.min(b), a.max(b));
    s[char_to_byte(s, a)..char_to_byte(s, b)].to_owned()
}

/// Replaces the char range `a..b` with `with`; returns the new caret position (in chars).
pub fn replace_chars(s: &mut String, a: usize, b: usize, with: &str) -> usize {
    let (a, b) = (a.min(b), a.max(b));
    let (ba, bb) = (char_to_byte(s, a), char_to_byte(s, b));
    s.replace_range(ba..bb, with);
    a + with.chars().count()
}

/// Selection of a text edit as ordered `(start, end)` char indices (collapsed caret → `start == end`).
pub fn selection(ctx: &Context, id: Id) -> Option<(usize, usize)> {
    let r = TextEdit::load_state(ctx, id)?.cursor.char_range()?;
    let (a, b) = (r.primary.index, r.secondary.index);
    Some((a.min(b), a.max(b)))
}

/// Sets the selection of a text edit and gives it keyboard focus.
pub fn set_selection(ctx: &Context, id: Id, a: usize, b: usize) {
    let mut st = TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    st.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// Right-click menu (Cut / Copy / Paste / Select all) for any single text field. Call right after adding the
/// `TextEdit` with its response; `text` is the same buffer.
pub fn field_menu(ui: &Ui, pal: &Palette, resp: &Response, text: &mut String) -> bool {
    let ctx = ui.ctx().clone();
    let id = resp.id;
    let mkey = id.with("fmenu");
    let mut changed = false;

    // finish a pending async paste
    if let Some((a, b)) = ctx.data(|d| d.get_temp::<(usize, usize)>(id.with("fwait"))) {
        if let Some(clip) = crate::platform::take_paste() {
            let clip = clip.replace(['\r', '\n'], " ");
            let at = replace_chars(text, a, b, &clip);
            set_selection(&ctx, id, at, at);
            ctx.data_mut(|d| d.remove::<(usize, usize)>(id.with("fwait")));
            changed = true;
        }
    }
    if let Some(p) = right_clicked(ui, resp.rect) {
        let sel = selection(&ctx, id).unwrap_or((text.chars().count(), text.chars().count()));
        ctx.data_mut(|d| {
            d.insert_temp(mkey, (p, sel.0, sel.1));
            d.insert_temp(mkey.with("fresh"), true);
        });
    }
    let Some((pos, a, b)) = ctx.data(|d| d.get_temp::<(Pos2, usize, usize)>(mkey)) else { return changed };
    let mut fresh = ctx.data(|d| d.get_temp::<bool>(mkey.with("fresh"))).unwrap_or(false);
    let has_sel = a != b;
    let items = [
        CtxItem::new(crate::lucide::SCISSORS, "Cut").hint("Ctrl+X").enabled(has_sel),
        CtxItem::new(crate::lucide::COPY, "Copy").hint("Ctrl+C").enabled(has_sel),
        CtxItem::new(crate::lucide::CLIPBOARD_PASTE, "Paste").hint("Ctrl+V"),
        CtxItem::new(crate::lucide::TEXT_SELECT, "Select all").hint("Ctrl+A").sep().enabled(!text.is_empty()),
    ];
    let (open, pick) = context_menu(&ctx, mkey, pos, pal, &mut fresh, &items);
    ctx.data_mut(|d| d.insert_temp(mkey.with("fresh"), fresh));
    match pick {
        Some(0) | Some(1) => {
            ctx.copy_text(slice_chars(text, a, b));
            if pick == Some(0) {
                let at = replace_chars(text, a, b, "");
                set_selection(&ctx, id, at, at);
                changed = true;
            } else {
                set_selection(&ctx, id, a, b);
            }
        }
        Some(2) => {
            crate::platform::request_paste(&ctx);
            set_selection(&ctx, id, a, b);
            ctx.data_mut(|d| d.insert_temp(id.with("fwait"), (a, b)));
        }
        Some(3) => set_selection(&ctx, id, 0, text.chars().count()),
        _ => {}
    }
    if !open {
        ctx.data_mut(|d| d.remove::<(Pos2, usize, usize)>(mkey));
    }
    changed
}
