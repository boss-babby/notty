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

fn avatar(p: &Painter, c: Pos2, r: f32, peer: &PeerInfo, ring: Color32) {
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
fn button(ui: &mut Ui, pal: &Palette, label: &str, kind: u8, w: f32) -> egui::Response {
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

fn glyph_button(ui: &mut Ui, pal: &Palette, g: &str, tip: &str, tint: Option<Color32>) -> egui::Response {
    let s = if crate::touch::touchy() { 36.0 } else { 26.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(s, s), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(rect, 7.0, with_alpha(pal.hover, t));
    crate::icons::glyph(ui.painter(), rect.center(), 15.0, g, tint.unwrap_or_else(|| crate::theme::lerp_color(pal.muted, pal.text, t)));
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
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

fn menu(ctx: &Context, pal: &Palette, c: &mut Collab, ws: &crate::model::Workspace, out: &mut Out) {
    let screen = ctx.screen_rect();
    let w = 330.0_f32.min(screen.width() - 16.0);
    let anchor = c.ui.chip.map(|r| r.left_bottom() + vec2(0.0, 6.0)).unwrap_or(pos2(60.0, 44.0));
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
                        (Mode::Joined, _) => format!("Shared with you · {}", if c.role == Role::Viewer { "view only" } else { "can edit" }),
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
            let reqs: Vec<(ConnId, PeerInfo, String)> = c.requests.iter().map(|r| (r.conn, r.peer.clone(), r.addr.clone())).collect();
            for (conn, p, addr) in reqs {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
                ui.painter().rect_filled(r, 10.0, with_alpha(pal.accent_soft, 0.9));
                avatar(ui.painter(), pos2(r.left() + 24.0, r.center().y), 12.0, &p, pal.accent_soft);
                ui.painter().text(pos2(r.left() + 44.0, r.center().y - 8.0), Align2::LEFT_CENTER, &p.name, bold(13.0), pal.text);
                ui.painter().text(pos2(r.left() + 44.0, r.center().y + 8.0), Align2::LEFT_CENTER, format!("wants to join · {}", platform_label(&p.platform)), body(11.5), pal.muted);
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
                    let sub = on.map(|t| format!("on {t}")).unwrap_or_else(|| platform_label(&p.platform).to_owned());
                    ui.painter().text(pos2(r.left() + 38.0, r.center().y + 8.0), Align2::LEFT_CENTER, sub, body(11.5), pal.muted);
                    let chip_txt = if *role == Role::Viewer { "View" } else { "Edit" };
                    ui.painter().text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, chip_txt, body(11.5), pal.muted);
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
                        if hosting {
                            let (lbl, nr) = if role == Role::Editor { ("Make viewer", Role::Viewer) } else { ("Make editor", Role::Editor) };
                            if button(ui, pal, lbl, 0, 104.0).clicked() {
                                c.set_role(pid, nr);
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
    if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().map_or(false, |p| !inside.contains(p) && !chip_r.contains(p))) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        c.ui.menu = false;
    }
}

// ───────────── Share / Nearby dialog ─────────────

fn dialog(ui: &mut Ui, pal: &Palette, c: &mut Collab, ws: &mut crate::model::Workspace, ws_id: u64, current: Option<u64>) {
    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
    // tabs + close
    ui.horizontal(|ui| {
        for (i, (g, lbl)) in [(l::USERS, "Share"), (l::RADAR, "Nearby")].into_iter().enumerate() {
            let sel = c.ui.tab == i as u8;
            let (r, rs) = ui.allocate_exact_size(vec2(104.0, 32.0), Sense::click());
            if sel {
                ui.painter().rect_filled(r, 9.0, pal.accent_soft);
            } else if rs.hovered() {
                ui.painter().rect_filled(r, 9.0, pal.hover);
            }
            crate::icons::glyph(ui.painter(), pos2(r.left() + 20.0, r.center().y), 15.0, g, if sel { pal.accent } else { pal.muted });
            ui.painter().text(pos2(r.left() + 36.0, r.center().y), Align2::LEFT_CENTER, lbl, bold(13.5), if sel { pal.accent } else { pal.text });
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
            // this device's name, editable in place
            let te = egui::TextEdit::singleline(&mut c.ui.name_edit).desired_width(120.0).font(body(13.0)).hint_text("Your name").frame(false);
            let r = ui.add(te).on_hover_text("How other devices see you");
            if r.lost_focus() {
                let n = c.ui.name_edit.clone();
                c.set_name(&n);
            }
        });
    });
    ui.add_space(2.0);
    if c.ui.tab == 0 {
        share_tab(ui, pal, c, ws, ws_id, current);
    } else {
        nearby_tab(ui, pal, c);
    }
    if let Some(e) = c.last_error.clone() {
        ui.label(egui::RichText::new(e).font(body(12.5)).color(pal.danger));
    }
}

fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(with_alpha(pal.hover, 0.45)).corner_radius(egui::CornerRadius::same(12)).stroke(Stroke::new(1.0_f32, with_alpha(pal.border, 0.8))).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn share_tab(ui: &mut Ui, pal: &Palette, c: &mut Collab, ws: &mut crate::model::Workspace, ws_id: u64, current: Option<u64>) {
    let hosting = c.mode == Mode::Hosting;
    let cur_title = current.and_then(|id| ws.page(id)).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() });
    if !hosting {
        card(ui, pal, |ui| {
            ui.label(egui::RichText::new("What do you want to share?").font(bold(13.5)).color(pal.text));
            let opts: [(&str, &str, &str); 3] = [(l::GLOBE, "Workspace", "Everything in this workspace — every page, database and board"), (l::FILE_TEXT, "This page", "Only the page you have open"), (l::NETWORK, "Page + nested", "The open page and every page inside it")];
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 16.0) / 3.0;
                for (i, (g, lbl, tip)) in opts.iter().enumerate() {
                    let sel = c.ui.scope_choice == i as u8;
                    let (r, rs) = ui.allocate_exact_size(vec2(w, 62.0), Sense::click());
                    ui.painter().rect_filled(r, 10.0, if sel { pal.accent_soft } else { with_alpha(pal.bg, 0.7) });
                    ui.painter().rect_stroke(r, 10.0, Stroke::new(if sel { 1.5_f32 } else { 1.0_f32 }, if sel { pal.accent } else { pal.border }), StrokeKind::Inside);
                    crate::icons::glyph(ui.painter(), pos2(r.center().x, r.top() + 22.0), 20.0, g, if sel { pal.accent } else { pal.muted });
                    ui.painter().text(pos2(r.center().x, r.bottom() - 16.0), Align2::CENTER_CENTER, lbl, bold(12.5), if sel { pal.accent } else { pal.text });
                    if rs.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(*tip).clicked() {
                        c.ui.scope_choice = i as u8;
                    }
                }
            });
            if c.ui.scope_choice > 0 {
                ui.label(egui::RichText::new(cur_title.clone().map(|t| format!("“{t}”")).unwrap_or_else(|| "Open a page first".into())).font(body(12.5)).color(pal.muted));
            }
        });
        let can = c.ui.scope_choice == 0 || current.is_some();
        let w = ui.available_width();
        ui.add_enabled_ui(can, |ui| {
            if button(ui, pal, "Start sharing on this Wi-Fi", 1, w).clicked() {
                let scope = match c.ui.scope_choice {
                    0 => Scope::ALL,
                    1 => Scope { kind: ScopeKind::Page, root: current.unwrap_or(0) },
                    _ => Scope { kind: ScopeKind::Tree, root: current.unwrap_or(0) },
                };
                let relay = if c.ui.use_relay && !c.ui.relay_url.trim().is_empty() { Some(c.ui.relay_url.trim().to_owned()) } else { None };
                c.start_host(ws, ws_id, scope, relay);
            }
        });
        // advanced: link relay (also how the web version takes part)
        ui.horizontal(|ui| {
            if !c.ui.relay_url.trim().is_empty() {
                if switch(ui, pal, c.ui.use_relay).on_hover_text("Also create a link that works outside this Wi-Fi (goes through your Lumen site's relay)").clicked() {
                    c.ui.use_relay = !c.ui.use_relay;
                }
                ui.label(egui::RichText::new("Also create a link").font(body(12.5)).color(pal.muted));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if glyph_button(ui, pal, l::SETTINGS, "Relay server for links", None).clicked() {
                    c.ui.show_relay = !c.ui.show_relay;
                }
            });
        });
        if c.ui.show_relay {
            let r = ui.add(egui::TextEdit::singleline(&mut c.ui.relay_url).hint_text("https://your-site.example").desired_width(f32::INFINITY).font(body(13.0)));
            if r.lost_focus() {
                let v = c.ui.relay_url.clone();
                ui.ctx().data_mut(|d| d.insert_persisted(Id::new("collab_relay"), v));
            }
        }
        return;
    }
    // ── hosting: status, QR, link, people ──
    let session = c.session.clone();
    card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            let pulse = (ui.input(|i| i.time) * 2.0).sin() as f32 * 0.5 + 0.5;
            ui.painter().circle_filled(r.center(), 4.0 + pulse * 1.5, with_alpha(COLORS[3], 0.35 + 0.4 * pulse));
            ui.painter().circle_filled(r.center(), 3.5, COLORS[3]);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
            ui.label(egui::RichText::new(format!("Sharing · {}", session.as_ref().map(|s| Collab::scope_label(s.scope)).unwrap_or(""))).font(bold(13.5)).color(pal.text));
            ui.label(egui::RichText::new(session.as_ref().map(|s| s.name.clone()).unwrap_or_default()).font(body(13.0)).color(pal.muted));
        });
        let base = c.ui.relay_url.trim().to_owned();
        let links: Vec<(&str, String)> = [("Wi-Fi", c.lan_link()), ("Link", c.relay_link(&base))].into_iter().filter_map(|(n, v)| v.map(|v| (n, v))).collect();
        if !links.is_empty() {
            let pick = (c.ui.qr_pick as usize).min(links.len() - 1);
            let (name, text) = (links[pick].0, links[pick].1.clone());
            ui.horizontal(|ui| {
                qr(ui, c, &text, 132.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(egui::RichText::new("Scan or enter").font(body(12.0)).color(pal.muted));
                    ui.label(egui::RichText::new(&c.token).font(egui::FontId::monospace(24.0)).color(pal.text)).on_hover_text("Anyone with this code joins without asking");
                    ui.label(egui::RichText::new(c.addr_hint.first().cloned().unwrap_or_default()).font(egui::FontId::monospace(12.0)).color(pal.muted));
                    ui.horizontal(|ui| {
                        if button(ui, pal, if mono_ms() - c.ui.copied_at < 1500.0 { "Copied" } else { "Copy link" }, 0, 90.0).clicked() {
                            ui.ctx().copy_text(text.clone());
                            c.ui.copied_at = mono_ms();
                        }
                        if links.len() > 1 && button(ui, pal, name, 3, 60.0).on_hover_text("Switch between the Wi-Fi code and the internet link").clicked() {
                            c.ui.qr_pick = 1 - pick as u8;
                        }
                    });
                });
            });
        }
    });
    // join requests inline
    let reqs: Vec<(ConnId, PeerInfo)> = c.requests.iter().map(|r| (r.conn, r.peer.clone())).collect();
    for (conn, p) in reqs {
        card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                avatar(ui.painter(), r.center(), 13.0, &p, pal.popup);
                ui.label(egui::RichText::new(format!("{} wants to join", p.name)).font(bold(13.0)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, pal, "Edit", 1, 52.0).clicked() {
                        c.approve(conn, Role::Editor);
                    }
                    if button(ui, pal, "View", 0, 52.0).clicked() {
                        c.approve(conn, Role::Viewer);
                    }
                    if button(ui, pal, "No", 3, 40.0).clicked() {
                        c.deny(conn);
                    }
                });
            });
        });
    }
    let w = ui.available_width();
    if button(ui, pal, "Stop sharing", 2, w).clicked() {
        c.stop(true);
        c.toast("Sharing stopped");
    }
}

fn qr(ui: &mut Ui, c: &mut Collab, text: &str, size: f32) {
    if c.ui.qr_cache.as_ref().map_or(true, |q| q.0 != text) {
        let cells = qrcode::QrCode::new(text.as_bytes()).map(|q| (q.width(), q.to_colors().into_iter().map(|x| x == qrcode::Color::Dark).collect::<Vec<bool>>()));
        c.ui.qr_cache = cells.ok().map(|(w, v)| (text.to_owned(), v, w));
    }
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, Color32::WHITE);
    if let Some((_, cells, n)) = &c.ui.qr_cache {
        let inner = rect.shrink(8.0);
        let m = inner.width() / *n as f32;
        for y in 0..*n {
            for x in 0..*n {
                if cells[y * n + x] {
                    let r = Rect::from_min_size(pos2(inner.left() + x as f32 * m, inner.top() + y as f32 * m), vec2(m + 0.4, m + 0.4));
                    ui.painter().rect_filled(r, 0.0, Color32::from_rgb(20, 20, 24));
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
                ui.label(egui::RichText::new("Waiting for the host to accept you…").font(body(13.5)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, pal, "Cancel", 3, 64.0).clicked() {
                        c.stop(false);
                    }
                });
            });
        });
        return;
    }
    card(ui, pal, |ui| {
        ui.label(egui::RichText::new("Devices on this Wi-Fi").font(bold(13.5)).color(pal.text));
        if cfg!(target_arch = "wasm32") {
            ui.label(egui::RichText::new("Browsers cannot scan the network. Enter a code or address below.").font(body(12.5)).color(pal.muted));
        } else if c.found.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                dots(ui, pal);
                ui.label(egui::RichText::new("Looking for nearby devices").font(body(12.5)).color(pal.muted));
            });
        }
        let list: Vec<Found> = c.found.clone();
        for f in list {
            let (r, rs) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
            let hv = rs.hovered();
            ui.painter().rect_filled(r, 10.0, if hv { pal.hover } else { with_alpha(pal.bg, 0.7) });
            let info = PeerInfo { id: f.beacon.id, name: f.beacon.device.clone(), device: f.beacon.device.clone(), platform: f.beacon.platform.clone(), color: (f.beacon.id % 8) as u8 };
            avatar(ui.painter(), pos2(r.left() + 26.0, r.center().y), 14.0, &info, if hv { pal.hover } else { pal.bg });
            ui.painter().text(pos2(r.left() + 50.0, r.center().y - 8.0), Align2::LEFT_CENTER, &f.beacon.device, bold(13.5), pal.text);
            ui.painter().text(pos2(r.left() + 50.0, r.center().y + 9.0), Align2::LEFT_CENTER, format!("{} · {}", f.beacon.what, f.beacon.title), body(12.0), pal.muted);
            let a = ui.ctx().animate_bool_with_time(rs.id.with("j"), hv, 0.12);
            if a > 0.0 {
                ui.painter().text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, "Join", bold(13.0), with_alpha(pal.accent, a));
            }
            let tip = format!("{} · {} · {} {} online · {}", f.beacon.name, platform_label(&f.beacon.platform), f.beacon.people, if f.beacon.people == 1 { "person" } else { "people" }, f.ip);
            if rs.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                c.join_addr(&f.ip, "");
            }
        }
    });
    card(ui, pal, |ui| {
        ui.label(egui::RichText::new("Code, address or link").font(bold(13.5)).color(pal.text));
        ui.horizontal(|ui| {
            let w = ui.available_width() - 76.0;
            let r = ui.add(egui::TextEdit::singleline(&mut c.ui.join_text).hint_text("192.168.1.20:47615  ·  ABC123  ·  link").desired_width(w).font(body(13.5)));
            let go = button(ui, pal, "Join", 1, 66.0).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && !c.ui.join_text.trim().is_empty() {
                let t = c.ui.join_text.clone();
                c.join_text(&t, &base);
            }
        });
        ui.label(egui::RichText::new("Phones: show the QR on the host, or type the 6-character code.").font(body(12.0)).color(pal.muted));
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
