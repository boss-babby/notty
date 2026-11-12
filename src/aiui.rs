//! Lumen AI — user interface: the chat panel (side panel on wide screens, full-screen sheet on phones), the tool-call cards,
//! the markdown renderer and the settings dialog (providers, agent behaviour, MCP connections).

use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Align, Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Order, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::agent_tools::Access;
use crate::ai::{self, AiHost, Msg, Phase, Role, SettingsTab, PROVIDERS};
use crate::icons::{self, Icon};
use crate::lucide as l;
use crate::model::Workspace;
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{field, ghost_button, icon_button, primary_button, section_label, with_alpha};

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

// ───────────────────────────── small widgets ─────────────────────────────

fn chip(ui: &mut Ui, pal: &Palette, text: &str, on: bool) -> egui::Response {
    let touchy = crate::touch::touchy();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), if on { bold(12.5) } else { body(12.5) }, pal.text);
    let h = if touchy { 32.0 } else { 26.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, h), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let fill = if on { pal.accent_soft } else { lerp_color(with_alpha(pal.muted, 0.08), pal.hover, t) };
    ui.painter().rect_filled(rect, h / 2.0, fill);
    if on {
        ui.painter().rect_stroke(rect, h / 2.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.55)), egui::StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, if on { bold(12.5) } else { body(12.5) }, if on { pal.accent } else { pal.text });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn switch(ui: &mut Ui, pal: &Palette, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(40.0, 24.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
    ui.painter().rect_filled(rect, 12.0, lerp_color(with_alpha(pal.muted, 0.35), pal.accent, t));
    let x = rect.left() + 12.0 + t * 16.0;
    ui.painter().circle_filled(pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn toggle_row(ui: &mut Ui, pal: &Palette, title: &str, hint: &str, on: &mut bool) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width(w - 54.0);
            ui.label(RichText::new(title).font(bold(14.0)).color(pal.text));
            if !hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(hint).font(body(12.5)).color(pal.muted)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            switch(ui, pal, on);
        });
    });
}

fn card(pal: &Palette) -> Frame {
    Frame::new().fill(with_alpha(pal.muted, 0.07)).stroke(Stroke::new(1.0_f32, with_alpha(pal.border, 0.9))).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(11, 9))
}

fn glyph(ui: &mut Ui, g: &str, size: f32, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(size + 4.0, size + 4.0), Sense::hover());
    ui.painter().text(r.center(), Align2::CENTER_CENTER, g, icons::font(size), color);
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
                Frame::new().fill(pal.code_bg).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(9)).show(ui, |ui| {
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

fn spark_avatar(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    crate::widgets::gradient_rect(ui, r, pal.accent, lerp_color(pal.accent, Color32::from_rgb(190, 90, 220), 0.7));
    ui.painter().text(r.center(), Align2::CENTER_CENTER, l::SPARKLES, icons::font(13.0), Color32::WHITE);
}

fn thinking_dots(ui: &mut Ui, pal: &Palette, label: &str) {
    let t = ui.input(|i| i.time) as f32;
    ui.horizontal(|ui| {
        ui.add_space(30.0);
        for k in 0..3 {
            let a = ((t * 3.2 - k as f32 * 0.5).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 2.6 + a * 1.4, with_alpha(pal.accent, 0.35 + 0.65 * a));
        }
        ui.label(RichText::new(label).font(body(12.5)).color(pal.muted));
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
}

fn tool_card(ui: &mut Ui, pal: &Palette, ws: &Workspace, key: Id, call: &ai::Call, res: Option<&ai::ToolRes>, running: bool, acts: &mut Vec<Act>) {
    let label = ai::summarize_call(call, res);
    let open_id = Id::new(("ai_tool_open", key));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    card(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let head = ui.horizontal(|ui| {
            match res {
                Some(r) if r.ok => glyph(ui, l::CIRCLE_CHECK, 15.0, Color32::from_rgb(70, 170, 110)),
                Some(_) => glyph(ui, l::CIRCLE_ALERT, 15.0, pal.danger),
                None if running => {
                    let t = ui.input(|i| i.time) as f32;
                    let (r, _) = ui.allocate_exact_size(vec2(19.0, 19.0), Sense::hover());
                    let a = (t * 4.0).sin() * 0.5 + 0.5;
                    ui.painter().circle_stroke(r.center(), 5.5 + a, Stroke::new(1.6_f32, with_alpha(pal.accent, 0.4 + 0.6 * a)));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
                }
                None => glyph(ui, l::CLOCK, 15.0, pal.muted),
            }
            let w = ui.available_width() - 62.0;
            ui.add_sized(vec2(w.max(40.0), 18.0), egui::Label::new(RichText::new(&label).font(body(13.0)).color(pal.text)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(if open { l::CHEVRON_DOWN } else { l::CHEVRON_RIGHT }).font(icons::font(13.0)).color(pal.muted));
                if let Some(id) = res.and_then(|r| r.open).filter(|id| ws.page(*id).is_some()) {
                    if crate::widgets::ghost_button(ui, pal, "Open").clicked() {
                        acts.push(Act::Open(id));
                    }
                }
            });
        });
        let click = ui.interact(head.response.rect, key.with("head"), Sense::click());
        if click.clicked() {
            open = !open;
        }
        if open {
            ui.add_space(4.0);
            let args = serde_json::to_string_pretty(&call.args).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(format!("{}\n{}", call.name, ai_trunc(&args, 1200))).font(FontId::monospace(11.5)).color(pal.muted)).wrap());
            if let Some(r) = res {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(ai_trunc(&r.text, 1500)).font(FontId::monospace(11.5)).color(if r.ok { pal.text } else { pal.danger })).wrap());
            }
        }
    });
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
            ui.add_space(8.0);
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                Frame::new().fill(pal.accent_soft).corner_radius(CornerRadius { nw: 14, ne: 14, sw: 14, se: 4 }).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_max_width(w * 0.86);
                    ui.add(egui::Label::new(RichText::new(&m.text).font(body(14.5)).color(pal.text)).wrap());
                });
            });
        }
        Role::Assistant => {
            ui.add_space(8.0);
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
                Frame::new().fill(with_alpha(pal.danger, 0.1)).stroke(Stroke::new(1.0_f32, with_alpha(pal.danger, 0.5))).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(11, 9)).show(ui, |ui| {
                    ui.set_width(w - 24.0);
                    ui.horizontal_top(|ui| {
                        glyph(ui, l::TRIANGLE_ALERT, 15.0, pal.danger);
                        ui.add(egui::Label::new(RichText::new(&m.text).font(body(13.5)).color(pal.text)).wrap());
                    });
                    if m.text.to_lowercase().contains("key") || m.text.contains("401") || m.text.contains("404") {
                        if ghost_button(ui, pal, "Open AI settings").clicked() {
                            acts.push(Act::Settings);
                        }
                    }
                });
                return;
            }
            if !shown.trim().is_empty() {
                ui.horizontal_top(|ui| {
                    spark_avatar(ui, pal);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        markdown(ui, pal, &shown);
                    });
                });
            }
            for (k, c) in m.calls.iter().enumerate() {
                ui.add_space(5.0);
                let res = next.and_then(|n| n.results.iter().find(|r| r.call_id == c.id));
                let running = is_last && res.is_none() && ai_rt.phase == Phase::Waiting;
                ui.horizontal(|ui| {
                    ui.add_space(28.0);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        tool_card(ui, pal, ws, Id::new(("ai_call", idx, k)), c, res, running, acts);
                    });
                });
            }
        }
        Role::Tool => {}
    }
}

// ───────────────────────────── the panel ─────────────────────────────

fn header(ui: &mut Ui, pal: &Palette, ai: &mut AiHost, out: &mut PanelOut) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        spark_avatar(ui, pal);
        ui.label(RichText::new("Lumen AI").font(bold(15.5)).color(pal.text));
        if ai.rt.phase != Phase::Idle {
            ui.label(RichText::new("working").font(body(11.5)).color(pal.accent));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            if icon_button(ui, pal, Icon::Close, 15.0, "Close").clicked() {
                out.close = true;
            }
            if icon_button(ui, pal, Icon::Settings, 15.0, "AI settings, API keys & agent connections").clicked() {
                ai.rt.settings = Some(if ai.ready().is_ok() { SettingsTab::Agent } else { SettingsTab::Providers });
            }
            if icon_button(ui, pal, Icon::History, 15.0, "Chat history").clicked() {
                ai.rt.history = !ai.rt.history;
            }
            if ui.add_enabled_ui(!ai.busy(), |ui| icon_button(ui, pal, Icon::Plus, 15.0, "New chat")).inner.clicked() {
                ai.new_chat();
            }
        });
    });
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(10.0);
        let def = ai::provider(&ai.st.active);
        let model = ai.st.model_of(&ai.st.active);
        if chip(ui, pal, &format!("{} · {}", def.name, ai_trunc(&model, 22)), false).clicked() {
            ai.rt.settings = Some(SettingsTab::Providers);
        }
        let acc = ai.st.access;
        let (label, col) = (match acc { Access::ReadOnly => "Read only", Access::Edit => "Can edit", Access::Full => "Full access" }, acc);
        let r = chip(ui, pal, label, col == Access::Full);
        if r.clicked() {
            ai.st.access = match acc { Access::ReadOnly => Access::Edit, Access::Edit => Access::Full, Access::Full => Access::ReadOnly };
        }
        r.on_hover_text(format!("{} Tap to change.", acc.hint()));
        if ai.st.confirm {
            if chip(ui, pal, "Asks first", true).on_hover_text("Every change needs your OK. Turn off in AI settings.").clicked() {
                ai.st.confirm = false;
            }
        }
    });
    ui.add_space(4.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0_f32, with_alpha(pal.border, 0.8)));
}

fn history_view(ui: &mut Ui, pal: &Palette, ai: &mut AiHost) {
    ui.add_space(6.0);
    section_label(ui, pal, "CHATS");
    let mut pick = None;
    let mut del = None;
    for (i, t) in ai.st.threads.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let w = ui.available_width() - 44.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 38.0), Sense::click());
            let on = i == ai.st.cur;
            if on || resp.hovered() {
                ui.painter().rect_filled(rect, 8.0, if on { pal.accent_soft } else { pal.hover });
            }
            ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, ai_trunc(&t.title, 34), if on { bold(13.5) } else { body(13.5) }, pal.text);
            if resp.clicked() {
                pick = Some(i);
            }
            if icon_button(ui, pal, Icon::Trash, 13.0, "Delete chat").clicked() {
                del = Some(i);
            }
        });
    }
    if let Some(i) = pick {
        ai.st.cur = i;
        ai.rt.history = false;
        ai.rt.reveal_msg = usize::MAX;
    }
    if let Some(i) = del {
        if !ai.busy() {
            ai.st.threads.remove(i);
            ai.st.cur = 0;
        }
    }
    if ai.st.threads.is_empty() {
        ui.label(RichText::new("No chats yet.").font(body(13.0)).color(pal.muted));
    }
}

fn empty_state(ui: &mut Ui, pal: &Palette, ai: &AiHost, acts: &mut Vec<Act>) {
    ui.add_space(26.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(54.0, 54.0), Sense::hover());
        crate::widgets::radial_glow(ui.painter(), r.center(), 44.0, with_alpha(pal.accent, 0.25));
        crate::widgets::gradient_rect(ui, r.shrink(4.0), pal.accent, lerp_color(pal.accent, Color32::from_rgb(190, 90, 220), 0.7));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, l::SPARKLES, icons::font(26.0), Color32::WHITE);
        ui.add_space(8.0);
        ui.label(RichText::new("What should we build?").font(bold(20.0)).color(pal.text));
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new("I can create pages, databases, dashboards, canvases, flows and automations — and edit what you already have.").font(body(13.5)).color(pal.muted)).wrap());
    });
    ui.add_space(14.0);
    if let Err(e) = ai.ready() {
        card(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                glyph(ui, l::KEY_ROUND, 16.0, pal.accent);
                ui.add(egui::Label::new(RichText::new(e).font(body(13.5)).color(pal.text)).wrap());
            });
            ui.add_space(4.0);
            if primary_button(ui, pal, "Add API key", 140.0).clicked() {
                acts.push(Act::Settings);
            }
        });
        ui.add_space(10.0);
    }
    for (label, prompt) in ai::SUGGESTIONS {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(rect, 10.0, lerp_color(with_alpha(pal.muted, 0.06), pal.hover, t));
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)), egui::StrokeKind::Inside);
        ui.painter().text(pos2(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, l::SPARKLES, icons::font(14.0), pal.accent);
        ui.painter().text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, *label, body(14.0), pal.text);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            acts.push(Act::Send((*prompt).to_owned()));
        }
        ui.add_space(5.0);
    }
}

fn composer(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, acts: &mut Vec<Act>) {
    let touchy = crate::touch::touchy();
    let id = Id::new("ai_input");
    let busy = ai.busy();
    let enter = !touchy && !busy && ctx.memory(|m| m.has_focus(id)) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        let w = ui.available_width() - 10.0;
        let focused = ctx.memory(|m| m.has_focus(id));
        let t = ctx.animate_bool_with_time(id.with("f"), focused, 0.15);
        Frame::new()
            .fill(pal.bg)
            .stroke(Stroke::new(1.0_f32 + t * 0.7, lerp_color(pal.border.gamma_multiply(1.6), pal.accent, t)))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin { left: 12, right: 6, top: 8, bottom: 6 })
            .show(ui, |ui| {
                ui.set_width(w - 20.0);
                ui.horizontal_top(|ui| {
                    let tw = ui.available_width() - 44.0;
                    ScrollArea::vertical().id_salt("ai_input_scroll").max_height(if touchy { 110.0 } else { 140.0 }).auto_shrink([false, true]).show(ui, |ui| {
                        let hint_txt = if ai.thread_ref().map_or(true, |t| t.msgs.is_empty()) { "Ask or tell Lumen AI to do anything…" } else { "Reply…" };
                        let r = ui.add(
                            TextEdit::multiline(&mut ai.rt.input)
                                .id(id)
                                .frame(false)
                                .desired_rows(1)
                                .desired_width(tw)
                                .font(body(15.0))
                                .hint_text(RichText::new(hint_txt).color(with_alpha(pal.muted, 0.85))),
                        );
                        if ai.rt.focus_input {
                            r.request_focus();
                            ai.rt.focus_input = false;
                        }
                    });
                    ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                        let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
                        let has = !ai.rt.input.trim().is_empty();
                        let fill = if busy { pal.text } else if has { pal.accent } else { with_alpha(pal.muted, 0.3) };
                        ui.painter().circle_filled(rect.center(), 15.0, fill);
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, if busy { l::SQUARE } else { l::ARROW_UP }, icons::font(if busy { 12.0 } else { 17.0 }), if busy { pal.bg } else { Color32::WHITE });
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
        ui.add_space(14.0);
        ui.label(RichText::new("AI can make mistakes.").font(body(11.5)).color(with_alpha(pal.muted, 0.9)));
        if ai.rt.snapshot.is_some() && !busy {
            if ghost_button(ui, pal, "↩ Undo AI changes").clicked() {
                ai.rt.status = "undo".into();
            }
        }
    });
    ui.add_space(4.0);
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
    egui::TopBottomPanel::bottom(Id::new("ai_comp")).frame(Frame::NONE).show_separator_line(false).show_inside(ui, |ui| {
        if !ai.rt.history {
            composer(ui, ctx, pal, ai, &mut acts);
        }
    });
    egui::CentralPanel::default().frame(Frame::NONE).show_inside(ui, |ui| {
        if ai.rt.history {
            ScrollArea::vertical().id_salt("ai_hist").auto_shrink([false, false]).show(ui, |ui| history_view(ui, pal, ai));
            return;
        }
        ai.thread();
        let cur = ai.st.cur;
        let msgs = std::mem::take(&mut ai.st.threads[cur].msgs);
        ScrollArea::vertical().id_salt("ai_scroll").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            ui.add_space(4.0);
            Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if msgs.is_empty() {
                    empty_state(ui, pal, ai, &mut acts);
                }
                let n = msgs.len();
                for (i, m) in msgs.iter().enumerate() {
                    message(ui, ctx, pal, ws, &mut ai.rt, i, m, msgs.get(i + 1), i + 1 == n, &mut acts);
                }
                if ai.rt.phase == Phase::Approval {
                    ui.add_space(8.0);
                    card(pal).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Lumen AI wants to make changes").font(bold(13.5)).color(pal.text));
                        for c in &ai.rt.pending {
                            ui.add(egui::Label::new(RichText::new(format!("• {}", ai::summarize_call(c, Some(&ai::ToolRes { call_id: String::new(), name: String::new(), ok: true, text: String::new(), open: None })))).font(body(13.0)).color(pal.muted)).wrap());
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if primary_button(ui, pal, "Allow", 110.0).clicked() {
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
                    let label = if ai.rt.steps == 0 { "Thinking…".to_owned() } else { format!("Working… step {}", ai.rt.steps + 1) };
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
        ui.painter().rect_filled(rect, 0.0, pal.bg);
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

fn code_box(ui: &mut Ui, ctx: &Context, pal: &Palette, text: &str, toast: &mut Option<String>) {
    Frame::new().fill(pal.code_bg).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(9)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).font(FontId::monospace(11.5)).color(pal.text)).wrap());
    });
    if ghost_button(ui, pal, "Copy").clicked() {
        ctx.copy_text(text.to_owned());
        *toast = Some("Copied".into());
    }
}

fn providers_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, ws: &Workspace) {
    if ai.rt.sel_provider.is_empty() {
        ai.rt.sel_provider = ai.st.active.clone();
    }
    let configured = ai.st.configured();
    ui.horizontal_wrapped(|ui| {
        for p in PROVIDERS {
            let label = if configured.contains(&p.id) { format!("● {}", p.name) } else { p.name.to_owned() };
            if chip(ui, pal, &label, ai.rt.sel_provider == p.id).clicked() {
                ai.rt.sel_provider = p.id.to_owned();
                ai.rt.test_msg = None;
            }
        }
    });
    ui.add_space(10.0);
    let pid = ai.rt.sel_provider.clone();
    let def = ai::provider(&pid);
    ui.add(egui::Label::new(RichText::new(def.blurb).font(body(13.0)).color(pal.muted)).wrap());
    ui.add_space(8.0);
    let w = ui.available_width();
    if def.needs_key || pid == "custom" {
        ui.label(RichText::new(if def.needs_key { "API key" } else { "API key (optional)" }).font(bold(12.5)).color(pal.muted));
        let mut key = ai.st.cfg(&pid).key.clone();
        if field(ui, pal, pal.bg, &mut key, "Paste your key", true, w, Id::new(("ai_key", &pid))).changed() {
            ai.st.cfg_mut(&pid).key = key.trim().to_owned();
        }
        if !def.key_url.is_empty() {
            if ghost_button(ui, pal, &format!("Get a key ↗  {}", def.key_url.trim_start_matches("https://"))).clicked() {
                crate::platform::open_url(ctx, def.key_url);
            }
        }
        ui.add_space(6.0);
    }
    ui.label(RichText::new("Model").font(bold(12.5)).color(pal.muted));
    let mut model = ai.st.cfg(&pid).model.clone();
    let hint = def.models.first().copied().unwrap_or("model name");
    if field(ui, pal, pal.bg, &mut model, hint, false, w, Id::new(("ai_model", &pid))).changed() {
        ai.st.cfg_mut(&pid).model = model.trim().to_owned();
    }
    if !def.models.is_empty() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let cur = ai.st.model_of(&pid);
            for m in def.models {
                if chip(ui, pal, m, cur == *m).clicked() {
                    ai.st.cfg_mut(&pid).model = (*m).to_owned();
                }
            }
        });
    }
    ui.add_space(6.0);
    ui.label(RichText::new(if pid == "custom" { "Server URL (…/v1)" } else { "Server URL (advanced — leave empty)" }).font(bold(12.5)).color(pal.muted));
    let mut base = ai.st.cfg(&pid).base.clone();
    if field(ui, pal, pal.bg, &mut base, if def.base.is_empty() { "https://your-server/v1" } else { def.base }, false, w, Id::new(("ai_base", &pid))).changed() {
        ai.st.cfg_mut(&pid).base = base.trim().to_owned();
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let active = ai.st.active == pid;
        if primary_button(ui, pal, if active { "✓ In use" } else { "Use this provider" }, 150.0).clicked() {
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
        ui.add(egui::Label::new(RichText::new(msg).font(body(13.0)).color(if *ok { Color32::from_rgb(70, 170, 110) } else { pal.danger })).wrap());
    }
    ui.add_space(10.0);
    ui.add(egui::Label::new(RichText::new(if cfg!(target_arch = "wasm32") { "Keys stay in this browser's storage. Requests to cloud providers are relayed by this website's proxy (never logged or stored). Chat history is stored on this device only." } else { "Keys are stored on this device only and sent straight to the provider." }).font(body(12.0)).color(with_alpha(pal.muted, 0.9))).wrap());
}

fn agent_tab(ui: &mut Ui, pal: &Palette, ai: &mut AiHost) {
    section_label(ui, pal, "WHAT MAY LUMEN AI DO?");
    for a in [Access::ReadOnly, Access::Edit, Access::Full] {
        let on = ai.st.access == a;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(rect, 10.0, if on { pal.accent_soft } else { lerp_color(with_alpha(pal.muted, 0.06), pal.hover, t) });
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(if on { 1.4_f32 } else { 1.0 }, if on { pal.accent } else { with_alpha(pal.border, 0.9) }), egui::StrokeKind::Inside);
        ui.painter().circle_stroke(pos2(rect.left() + 20.0, rect.center().y), 8.0, Stroke::new(1.5_f32, if on { pal.accent } else { pal.muted }));
        if on {
            ui.painter().circle_filled(pos2(rect.left() + 20.0, rect.center().y), 4.5, pal.accent);
        }
        ui.painter().text(pos2(rect.left() + 40.0, rect.top() + 18.0), Align2::LEFT_CENTER, a.label(), bold(14.0), pal.text);
        let galley = ui.painter().layout(a.hint().to_owned(), body(12.0), pal.muted, rect.width() - 52.0);
        ui.painter().galley(pos2(rect.left() + 40.0, rect.top() + 29.0), galley, pal.muted);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            ai.st.access = a;
        }
        ui.add_space(5.0);
    }
    ui.add_space(8.0);
    toggle_row(ui, pal, "Ask before making changes", "Every create / edit waits for your Allow.", &mut ai.st.confirm);
    ui.add_space(10.0);
    ui.label(RichText::new(format!("Max steps per request: {}", ai.st.max_steps)).font(bold(13.0)).color(pal.text));
    ui.add(egui::Slider::new(&mut ai.st.max_steps, 4..=48).show_value(false));
    ui.add_space(8.0);
    ui.label(RichText::new("Standing instructions").font(bold(13.0)).color(pal.text));
    ui.add(egui::Label::new(RichText::new("Always applied — tone, language, naming conventions, who you are.").font(body(12.0)).color(pal.muted)).wrap());
    Frame::new().fill(pal.bg).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(9)).inner_margin(Margin::same(9)).show(ui, |ui| {
        ui.add(TextEdit::multiline(&mut ai.st.instructions).desired_rows(4).desired_width(ui.available_width()).frame(false).font(body(14.0)).hint_text("e.g. I run a design studio. Reply in German. Use emoji icons."));
    });
    ui.add_space(12.0);
    if ghost_button(ui, pal, "Delete all chats").clicked() && !ai.busy() {
        ai.st.threads.clear();
        ai.st.cur = 0;
    }
}

fn connect_tab(ui: &mut Ui, ctx: &Context, pal: &Palette, ai: &mut AiHost, toast: &mut Option<String>) {
    ui.add(egui::Label::new(RichText::new("Let other AI agents (Claude, ChatGPT connectors, Cursor, your own scripts …) use this workspace through the Model Context Protocol. They get the same tools Lumen AI has.").font(body(13.0)).color(pal.muted)).wrap());
    ui.add_space(8.0);
    section_label(ui, pal, "WHAT MAY EXTERNAL AGENTS DO?");
    ui.horizontal_wrapped(|ui| {
        for a in [Access::ReadOnly, Access::Edit, Access::Full] {
            if chip(ui, pal, a.label(), ai.st.mcp.access == a).clicked() {
                ai.st.mcp.access = a;
            }
        }
    });
    ui.add(egui::Label::new(RichText::new(ai.st.mcp.access.hint()).font(body(12.0)).color(pal.muted)).wrap());
    ui.add_space(10.0);
    let w = ui.available_width();
    let origin = {
        let o = crate::platform::site_origin();
        if o.is_empty() { ai.st.mcp.bridge_url.trim().trim_end_matches('/').to_owned() } else { o }
    };
    let site = if origin.is_empty() { "https://YOUR-LUMEN-SITE".to_owned() } else { origin };
    section_label(ui, pal, "CLOUD BRIDGE — WORKS FROM ANYWHERE");
    toggle_row(ui, pal, "Connect to the Lumen site", "Agents call your site's /api/mcp; the site relays to this app while it is open. Works on web, Android and Windows.", &mut ai.st.mcp.bridge_on);
    if ai.st.mcp.bridge_on {
        ui.add_space(4.0);
        if !cfg!(target_arch = "wasm32") {
            ui.label(RichText::new("Site URL").font(bold(12.5)).color(pal.muted));
            let mut u = ai.st.mcp.bridge_url.clone();
            if field(ui, pal, pal.bg, &mut u, "https://your-lumen-site.example", false, w, Id::new("mcp_url")).changed() {
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
        section_label(ui, pal, "LOCAL SERVER — THIS DEVICE ONLY");
        toggle_row(ui, pal, "Run a local MCP server", "http://127.0.0.1 only. No internet needed.", &mut ai.st.mcp.local_on);
        if ai.st.mcp.local_on {
            let mut port = ai.st.mcp.port.to_string();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Port").font(bold(12.5)).color(pal.muted));
                if field(ui, pal, pal.bg, &mut port, "7878", false, 100.0, Id::new("mcp_port")).changed() {
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
    section_label(ui, pal, "RECENT AGENT ACTIVITY");
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

pub fn settings(ctx: &Context, ai: &mut AiHost, ws: &Workspace, pal: &Palette) {
    let Some(tab) = ai.rt.settings else { return };
    let mut tab = tab;
    let mut toast = None;
    let max_h = (ctx.screen_rect().height() - 150.0).max(240.0);
    let open = crate::app::modal(ctx, Id::new("ai_settings"), pal, 600.0, 36.0, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            spark_avatar(ui, pal);
            ui.label(RichText::new("AI settings").font(bold(19.0)));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            for (t, name) in [(SettingsTab::Providers, "Providers & keys"), (SettingsTab::Agent, "Agent"), (SettingsTab::Connect, "Connect agents (MCP)")] {
                if chip(ui, pal, name, tab == t).clicked() {
                    tab = t;
                }
            }
        });
        ui.add_space(8.0);
        ScrollArea::vertical().id_salt("ai_settings_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            Frame::new().inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                match tab {
                    SettingsTab::Providers => providers_tab(ui, ctx, pal, ai, ws),
                    SettingsTab::Agent => agent_tab(ui, pal, ai),
                    SettingsTab::Connect => connect_tab(ui, ctx, pal, ai, &mut toast),
                }
                ui.add_space(10.0);
            });
        });
    });
    if toast.is_some() {
        ai.rt.toast = toast;
    }
    ai.rt.settings = if open { Some(tab) } else { None };
    if !open {
        crate::widgets::reset_appear(ctx, Id::new("ai_settings").with("a"));
    }
    let _ = (Rect::NOTHING, vec2(0.0, 0.0));
}
