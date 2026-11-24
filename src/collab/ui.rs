//! Collaboration UI: a people chip in the top bar (avatars → drop-down), the Share / Nearby dialog, remote cursors.
//!
//! Principles: almost no text on the surface — avatars, one-word chips and icons; the detail (device, platform, page, address)
//! lives in hover cards. The dialog has two tabs: **Share** (what to share, who is in) and **Nearby** (devices on this Wi-Fi + a code).
use super::*;
use crate::lucide as l;
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;
use egui::{pos2, vec2, Align2, Color32, Context, CursorIcon, Id, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

pub const COLORS: [Color32; 8] = [
    Color32::from_rgb(66, 133, 244),
    Color32::from_rgb(234, 67, 53),
    Color32::from_rgb(242, 153, 0),
    Color32::from_rgb(52, 168, 83),
    Color32::from_rgb(171, 71, 188),
    Color32::from_rgb(255, 112, 67),
    Color32::from_rgb(0, 172, 193),
    Color32::from_rgb(236, 64, 122),
];

pub fn color_of(p: &PeerInfo) -> Color32 {
    COLORS[(p.color % 8) as usize]
}

pub fn with_alpha_pub(c: Color32, a: f32) -> Color32 {
    with_alpha(c, a)
}

pub fn first_name(n: &str) -> String {
    n.split_whitespace().next().unwrap_or(n).chars().take(14).collect()
}

fn initials(name: &str) -> String {
    let mut it = name.split_whitespace();
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.next().and_then(|w| w.chars().next());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

pub(super) fn avatar(p: &Painter, c: Pos2, r: f32, peer: &PeerInfo, ring: Color32) {
    p.circle_filled(c, r + 1.8, ring);
    p.circle_filled(c, r, color_of(peer));
    p.text(c, Align2::CENTER_CENTER, initials(&peer.name), bold(r * 0.88), Color32::WHITE);
}

fn platform_label(p: &str) -> &'static str {
    match p {
        "windows" => "Windows",
        "android" => "Android",
        "web" => "Browser",
        "mac" => "Mac",
        _ => "Linux",
    }
}

#[derive(Default)]
pub struct Out {
    /// Open this page (the user chose "follow" on someone).
    pub open: Option<u64>,
}

/// 0 = secondary, 1 = primary, 2 = danger, 3 = quiet.
pub(super) fn button(ui: &mut Ui, pal: &Palette, label: &str, kind: u8, w: f32) -> egui::Response {
    let h = if crate::touch::touchy() { 38.0 } else { 30.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let (fill, fg, stroke) = match kind {
        1 => (crate::theme::lerp_color(pal.accent, Color32::BLACK, t * 0.12), Color32::WHITE, Color32::TRANSPARENT),
        2 => (with_alpha(pal.danger, 0.10 + 0.10 * t), pal.danger, Color32::TRANSPARENT),
        3 => (with_alpha(pal.hover, t), pal.muted, Color32::TRANSPARENT),
        _ => (with_alpha(pal.hover, 0.5 + 0.5 * t), pal.text, pal.border),
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    if stroke != Color32::TRANSPARENT {
        ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, stroke), StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, bold(13.0), fg);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub(super) fn glyph_button(ui: &mut Ui, pal: &Palette, g: &str, tip: &str, tint: Option<Color32>) -> egui::Response {
    let s = if crate::touch::touchy() { 36.0 } else { 26.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(s, s), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(rect, 7.0, with_alpha(pal.hover, t));
    crate::icons::glyph(ui.painter(), rect.center(), 15.0, g, tint.unwrap_or_else(|| crate::theme::lerp_color(pal.muted, pal.text, t)));
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

pub(super) fn switch_public(ui: &mut Ui, pal: &Palette, on: bool) -> egui::Response {
    switch(ui, pal, on)
}

fn switch(ui: &mut Ui, pal: &Palette, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 20.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.15);
    ui.painter().rect_filled(rect, 10.0, crate::theme::lerp_color(with_alpha(pal.muted, 0.45), pal.accent, t));
    ui.painter().circle_filled(pos2(rect.left() + 10.0 + 14.0 * t, rect.center().y), 7.5, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// ───────────── top bar chip ─────────────

/// The sharing button: an icon when nobody is connected, a stack of avatars while a session runs.
/// Comments button of the top bar: speech bubble with the number of open comments on this page.
pub fn comments_button(ui: &mut Ui, pal: &Palette, c: &mut Collab, open_count: usize) {
    let touch = crate::touch::touchy();
    let h = if touch { 38.0 } else { 28.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(h, h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || c.ui.comments, 0.12);
    ui.painter().rect_filled(rect, 7.0, with_alpha(pal.hover, t));
    crate::icons::glyph(ui.painter(), rect.center(), 16.0, l::MESSAGE_SQUARE, crate::theme::lerp_color(pal.muted, pal.text, t));
    if open_count > 0 {
        let p = pos2(rect.right() - 6.0, rect.top() + 7.0);
        ui.painter().circle_filled(p, 6.5, pal.accent);
        ui.painter().text(p, Align2::CENTER_CENTER, open_count.min(9).to_string(), bold(9.0), Color32::WHITE);
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Comments").clicked() {
        c.ui.comments = !c.ui.comments;
    }
}

/// Top-bar button of the review window (only while a session runs or something is waiting for review).
pub fn changes_button(ui: &mut Ui, pal: &Palette, c: &mut Collab) {
    if !c.active() && c.changes.is_empty() && c.suggestions.is_empty() && c.away.is_empty() {
        return;
    }
    let touch = crate::touch::touchy();
    let h = if touch { 38.0 } else { 28.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(h, h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || c.ui.changes, 0.12);
    ui.painter().rect_filled(rect, 7.0, with_alpha(pal.hover, t));
    crate::icons::glyph(ui.painter(), rect.center(), 16.0, l::HISTORY, crate::theme::lerp_color(pal.muted, pal.text, t));
    let n = c.changes.len() + c.suggestions.len() + c.away.len();
    if n > 0 {
        let p = pos2(rect.right() - 6.0, rect.top() + 7.0);
        ui.painter().circle_filled(p, 6.5, COLORS[3]);
        ui.painter().text(p, Align2::CENTER_CENTER, n.min(9).to_string(), bold(9.0), Color32::WHITE);
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Changes — what others edited").clicked() {
        c.ui.changes = !c.ui.changes;
    }
}

pub fn chip(ui: &mut Ui, pal: &Palette, c: &mut Collab) {
    let people = c.people();
    let n = if c.active() { people.len().min(3) } else { 0 };
    let touch = crate::touch::touchy();
    let h = if touch { 38.0 } else { 28.0 };
    let w = if n == 0 { h } else { 14.0 + 17.0 * n as f32 + if people.len() > 3 { 16.0 } else { 0.0 } + 10.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || c.ui.menu, 0.12);
    ui.painter().rect_filled(rect, 7.0, with_alpha(pal.hover, t));
    if n == 0 {
        crate::icons::glyph(ui.painter(), rect.center(), 16.0, l::USERS, crate::theme::lerp_color(pal.muted, pal.text, t));
    } else {
        for (i, (p, _, _)) in people.iter().take(n).enumerate().rev() {
            avatar(ui.painter(), pos2(rect.left() + 13.0 + 17.0 * i as f32, rect.center().y), 9.0, p, pal.bg);
        }
        if people.len() > 3 {
            ui.painter().text(pos2(rect.right() - 12.0, rect.center().y), Align2::CENTER_CENTER, format!("+{}", people.len() - 3), bold(11.0), pal.muted);
        }
        // connection light
        ui.painter().circle_filled(pos2(rect.right() - 4.0, rect.top() + 6.0), 3.2, if c.mode == Mode::Joining { COLORS[2] } else { COLORS[3] });
    }
    if !c.requests.is_empty() {
        ui.painter().circle_filled(pos2(rect.right() - 3.0, rect.top() + 4.0), 4.5, pal.danger);
    }
    c.ui.chip = Some(rect);
    let tip = if c.active() { "People & sharing" } else { "Share & collaborate" };
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked() {
        if c.active() {
            c.ui.menu = !c.ui.menu;
        } else {
            open_dialog(c, 0);
        }
    }
}

pub fn open_dialog(c: &mut Collab, tab: u8) {
    c.ui.dialog = true;
    c.ui.menu = false;
    c.ui.tab = tab;
    c.ui.name_edit = c.me.name.clone();
    if tab == 1 {
        c.discover(true);
    }
}

// ───────────── everything that floats ─────────────

pub fn overlays(ctx: &Context, pal: &Palette, c: &mut Collab, ws: &mut crate::model::Workspace, ws_id: u64, current: Option<u64>) -> Out {
    let mut out = Out::default();
    if c.ui.relay_url.is_empty() && !c.ui.relay_loaded {
        c.ui.relay_loaded = true;
        c.ui.relay_url = ctx.data_mut(|d| d.get_persisted::<String>(Id::new("collab_relay"))).unwrap_or_else(default_relay);
    }
    if c.ui.menu && c.active() {
        menu(ctx, pal, c, ws, &mut out);
    }
    super::comments::notify(c, ws);
    if c.ui.comments {
        super::comments::window(ctx, pal, c, ws, current);
    }
    if c.ui.changes {
        if let Some(pg) = super::changes::window(ctx, pal, c, ws) {
            out.open = Some(pg);
        }
    }
    if c.ui.dialog {
        let id = Id::new("collab_dialog");
        let keep = crate::app::modal(ctx, id, pal, 440.0, 70.0, |ui| dialog(ui, pal, c, ws, ws_id, current));
        if !keep {
            c.ui.dialog = false;
            c.discover(false);
        }
    }
    // follow
    if let Some(f) = c.ui.follow {
        if c.roster.iter().all(|(p, _)| p.id != f) {
            c.ui.follow = None;
        } else if let Some(pg) = c.presence_of(f).and_then(|p| p.page) {
            if Some(pg) != current && ws.page(pg).is_some() {
                out.open = Some(pg);
            }
        }
    }
    // a pill while following: who, whether the cursor is chased too, and a way out that is always one tap away (phones included)
    if let Some(f) = c.ui.follow {
        if let Some((p, _)) = c.roster.iter().find(|(p, _)| p.id == f).cloned() {
            let mut stop = false;
            let mut toggle = false;
            egui::Area::new(Id::new("collab_follow_pill"))
                .order(egui::Order::Foreground)
                .anchor(Align2::CENTER_TOP, vec2(0.0, 52.0))
                .show(ctx, |ui| {
                    crate::widgets::popup_frame(pal)
                        .corner_radius(egui::CornerRadius::same(20))
                        .inner_margin(egui::Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Following {}",
                                        first_name(&p.name)
                                    ))
                                    .font(body(13.0))
                                    .color(pal.text),
                                );
                                let cur = !c.ui.follow_cursor_off;
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(if cur {
                                                "Cursor ✓"
                                            } else {
                                                "Cursor"
                                            })
                                            .font(body(12.5))
                                            .color(if cur { pal.accent } else { pal.muted }),
                                        )
                                        .frame(false)
                                        .min_size(vec2(64.0, 32.0)),
                                    )
                                    .on_hover_text("Chase their caret / pointer")
                                    .clicked()
                                {
                                    toggle = true;
                                }
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("Stop")
                                                .font(body(12.5))
                                                .color(pal.muted),
                                        )
                                        .frame(false)
                                        .min_size(vec2(48.0, 32.0)),
                                    )
                                    .clicked()
                                {
                                    stop = true;
                                }
                            });
                        });
                });
            if toggle {
                c.ui.follow_cursor_off = !c.ui.follow_cursor_off;
            }
            if stop {
                c.ui.follow = None;
            }
        }
    }
    // toast
    if let Some((msg, t0)) = c.ui.toast.clone() {
        let age = mono_ms() - t0;
        if age > 3200.0 {
            c.ui.toast = None;
        } else {
            let a = ((1.0 - ((age - 2600.0) / 600.0).clamp(0.0, 1.0)) * 255.0) as u8;
            egui::Area::new(Id::new("collab_toast")).order(egui::Order::Tooltip).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0)).interactable(false).show(ctx, |ui| {
                ui.set_opacity(a as f32 / 255.0);
                crate::widgets::popup_frame(pal).corner_radius(egui::CornerRadius::same(20)).inner_margin(egui::Margin::symmetric(16, 9)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(l::USERS).font(body(14.0)).color(pal.accent));
                        ui.label(egui::RichText::new(msg).font(body(13.5)).color(pal.text));
                    });
                });
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    out
}

fn default_relay() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(w) = web_sys::window() {
            if let Ok(o) = w.location().origin() {
                return o;
            }
        }
    }
    option_env!("LUMEN_RELAY").unwrap_or("").to_owned()
}

fn menu(
    ctx: &Context,
    pal: &Palette,
    c: &mut Collab,
    ws: &crate::model::Workspace,
    _out: &mut Out,
) {
    let screen = ctx.screen_rect();
    let w = 330.0_f32.min(screen.width() - 16.0);
    let anchor =
        c.ui.chip
            .map(|r| r.left_bottom() + vec2(0.0, 6.0))
            .unwrap_or(pos2(60.0, 44.0));
    let pos = pos2(anchor.x.min(screen.right() - w - 8.0).max(8.0), anchor.y);
    let people = c.people();
    let hosting = c.mode == Mode::Hosting;
    let resp = egui::Area::new(Id::new("collab_menu")).order(crate::widgets::HOST_ORDER).fixed_pos(pos).show(ctx, |ui| {
        crate::widgets::popup_frame(pal).corner_radius(egui::CornerRadius::same(14)).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(w - 24.0);
            // header: what is shared + state
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                ui.painter().rect_filled(r, 9.0, pal.accent_soft);
                crate::icons::glyph(ui.painter(), r.center(), 16.0, if c.session.as_ref().map_or(false, |s| s.scope.kind == ScopeKind::Workspace) { l::GLOBE } else { l::FILE_TEXT }, pal.accent);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let title = c.session.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                    ui.add(egui::Label::new(egui::RichText::new(title).font(bold(14.0)).color(pal.text)).truncate());
                    let sub = match (c.mode, c.session.as_ref()) {
                        (Mode::Hosting, Some(s)) => format!("Sharing · {}", Collab::scope_label(s.scope)),
                        (Mode::Joined, _) => format!("Shared with you · {}", match c.role { Role::Viewer => "view only", Role::Suggester => "suggesting", Role::Editor => "can edit" }),
                        _ => "Connecting…".into(),
                    };
                    ui.label(egui::RichText::new(sub).font(body(12.0)).color(pal.muted));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if glyph_button(ui, pal, l::SETTINGS, "Sharing options", None).clicked() {
                        open_dialog(c, 0);
                    }
                });
            });
            ui.add_space(8.0);
            // join requests
            let reqs: Vec<(ConnId, PeerInfo, String, String)> = c.requests.iter().map(|r| (r.conn, r.peer.clone(), r.addr.clone(), r.code.clone())).collect();
            for (conn, p, addr, code) in reqs {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
                ui.painter().rect_filled(r, 10.0, with_alpha(pal.accent_soft, 0.9));
                avatar(ui.painter(), pos2(r.left() + 24.0, r.center().y), 12.0, &p, pal.accent_soft);
                ui.painter().text(pos2(r.left() + 44.0, r.center().y - 8.0), Align2::LEFT_CENTER, &p.name, bold(13.0), pal.text);
                ui.painter().text(pos2(r.left() + 44.0, r.center().y + 8.0), Align2::LEFT_CENTER, format!("wants to join · {} · code {}", platform_label(&p.platform), code), body(11.5), pal.muted);
                let mut x = r.right() - 6.0;
                for (g, tip, act) in [(l::X, "Decline", 0), (l::EYE, "Allow to view only", 1), (l::CHECK, "Allow to edit", 2)] {
                    let b = Rect::from_center_size(pos2(x - 14.0, r.center().y), vec2(28.0, 28.0));
                    let rs = ui.interact(b, Id::new(("req", conn, act)), Sense::click());
                    ui.painter().circle_filled(b.center(), 14.0, with_alpha(if act == 2 { pal.accent } else { pal.hover }, if act == 2 { 1.0 } else { 0.9 }));
                    crate::icons::glyph(ui.painter(), b.center(), 14.0, g, if act == 2 { Color32::WHITE } else { pal.text });
                    let rs = rs.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("{tip}  ({addr})"));
                    if rs.clicked() {
                        match act {
                            0 => c.deny(conn),
                            1 => c.approve(conn, Role::Viewer),
                            _ => c.approve(conn, Role::Editor),
                        }
                    }
                    x -= 32.0;
                }
                ui.add_space(4.0);
            }
            // avatars side by side, "…" for the rest, "+" at the end
            ui.horizontal(|ui| {
                let max = if ui.available_width() > 260.0 { 6 } else { 4 };
                for (p, _, you) in people.iter().take(max) {
                    let (r, rs) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                    avatar(ui.painter(), r.center(), 14.0, p, pal.popup);
                    let rs = rs.on_hover_cursor(CursorIcon::PointingHand);
                    rs.clone().on_hover_text(if *you { format!("{} (you)", p.name) } else { p.name.clone() });
                    if rs.clicked() && !*you {
                        c.ui.person_menu = if c.ui.person_menu == Some(p.id) { None } else { Some(p.id) };
                    }
                }
                if people.len() > max {
                    let (r, rs) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                    ui.painter().circle_filled(r.center(), 14.0, pal.hover);
                    crate::icons::glyph(ui.painter(), r.center(), 15.0, l::ELLIPSIS, pal.muted);
                    if rs.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("{} more", people.len() - max)).clicked() {
                        c.ui.details = !c.ui.details;
                    }
                }
                let (r, rs) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(rs.id, rs.hovered(), 0.12);
                ui.painter().circle_stroke(r.center(), 14.0, Stroke::new(1.3_f32, crate::theme::lerp_color(pal.muted, pal.accent, t)));
                crate::icons::glyph(ui.painter(), r.center(), 15.0, l::PLUS, crate::theme::lerp_color(pal.muted, pal.accent, t));
                if rs.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Add people").clicked() {
                    open_dialog(c, if hosting { 1 } else { 0 });
                }
            });
            // detailed list (only when asked for)
            if c.ui.details || people.len() <= 4 {
                ui.add_space(6.0);
                for (p, role, you) in &people {
                    let (r, rs) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                    let hv = rs.hovered();
                    if hv {
                        ui.painter().rect_filled(r, 8.0, pal.hover);
                    }
                    avatar(ui.painter(), pos2(r.left() + 18.0, r.center().y), 11.0, p, if hv { pal.hover } else { pal.popup });
                    let pres = c.presence_of(p.id).cloned();
                    let on = pres.as_ref().and_then(|x| x.page).and_then(|pg| ws.page(pg)).map(|pg| if pg.title.is_empty() { "Untitled".to_owned() } else { pg.title.clone() });
                    ui.painter().text(pos2(r.left() + 38.0, r.center().y - 7.0), Align2::LEFT_CENTER, if *you { format!("{} · you", p.name) } else { p.name.clone() }, bold(13.0), pal.text);
                    let sub = if pres.as_ref().is_some_and(|x| x.typing) { "typing…".to_owned() } else { on.map(|t| format!("on {t}")).unwrap_or_else(|| platform_label(&p.platform).to_owned()) };
                    ui.painter().text(pos2(r.left() + 38.0, r.center().y + 8.0), Align2::LEFT_CENTER, sub, body(11.5), pal.muted);
                    let chip_txt = match role { Role::Viewer => "View", Role::Suggester => "Suggest", Role::Editor => "Edit" };
                    let pk = if *you {
                        if hosting { iroh_transport::PathKind::Unknown } else { c.primary_path() }
                    } else {
                        c.peer_path(p.id)
                    };
                    if pk != iroh_transport::PathKind::Unknown {
                        let dot_col = path_color(pk, pal);
                        ui.painter().text(pos2(r.right() - 8.0, r.center().y - 6.0), Align2::RIGHT_CENTER, chip_txt, body(11.0), pal.muted);
                        ui.painter().text(pos2(r.right() - 8.0, r.center().y + 7.5), Align2::RIGHT_CENTER, pk.label(), body(10.5), dot_col);
                        ui.painter().circle_filled(pos2(r.right() - 8.0 - (pk.label().len() as f32 * 5.6 + 6.0), r.center().y + 7.5), 2.8, dot_col);
                    } else {
                        ui.painter().text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, chip_txt, body(11.5), pal.muted);
                    }
                    if !*you {
                        if rs.clicked() {
                            c.ui.person_menu = if c.ui.person_menu == Some(p.id) { None } else { Some(p.id) };
                        }
                        let follow = c.ui.follow == Some(p.id);
                        rs.on_hover_ui(|ui| {
                            ui.label(egui::RichText::new(&p.name).font(bold(13.0)));
                            ui.label(egui::RichText::new(format!("{} · {}", p.device, platform_label(&p.platform))).font(body(12.0)).color(pal.muted));
                            ui.label(egui::RichText::new(if follow { "Following — click to stop" } else { "Click for actions" }).font(body(12.0)).color(pal.muted));
                        });
                    }
                }
            }
            // per-person actions
            if let Some(pid) = c.ui.person_menu {
                if let Some((p, role)) = c.roster.iter().find(|(p, _)| p.id == pid).cloned() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if button(ui, pal, if c.ui.follow == Some(pid) { "Stop following" } else { "Follow" }, 0, 100.0).clicked() {
                            c.ui.follow = if c.ui.follow == Some(pid) { None } else { Some(pid) };
                        }
                        if c.ui.follow == Some(pid) {
                            let lbl = if c.ui.follow_cursor_off { "Follow cursor: off" } else { "Follow cursor: on" };
                            if button(ui, pal, lbl, 0, 140.0).on_hover_text("Scroll / pan to wherever their caret or pointer is, not only to their page").clicked() {
                                c.ui.follow_cursor_off = !c.ui.follow_cursor_off;
                            }
                        }
                        if hosting {
                            for (lbl, nr, w) in [("Make editor", Role::Editor, 104.0), ("Suggest only", Role::Suggester, 104.0), ("Make viewer", Role::Viewer, 104.0)] {
                                if role != nr && button(ui, pal, lbl, 0, w).on_hover_text(match nr { Role::Suggester => "Their changes wait for your approval", Role::Viewer => "They can look but not change anything", Role::Editor => "They can edit freely" }).clicked() {
                                    c.set_role(pid, nr);
                                }
                            }
                            if button(ui, pal, "Remove", 2, 76.0).clicked() {
                                c.kick(pid);
                                c.ui.person_menu = None;
                            }
                        }
                        let _ = &p;
                    });
                }
            }
            ui.add_space(8.0);
            ui.painter().line_segment([pos2(ui.min_rect().left(), ui.cursor().top()), pos2(ui.min_rect().right(), ui.cursor().top())], Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
            ui.add_space(6.0);
            // cursors switch + leave
            ui.horizontal(|ui| {
                let on = c.show_cursors;
                crate::icons::glyph(ui.painter(), ui.cursor().left_center() + vec2(8.0, 0.0), 15.0, if on { l::EYE } else { l::EYE_OFF }, pal.muted);
                ui.add_space(22.0);
                ui.label(egui::RichText::new("Others' cursors").font(body(13.0)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if switch(ui, pal, on).on_hover_text("Show or hide other people's cursors and selections").clicked() {
                        c.set_cursors(!on);
                    }
                });
            });
            ui.add_space(6.0);
            if hosting && !c.successors().is_empty() {
                // hand the session to somebody else: they host from now on, everybody (you too) reconnects to them
                let first = c.successors().first().map(|p| p.id);
                let waiting = c.handing_over();
                let label = if waiting { "Handing over…" } else { "Hand over hosting & leave" };
                if button(ui, pal, label, 2, ui.available_width()).on_hover_text("The session keeps running on another device — nothing is lost").clicked() && !waiting {
                    c.hand_over(first, true);
                    c.ui.menu = false;
                }
                ui.add_space(4.0);
            }
            if button(ui, pal, if hosting { "Stop sharing" } else { "Leave session" }, 2, ui.available_width()).clicked() {
                c.stop(true);
                c.ui.menu = false;
                c.toast(if hosting { "Sharing stopped" } else { "You left the session" });
            }
        });
    });
    let _ = ws;
    // click outside closes
    let inside = resp.response.rect.expand(2.0);
    let chip_r = c.ui.chip.unwrap_or(Rect::NOTHING);
    if ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .map_or(false, |p| !inside.contains(p) && !chip_r.contains(p))
    }) || ctx.input(|i| i.key_pressed(egui::Key::Escape))
    {
        c.ui.menu = false;
    }
}

// ───────────── Share / Nearby dialog ─────────────

fn path_color(k: iroh_transport::PathKind, pal: &Palette) -> Color32 {
    match k {
        iroh_transport::PathKind::DirectLan => COLORS[3],
        iroh_transport::PathKind::DirectP2p => pal.accent,
        iroh_transport::PathKind::Relayed => pal.muted,
        iroh_transport::PathKind::Unknown => pal.muted,
    }
}

fn hairline(ui: &mut Ui, pal: &Palette) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().line_segment(
        [pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)],
        Stroke::new(1.0_f32, pal.border),
    );
}

/// Calm sentence-case section label matching Lumen's Notion sidebar typography.
fn section_label(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).font(body(12.0)).color(pal.muted));
}

/// Clean Notion-style single-line input box with proper padding and exact height alignment.
fn styled_input(
    ui: &mut Ui,
    pal: &Palette,
    val: &mut String,
    hint: &str,
    w: f32,
    h: f32,
) -> egui::Response {
    let mut out_resp = None;
    egui::Frame::new()
        .fill(pal.code_bg)
        .corner_radius(egui::CornerRadius::same(7))
        .stroke(Stroke::new(1.0_f32, pal.border))
        .inner_margin(egui::Margin::symmetric(10, 0))
        .show(ui, |ui| {
            ui.set_min_size(vec2((w - 20.0).max(40.0), h));
            ui.set_max_size(vec2((w - 20.0).max(40.0), h));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(val)
                        .hint_text(egui::RichText::new(hint).color(pal.muted))
                        .desired_width(f32::INFINITY)
                        .font(body(13.0))
                        .frame(false),
                );
                out_resp = Some(r);
            });
        });
    out_resp.unwrap()
}

/// Clean pill button with exact height control matching Notion's subtle elevation.
fn action_btn(ui: &mut Ui, pal: &Palette, label: &str, kind: u8, w: f32, h: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let (fill, fg, stroke) = match kind {
        1 => (
            crate::theme::lerp_color(pal.accent, Color32::WHITE, t * 0.08),
            Color32::WHITE,
            Color32::TRANSPARENT,
        ),
        2 => (
            with_alpha(pal.danger, 0.10 + 0.08 * t),
            pal.danger,
            with_alpha(pal.danger, 0.25),
        ),
        3 => (with_alpha(pal.hover, t), pal.muted, Color32::TRANSPARENT),
        _ => (
            crate::theme::lerp_color(pal.callout, pal.hover, t),
            pal.text,
            pal.border,
        ),
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    if stroke != Color32::TRANSPARENT {
        ui.painter()
            .rect_stroke(rect, 7.0, Stroke::new(1.0_f32, stroke), StrokeKind::Inside);
    }
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, bold(13.0), fg);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn dialog(
    ui: &mut Ui,
    pal: &Palette,
    c: &mut Collab,
    ws: &mut crate::model::Workspace,
    ws_id: u64,
    current: Option<u64>,
) {
    ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
    // Clean Notion-style header: quiet underline tabs on left, close button on right (no clipped device name).
    ui.horizontal(|ui| {
        for (i, (g, lbl)) in [(l::USERS, "Share"), (l::RADAR, "Nearby")]
            .into_iter()
            .enumerate()
        {
            let sel = c.ui.tab == i as u8;
            let (r, rs) = ui.allocate_exact_size(vec2(84.0, 32.0), Sense::click());
            if rs.hovered() && !sel {
                ui.painter()
                    .rect_filled(r, 6.0, with_alpha(pal.hover, 0.65));
            }
            crate::icons::glyph(
                ui.painter(),
                pos2(r.left() + 14.0, r.center().y),
                14.5,
                g,
                if sel { pal.text } else { pal.muted },
            );
            ui.painter().text(
                pos2(r.left() + 30.0, r.center().y),
                Align2::LEFT_CENTER,
                lbl,
                if sel { bold(13.5) } else { body(13.5) },
                if sel { pal.text } else { pal.muted },
            );
            if sel {
                ui.painter().line_segment(
                    [
                        pos2(r.left() + 6.0, r.bottom() - 1.0),
                        pos2(r.right() - 6.0, r.bottom() - 1.0),
                    ],
                    Stroke::new(2.0_f32, pal.text),
                );
            }
            if rs.on_hover_cursor(CursorIcon::PointingHand).clicked() && !sel {
                c.ui.tab = i as u8;
                c.discover(i == 1);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if glyph_button(ui, pal, l::X, "Close", None).clicked() {
                c.ui.dialog = false;
                c.discover(false);
            }
        });
    });
    hairline(ui, pal);
    ui.add_space(2.0);
    if c.ui.tab == 0 {
        share_tab(ui, pal, c, ws, ws_id, current);
    } else {
        nearby_tab(ui, pal, c);
    }
    if let Some(e) = c.last_error.clone() {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(e).font(body(12.0)).color(pal.danger));
    }
}

/// Notion-style flat panel with subtle 1px border.
fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(pal.callout)
        .corner_radius(egui::CornerRadius::same(8))
        .stroke(Stroke::new(1.0_f32, pal.border))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn share_tab(
    ui: &mut Ui,
    pal: &Palette,
    c: &mut Collab,
    ws: &mut crate::model::Workspace,
    ws_id: u64,
    current: Option<u64>,
) {
    let hosting = c.mode == Mode::Hosting;
    let cur_title = current.and_then(|id| ws.page(id)).map(|p| {
        if p.title.is_empty() {
            "Untitled".to_owned()
        } else {
            p.title.clone()
        }
    });
    if !hosting {
        // ── Scope segmented track (Notion neutral surface, zero neon boxes) ──
        section_label(ui, pal, "Share scope");
        let opts: [(&str, &str, &str); 3] = [
            (l::GLOBE, "Workspace", "Every page, database and board"),
            (l::FILE_TEXT, "This page", "Only the open page"),
            (l::NETWORK, "Page + nested", "Open page and sub-pages"),
        ];
        egui::Frame::new()
            .fill(pal.code_bg)
            .corner_radius(egui::CornerRadius::same(8))
            .stroke(Stroke::new(1.0_f32, pal.border))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let w = (ui.available_width() - 6.0).max(60.0) / 3.0;
                    for (i, (g, lbl, tip)) in opts.iter().enumerate() {
                        let sel = c.ui.scope_choice == i as u8;
                        let (r, rs) = ui.allocate_exact_size(vec2(w, 50.0), Sense::click());
                        if sel {
                            ui.painter().rect_filled(r, 6.0, pal.active);
                            ui.painter().rect_stroke(
                                r,
                                6.0,
                                Stroke::new(1.0_f32, pal.border),
                                StrokeKind::Inside,
                            );
                        } else if rs.hovered() {
                            ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, 0.7));
                        }
                        crate::icons::glyph(
                            ui.painter(),
                            pos2(r.center().x, r.top() + 17.0),
                            16.0,
                            g,
                            if sel { pal.text } else { pal.muted },
                        );
                        ui.painter().text(
                            pos2(r.center().x, r.bottom() - 13.0),
                            Align2::CENTER_CENTER,
                            lbl,
                            if sel { bold(12.0) } else { body(12.0) },
                            if sel { pal.text } else { pal.muted },
                        );
                        if rs
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text(*tip)
                            .clicked()
                        {
                            c.ui.scope_choice = i as u8;
                        }
                    }
                });
            });
        if c.ui.scope_choice > 0 {
            ui.label(
                egui::RichText::new(
                    cur_title
                        .clone()
                        .map(|t| format!("Page: {t}"))
                        .unwrap_or_else(|| "Open a page first".into()),
                )
                .font(body(12.0))
                .color(pal.muted),
            );
        }

        // ── Grouped Notion settings card ──
        card(ui, pal, |ui| {
            ui.spacing_mut().item_spacing.y = 9.0;
            // Row 1: Copy lifetime inline segmented pill
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(
                        egui::RichText::new("Copy on other devices")
                            .font(body(13.0))
                            .color(pal.text),
                    );
                    ui.label(
                        egui::RichText::new(if c.keep_copy {
                            "Joiners keep a synced copy"
                        } else {
                            "Removed when session ends"
                        })
                        .font(body(11.5))
                        .color(pal.muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::Frame::new()
                        .fill(pal.code_bg)
                        .corner_radius(egui::CornerRadius::same(6))
                        .stroke(Stroke::new(1.0_f32, pal.border))
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for (keep, lbl) in [(false, "Temp"), (true, "Keep")] {
                                    let sel = c.keep_copy == keep;
                                    let (r, rs) =
                                        ui.allocate_exact_size(vec2(52.0, 24.0), Sense::click());
                                    if sel {
                                        ui.painter().rect_filled(r, 4.5, pal.active);
                                    } else if rs.hovered() {
                                        ui.painter().rect_filled(
                                            r,
                                            4.5,
                                            with_alpha(pal.hover, 0.7),
                                        );
                                    }
                                    ui.painter().text(
                                        r.center(),
                                        Align2::CENTER_CENTER,
                                        lbl,
                                        if sel { bold(11.5) } else { body(11.5) },
                                        if sel { pal.text } else { pal.muted },
                                    );
                                    if rs.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                        c.keep_copy = keep;
                                    }
                                }
                            });
                        });
                });
            });
            hairline(ui, pal);
            // Row 2: Approval switch
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(
                        egui::RichText::new("Require approval")
                            .font(body(13.0))
                            .color(pal.text),
                    );
                    ui.label(
                        egui::RichText::new(if c.auto_admit {
                            "Anyone with the link joins immediately"
                        } else {
                            "New devices wait for your Accept"
                        })
                        .font(body(11.5))
                        .color(if c.auto_admit {
                            COLORS[2]
                        } else {
                            pal.muted
                        }),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if switch(ui, pal, !c.auto_admit)
                        .on_hover_text("Trusted devices reconnect automatically without asking")
                        .clicked()
                    {
                        c.auto_admit = !c.auto_admit;
                    }
                });
            });
        });

        ui.add_space(2.0);
        let can = c.ui.scope_choice == 0 || current.is_some();
        let w = ui.available_width();
        let h = if crate::touch::touchy() { 38.0 } else { 34.0 };
        ui.add_enabled_ui(can, |ui| {
            if action_btn(ui, pal, "Start sharing", 1, w, h).clicked() {
                let scope = match c.ui.scope_choice {
                    0 => Scope::ALL,
                    1 => Scope {
                        kind: ScopeKind::Page,
                        root: current.unwrap_or(0),
                    },
                    _ => Scope {
                        kind: ScopeKind::Tree,
                        root: current.unwrap_or(0),
                    },
                };
                c.start_host(ws, ws_id, scope, None);
            }
        });

        // ── Quiet collapsible settings disclosure (device name + optional custom relay) ──
        let (dr, drs) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
        if drs.hovered() {
            ui.painter()
                .rect_filled(dr, 6.0, with_alpha(pal.hover, 0.5));
        }
        crate::icons::glyph(
            ui.painter(),
            pos2(dr.left() + 10.0, dr.center().y),
            13.0,
            if c.ui.show_relay {
                l::CHEVRON_DOWN
            } else {
                l::CHEVRON_RIGHT
            },
            pal.muted,
        );
        ui.painter().text(
            pos2(dr.left() + 24.0, dr.center().y),
            Align2::LEFT_CENTER,
            format!("Settings · {}", c.me.name),
            body(12.0),
            pal.muted,
        );
        if drs.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            c.ui.show_relay = !c.ui.show_relay;
        }
        if c.ui.show_relay {
            card(ui, pal, |ui| {
                let fw = ui.available_width();
                section_label(ui, pal, "Display name");
                let nr = styled_input(ui, pal, &mut c.ui.name_edit, "Your name", fw, 30.0);
                if nr.lost_focus() {
                    let n = c.ui.name_edit.clone();
                    c.set_name(&n);
                }
                ui.add_space(2.0);
                section_label(ui, pal, "Custom relay URL (optional)");
                let rr = styled_input(
                    ui,
                    pal,
                    &mut c.custom_iroh_relay,
                    "Automatic (default public relays)",
                    fw,
                    30.0,
                );
                if rr.lost_focus() {
                    let v = c.custom_iroh_relay.clone();
                    ui.ctx()
                        .data_mut(|d| d.insert_persisted(Id::new("collab_iroh_relay"), v));
                }
            });
        }
        return;
    }

    // ── Active Hosting View ──
    let session = c.session.clone();
    card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            let pulse = (ui.input(|i| i.time) * 2.0).sin() as f32 * 0.5 + 0.5;
            ui.painter().circle_filled(
                r.center(),
                4.0 + pulse * 1.4,
                with_alpha(COLORS[3], 0.30 + 0.35 * pulse),
            );
            ui.painter().circle_filled(r.center(), 3.2, COLORS[3]);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
            let title = session
                .as_ref()
                .map(|s| s.name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Workspace".into());
            ui.label(
                egui::RichText::new(format!("Sharing · {title}"))
                    .font(bold(13.0))
                    .color(pal.text),
            );
            let status = c.iroh_status();
            if !status.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(status)
                            .font(body(11.5))
                            .color(pal.muted),
                    );
                });
            }
        });
        let base = c.ui.relay_url.trim().to_owned();
        let links: Vec<(&str, String)> = [
            ("App link", c.iroh_link()),
            ("Web link", c.iroh_web_link(&base)),
        ]
        .into_iter()
        .filter_map(|(n, v)| v.map(|v| (n, v)))
        .collect();
        if let Some((name, text)) = links
            .get((c.ui.qr_pick as usize).min(links.len().saturating_sub(1)))
            .cloned()
        {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                qr(ui, c, &text, 124.0);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        egui::RichText::new("Scan QR or copy invite link")
                            .font(body(12.0))
                            .color(pal.muted),
                    );
                    ui.label(
                        egui::RichText::new(&c.token)
                            .font(egui::FontId::monospace(19.0))
                            .color(pal.text),
                    )
                    .on_hover_text("Session code — new devices still require your approval");
                    if let Some(eid) = c.iroh_endpoint_id() {
                        let short: String = eid.chars().take(12).collect();
                        ui.label(
                            egui::RichText::new(format!("Node {short}…"))
                                .font(egui::FontId::monospace(11.5))
                                .color(pal.muted),
                        );
                    }
                    ui.horizontal(|ui| {
                        let copied = mono_ms() - c.ui.copied_at < 1500.0;
                        if action_btn(
                            ui,
                            pal,
                            if copied { "Copied" } else { "Copy link" },
                            1,
                            92.0,
                            30.0,
                        )
                        .clicked()
                        {
                            ui.ctx().copy_text(text.clone());
                            c.ui.copied_at = mono_ms();
                        }
                        if links.len() > 1
                            && action_btn(ui, pal, name, 0, 74.0, 30.0)
                                .on_hover_text("Switch between app invite link and browser link")
                                .clicked()
                        {
                            c.ui.qr_pick = 1 - (c.ui.qr_pick % 2);
                        }
                    });
                });
            });
        } else {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                dots(ui, pal);
                ui.label(
                    egui::RichText::new("Preparing invite link…")
                        .font(body(12.5))
                        .color(pal.muted),
                );
            });
        }
    });

    // Offline Hotspot (Android only, for zero-internet airplane/classroom mode)
    if Collab::hotspot_supported() {
        card(ui, pal, |ui| {
            let on = c.hs.0 != "idle";
            let mut flip = false;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Offline hotspot (no internet)")
                        .font(body(13.0))
                        .color(pal.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if switch(ui, pal, on)
                        .on_hover_text("Creates a private Wi-Fi hotspot so nearby phones can join with zero internet")
                        .clicked()
                    {
                        flip = true;
                    }
                });
            });
            if flip {
                c.set_hotspot(!on);
            }
            match c.hs.0.as_str() {
                "starting" | "perm" => {
                    ui.label(
                        egui::RichText::new(if c.hs.0 == "perm" {
                            "Allow nearby devices permission…"
                        } else {
                            "Starting hotspot…"
                        })
                        .font(body(11.5))
                        .color(pal.muted),
                    );
                }
                "up" => {
                    ui.label(
                        egui::RichText::new(format!("Hotspot “{}” · Password: {}", c.hs.1, c.hs.2))
                            .font(egui::FontId::monospace(11.5))
                            .color(COLORS[3]),
                    );
                }
                "failed" => {
                    ui.label(
                        egui::RichText::new(c.hs.1.clone())
                            .font(body(11.5))
                            .color(pal.danger),
                    );
                }
                _ => {}
            }
        });
    }

    // Inline join requests
    let reqs: Vec<(ConnId, PeerInfo, String)> = c
        .requests
        .iter()
        .map(|r| (r.conn, r.peer.clone(), r.code.clone()))
        .collect();
    for (conn, p, code) in reqs {
        card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                avatar(ui.painter(), r.center(), 12.0, &p, pal.popup);
                ui.label(
                    egui::RichText::new(format!("{} · code {}", p.name, code))
                        .font(bold(12.5))
                        .color(pal.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if action_btn(ui, pal, "Edit", 1, 50.0, 28.0).clicked() {
                        c.approve(conn, Role::Editor);
                    }
                    if action_btn(ui, pal, "Suggest", 0, 62.0, 28.0).clicked() {
                        c.approve(conn, Role::Suggester);
                    }
                    if action_btn(ui, pal, "View", 0, 50.0, 28.0).clicked() {
                        c.approve(conn, Role::Viewer);
                    }
                    if action_btn(ui, pal, "Deny", 3, 44.0, 28.0).clicked() {
                        c.deny(conn);
                    }
                });
            });
        });
    }

    let trusted = c.trusted_list();
    if !trusted.is_empty() {
        card(ui, pal, |ui| {
            section_label(ui, pal, "Trusted devices");
            for (key, name, viewer) in trusted {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{name} · {}",
                            if viewer { "view" } else { "edit" }
                        ))
                        .font(body(12.5))
                        .color(pal.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if action_btn(ui, pal, "Forget", 3, 58.0, 26.0).clicked() {
                            c.forget_trusted(&key);
                        }
                    });
                });
            }
        });
    }
    grants_card(ui, pal, c);
    let w = ui.available_width();
    if action_btn(ui, pal, "Stop sharing", 2, w, 34.0).clicked() {
        c.stop(true);
        c.toast("Sharing stopped");
    }
}

fn qr(ui: &mut Ui, c: &mut Collab, text: &str, size: f32) {
    if c.ui.qr_cache.as_ref().map_or(true, |q| q.0 != text) {
        let cells = qrcode::QrCode::new(text.as_bytes()).map(|q| {
            (
                q.width(),
                q.to_colors()
                    .into_iter()
                    .map(|x| x == qrcode::Color::Dark)
                    .collect::<Vec<bool>>(),
            )
        });
        c.ui.qr_cache = cells.ok().map(|(w, v)| (text.to_owned(), v, w));
    }
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, 8.0, Color32::WHITE);
    if let Some((_, cells, n)) = &c.ui.qr_cache {
        let inner = rect.shrink(8.0);
        let m = inner.width() / *n as f32;
        for y in 0..*n {
            for x in 0..*n {
                if cells[y * n + x] {
                    let r = Rect::from_min_size(
                        pos2(inner.left() + x as f32 * m, inner.top() + y as f32 * m),
                        vec2(m + 0.4, m + 0.4),
                    );
                    ui.painter()
                        .rect_filled(r, 0.0, Color32::from_rgb(20, 20, 24));
                }
            }
        }
    }
}

fn nearby_tab(ui: &mut Ui, pal: &Palette, c: &mut Collab) {
    let base = c.ui.relay_url.trim().to_owned();
    if c.mode == Mode::Joining {
        card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                dots(ui, pal);
                ui.label(
                    egui::RichText::new("Connecting & waiting for host approval…")
                        .font(body(13.0))
                        .color(pal.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if action_btn(ui, pal, "Cancel", 3, 60.0, 28.0).clicked() {
                        c.stop(false);
                    }
                });
            });
        });
        return;
    }

    // ── Aligned Notion-style invite link input + Join button ──
    section_label(ui, pal, "Join with invite link");
    let row_h = if crate::touch::touchy() { 38.0 } else { 34.0 };
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 74.0).max(120.0);
        let r = styled_input(
            ui,
            pal,
            &mut c.ui.join_text,
            "Paste lumen://join?iroh=… or ticket",
            w,
            row_h,
        );
        let go = action_btn(ui, pal, "Join", 1, 66.0, row_h).clicked()
            || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        if go && !c.ui.join_text.trim().is_empty() {
            let t = c.ui.join_text.clone();
            c.join_text(&t, &base);
        }
    });
    #[cfg(target_os = "android")]
    {
        let w = ui.available_width();
        if action_btn(ui, pal, "Scan QR code", 0, w, row_h).clicked() {
            crate::android::scan_qr();
        }
    }

    ui.add_space(4.0);
    section_label(ui, pal, "Nearby on this Wi-Fi");

    // ── Calm, airy Nearby container ──
    card(ui, pal, |ui| {
        if cfg!(target_arch = "wasm32") {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Browser tabs connect via invite link above")
                        .font(body(12.5))
                        .color(pal.muted),
                );
                ui.add_space(6.0);
            });
        } else if c.found.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - 180.0) * 0.5).max(0.0));
                    dots(ui, pal);
                    ui.label(
                        egui::RichText::new("Scanning this Wi-Fi…")
                            .font(body(12.5))
                            .color(pal.text),
                    );
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Open Share on another device on the same network")
                        .font(body(11.5))
                        .color(pal.muted),
                );
                ui.add_space(8.0);
            });
        } else {
            let list: Vec<Found> = c.found.clone();
            for (idx, f) in list.into_iter().enumerate() {
                if idx > 0 {
                    hairline(ui, pal);
                }
                let (r, rs) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
                let hv = rs.hovered();
                if hv {
                    ui.painter().rect_filled(r, 6.0, pal.hover);
                }
                let info = PeerInfo {
                    id: f.beacon.id,
                    name: f.beacon.device.clone(),
                    device: f.beacon.device.clone(),
                    platform: f.beacon.platform.clone(),
                    color: (f.beacon.id % 8) as u8,
                };
                avatar(
                    ui.painter(),
                    pos2(r.left() + 20.0, r.center().y),
                    13.0,
                    &info,
                    if hv { pal.hover } else { pal.callout },
                );
                ui.painter().text(
                    pos2(r.left() + 42.0, r.center().y - 7.5),
                    Align2::LEFT_CENTER,
                    &f.beacon.device,
                    bold(13.0),
                    pal.text,
                );
                ui.painter().text(
                    pos2(r.left() + 42.0, r.center().y + 8.0),
                    Align2::LEFT_CENTER,
                    format!(
                        "{} · {} · {}",
                        f.beacon.what,
                        f.beacon.title,
                        platform_label(&f.beacon.platform)
                    ),
                    body(11.5),
                    pal.muted,
                );
                // Always-visible subtle Join pill so mobile & desktop users see the action at a glance
                let jr =
                    Rect::from_center_size(pos2(r.right() - 28.0, r.center().y), vec2(48.0, 24.0));
                ui.painter()
                    .rect_filled(jr, 5.0, if hv { pal.accent } else { pal.active });
                ui.painter().text(
                    jr.center(),
                    Align2::CENTER_CENTER,
                    "Join",
                    bold(11.5),
                    if hv { Color32::WHITE } else { pal.text },
                );
                if rs
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(format!("{} · {} online", f.beacon.name, f.beacon.people))
                    .clicked()
                {
                    if !f.beacon.iroh.is_empty() {
                        c.join_iroh(&f.beacon.iroh, "");
                    } else {
                        c.last_error =
                            Some("This nearby device is running an older non-Iroh build".into());
                    }
                }
            }
        }
    });
}

fn dots(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(40.0, 14.0), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    for i in 0..4 {
        let ph = ((t * 3.0 - i as f32 * 0.55).sin() * 0.5 + 0.5).powi(2);
        ui.painter().circle_filled(pos2(r.left() + 4.0 + i as f32 * 10.0, r.center().y - ph * 3.0), 2.6 + ph * 0.8, with_alpha(pal.accent, 0.35 + 0.65 * ph));
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

// ───────────── remote cursors ─────────────

/// Draws pointers (`w2s` maps canvas world coordinates to screen).
pub fn paint_cursor_list(p: &Painter, _pal: &Palette, list: &[(&PeerInfo, &Presence)], w2s: &dyn Fn(Pos2) -> Pos2) {
    let clip = p.clip_rect();
    for (peer, pres) in list {
        let Some(cur) = pres.cur else { continue };
        let at = w2s(pos2(cur[0], cur[1]));
        if !clip.expand(40.0).contains(at) {
            continue;
        }
        let col = color_of(peer);
        let mut mesh = egui::epaint::Mesh::default();
        for pt in [(0.0, 0.0), (0.0, 16.0), (4.6, 12.0), (11.5, 12.0)] {
            mesh.colored_vertex(at + vec2(pt.0, pt.1), col);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(egui::Shape::mesh(mesh));
        let galley = p.layout_no_wrap(peer.name.clone(), bold(11.5), Color32::WHITE);
        let r = Rect::from_min_size(at + vec2(11.0, 15.0), galley.size() + vec2(12.0, 6.0));
        p.rect_filled(r, 9.0, col);
        p.galley(r.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    }
}

/// Per-document share links: own role, lifetime and use count, copyable and revocable (`Collab::create_grant` / `revoke_grant`).
fn grants_card(ui: &mut Ui, pal: &Palette, c: &mut Collab) {
    if c.provisional() {
        return;
    }
    const ROLES: [(&str, Role); 3] = [
        ("Can edit", Role::Editor),
        ("Can suggest", Role::Suggester),
        ("Can view", Role::Viewer),
    ];
    const TTLS: [(&str, u64); 4] = [
        ("1 hour", 3_600_000),
        ("24 hours", 86_400_000),
        ("7 days", 604_800_000),
        ("No expiry", 0),
    ];
    const USES: [(&str, u32); 3] = [
        ("Any number of people", 0),
        ("One person", 1),
        ("Up to 5 people", 5),
    ];
    let now = now_ms();
    let base = c.ui.relay_url.trim().to_owned();
    let rows: Vec<(String, String, String)> = c
        .grants
        .list
        .iter()
        .filter(|g| g.live(now))
        .map(|g| (g.id.clone(), g.token.clone(), g.describe(now)))
        .collect();
    card(ui, pal, |ui| {
        ui.label(
            egui::RichText::new("Share links")
                .font(bold(13.5))
                .color(pal.text),
        );
        ui.label(egui::RichText::new("Give each person their own link: choose what they can do, how long it works and how many can use it. Revoking removes whoever joined with it.").font(body(12.0)).color(pal.muted));
        for (id, token, text) in rows {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(text).font(body(12.5)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, pal, "Revoke", 3, 64.0).clicked() {
                        c.revoke_grant(&id);
                    }
                    if button(ui, pal, "Copy", 0, 52.0).clicked() {
                        if let Some(link) = c
                            .grant_link(&token, &base)
                            .or_else(|| c.grant_lan_text(&token))
                        {
                            ui.ctx().copy_text(link);
                            c.toast("Link copied");
                        }
                    }
                });
            });
        }
        ui.horizontal_wrapped(|ui| {
            if button(ui, pal, ROLES[c.ui.grant_role as usize % 3].0, 0, 104.0)
                .on_hover_text("What the person can do — tap to change")
                .clicked()
            {
                c.ui.grant_role = (c.ui.grant_role + 1) % 3;
            }
            if button(ui, pal, TTLS[c.ui.grant_ttl as usize % 4].0, 0, 92.0)
                .on_hover_text("How long the link works — tap to change")
                .clicked()
            {
                c.ui.grant_ttl = (c.ui.grant_ttl + 1) % 4;
            }
            if button(ui, pal, USES[c.ui.grant_uses as usize % 3].0, 0, 150.0)
                .on_hover_text("How many people can use it — tap to change")
                .clicked()
            {
                c.ui.grant_uses = (c.ui.grant_uses + 1) % 3;
            }
            if button(ui, pal, "+ Link", 1, 76.0).clicked() {
                let (role, ttl, uses) = (
                    ROLES[c.ui.grant_role as usize % 3].1,
                    TTLS[c.ui.grant_ttl as usize % 4].1,
                    USES[c.ui.grant_uses as usize % 3].1,
                );
                let label = format!("Link {}", c.grants.list.len() + 1);
                match c.create_grant(role, &label, ttl, uses) {
                    Some(token) => {
                        if let Some(link) = c
                            .grant_link(&token, &base)
                            .or_else(|| c.grant_lan_text(&token))
                        {
                            ui.ctx().copy_text(link);
                        }
                        c.toast("Link created and copied");
                    }
                    None => c.toast("Only the host can create links"),
                }
            }
        });
    });
}
