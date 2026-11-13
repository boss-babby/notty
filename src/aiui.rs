//! Lumen AI — user interface (v12 redesign): a seamless, borderless assistant panel in the same quiet style as the rest of the app.
//!
//! * the panel has no frame lines: the header is one calm row, messages are plain text, tool activity is a compact one-line log,
//!   the composer is a soft rounded field with the model / access drop-downs inside it;
//! * every choice list (provider, model, access, chats) is a drop-down;
//! * settings are a two-column page (Model · Behavior · On-device · Connections) that collapses to tabs on phones.

use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Align, Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Order, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::agent_tools::Access;
use crate::ai::{self, AiHost, Msg, Phase, Role, SettingsTab, PROVIDERS};
use crate::icons::{self, Icon};
use crate::lucide as l;
use crate::model::Workspace;
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{field, ghost_button, icon_button, primary_button, with_alpha};

#[derive(Default)]
pub struct PanelOut {
    pub close: bool,
    pub open_page: Option<u64>,
}

enum Act {
    Send(String),
    Open(u64),
    Approve(bool),
    Settings,
}

// ───────────────────────────── look ─────────────────────────────

/// Background of the assistant panel: the page colour with a breath of the sidebar tone — no border needed.
pub fn panel_bg(pal: &Palette) -> Color32 {
    lerp_color(pal.bg, pal.sidebar, 0.55)
}

/// Fill of inputs, the composer and code blocks (opaque, so it can be mixed safely).
fn soft(pal: &Palette) -> Color32 {
    lerp_color(pal.bg, pal.hover, 0.55)
}

fn green() -> Color32 {
    Color32::from_rgb(70, 170, 110)
}

fn glyph(ui: &mut Ui, g: &str, size: f32, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(size + 4.0, size + 4.0), Sense::hover());
    ui.painter().text(r.center(), Align2::CENTER_CENTER, g, icons::font(size), color);
}

fn switch(ui: &mut Ui, pal: &Palette, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
    ui.painter().rect_filled(rect, 11.0, lerp_color(lerp_color(pal.bg, pal.muted, 0.45), pal.accent, t));
    ui.painter().circle_filled(pos2(rect.left() + 11.0 + t * 16.0, rect.center().y), 8.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn toggle_row(ui: &mut Ui, pal: &Palette, title: &str, hint: &str, on: &mut bool) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width(w - 52.0);
            ui.label(RichText::new(title).font(bold(13.5)).color(pal.text));
            if !hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(hint).font(body(12.0)).color(pal.muted)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            switch(ui, pal, on);
        });
    });
}

fn sub_head(ui: &mut Ui, pal: &Palette, title: &str, hint: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).font(bold(14.0)).color(pal.text));
    if !hint.is_empty() {
        ui.label(RichText::new(hint).font(body(12.0)).color(pal.muted));
    }
    ui.add_space(4.0);
}

/// Label + hint above a control (works at any width).
fn setting(ui: &mut Ui, pal: &Palette, title: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).font(bold(13.5)).color(pal.text));
    if !hint.is_empty() {
        ui.add(egui::Label::new(RichText::new(hint).font(body(12.0)).color(pal.muted)).wrap());
    }
    ui.add_space(4.0);
    add(ui);
}

fn soft_field(ui: &mut Ui, pal: &Palette, text: &mut String, hint: &str, password: bool, id: Id) -> egui::Response {
    field(ui, pal, soft(pal), text, hint, password, ui.available_width(), id)
}

// ───────────────────────────── drop-down ─────────────────────────────

struct Item {
    key: String,
    label: String,
    sub: String,
}

fn item(key: &str, label: &str, sub: &str) -> Item {
    Item { key: key.to_owned(), label: label.to_owned(), sub: sub.to_owned() }
}

fn access_items() -> Vec<Item> {
    [Access::ReadOnly, Access::Edit, Access::Full].iter().map(|a| item(access_key(*a), a.label(), a.hint())).collect()
}

fn access_key(a: Access) -> &'static str {
    match a {
        Access::ReadOnly => "read",
        Access::Edit => "edit",
        Access::Full => "full",
    }
}

fn access_from(k: &str) -> Access {
    match k {
        "read" => Access::ReadOnly,
        "full" => Access::Full,
        _ => Access::Edit,
    }
}

/// A drop-down: a quiet trigger (pill or full-width field) and a floating list. Returns the key that was picked this frame.
#[allow(clippy::too_many_arguments)]
fn dropdown(ui: &mut Ui, pal: &Palette, id: Id, label: &str, items: &[Item], cur: &str, up: bool, full: bool, pop_w: f32) -> Option<String> {
    let ctx = ui.ctx().clone();
    let touchy = crate::touch::touchy();
    let h = if touchy { 36.0 } else { 30.0 };
    let tw = ui.painter().layout_no_wrap(label.to_owned(), body(13.0), pal.text).size().x;
    let w = if full { ui.available_width() } else { (tw + 36.0).min(ui.available_width().max(80.0)) };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let (open_id, fresh_id, closed_id) = (id.with("open"), id.with("fresh"), id.with("closed"));
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let mut fresh = ctx.data(|d| d.get_temp::<bool>(fresh_id)).unwrap_or(false);
    let t = ctx.animate_bool_with_time(id.with("hov"), resp.hovered() || open, 0.1);
    let idle = if full { soft(pal) } else { panel_bg(pal) };
    ui.painter().rect_filled(rect, 8.0, lerp_color(idle, pal.hover, t));
    crate::widgets::text_fit(ui.painter(), pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, body(13.0), pal.text, rect.width() - 32.0);
    ui.painter().text(pos2(rect.right() - 13.0, rect.center().y), Align2::CENTER_CENTER, l::CHEVRON_DOWN, icons::font(12.0), pal.muted);
    let mut pick = None;
    if open {
        let pid = id.with("pop");
        let ph = ctx.data(|d| d.get_temp::<f32>(pid.with("h"))).unwrap_or(220.0);
        let pos = if up { pos2(rect.left(), rect.top() - ph - 6.0) } else { rect.left_bottom() + vec2(0.0, 6.0) };
        let keep = crate::widgets::popup_area(&ctx, pid, pos, pal, &mut fresh, pop_w.max(rect.width()), |ui| {
            for it in items {
                let sel = it.key == cur;
                let r = if it.sub.is_empty() {
                    crate::widgets::menu_item(ui, pal, if sel { l::CHECK } else { " " }, &it.label, None, sel, false)
                } else {
                    crate::widgets::menu_item(ui, pal, "", &it.label, Some(&it.sub), sel, false)
                };
                if r.clicked() {
                    pick = Some(it.key.clone());
                }
            }
        });
        if !keep || pick.is_some() {
            open = false;
            ctx.data_mut(|d| d.insert_temp(closed_id, ctx.input(|i| i.time)));
        }
    }
    if resp.clicked() {
        let recently = ctx.data(|d| d.get_temp::<f64>(closed_id)).map_or(false, |t| ctx.input(|i| i.time) - t < 0.35);
        if !open && !recently {
            open = true;
            fresh = true;
        } else {
            open = false;
        }
    }
    ctx.data_mut(|d| {
        d.insert_temp(open_id, open);
        d.insert_temp(fresh_id, fresh);
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    pick
}

// ───────────────────────────── markdown ─────────────────────────────

fn inline_job(pal: &Palette, text: &str, size: f32, color: Color32, force_bold: bool, wrap: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap.max(40.0);
    let chars: Vec<char> = text.chars().collect();
    let (mut bold_on, mut code_on, mut ital_on) = (false, false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, b: bool, c: bool, i: bool, link: bool| {
        if buf.is_empty() {
            return;
        }
        let font = if c { FontId::monospace(size - 1.0) } else if b || force_bold { bold(size) } else { body(size) };
        let mut f = TextFormat { font_id: font, color: if link { pal.accent } else { color }, italics: i, ..Default::default() };
        if c {
            f.background = pal.code_bg;
        }
        if link {
            f.underline = Stroke::new(1.0_f32, pal.accent);
        }
        job.append(buf, 0.0, f);
        buf.clear();
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            flush(&mut job, &mut buf, bold_on, code_on, ital_on, false);
            code_on = !code_on;
            i += 1;
            continue;
        }
        if !code_on {
            if c == '*' && chars.get(i + 1) == Some(&'*') {
                flush(&mut job, &mut buf, bold_on, code_on, ital_on, false);
                bold_on = !bold_on;
                i += 2;
                continue;
            }
            if c == '*' && chars.get(i + 1).map_or(false, |n| !n.is_whitespace()) && (ital_on || chars.get(i + 1) != Some(&' ')) {
                flush(&mut job, &mut buf, bold_on, code_on, ital_on, false);
                ital_on = !ital_on;
                i += 1;
                continue;
            }
            if c == '[' {
                // [label](url)
                if let Some(close) = chars[i..].iter().position(|x| *x == ']') {
                    let after = i + close + 1;
                    if chars.get(after) == Some(&'(') {
                        if let Some(end) = chars[after..].iter().position(|x| *x == ')') {
                            let label: String = chars[i + 1..i + close].iter().collect();
                            flush(&mut job, &mut buf, bold_on, code_on, ital_on, false);
                            buf = label;
                            flush(&mut job, &mut buf, bold_on, code_on, ital_on, true);
                            i = after + end + 1;
                            continue;
                        }
                    }
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut job, &mut buf, bold_on, code_on, ital_on, false);
    job
}

fn markdown(ui: &mut Ui, pal: &Palette, text: &str) {
    let w = ui.available_width();
    let mut in_code = false;
    let mut code = String::new();
    let mut first = true;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            if in_code {
                Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(9)).show(ui, |ui| {
                    ui.set_width(w - 18.0);
                    ui.add(egui::Label::new(RichText::new(code.trim_end()).font(FontId::monospace(12.5)).color(pal.text)).wrap());
                });
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            if !first {
                ui.add_space(5.0);
            }
            continue;
        }
        first = false;
        let lead = line.len() - line.trim_start().len();
        let t = line.trim_start();
        let ind = (lead as f32 / 2.0).min(4.0) * 14.0;
        if let Some(r) = t.strip_prefix("### ").or_else(|| t.strip_prefix("#### ")) {
            ui.add_space(3.0);
            ui.add(egui::Label::new(inline_job(pal, r, 14.5, pal.text, true, w)).wrap());
        } else if let Some(r) = t.strip_prefix("## ") {
            ui.add_space(5.0);
            ui.add(egui::Label::new(inline_job(pal, r, 16.0, pal.text, true, w)).wrap());
        } else if let Some(r) = t.strip_prefix("# ") {
            ui.add_space(6.0);
            ui.add(egui::Label::new(inline_job(pal, r, 18.0, pal.text, true, w)).wrap());
        } else if t == "---" || t == "***" {
            ui.add_space(3.0);
            ui.separator();
        } else if let Some(r) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("* [ ] ")).or_else(|| t.strip_prefix("- [x] ")).or_else(|| t.strip_prefix("* [x] ")) {
            let done = t.starts_with("- [x]") || t.starts_with("* [x]");
            ui.horizontal_top(|ui| {
                ui.add_space(ind);
                ui.label(RichText::new(if done { l::SQUARE_CHECK } else { l::SQUARE }).font(icons::font(14.0)).color(if done { pal.accent } else { pal.muted }));
                ui.add(egui::Label::new(inline_job(pal, r, 14.0, pal.text, false, w - ind - 26.0)).wrap());
            });
        } else if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")) {
            ui.horizontal_top(|ui| {
                ui.add_space(ind + 2.0);
                ui.label(RichText::new("•").font(body(14.0)).color(pal.muted));
                ui.add(egui::Label::new(inline_job(pal, r, 14.0, pal.text, false, w - ind - 22.0)).wrap());
            });
        } else if t.chars().next().map_or(false, |c| c.is_ascii_digit()) && t.find(". ").map_or(false, |p| p <= 3 && t[..p].chars().all(|c| c.is_ascii_digit())) {
            let p = t.find(". ").unwrap_or(0);
            ui.horizontal_top(|ui| {
                ui.add_space(ind + 2.0);
                ui.label(RichText::new(format!("{}.", &t[..p])).font(body(14.0)).color(pal.muted));
                ui.add(egui::Label::new(inline_job(pal, &t[p + 2..], 14.0, pal.text, false, w - ind - 30.0)).wrap());
            });
        } else if let Some(r) = t.strip_prefix("> ") {
            ui.horizontal_top(|ui| {
                let (bar, _) = ui.allocate_exact_size(vec2(3.0, 18.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, pal.accent);
                ui.add(egui::Label::new(inline_job(pal, r, 14.0, pal.muted, false, w - 14.0)).wrap());
            });
        } else if t.starts_with('|') {
            if t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                continue;
            }
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(|c| c.trim()).collect();
            ui.add(egui::Label::new(RichText::new(cells.join("   │   ")).font(body(13.0)).color(pal.text)).wrap());
        } else {
            ui.add(egui::Label::new(inline_job(pal, t, 14.0, pal.text, false, w)).wrap());
        }
    }
}

// ───────────────────────────── messages ─────────────────────────────

fn thinking_dots(ui: &mut Ui, pal: &Palette, label: &str) {
    let t = ui.input(|i| i.time) as f32;
    ui.horizontal(|ui| {
        for k in 0..3 {
            let a = ((t * 3.2 - k as f32 * 0.5).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 2.4 + a * 1.2, with_alpha(pal.muted, 0.35 + 0.65 * a));
        }
        ui.label(RichText::new(label).font(body(12.5)).color(pal.muted));
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
}

/// One finished / running tool call as a single quiet line; click to see arguments and result.
fn tool_row(ui: &mut Ui, pal: &Palette, ws: &Workspace, key: Id, call: &ai::Call, res: Option<&ai::ToolRes>, running: bool, acts: &mut Vec<Act>) {
    let label = ai::summarize_call(call, res);
    let open_id = Id::new(("ai_tool_open", key));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let h = if crate::touch::touchy() { 32.0 } else { 26.0 };
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if row.hovered() || open {
        ui.painter().rect_filled(rect, 7.0, lerp_color(panel_bg(pal), pal.hover, 0.8));
    }
    let cy = rect.center().y;
    let c = pos2(rect.left() + 13.0, cy);
    match res {
        Some(r) if r.ok => ui.painter().text(c, Align2::CENTER_CENTER, l::CHECK, icons::font(13.0), green()),
        Some(_) => ui.painter().text(c, Align2::CENTER_CENTER, l::CIRCLE_ALERT, icons::font(13.0), pal.danger),
        None if running => {
            let t = ui.input(|i| i.time) as f32;
            let a = (t * 4.0).sin() * 0.5 + 0.5;
            ui.painter().circle_stroke(c, 4.0 + a * 1.5, Stroke::new(1.5_f32, with_alpha(pal.accent, 0.4 + 0.6 * a)));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
            egui::Rect::NOTHING
        }
        None => ui.painter().text(c, Align2::CENTER_CENTER, l::CLOCK, icons::font(13.0), pal.muted),
    };
    let open_page = res.and_then(|r| r.open).filter(|id| ws.page(*id).is_some());
    let right_pad = if open_page.is_some() { 60.0 } else { 26.0 };
    crate::widgets::text_fit(ui.painter(), pos2(rect.left() + 28.0, cy), Align2::LEFT_CENTER, &label, body(13.0), if running { pal.text } else { pal.muted }, rect.width() - 28.0 - right_pad);
    ui.painter().text(pos2(rect.right() - 12.0, cy), Align2::CENTER_CENTER, if open { l::CHEVRON_DOWN } else { l::CHEVRON_RIGHT }, icons::font(12.0), with_alpha(pal.muted, 0.8));
    if let Some(id) = open_page {
        let r = egui::Rect::from_min_size(pos2(rect.right() - 62.0, rect.top() + 2.0), vec2(40.0, h - 4.0));
        let resp = ui.interact(r, key.with("open"), Sense::click());
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "Open", bold(12.5), pal.accent);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            acts.push(Act::Open(id));
            return;
        }
    }
    if row.clicked() {
        open = !open;
    }
    if open {
        Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(9)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let args = serde_json::to_string_pretty(&call.args).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(format!("{}\n{}", call.name, ai_trunc(&args, 1200))).font(FontId::monospace(11.5)).color(pal.muted)).wrap());
            if let Some(r) = res {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(ai_trunc(&r.text, 1500)).font(FontId::monospace(11.5)).color(if r.ok { pal.text } else { pal.danger })).wrap());
            }
        });
        ui.add_space(3.0);
    }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
}

fn ai_trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn message(ui: &mut Ui, ctx: &Context, pal: &Palette, ws: &Workspace, ai_rt: &mut ai::AiRuntime, idx: usize, m: &Msg, next: Option<&Msg>, is_last: bool, acts: &mut Vec<Act>) {
    let w = ui.available_width();
    match m.role {
        Role::User => {
            ui.add_space(12.0);
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                Frame::new().fill(lerp_color(pal.bg, pal.hover, 0.9)).corner_radius(CornerRadius::same(14)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_max_width(w * 0.88);
                    ui.add(egui::Label::new(RichText::new(&m.text).font(body(14.5)).color(pal.text)).wrap());
                });
            });
            ui.add_space(4.0);
        }
        Role::Assistant => {
            ui.add_space(6.0);
            let mut shown = m.text.clone();
            if idx == ai_rt.reveal_msg {
                let total = m.text.chars().count() as f32;
                if ai_rt.reveal < total {
                    ai_rt.reveal = (ai_rt.reveal + ctx.input(|i| i.stable_dt).min(0.05) * 900.0).min(total);
                    shown = m.text.chars().take(ai_rt.reveal as usize).collect();
                    ctx.request_repaint();
                }
            }
            if m.error {
                Frame::new().fill(lerp_color(pal.bg, pal.danger, 0.10)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(11, 9)).show(ui, |ui| {
                    ui.set_width(w - 22.0);
                    ui.horizontal_top(|ui| {
                        glyph(ui, l::TRIANGLE_ALERT, 14.0, pal.danger);
                        ui.add(egui::Label::new(RichText::new(&m.text).font(body(13.0)).color(pal.text)).wrap());
                    });
                    if m.text.to_lowercase().contains("key") || m.text.contains("401") || m.text.contains("404") || m.text.contains("connect") || m.text.contains("reach") {
                        if ghost_button(ui, pal, "Open AI settings").clicked() {
                            acts.push(Act::Settings);
                        }
                    }
                });
                return;
            }
            if !shown.trim().is_empty() {
                markdown(ui, pal, &shown);
            }
            for (k, c) in m.calls.iter().enumerate() {
                let res = next.and_then(|n| n.results.iter().find(|r| r.call_id == c.id));
                let running = is_last && res.is_none() && ai_rt.phase == Phase::Waiting;
                tool_row(ui, pal, ws, Id::new(("ai_call", idx, k)), c, res, running, acts);
            }
        }
        Role::Tool => {}
    }
}

// ───────────────────────────── the panel ─────────────────────────────

fn header(ui: &mut Ui, pal: &Palette, ai: &mut AiHost, out: &mut PanelOut) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        glyph(ui, l::SPARKLES, 15.0, pal.accent);
        let title = ai.thread_ref().map(|t| t.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "New chat".into());
        let mut items = vec![item("__new", "New chat", "")];
        for (i, t) in ai.st.threads.iter().enumerate() {
            items.push(item(&i.to_string(), &ai_trunc(&t.title, 38), ""));
        }
        if !ai.st.threads.is_empty() {
            items.push(item("__clear", "Delete all chats", ""));
        }
        let cur = ai.st.cur.to_string();
        let room = (ui.available_width() - 118.0).max(110.0);
        let picked = ui.scope(|ui| {
            ui.set_max_width(room);
            dropdown(ui, pal, Id::new("ai_chats_dd"), &ai_trunc(&title, 24), &items, &cur, false, false, 300.0)
        });
        if let Some(k) = picked.inner {
            match k.as_str() {
                "__new" => ai.new_chat(),
                "__clear" => {
                    if !ai.busy() {
                        ai.st.threads.clear();
                        ai.st.cur = 0;
                    }
                }
                n => {
                    if let Ok(i) = n.parse::<usize>() {
                        if !ai.busy() && i < ai.st.threads.len() {
                            ai.st.cur = i;
                            ai.rt.reveal_msg = usize::MAX;
                        }
                    }
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            if icon_button(ui, pal, Icon::Close, 15.0, "Close").clicked() {
                out.close = true;
            }
            if icon_button(ui, pal, Icon::Settings, 15.0, "AI settings").clicked() {
                ai.rt.settings = Some(if ai.ready().is_ok() { SettingsTab::Agent } else { SettingsTab::Providers });
            }
            if ui.add_enabled_ui(!ai.busy(), |ui| icon_button(ui, pal, Icon::Plus, 15.0, "New chat")).inner.clicked() {
                ai.new_chat();
            }
        });
    });
    ui.add_space(2.0);
}

fn empty_state(ui: &mut Ui, pal: &Palette, ai: &AiHost, acts: &mut Vec<Act>) {
    ui.add_space(34.0);
    ui.vertical_centered(|ui| {
        glyph(ui, l::SPARKLES, 30.0, pal.accent);
        ui.add_space(6.0);
        ui.label(RichText::new("Ask Lumen AI").font(bold(20.0)).color(pal.text));
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new("Build pages, databases, dashboards, flows and automations — or edit what you already have.").font(body(13.5)).color(pal.muted)).wrap());
    });
    ui.add_space(16.0);
    if let Err(e) = ai.ready() {
        Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(e).font(body(13.0)).color(pal.text)).wrap());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if primary_button(ui, pal, "Choose a model", 140.0).clicked() {
                    acts.push(Act::Settings);
                }
            });
        });
        ui.add_space(10.0);
    }
    for (label, prompt) in ai::SUGGESTIONS {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), if crate::touch::touchy() { 42.0 } else { 34.0 }), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(rect, 8.0, lerp_color(panel_bg(pal), pal.hover, t));
        ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, l::SPARKLES, icons::font(13.0), with_alpha(pal.accent, 0.9));
        ui.painter().text(pos2(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, *label, body(13.5), pal.text);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            acts.push(Act::Send((*prompt).to_owned()));
        }
    }
}

fn model_items(ai: &AiHost) -> Vec<Item> {
    let mut v = vec![];
    for pid in ai.st.configured() {
        let d = ai::provider(pid);
        v.push(item(&format!("p:{pid}"), &format!("{} · {}", d.name, ai_trunc(&ai.st.model_of(pid), 28)), ""));
    }
    if !v.iter().any(|i| i.key == format!("p:{}", ai.st.active)) {
        let d = ai::provider(&ai.st.active);
        v.push(item(&format!("p:{}", ai.st.active), &format!("{} · {}", d.name, ai_trunc(&ai.st.model_of(&ai.st.active), 28)), ""));
    }
    v.push(item("__settings", "Models, keys & on-device AI…", ""));
    v
}

fn composer(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, acts: &mut Vec<Act>) {
    let touchy = crate::touch::touchy();
    let id = Id::new("ai_input");
    let busy = ai.busy();
    let enter = !touchy && !busy && ctx.memory(|m| m.has_focus(id)) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    ui.add_space(6.0);
    Frame::new().inner_margin(Margin::symmetric(10, 0)).show(ui, |ui| {
        let focused = ctx.memory(|m| m.has_focus(id));
        let t = ctx.animate_bool_with_time(id.with("f"), focused, 0.15);
        Frame::new()
            .fill(soft(pal))
            .stroke(Stroke::new(1.0_f32, lerp_color(soft(pal), pal.accent, t * 0.75)))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin { left: 12, right: 8, top: 10, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let hint_txt = if ai.thread_ref().map_or(true, |t| t.msgs.is_empty()) { "Ask or tell Lumen AI to do anything…" } else { "Reply…" };
                ScrollArea::vertical().id_salt("ai_input_scroll").max_height(if touchy { 120.0 } else { 150.0 }).auto_shrink([false, true]).show(ui, |ui| {
                    let r = ui.add(TextEdit::multiline(&mut ai.rt.input).id(id).frame(false).desired_rows(1).desired_width(ui.available_width()).font(body(15.0)).hint_text(RichText::new(hint_txt).color(with_alpha(pal.muted, 0.85))));
                    if ai.rt.focus_input {
                        r.request_focus();
                        ai.rt.focus_input = false;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let def = ai::provider(&ai.st.active);
                    let label = format!("{} · {}", def.name.split(' ').next().unwrap_or(def.name), ai_trunc(&ai.st.model_of(&ai.st.active), 18));
                    let cur = format!("p:{}", ai.st.active);
                    let items = model_items(ai);
                    if let Some(k) = dropdown(ui, pal, Id::new("ai_model_dd"), &label, &items, &cur, true, false, 340.0) {
                        if k == "__settings" {
                            acts.push(Act::Settings);
                        } else if let Some(p) = k.strip_prefix("p:") {
                            ai.st.active = p.to_owned();
                        }
                    }
                    let acc = ai.st.access;
                    if let Some(k) = dropdown(ui, pal, Id::new("ai_access_dd"), acc.label(), &access_items(), access_key(acc), true, false, 330.0) {
                        ai.st.access = access_from(&k);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                        let has = !ai.rt.input.trim().is_empty();
                        let fill = if busy { pal.text } else if has { pal.accent } else { lerp_color(pal.bg, pal.muted, 0.35) };
                        ui.painter().circle_filled(rect.center(), 14.0, fill);
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, if busy { l::SQUARE } else { l::ARROW_UP }, icons::font(if busy { 11.0 } else { 16.0 }), if busy { pal.bg } else { Color32::WHITE });
                        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if busy { "Stop" } else { "Send" }).clicked() {
                            if busy {
                                ai.stop();
                            } else if has {
                                acts.push(Act::Send(std::mem::take(&mut ai.rt.input)));
                            }
                        }
                    });
                });
            });
    });
    if enter && !ai.rt.input.trim().is_empty() {
        acts.push(Act::Send(std::mem::take(&mut ai.rt.input)));
    }
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        let usage = if ai.rt.tokens_in + ai.rt.tokens_out > 0 { format!("{} in · {} out this session", kfmt(ai.rt.tokens_in), kfmt(ai.rt.tokens_out)) } else { "AI can make mistakes.".to_owned() };
        ui.label(RichText::new(usage).font(body(11.5)).color(with_alpha(pal.muted, 0.9)));
        if ai.rt.snapshot.is_some() && !busy && ghost_button(ui, pal, "↩ Undo AI changes").clicked() {
            ai.rt.status = "undo".into();
        }
    });
    ui.add_space(4.0);
}

fn kfmt(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

fn run_acts(ctx: &Context, ai: &mut AiHost, ws: &mut Workspace, acts: Vec<Act>, out: &mut PanelOut) {
    for a in acts {
        match a {
            Act::Send(t) => ai.send(ws, ctx, &t),
            Act::Open(id) => out.open_page = Some(id),
            Act::Approve(ok) => {
                ai.approve(ws, ctx, ok);
            }
            Act::Settings => ai.rt.settings = Some(SettingsTab::Providers),
        }
    }
    if ai.rt.status == "undo" {
        ai.rt.status.clear();
        if ai.undo(ws) {
            ai.rt.toast = Some("AI changes undone".into());
        }
    }
}

/// The chat. `ui` is the inside of a side panel or of the full-screen sheet.
pub fn panel(ui: &mut Ui, ctx: &Context, ai: &mut AiHost, ws: &mut Workspace, pal: &Palette) -> PanelOut {
    let mut out = PanelOut::default();
    let mut acts: Vec<Act> = vec![];
    egui::TopBottomPanel::top(Id::new("ai_head")).frame(Frame::NONE).show_separator_line(false).show_inside(ui, |ui| header(ui, pal, ai, &mut out));
    egui::TopBottomPanel::bottom(Id::new("ai_comp")).frame(Frame::NONE).show_separator_line(false).show_inside(ui, |ui| composer(ui, ctx, pal, ai, &mut acts));
    egui::CentralPanel::default().frame(Frame::NONE).show_inside(ui, |ui| {
        ai.thread();
        let cur = ai.st.cur;
        let msgs = std::mem::take(&mut ai.st.threads[cur].msgs);
        ScrollArea::vertical().id_salt("ai_scroll").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            ui.add_space(2.0);
            Frame::new().inner_margin(Margin::symmetric(16, 0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if msgs.is_empty() {
                    empty_state(ui, pal, ai, &mut acts);
                }
                let n = msgs.len();
                for (i, m) in msgs.iter().enumerate() {
                    message(ui, ctx, pal, ws, &mut ai.rt, i, m, msgs.get(i + 1), i + 1 == n, &mut acts);
                }
                if ai.rt.phase == Phase::Approval {
                    ui.add_space(10.0);
                    Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Lumen AI wants to make changes").font(bold(13.5)).color(pal.text));
                        for c in &ai.rt.pending {
                            ui.add(egui::Label::new(RichText::new(format!("• {}", ai::summarize_call(c, Some(&ai::ToolRes { call_id: String::new(), name: String::new(), ok: true, text: String::new(), open: None })))).font(body(12.5)).color(pal.muted)).wrap());
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if primary_button(ui, pal, "Allow", 100.0).clicked() {
                                acts.push(Act::Approve(true));
                            }
                            if ghost_button(ui, pal, "Don't allow").clicked() {
                                acts.push(Act::Approve(false));
                            }
                        });
                    });
                }
                if ai.rt.phase == Phase::Waiting {
                    ui.add_space(8.0);
                    let label = if !ai.rt.status.is_empty() { ai.rt.status.clone() } else if ai.rt.steps == 0 { "Thinking…".to_owned() } else { format!("Working… step {}", ai.rt.steps + 1) };
                    thinking_dots(ui, pal, &label);
                }
                ui.add_space(16.0);
            });
        });
        ai.st.threads[cur].msgs = msgs;
    });
    run_acts(ctx, ai, ws, acts, &mut out);
    out
}

/// Phones: the chat covers the screen like a native sheet.
pub fn fullscreen(ctx: &Context, ai: &mut AiHost, ws: &mut Workspace, pal: &Palette) -> PanelOut {
    let screen = ctx.screen_rect();
    let mut out = PanelOut::default();
    let t = crate::widgets::appear(ctx, Id::new("ai_sheet_a"), 0.22);
    let area = Area::new(Id::new("ai_sheet")).order(Order::Foreground).fixed_pos(pos2(screen.min.x, screen.min.y + (1.0 - t) * 40.0)).show(ctx, |ui| {
        ui.set_opacity(t);
        let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, panel_bg(pal));
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
        inner.set_clip_rect(rect);
        out = panel(&mut inner, ctx, ai, ws, pal);
    });
    ctx.move_to_top(area.response.layer_id);
    if ctx.input(|i| i.key_pressed(Key::Escape)) && ai.rt.settings.is_none() {
        out.close = true;
    }
    out
}

// ───────────────────────────── settings ─────────────────────────────

fn provider_items(ai: &AiHost) -> Vec<Item> {
    let configured = ai.st.configured();
    let mut v = vec![];
    for p in PROVIDERS.iter().filter(|p| !ai::is_on_device(p.id) && p.id != "custom") {
        v.push(item(p.id, &format!("{}{}", if configured.contains(&p.id) { "● " } else { "" }, p.name), p.blurb));
    }
    for p in PROVIDERS.iter().filter(|p| ai::is_on_device(p.id) || p.id == "custom") {
        v.push(item(p.id, &format!("{}{}", if configured.contains(&p.id) { "● " } else { "" }, p.name), p.blurb));
    }
    v
}

fn model_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, ws: &Workspace) {
    if ai.rt.sel_provider.is_empty() {
        ai.rt.sel_provider = ai.st.active.clone();
    }
    let pid0 = ai.rt.sel_provider.clone();
    let def0 = ai::provider(&pid0);
    setting(ui, pal, "Provider", def0.blurb, |ui| {
        if let Some(k) = dropdown(ui, pal, Id::new("set_provider_dd"), def0.name, &provider_items(ai), &pid0, false, true, 480.0) {
            ai.rt.sel_provider = k;
            ai.rt.test_msg = None;
        }
    });
    let pid = ai.rt.sel_provider.clone();
    let def = ai::provider(&pid);
    if def.needs_key || pid == "custom" {
        setting(ui, pal, if def.needs_key { "API key" } else { "API key (optional)" }, "Stored on this device only.", |ui| {
            let mut key = ai.st.cfg(&pid).key.clone();
            if soft_field(ui, pal, &mut key, "Paste your key", true, Id::new(("ai_key", &pid))).changed() {
                ai.st.cfg_mut(&pid).key = key.trim().to_owned();
            }
            if !def.key_url.is_empty() && ghost_button(ui, pal, &format!("Get a key ↗  {}", def.key_url.trim_start_matches("https://"))).clicked() {
                crate::platform::open_url(ctx, def.key_url);
            }
        });
    }
    // model: a drop-down of the known + detected models, and a field for any other id
    let mut names: Vec<String> = def.models.iter().map(|m| (*m).to_owned()).collect();
    if let Some(found) = ai.rt.models.get(&pid) {
        for m in found {
            if !names.contains(m) {
                names.push(m.clone());
            }
        }
    }
    let cur_model = ai.st.model_of(&pid);
    if !cur_model.is_empty() && !names.contains(&cur_model) {
        names.insert(0, cur_model.clone());
    }
    setting(ui, pal, "Model", if ai::is_on_device(&pid) || pid == "custom" || ai.st.has_key(&pid) { "Pick one, refresh the list from the server, or type any model id." } else { "Pick one or type any model id." }, |ui| {
        let items: Vec<Item> = names.iter().map(|m| item(m, m, "")).collect();
        if !items.is_empty() {
            if let Some(k) = dropdown(ui, pal, Id::new(("set_model_dd", &pid)), &ai_trunc(&cur_model, 48), &items, &cur_model, false, true, 480.0) {
                ai.st.cfg_mut(&pid).model = k;
            }
            ui.add_space(4.0);
        }
        let mut model = ai.st.cfg(&pid).model.clone();
        if soft_field(ui, pal, &mut model, def.models.first().copied().unwrap_or("model id"), false, Id::new(("ai_model", &pid))).changed() {
            ai.st.cfg_mut(&pid).model = model.trim().to_owned();
        }
        if def.api == ai::Api::OpenAi && ghost_button(ui, pal, if ai.rt.detecting { "Looking for models …" } else { "↻ Refresh model list from the server" }).clicked() {
            ai.detect_models(ctx, Some(&pid));
        }
        if let Some(f) = ai.rt.models.get(&pid) {
            ui.label(RichText::new(if f.is_empty() { "The server did not list any models (is it running?)".to_owned() } else { format!("{} models found", f.len()) }).font(body(12.0)).color(pal.muted));
        }
    });
    ui.add_space(4.0);
    let id = Id::new(("adv_open", &pid));
    let mut adv = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(pid == "custom");
    if ghost_button(ui, pal, if adv { "▾ Advanced" } else { "▸ Advanced (server URL)" }).clicked() {
        adv = !adv;
    }
    ctx.data_mut(|d| d.insert_temp(id, adv));
    if adv {
        setting(ui, pal, if pid == "custom" { "Server URL (…/v1)" } else { "Server URL" }, "Leave empty to use the default.", |ui| {
            let mut base = ai.st.cfg(&pid).base.clone();
            if soft_field(ui, pal, &mut base, if def.base.is_empty() { "https://your-server/v1" } else { def.base }, false, Id::new(("ai_base", &pid))).changed() {
                ai.st.cfg_mut(&pid).base = base.trim().to_owned();
            }
        });
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let active = ai.st.active == pid;
        if primary_button(ui, pal, if active { "✓ In use" } else { "Use this model" }, 150.0).clicked() {
            ai.st.active = pid.clone();
        }
        if ghost_button(ui, pal, "Test connection").clicked() {
            ai.test(ws, ctx, &pid);
        }
    });
    if ai.rt.test_slot.is_some() {
        ui.label(RichText::new("Testing …").font(body(13.0)).color(pal.muted));
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if let Some((ok, msg)) = &ai.rt.test_msg {
        ui.add(egui::Label::new(RichText::new(msg).font(body(13.0)).color(if *ok { green() } else { pal.danger })).wrap());
    }
    ui.add_space(10.0);
    ui.add(egui::Label::new(RichText::new(if cfg!(target_arch = "wasm32") { "Keys stay in this browser. Requests to cloud providers are relayed by this website's proxy (never logged or stored). On-device models are called directly from your browser." } else { "Keys are stored on this device only and sent straight to the provider." }).font(body(12.0)).color(with_alpha(pal.muted, 0.9))).wrap());
}

fn agent_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost) {
    let _ = ctx;
    setting(ui, pal, "What may Lumen AI do?", ai.st.access.hint(), |ui| {
        if let Some(k) = dropdown(ui, pal, Id::new("set_access_dd"), ai.st.access.label(), &access_items(), access_key(ai.st.access), false, true, 480.0) {
            ai.st.access = access_from(&k);
        }
    });
    ui.add_space(12.0);
    toggle_row(ui, pal, "Ask before making changes", "Every create / edit waits for your Allow.", &mut ai.st.confirm);
    setting(ui, pal, "Compact mode", "Short prompt and a smaller tool set: far fewer tokens, and what small on-device models need.", |ui| {
        let items = vec![item("0", "Automatic (on for on-device models)", ""), item("1", "Always on", ""), item("2", "Always off (full toolset)", "")];
        let label = match ai.st.lite { 1 => "Always on", 2 => "Always off (full toolset)", _ => "Automatic (on for on-device models)" };
        if let Some(k) = dropdown(ui, pal, Id::new("set_lite_dd"), label, &items, &ai.st.lite.to_string(), false, true, 480.0) {
            ai.st.lite = k.parse().unwrap_or(0);
        }
    });
    setting(ui, pal, &format!("Max steps per request: {}", ai.st.max_steps), "How many tool rounds the agent may take before it stops and asks.", |ui| {
        ui.add(egui::Slider::new(&mut ai.st.max_steps, 4..=48).show_value(false));
    });
    setting(ui, pal, "Standing instructions", "Always applied — tone, language, naming conventions, who you are.", |ui| {
        Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(9)).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.add(TextEdit::multiline(&mut ai.st.instructions).desired_rows(4).desired_width(ui.available_width()).frame(false).font(body(14.0)).hint_text("e.g. I run a design studio. Reply in German. Use emoji icons."));
        });
    });
    ui.add_space(14.0);
    if ghost_button(ui, pal, "Delete all chats").clicked() && !ai.busy() {
        ai.st.threads.clear();
        ai.st.cur = 0;
    }
    ui.add_space(6.0);
    ui.label(RichText::new(format!("This session: {} tokens in · {} tokens out", ai.rt.tokens_in, ai.rt.tokens_out)).font(body(12.0)).color(pal.muted));
}

fn local_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, toast: &mut Option<String>) {
    let probed_id = Id::new("ai_local_probed");
    if !ctx.data(|d| d.get_temp::<bool>(probed_id)).unwrap_or(false) {
        ctx.data_mut(|d| d.insert_temp(probed_id, true));
        ai.detect_models(ctx, None);
    }
    ui.add(egui::Label::new(RichText::new("Run the model on this device or on your own network — your workspace never leaves it. Lumen talks to the runtime's local OpenAI-compatible server and automatically switches to a compact prompt and tool set that small models can follow.").font(body(13.0)).color(pal.muted)).wrap());
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if primary_button(ui, pal, if ai.rt.detecting { "Looking …" } else { "Detect runtimes" }, 150.0).clicked() {
            ai.detect_models(ctx, None);
        }
    });
    sub_head(ui, pal, "Runtimes on this device", "");
    for p in PROVIDERS.iter().filter(|p| ai::is_on_device(p.id)) {
        let found = ai.rt.models.get(p.id).cloned();
        Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(12, 9)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::hover());
                let up = found.as_ref().map_or(false, |f| !f.is_empty());
                ui.painter().circle_filled(r.center(), 4.5, if up { green() } else { with_alpha(pal.muted, 0.6) });
                ui.vertical(|ui| {
                    ui.label(RichText::new(p.name).font(bold(13.5)).color(pal.text));
                    let st = match &found {
                        Some(f) if !f.is_empty() => format!("running · {} model{}", f.len(), if f.len() == 1 { "" } else { "s" }),
                        Some(_) => "answers, but lists no models".to_owned(),
                        None if ai.rt.detecting => "checking …".to_owned(),
                        None => "not detected".to_owned(),
                    };
                    ui.label(RichText::new(format!("{}  ·  {}", p.base.replace("/v1", ""), st)).font(body(12.0)).color(pal.muted));
                });
            });
            if let Some(f) = found.filter(|f| !f.is_empty()) {
                ui.add_space(6.0);
                let cur = ai.st.model_of(p.id);
                let cur = if f.contains(&cur) { cur } else { f[0].clone() };
                let items: Vec<Item> = f.iter().map(|m| item(m, m, "")).collect();
                ui.horizontal(|ui| {
                    ui.set_max_width(ui.available_width());
                    if let Some(k) = dropdown(ui, pal, Id::new(("local_model_dd", p.id)), &ai_trunc(&cur, 30), &items, &cur, false, false, 360.0) {
                        ai.st.cfg_mut(p.id).model = k;
                    }
                    let active = ai.st.active == p.id;
                    if primary_button(ui, pal, if active { "✓ In use" } else { "Use" }, 90.0).clicked() {
                        ai.st.cfg_mut(p.id).model = cur.clone();
                        ai.st.active = p.id.to_owned();
                        ai.rt.sel_provider = p.id.to_owned();
                        *toast = Some(format!("Lumen AI now runs on {} · {}", p.name, cur));
                    }
                });
            }
        });
        ui.add_space(6.0);
    }
    sub_head(ui, pal, "Set it up", "");
    let platform = if cfg!(target_os = "android") { "Android (Termux)" } else if cfg!(target_arch = "wasm32") { "Browser" } else { "Windows" };
    ui.label(RichText::new(format!("You are on: {platform}")).font(body(12.5)).color(pal.muted));
    ui.add_space(4.0);
    let steps: &str = if cfg!(target_os = "android") {
        "1. Install Termux (F-Droid), then:\n   pkg install ollama\n   ollama serve &\n   ollama pull qwen2.5:3b\n2. Come back here and tap “Detect runtimes”.\n(llama.cpp: llama-server -m model.gguf --port 8080)"
    } else if cfg!(target_arch = "wasm32") {
        "1. Install Ollama (ollama.com) and run:\n   OLLAMA_ORIGINS=* ollama serve\n   ollama pull qwen2.5:7b\n2. Tap “Detect runtimes”. Chrome asks once to allow access to local devices.\nThe Windows and Android apps need no CORS setting."
    } else {
        "1. Install Ollama (ollama.com) or LM Studio, then pull a model:\n   ollama pull qwen2.5:7b\n2. Tap “Detect runtimes” — nothing else to configure.\n(llama.cpp: llama-server -m model.gguf --port 8080)"
    };
    code_box(ui, ctx, pal, steps, toast);
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new("Tip: models with tool support work best (Qwen 2.5, Llama 3.1/3.2, Mistral). Compact mode keeps each request small, so even 3B models stay fast.").font(body(12.0)).color(with_alpha(pal.muted, 0.9))).wrap());
}

fn nav_item(ui: &mut Ui, pal: &Palette, glyph_s: &str, label: &str, on: bool, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, if crate::touch::touchy() { 40.0 } else { 32.0 }), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || on, 0.1);
    ui.painter().rect_filled(rect, 7.0, lerp_color(pal.popup, pal.hover, if on { 1.0 } else { t * 0.7 }));
    ui.painter().text(pos2(rect.left() + 14.0, rect.center().y), Align2::CENTER_CENTER, glyph_s, icons::font(14.0), if on { pal.accent } else { pal.muted });
    ui.painter().text(pos2(rect.left() + 30.0, rect.center().y), Align2::LEFT_CENTER, label, if on { bold(13.5) } else { body(13.5) }, pal.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn settings(ctx: &Context, ai: &mut AiHost, ws: &Workspace, pal: &Palette) {
    let Some(tab) = ai.rt.settings else { return };
    let mut tab = tab;
    let mut toast = None;
    let narrow = ctx.screen_rect().width() < 680.0;
    let max_h = (ctx.screen_rect().height() - if narrow { 120.0 } else { 150.0 }).max(260.0);
    let tabs = [(SettingsTab::Providers, l::SPARKLES, "Model"), (SettingsTab::Agent, l::SETTINGS, "Behavior"), (SettingsTab::Local, l::ZAP, "On-device"), (SettingsTab::Connect, l::NETWORK, "Connections")];
    let open = crate::app::modal(ctx, Id::new("ai_settings"), pal, 800.0, if narrow { 12.0 } else { 36.0 }, |ui| {
        let mut body_ui = |ui: &mut Ui, tab: SettingsTab, toast: &mut Option<String>| {
            ScrollArea::vertical().id_salt("ai_settings_scroll").max_height(max_h - 20.0).auto_shrink([false, true]).show(ui, |ui| {
                Frame::new().inner_margin(Margin::symmetric(10, 0)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match tab {
                        SettingsTab::Providers => model_tab(ui, ctx, pal, ai, ws),
                        SettingsTab::Agent => agent_tab(ui, ctx, pal, ai),
                        SettingsTab::Local => local_tab(ui, ctx, pal, ai, toast),
                        SettingsTab::Connect => connect_tab(ui, ctx, pal, ai, toast),
                    }
                    ui.add_space(14.0);
                });
            });
        };
        if narrow {
            ui.add_space(4.0);
            ScrollArea::horizontal().id_salt("ai_settings_tabs").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (t, g, name) in tabs {
                        if nav_item(ui, pal, g, name, tab == t, 128.0).clicked() {
                            tab = t;
                        }
                    }
                });
            });
            ui.add_space(6.0);
            body_ui(ui, tab, &mut toast);
        } else {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(176.0);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        glyph(ui, l::SPARKLES, 16.0, pal.accent);
                        ui.label(RichText::new("Lumen AI").font(bold(16.0)).color(pal.text));
                    });
                    ui.add_space(8.0);
                    for (t, g, name) in tabs {
                        if nav_item(ui, pal, g, name, tab == t, 170.0).clicked() {
                            tab = t;
                        }
                        ui.add_space(2.0);
                    }
                });
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    body_ui(ui, tab, &mut toast);
                });
            });
        }
    });
    if toast.is_some() {
        ai.rt.toast = toast;
    }
    ai.rt.settings = if open { Some(tab) } else { None };
    if !open {
        crate::widgets::reset_appear(ctx, Id::new("ai_settings").with("a"));
    }
}

fn code_box(ui: &mut Ui, ctx: &Context, pal: &Palette, text: &str, toast: &mut Option<String>) {
    Frame::new().fill(soft(pal)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).font(FontId::monospace(11.5)).color(pal.text)).wrap());
    });
    if ghost_button(ui, pal, "Copy").clicked() {
        ctx.copy_text(text.to_owned());
        *toast = Some("Copied".into());
    }
}

fn connect_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, toast: &mut Option<String>) {
    ui.add(egui::Label::new(RichText::new("Let other AI agents (Claude, ChatGPT connectors, Cursor, your own scripts …) use this workspace through the Model Context Protocol. They get the same tools Lumen AI has.").font(body(13.0)).color(pal.muted)).wrap());
    ui.add_space(8.0);
    sub_head(ui, pal, "What may external agents do?", "");
    if let Some(k) = dropdown(ui, pal, Id::new("mcp_access_dd"), ai.st.mcp.access.label(), &access_items(), access_key(ai.st.mcp.access), false, true, 360.0) {
        ai.st.mcp.access = access_from(&k);
    }
    ui.add(egui::Label::new(RichText::new(ai.st.mcp.access.hint()).font(body(12.0)).color(pal.muted)).wrap());
    ui.add_space(10.0);
    let w = ui.available_width();
    let origin = {
        let o = crate::platform::site_origin();
        if o.is_empty() { ai.st.mcp.bridge_url.trim().trim_end_matches('/').to_owned() } else { o }
    };
    let site = if origin.is_empty() { "https://YOUR-LUMEN-SITE".to_owned() } else { origin };
    sub_head(ui, pal, "Cloud bridge", "Works from anywhere");
    toggle_row(ui, pal, "Connect to the Lumen site", "Agents call your site's /api/mcp; the site relays to this app while it is open. Works on web, Android and Windows.", &mut ai.st.mcp.bridge_on);
    if ai.st.mcp.bridge_on {
        ui.add_space(4.0);
        if !cfg!(target_arch = "wasm32") {
            ui.label(RichText::new("Site URL").font(bold(12.5)).color(pal.muted));
            let mut u = ai.st.mcp.bridge_url.clone();
            if field(ui, pal, soft(pal), &mut u, "https://your-lumen-site.example", false, w, Id::new("mcp_url")).changed() {
                ai.st.mcp.bridge_url = u.trim().to_owned();
            }
        }
        let (ok, msg) = ai.rt.mcp.bridge_status();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, if ok { Color32::from_rgb(70, 170, 110) } else { pal.muted });
            ui.add(egui::Label::new(RichText::new(msg).font(body(12.5)).color(pal.muted)).wrap());
        });
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Access token").font(bold(12.5)).color(pal.muted));
    if ai.st.mcp.token.is_empty() {
        ai.st.mcp.token = crate::mcp::new_token();
    }
    let tok = ai.st.mcp.token.clone();
    let masked = format!("{}…{}", &tok[..8.min(tok.len())], &tok[tok.len().saturating_sub(4)..]);
    ui.horizontal(|ui| {
        ui.label(RichText::new(masked).font(FontId::monospace(13.0)).color(pal.text));
        if ghost_button(ui, pal, "Copy").clicked() {
            ctx.copy_text(tok.clone());
            *toast = Some("Token copied".into());
        }
        if ghost_button(ui, pal, "New token").clicked() {
            ai.st.mcp.token = crate::mcp::new_token();
        }
    });
    ui.add(egui::Label::new(RichText::new("Anyone with the token can use the workspace at the access level above. Make a new token to cut off old connections.").font(body(12.0)).color(with_alpha(pal.muted, 0.9))).wrap());
    ui.add_space(8.0);
    ui.label(RichText::new("Endpoint").font(bold(12.5)).color(pal.muted));
    code_box(ui, ctx, pal, &format!("{site}/api/mcp"), toast);
    ui.add_space(4.0);
    ui.label(RichText::new("Config for Claude Desktop, Cursor, Windsurf, VS Code …").font(bold(12.5)).color(pal.muted));
    let cfg = format!("{{\n  \"mcpServers\": {{\n    \"lumen\": {{\n      \"url\": \"{site}/api/mcp\",\n      \"headers\": {{ \"Authorization\": \"Bearer {tok}\" }}\n    }}\n  }}\n}}");
    code_box(ui, ctx, pal, &cfg, toast);
    ui.add(egui::Label::new(RichText::new("Clients that only speak stdio can bridge with:  npx mcp-remote … --header \"Authorization: Bearer <token>\"").font(body(12.0)).color(with_alpha(pal.muted, 0.9))).wrap());

    #[cfg(not(target_arch = "wasm32"))]
    {
        ui.add_space(12.0);
        sub_head(ui, pal, "Local server", "This device only — no internet needed");
        toggle_row(ui, pal, "Run a local MCP server", "http://127.0.0.1 only. No internet needed.", &mut ai.st.mcp.local_on);
        if ai.st.mcp.local_on {
            let mut port = ai.st.mcp.port.to_string();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Port").font(bold(12.5)).color(pal.muted));
                if field(ui, pal, soft(pal), &mut port, "7878", false, 100.0, Id::new("mcp_port")).changed() {
                    if let Ok(p) = port.trim().parse::<u16>() {
                        if p >= 1024 {
                            ai.st.mcp.port = p;
                        }
                    }
                }
            });
            ui.add(egui::Label::new(RichText::new(ai.rt.mcp.local_status()).font(body(12.5)).color(pal.muted)).wrap());
        }
    }
    ui.add_space(12.0);
    sub_head(ui, pal, "Recent agent activity", "");
    if ai.rt.mcp.activity.is_empty() {
        ui.label(RichText::new("Nothing yet — connect an agent and ask it to list your pages.").font(body(12.5)).color(pal.muted));
    }
    for (t, what, ok) in ai.rt.mcp.activity.iter().rev().take(8) {
        let secs = ((crate::auth::now_ms() - t) / 1000.0).max(0.0) as u64;
        ui.horizontal(|ui| {
            glyph(ui, if *ok { l::CHECK } else { l::X }, 12.0, if *ok { Color32::from_rgb(70, 170, 110) } else { pal.danger });
            ui.label(RichText::new(what.replace('_', " ")).font(body(12.5)).color(pal.text));
            ui.label(RichText::new(if secs < 60 { format!("{secs}s ago") } else { format!("{}m ago", secs / 60) }).font(body(11.5)).color(pal.muted));
        });
    }
}

