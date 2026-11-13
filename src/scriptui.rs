//! UI of LumenScript: the code editor (syntax highlighting, line numbers, live error), the Scripts tab (library, test / run, API reference,
//! "create rule", copy-for-AI), the inline editor used by the "Run script" action, and the Roadmap tab (what is done, what is deferred).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, RichText, Sense, Stroke, TextEdit, Ui};

use crate::automation::{self as au, Action, Output, Rule, Runtime, Script, Trigger};
use crate::autoui::{self, caps, card, combo, databases, icon_btn, label, pill, Out};
use crate::lucide as l;
use crate::model::Workspace;
use crate::script::{self, Hl, ScriptError};
use crate::scripthost::{self, ScriptCtx};
use crate::theme::{bold, body, Palette};
use crate::widgets::{self, with_alpha};

// ───────────────────────────── state ─────────────────────────────

/// (kind, text): 0 = output, 1 = effect, 2 = error, 3 = info
type Line = (u8, String);

#[derive(Default)]
struct St {
    open: Option<u64>,
    console: HashMap<u64, Vec<Line>>,
    target: HashMap<u64, u64>,
    docs: bool,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

fn st<R>(f: impl FnOnce(&mut St) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// Opens a script of the library in the Scripts tab.
pub(crate) fn open_script(id: u64) {
    st(|s| s.open = Some(id));
}

fn is_dark(pal: &Palette) -> bool {
    (pal.bg.r() as u32 + pal.bg.g() as u32 + pal.bg.b() as u32) < 3 * 110
}

fn hl_color(pal: &Palette, k: Hl) -> Color32 {
    let dark = is_dark(pal);
    let c = |l: (u8, u8, u8), d: (u8, u8, u8)| {
        let (r, g, b) = if dark { d } else { l };
        Color32::from_rgb(r, g, b)
    };
    match k {
        Hl::Plain => pal.text,
        Hl::Keyword => c((171, 52, 150), (214, 140, 232)),
        Hl::Literal => c((196, 96, 0), (240, 164, 90)),
        Hl::Number => c((28, 122, 180), (110, 190, 240)),
        Hl::Str => c((30, 140, 80), (130, 214, 150)),
        Hl::Comment => pal.muted,
        Hl::Func => c((35, 100, 200), (125, 170, 250)),
        Hl::Punct => c((120, 120, 120), (160, 160, 160)),
    }
}

fn hash_of(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Syntax check cached per text (parsing every frame would be wasteful).
fn cached_check(ctx: &egui::Context, id: Id, code: &str) -> Option<ScriptError> {
    let h = hash_of(code);
    if let Some((hh, r)) = ctx.data(|d| d.get_temp::<(u64, Option<ScriptError>)>(id)) {
        if hh == h {
            return r;
        }
    }
    let r = script::check(code).err();
    ctx.data_mut(|d| d.insert_temp(id, (h, r.clone())));
    r
}

// ───────────────────────────── code editor ─────────────────────────────

/// Multi-line code editor with syntax highlighting, line numbers and an optional error-line marker. Returns `true` when the text changed.
pub fn code_editor(ui: &mut Ui, pal: &Palette, salt: impl Hash, code: &mut String, rows: usize, err_line: Option<u32>) -> bool {
    let id = Id::new(salt);
    let font = FontId::monospace(if crate::touch::touchy() { 14.0 } else { 13.0 });
    let pal2 = pal.clone();
    let f2 = font.clone();
    let mut layouter = move |ui: &Ui, text: &str, wrap_width: f32| -> Arc<egui::Galley> {
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap_width;
        for (range, kind) in script::highlight(text) {
            job.append(&text[range], 0.0, TextFormat { font_id: f2.clone(), color: hl_color(&pal2, kind), ..Default::default() });
        }
        ui.fonts(|f| f.layout_job(job))
    };
    let mut changed = false;
    Frame::new().fill(pal.code_bg).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let out = TextEdit::multiline(code)
            .id(id)
            .font(font.clone())
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(rows)
            .lock_focus(true)
            .frame(false)
            .margin(Margin { left: 44, right: 8, top: 8, bottom: 8 })
            .layouter(&mut layouter)
            .show(ui);
        changed = out.response.changed();
        // line numbers + error marker, painted over the (transparent) margin
        let painter = ui.painter();
        let gx = out.galley_pos.x - 6.0;
        let mut line = 1u32;
        let mut new_line = true;
        for row in out.galley.rows.iter() {
            let y = out.galley_pos.y + row.rect.min.y;
            if new_line {
                let bad = err_line == Some(line);
                if bad {
                    painter.rect_filled(Rect::from_min_max(pos2(out.response.rect.left(), y), pos2(out.response.rect.right(), y + row.rect.height())), 0.0, with_alpha(pal.danger, 0.13));
                }
                painter.text(pos2(gx, y + row.rect.height() / 2.0), egui::Align2::RIGHT_CENTER, line.to_string(), FontId::monospace(11.5), if bad { pal.danger } else { pal.muted });
            }
            new_line = row.ends_with_newline;
            if new_line {
                line += 1;
            }
        }
        painter.line_segment([pos2(gx + 6.0 - 1.0, out.response.rect.top() + 4.0), pos2(gx + 6.0 - 1.0, out.response.rect.bottom() - 4.0)], Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
    });
    changed
}

fn status_line(ui: &mut Ui, pal: &Palette, code: &str, err: &Option<ScriptError>) {
    ui.horizontal_wrapped(|ui| match err {
        Some(e) => {
            ui.label(RichText::new(format!("⚠ {e}")).font(body(12.5)).color(pal.danger));
        }
        None => {
            let n = code.lines().count();
            ui.label(RichText::new(format!("✓ Syntax OK · {n} line{}", if n == 1 { "" } else { "s" })).font(body(12.5)).color(pal.muted));
        }
    });
}

fn console_view(ui: &mut Ui, pal: &Palette, lines: &[Line]) {
    Frame::new().fill(pal.code_bg).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("script_console").max_height(190.0).auto_shrink([false, true]).show(ui, |ui| {
            for (k, t) in lines {
                let (prefix, col) = match k {
                    1 => ("→ ", pal.accent),
                    2 => ("✖ ", pal.danger),
                    3 => ("", pal.muted),
                    _ => ("", pal.text),
                };
                ui.add(egui::Label::new(RichText::new(format!("{prefix}{t}")).font(FontId::monospace(12.5)).color(col)).wrap());
            }
        });
    });
}

// ───────────────────────────── running ─────────────────────────────

fn report_lines(rep: &scripthost::Report, dry: bool) -> Vec<Line> {
    let mut v: Vec<Line> = rep.output.iter().map(|o| (0u8, o.clone())).collect();
    for e in &rep.effects {
        v.push((1, if dry { format!("would {e}") } else { e.clone() }));
    }
    match &rep.error {
        Some(e) => v.push((2, e.to_string())),
        None => v.push((3, format!("finished · {} steps · {} change(s){}", rep.steps, rep.effects.len(), if dry { " (test run — nothing was saved)" } else { "" }))),
    }
    v
}

/// Test run on a copy of the workspace.
fn test_run(ws: &Workspace, code: &str, db: u64, row: Option<u64>) -> Vec<Line> {
    let ctx = ScriptCtx { row, page: row, db, source: "Script test".into(), kind: au::NoticeKind::System, now: crate::auth::now_ms() };
    report_lines(&scripthost::dry_run(ws, code, &ctx), true)
}

fn first_row(ws: &Workspace, db: u64) -> Option<u64> {
    if db == 0 { None } else { ws.rows(db).first().copied() }
}

/// Runs for real (once, or once per row of `target`). Returns console lines.
fn live_run(ws: &mut Workspace, rt: &mut Runtime, name: &str, code: &str, target: u64) -> Vec<Line> {
    let now = crate::auth::now_ms();
    let mut lines = vec![];
    let mut out = Output::default();
    let mut follow = vec![];
    let rows: Vec<Option<u64>> = if target == 0 { vec![None] } else { ws.rows(target).into_iter().take(500).map(Some).collect() };
    let n = rows.len();
    let (mut changes, mut errors) = (0, 0);
    for row in rows {
        let ctx = ScriptCtx { row, page: row, db: target, source: name.to_owned(), kind: au::NoticeKind::Button, now };
        let rep = scripthost::run_script(ws, code, &ctx, &mut out, &mut follow);
        changes += rep.effects.len();
        let title = row.map(|r| ws.display_title(r));
        let ls = report_lines(&rep, false);
        for (k, t) in ls {
            if n > 1 && k == 3 {
                continue;
            }
            lines.push((k, match &title {
                Some(tt) if n > 1 => format!("[{tt}] {t}"),
                _ => t,
            }));
        }
        if rep.error.is_some() {
            errors += 1;
            break; // stop at the first failing row: the same error would repeat
        }
        if lines.len() > 300 {
            lines.push((3, "… output truncated".into()));
            break;
        }
    }
    au::log(ws, name, &format!("script ran on {n} target(s): {changes} change(s){}", if errors > 0 { ", stopped by an error" } else { "" }), errors == 0, now);
    if n > 1 {
        lines.push((3, format!("finished · {n} row(s) · {changes} change(s)")));
    }
    rt.dirty = true;
    lines
}

// ───────────────────────────── inline editor (Run script action) ─────────────────────────────

/// Editor used inside a rule / button action list. `ws` is only read (the Test button works on a copy).
pub fn inline_editor(ui: &mut Ui, pal: &Palette, ws: &Workspace, db: u64, code: &mut String, salt: impl Hash + Copy) -> bool {
    let ctx = ui.ctx().clone();
    let err = cached_check(&ctx, Id::new((salt, "chk")), code);
    let mut ch = code_editor(ui, pal, (salt, "code"), code, 8, err.as_ref().map(|e| e.line));
    status_line(ui, pal, code, &err);
    let con_id = Id::new((salt, "console"));
    ui.horizontal_wrapped(|ui| {
        if widgets::ghost_button(ui, pal, "▶ Test (changes nothing)").clicked() {
            let lines = if let Some(e) = &err { vec![(2u8, e.to_string())] } else { test_run(ws, code, db, first_row(ws, db)) };
            ctx.data_mut(|d| d.insert_temp(con_id, lines));
        }
        let items: Vec<(String, usize)> = scripthost::EXAMPLES.iter().enumerate().map(|(i, e)| (e.name.to_owned(), i)).collect();
        egui::ComboBox::from_id_salt(Id::new((salt, "ex"))).selected_text(RichText::new("Insert example").font(body(13.0))).width((150.0_f32).min((ui.available_width() - 12.0).max(90.0))).show_ui(ui, |ui| {
            for (n, i) in &items {
                if ui.selectable_label(false, RichText::new(n).font(body(13.5))).on_hover_text(scripthost::EXAMPLES[*i].desc).clicked() {
                    *code = scripthost::EXAMPLES[*i].code.to_owned();
                    ch = true;
                }
            }
        });
    });
    label(ui, pal, "Uses `row`, `db(\"Name\")`, `notify()` … — full reference in Automations → Scripts.");
    if let Some(lines) = ctx.data(|d| d.get_temp::<Vec<Line>>(con_id)) {
        ui.add_space(4.0);
        console_view(ui, pal, &lines);
    }
    ch
}

// ───────────────────────────── Scripts tab ─────────────────────────────

pub fn scripts_tab(ui: &mut Ui, ws: &mut Workspace, rt: &mut Runtime, pal: &Palette, out: &mut Out) {
    let dbs = databases(ws);
    card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            ui.painter().rect_filled(r, 10.0, with_alpha(pal.accent, 0.14));
            crate::icons::glyph(ui.painter(), r.center(), 18.0, l::CODE_XML, pal.accent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("Scripts").font(bold(15.5)));
                ui.label(RichText::new("LumenScript — automate with code: loops, conditions, several databases").font(body(12.5)).color(pal.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::primary_button(ui, pal, "+ New script", 118.0).clicked() {
                    let id = ws.id();
                    ws.auto.scripts.push(Script { id, name: "New script".into(), code: scripthost::EXAMPLES[1].code.to_owned(), runs: 0, last_run: 0.0, every: 0 });
                    st(|s| s.open = Some(id));
                }
            });
        });
        ui.add_space(6.0);
        label(ui, pal, "Scripts run in a sandbox: no files, no network, an instruction budget and hard limits on rows and notifications. Use ▶ Test to see what a script would do before it touches your data. Use a script inside any rule or button with the “Run script” action.");
    });
    ui.add_space(10.0);

    if ws.auto.scripts.is_empty() {
        card(ui, pal, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("No scripts yet").font(bold(15.0)));
                label(ui, pal, "Start from an example, or write your own.");
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for ex in scripthost::EXAMPLES.iter() {
                        if widgets::ghost_button(ui, pal, ex.name).on_hover_text(ex.desc).clicked() {
                            let id = ws.id();
                            ws.auto.scripts.push(Script { id, name: ex.name.into(), code: ex.code.into(), runs: 0, last_run: 0.0, every: 0 });
                            st(|s| s.open = Some(id));
                        }
                    }
                });
                ui.add_space(8.0);
            });
        });
        ui.add_space(8.0);
    }

    let ids: Vec<u64> = ws.auto.scripts.iter().map(|s| s.id).collect();
    let mut delete: Option<u64> = None;
    let mut to_rule: Option<u64> = None;
    for id in ids {
        let Some(idx) = ws.auto.scripts.iter().position(|s| s.id == id) else { continue };
        let mut sc = ws.auto.scripts[idx].clone();
        let before = sc.clone();
        let open = st(|s| s.open) == Some(id);
        let mut toggle = false;
        let mut run_kind = 0u8; // 1 = test, 2 = run
        card(ui, pal, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if open {
                        ui.add(TextEdit::singleline(&mut sc.name).desired_width((220.0_f32).min((ui.available_width() - 12.0).max(90.0))).font(bold(15.0)).margin(Margin::symmetric(6, 4)));
                    } else if ui.add(egui::Label::new(RichText::new(&sc.name).font(bold(15.0))).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        toggle = true;
                    }
                    let lines = sc.code.lines().count();
                    let last = if sc.last_run > 0.0 { format!("last run {} ago", crate::app::ago(sc.last_run)) } else { "never run".into() };
                    label(ui, pal, &format!("{lines} line(s) · {} run(s) · {last}", sc.runs));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_btn(ui, pal, l::TRASH_2, "Delete script", true).clicked() {
                        delete = Some(id);
                    }
                    if icon_btn(ui, pal, if open { l::CHEVRONS_UP_DOWN } else { l::SQUARE_PEN }, if open { "Collapse" } else { "Edit script" }, false).clicked() {
                        toggle = true;
                    }
                });
            });
            if open {
                ui.add_space(6.0);
                let ctx = ui.ctx().clone();
                let err = cached_check(&ctx, Id::new(("script_chk", id)), &sc.code);
                code_editor(ui, pal, ("script_code", id), &mut sc.code, 14, err.as_ref().map(|e| e.line));
                status_line(ui, pal, &sc.code, &err);
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    label(ui, pal, "Run on");
                    let mut items: Vec<(String, u64)> = vec![("once (no row)".to_owned(), 0)];
                    items.extend(dbs.iter().cloned());
                    let cur = st(|s| s.target.get(&id).copied()).unwrap_or(0);
                    let cur = if items.iter().any(|i| i.1 == cur) { cur } else { 0 };
                    let shown: Vec<(String, u64)> = items.iter().map(|(n, i)| (if *i == 0 { n.clone() } else { format!("every row of {n}") }, *i)).collect();
                    if let Some(n) = combo(ui, ("script_target", id), 210.0, &shown, &cur) {
                        st(|s| s.target.insert(id, n));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    label(ui, pal, "Repeat every");
                    ui.add(egui::DragValue::new(&mut sc.every).range(0..=86_400).suffix(" s"));
                    ui.label(RichText::new(if sc.every == 0 { "off — runs only when you press Run" } else { "● repeats in the background, on every page, while the app is open" }).font(body(12.0)).color(if sc.every == 0 { pal.muted } else { pal.accent }));
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(err.is_none(), |ui| {
                        if widgets::ghost_button(ui, pal, "▶ Test (changes nothing)").clicked() {
                            run_kind = 1;
                        }
                        if widgets::primary_button(ui, pal, "Run", 70.0).clicked() {
                            run_kind = 2;
                        }
                        if widgets::ghost_button(ui, pal, "+ Make a rule").on_hover_text("Creates a rule that runs this script — choose its trigger afterwards").clicked() {
                            to_rule = Some(id);
                        }
                    });
                    if widgets::ghost_button(ui, pal, "Copy").clicked() {
                        ui.ctx().copy_text(sc.code.clone());
                        out.toast = Some("Script copied".into());
                    }
                });
                let lines = st(|s| s.console.get(&id).cloned()).unwrap_or_default();
                if !lines.is_empty() {
                    ui.add_space(6.0);
                    caps(ui, pal, "Console");
                    console_view(ui, pal, &lines);
                }
            } else {
                let first: String = sc.code.lines().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//")).unwrap_or("").chars().take(80).collect();
                if !first.is_empty() {
                    ui.add_space(2.0);
                    ui.label(RichText::new(first).font(FontId::monospace(12.0)).color(pal.muted));
                }
            }
        });
        ui.add_space(8.0);
        if toggle {
            st(|s| s.open = if open { None } else { Some(id) });
        }
        match run_kind {
            1 => {
                let target = st(|s| s.target.get(&id).copied()).unwrap_or(0);
                let lines = test_run(ws, &sc.code, target, first_row(ws, target));
                st(|s| s.console.insert(id, lines));
            }
            2 => {
                let target = st(|s| s.target.get(&id).copied()).unwrap_or(0);
                let lines = live_run(ws, rt, &sc.name, &sc.code, target);
                let failed = lines.iter().any(|(k, _)| *k == 2);
                st(|s| s.console.insert(id, lines));
                sc.runs += 1;
                sc.last_run = crate::auth::now_ms();
                out.toast = Some(if failed { "Script stopped with an error — see the console".into() } else { "Script finished".into() });
            }
            _ => {}
        }
        if sc != before {
            if let Some(slot) = ws.auto.scripts.get_mut(idx) {
                *slot = sc;
            }
            rt.dirty = true;
        }
    }
    if let Some(id) = delete {
        ws.auto.scripts.retain(|s| s.id != id);
        out.toast = Some("Script deleted".into());
    }
    if let Some(id) = to_rule {
        if let Some(sc) = ws.auto.scripts.iter().find(|s| s.id == id).cloned() {
            let target = st(|s| s.target.get(&id).copied()).unwrap_or(0);
            let rid = ws.id();
            let trig = if target != 0 { Trigger::RowAdded } else { Trigger::Manual };
            ws.auto.rules.push(Rule::new(rid, &format!("Script: {}", sc.name), target, trig, vec![Action::Script { code: sc.code }]));
            autoui::focus_rule(rid);
            rt.dirty = true;
            out.toast = Some("Rule created — pick its trigger below".into());
        }
    }

    // examples
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Examples").font(bold(15.0)));
        label(ui, pal, "click to add to your library");
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for ex in scripthost::EXAMPLES.iter() {
            if widgets::ghost_button(ui, pal, &format!("+ {}", ex.name)).on_hover_text(ex.desc).clicked() {
                let id = ws.id();
                ws.auto.scripts.push(Script { id, name: ex.name.into(), code: ex.code.into(), runs: 0, last_run: 0.0, every: 0 });
                st(|s| s.open = Some(id));
            }
        }
    });

    // reference
    ui.add_space(14.0);
    let docs = st(|s| s.docs);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Reference").font(bold(15.0)));
        if widgets::ghost_button(ui, pal, if docs { "Hide" } else { "Show" }).clicked() {
            st(|s| s.docs = !docs);
        }
        if widgets::ghost_button(ui, pal, "Copy API for an AI assistant").on_hover_text("A compact prompt + the whole API: paste it into any AI chat and ask it to write a script").clicked() {
            ui.ctx().copy_text(scripthost::api_markdown());
            out.toast = Some("API reference copied — paste it into your AI assistant".into());
        }
    });
    if docs {
        ui.add_space(6.0);
        for d in scripthost::API {
            card(ui, pal, |ui| {
                ui.label(RichText::new(d.title).font(bold(14.0)));
                ui.add_space(4.0);
                for (sig, desc) in d.items {
                    ui.add(egui::Label::new(RichText::new(*sig).font(FontId::monospace(12.0)).color(pal.accent)).wrap());
                    ui.add(egui::Label::new(RichText::new(*desc).font(body(12.5)).color(pal.muted)).wrap());
                    ui.add_space(5.0);
                }
            });
            ui.add_space(6.0);
        }
    }
}

// ───────────────────────────── Roadmap tab ─────────────────────────────

pub fn roadmap_tab(ui: &mut Ui, pal: &Palette) {
    const DONE: &[(&str, &str)] = &[
        ("LumenScript", "Sandboxed scripting language: functions, closures, lists/maps, try/catch, string interpolation, 70+ built-ins."),
        ("Script editor", "Syntax highlighting, line numbers, live error markers, examples, Test-on-a-copy, library, “Make a rule”."),
        ("“Run script” action", "Use a script as an action of any rule or button block, next to the point-and-click actions."),
        ("AI-ready API", "“Copy API for an AI assistant” hands any AI the exact script reference. The assistant itself is not built yet (below)."),
        ("Pop-up drop-downs", "Drop-downs, menus and pickers now always open above the dialog that created them (modals, popovers, side panels)."),
        ("Recipes grid", "Recipes are aligned in equal-height tiles, one or two columns depending on the width; works on phones."),
        ("Windows + WebAssembly", "Both builds ship from the same Rust code; the web app is installable (PWA) and works offline."),
    ];
    const NEXT: &[(&str, &str, &str)] = &[
        ("AI assistant", "“Describe it, get a script”. The script API and sandbox are ready; it needs an AI provider key and network access.", "needs a key"),
        ("Webhook & HTTP triggers", "Start scripts from outside and call web services. Needs a server component and a network permission model.", "needs a server"),
        ("Server-side scheduler", "Run schedules and reminders while every device is closed.", "needs a server"),
        ("Sync & real-time collaboration", "Multi-user editing, presence, comments. The engine already runs on diffs, so it can plug in.", "needs a server"),
        ("Script autocomplete & debugger", "Highlighting and live error markers ship now; completion lists, breakpoints and stepping come later.", "planned"),
        ("Script libraries", "import shared helpers, community script gallery, per-workspace secrets.", "planned"),
        ("Relation setters in scripts", "Reading relations works; assigning them from a script is the next addition.", "planned"),
        ("macOS / Linux / mobile store apps", "Windows and the installable web app are delivered; other native packages need their own build machines.", "planned"),
    ];
    autoui::card(ui, pal, |ui| {
        ui.label(RichText::new("Shipped in this release").font(bold(15.0)));
        ui.add_space(6.0);
        for (t, d) in DONE {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("✓").font(bold(13.5)).color(pal.accent));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(*t).font(bold(13.5)));
                    ui.add(egui::Label::new(RichText::new(*d).font(body(12.5)).color(pal.muted)).wrap());
                });
            });
            ui.add_space(6.0);
        }
    });
    ui.add_space(10.0);
    autoui::card(ui, pal, |ui| {
        ui.label(RichText::new("Not in this release — kept for later").font(bold(15.0)));
        label(ui, pal, "Nothing here is half-built: each item needs infrastructure this offline app does not have yet.");
        ui.add_space(8.0);
        for (t, d, tag) in NEXT {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("○").font(bold(13.5)).color(pal.muted));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(*t).font(bold(13.5)));
                        pill(ui, pal, tag, pal.muted, pal.hover);
                    });
                    ui.add(egui::Label::new(RichText::new(*d).font(body(12.5)).color(pal.muted)).wrap());
                });
            });
            ui.add_space(6.0);
        }
    });
}

#[allow(dead_code)]
fn unused(_: Pos2) {}
