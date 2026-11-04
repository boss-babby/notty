//! Small reusable, animated widgets.

use egui::{
    emath::easing, pos2, vec2, Align2, Area, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Order,
    Pos2, Rect, Response, Sense, Shadow, Stroke, Ui,
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
    let x = pos.x.min(screen.right() - width - 12.0).max(8.0);
    let resp = Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(pos2(x, pos.y + (1.0 - t) * -6.0))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            popup_frame(pal).show(ui, |ui| {
                ui.set_width(width);
                add(ui);
            });
        });
    let rect = resp.response.rect;
    let mut open = true;
    if ctx.input(|i| i.pointer.any_pressed()) && !*fresh {
        if let Some(p) = ctx.pointer_interact_pos() {
            if !rect.contains(p) {
                open = false;
            }
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
    let (rect, resp) = ui.allocate_exact_size(vec2(size + 12.0, size + 12.0), Sense::click());
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
pub fn menu_item(
    ui: &mut Ui,
    pal: &Palette,
    lead: &str,
    label: &str,
    sub: Option<&str>,
    selected: bool,
    danger: bool,
) -> Response {
    let h = if sub.is_some() { 46.0 } else { 30.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || selected, 0.1);
    let p = ui.painter();
    if t > 0.0 {
        p.rect_filled(rect, 6.0, with_alpha(pal.hover, t));
    }
    let col = if danger { pal.danger } else { pal.text };
    let mut x = rect.left() + 8.0;
    if !lead.is_empty() {
        if sub.is_some() {
            let b = Rect::from_center_size(pos2(x + 17.0, rect.center().y), vec2(34.0, 34.0));
            p.rect_filled(b, 6.0, pal.bg);
            p.rect_stroke(b, 6.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
            p.text(b.center(), Align2::CENTER_CENTER, lead, bold(13.0), col);
            x += 44.0;
        } else {
            p.text(pos2(x + 9.0, rect.center().y), Align2::CENTER_CENTER, lead, bold(12.5), pal.muted);
            x += 26.0;
        }
    }
    match sub {
        Some(s) => {
            p.text(pos2(x, rect.center().y - 8.0), Align2::LEFT_CENTER, label, body(14.0), col);
            p.text(pos2(x, rect.center().y + 9.0), Align2::LEFT_CENTER, s, body(12.0), pal.muted);
        }
        None => {
            p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, body(14.0), col);
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
