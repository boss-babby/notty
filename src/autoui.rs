//! Automation UI: the Automations workspace (rules, recipes, reminders, inbox, activity), one-click button blocks with their action
//! editor, and the "repeat & remind" popover of date properties. The engine itself lives in `automation.rs`.

use std::cell::RefCell;

use egui::{pos2, vec2, Align2, Color32, ComboBox, CornerRadius, CursorIcon, Frame, Id, Margin, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::automation::{self as au, Action, ChangeTo, End, Freq, Notice, NoticeKind, Reminder, Repeat, Rule, Runtime, Setter, Trigger};
use crate::dates;
use crate::editor::EditorState;
use crate::icons;
use crate::lucide as l;
use crate::model::{Block, Database, Filter, FilterOp, PageKind, PropKind, Val, Workspace};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{self, with_alpha};

// ───────────────────────────── view state ─────────────────────────────

#[derive(Default)]
struct NewRem {
    msg: String,
    day: i64,
    minute: u16,
    lead: u32,
    rep: u8,
}

#[derive(Default)]
struct St {
    tab: u8,
    open_rule: Option<u64>,
    recipe_db: u64,
    rem: NewRem,
    row_pop: Option<(u64, u64, Pos2)>,
    row_pop_fresh: bool,
    msg: Option<String>,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

fn st<R>(f: impl FnOnce(&mut St) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

pub const TAB_RULES: u8 = 0;
pub const TAB_REMINDERS: u8 = 1;
pub const TAB_INBOX: u8 = 2;
pub const TAB_ACTIVITY: u8 = 3;
pub const TAB_SCRIPTS: u8 = 4;
pub const TAB_ROADMAP: u8 = 5;

/// Jumps to the Rules tab with this rule expanded.
pub(crate) fn focus_rule(id: u64) {
    st(|s| {
        s.tab = TAB_RULES;
        s.open_rule = Some(id);
    });
}

pub fn open_tab(tab: u8) {
    st(|s| s.tab = tab);
}

/// What the Automations page asks the app to do.
#[derive(Default)]
pub struct Out {
    pub close: bool,
    pub open: Option<u64>,
    pub toast: Option<String>,
}

// ───────────────────────────── small widgets ─────────────────────────────

pub(crate) fn roomy() -> bool {
    crate::touch::touchy()
}

/// Width of the close button of the page header.
fn roomy_w() -> f32 {
    if roomy() { 38.0 } else { 30.0 }
}

pub(crate) fn card<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(pal.bg)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .shadow(egui::epaint::Shadow { offset: [0, 1], blur: 6, spread: 0, color: Color32::from_black_alpha(10) })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub(crate) fn label(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).font(body(13.0)).color(pal.muted));
}

pub(crate) fn caps(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text.to_uppercase()).font(bold(11.0)).color(pal.muted));
    ui.add_space(2.0);
}

/// Square icon button; returns the response.
pub(crate) fn icon_btn(ui: &mut Ui, pal: &Palette, glyph: &str, tip: &str, danger: bool) -> egui::Response {
    let s = if roomy() { 38.0 } else { 30.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(s, s), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(r, 8.0, with_alpha(pal.hover, t));
    let col = lerp_color(pal.muted, if danger { pal.danger } else { pal.text }, t);
    icons::glyph(ui.painter(), r.center(), 15.0, glyph, col);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

/// iOS-style switch.
pub(crate) fn switch(ui: &mut Ui, pal: &Palette, on: &mut bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
    ui.painter().rect_filled(r, 11.0, lerp_color(pal.active, pal.accent, t));
    let x = r.left() + 11.0 + t * 16.0;
    ui.painter().circle_filled(pos2(x, r.center().y + 0.5), 9.0, Color32::from_black_alpha(30));
    ui.painter().circle_filled(pos2(x, r.center().y), 9.0, Color32::WHITE);
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        *on = !*on;
        return true;
    }
    false
}

pub(crate) fn pill(ui: &mut Ui, pal: &Palette, text: &str, fg: Color32, bg: Color32) {
    let g = ui.painter().layout_no_wrap(text.to_owned(), bold(11.5), fg);
    let (r, _) = ui.allocate_exact_size(g.size() + vec2(14.0, 6.0), Sense::hover());
    ui.painter().rect_filled(r, 9.0, bg);
    ui.painter().galley(r.center() - g.size() / 2.0, g, fg);
    let _ = pal;
}

pub(crate) fn combo<T: PartialEq + Clone>(ui: &mut Ui, salt: impl std::hash::Hash, width: f32, items: &[(String, T)], cur: &T) -> Option<T> {
    let text = items.iter().find(|(_, v)| v == cur).map(|(s, _)| s.clone()).unwrap_or_else(|| "—".into());
    let mut out = None;
    let width = width.min((ui.available_width() - 12.0).max(96.0));
    ComboBox::from_id_salt(Id::new(salt)).selected_text(RichText::new(text).font(body(13.5))).width(width).show_ui(ui, |ui| {
        for (name, v) in items {
            if ui.selectable_label(v == cur, RichText::new(name).font(body(13.5))).clicked() {
                out = Some(v.clone());
            }
        }
    });
    out
}

fn hm_picker(ui: &mut Ui, salt: impl std::hash::Hash, minute: &mut u16) -> bool {
    let (mut h, mut m) = (*minute / 60, *minute % 60);
    let id = Id::new(salt);
    let mut ch = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ch |= ui.add(egui::DragValue::new(&mut h).range(0..=23).custom_formatter(|v, _| format!("{:02}", v as u32))).on_hover_text("Hour").changed();
        ui.label(":");
        ch |= ui.add(egui::DragValue::new(&mut m).range(0..=59).speed(0.5).custom_formatter(|v, _| format!("{:02}", v as u32))).on_hover_text("Minute").changed();
    });
    let _ = id;
    if ch {
        *minute = h * 60 + m;
    }
    ch
}

/// Date input (ISO text) with quick buttons. `day` is a day number.
fn date_input(ui: &mut Ui, pal: &Palette, salt: impl std::hash::Hash, day: &mut i64) -> bool {
    let id = Id::new(salt);
    let ctx = ui.ctx().clone();
    let mut buf: String = ctx.data(|d| d.get_temp(id)).unwrap_or_else(|| dates::iso_of_day(*day));
    let mut ch = false;
    let focused = ctx.memory(|m| m.has_focus(id.with("te")));
    if !focused && dates::parse(&buf).map(dates::day_of) != Some(*day) {
        buf = dates::iso_of_day(*day);
    }
    ui.horizontal_wrapped(|ui| {
        let ok = dates::parse(&buf).is_some();
        let r = ui.add(TextEdit::singleline(&mut buf).id(id.with("te")).desired_width((104.0_f32).min((ui.available_width() - 12.0).max(90.0))).hint_text("YYYY-MM-DD").text_color(if ok { pal.text } else { pal.danger }).margin(Margin::symmetric(8, 6)));
        if r.changed() {
            if let Some(d) = dates::parse(&buf) {
                *day = dates::day_of(d);
                ch = true;
            }
        }
        for (name, off) in [("Today", 0), ("Tomorrow", 1), ("Next week", 7)] {
            if widgets::ghost_button(ui, pal, name).clicked() {
                *day = dates::today_day() + off;
                buf = dates::iso_of_day(*day);
                ch = true;
            }
        }
    });
    ctx.data_mut(|d| d.insert_temp(id, buf));
    ch
}

pub(crate) fn databases(ws: &Workspace) -> Vec<(String, u64)> {
    ws.pages.iter().filter(|p| p.kind == PageKind::Database && !p.trashed && p.db.is_some()).map(|p| (format!("{} {}", icons::icon_text(&p.icon), ws.display_title(p.id)).trim().to_owned(), p.id)).collect()
}

fn prop_items(d: &Database, filter: impl Fn(PropKind) -> bool) -> Vec<(String, u64)> {
    d.props.iter().filter(|p| filter(p.kind)).map(|p| (p.name.clone(), p.id)).collect()
}

fn settable(k: PropKind) -> bool {
    !k.computed() && k != PropKind::Relation
}

// ───────────────────────────── describing rules ─────────────────────────────

pub fn describe_trigger(ws: &Workspace, rule: &Rule) -> String {
    let d = ws.db(rule.db);
    let pname = |p: u64| if p == 0 { "Title".to_owned() } else { d.and_then(|d| d.prop(p)).map(|p| p.name.clone()).unwrap_or_else(|| "a property".into()) };
    let dbname = if rule.db == 0 { "the workspace".to_owned() } else { ws.display_title(rule.db) };
    match &rule.trigger {
        Trigger::RowAdded => format!("A row is added to {dbname}"),
        Trigger::PropChanged { prop, to } => match to {
            ChangeTo::Any => format!("{} changes", pname(*prop)),
            ChangeTo::Sel(o) | ChangeTo::Has(o) => format!("{} becomes {}", pname(*prop), d.and_then(|d| d.opt(*prop, *o)).map(|o| o.name.clone()).unwrap_or_else(|| "?".into())),
            ChangeTo::Check(b) => format!("{} is {}", pname(*prop), if *b { "checked" } else { "unchecked" }),
        },
        Trigger::DateReached { prop, offset_days, minute } => {
            let when = match offset_days.cmp(&0) {
                std::cmp::Ordering::Less => format!("{} day(s) before", -offset_days),
                std::cmp::Ordering::Equal => "On".to_owned(),
                std::cmp::Ordering::Greater => format!("{offset_days} day(s) after"),
            };
            format!("{when} {} at {}", pname(*prop), dates::hhmm(*minute))
        }
        Trigger::Schedule { repeat, minute, .. } => format!("{} at {}", repeat.label(), dates::hhmm(*minute)),
        Trigger::Manual => "You press Run now".into(),
    }
}

pub fn describe_action(ws: &Workspace, db: u64, a: &Action) -> String {
    let pname = |p: u64| if p == 0 { "Title".to_owned() } else { ws.db(db).and_then(|d| d.prop(p)).map(|p| p.name.clone()).unwrap_or_else(|| "property".into()) };
    match a {
        Action::Set { prop, value } => format!("Set {} → {}", pname(*prop), au::setter_text(ws, db, *prop, value)),
        Action::CreateRow { db: t, title, .. } => format!("Create “{}” in {}", title, ws.display_title(*t)),
        Action::MoveRow { db: t } => format!("Move to {}", ws.display_title(*t)),
        Action::Rename { title } => format!("Rename to “{title}”"),
        Action::Append { text, todo } => format!("Add {} “{text}”", if *todo { "to-do" } else { "text" }),
        Action::Notify { message } => format!("Notify “{message}”"),
        Action::Duplicate => "Duplicate the row".into(),
        Action::Trash => "Move the row to the trash".into(),
        Action::Script { code } => format!("Run script ({} lines)", code.lines().count()),
    }
}

// ───────────────────────────── editors: repeat, setter, action list ─────────────────────────────

pub fn repeat_editor(ui: &mut Ui, pal: &Palette, rep: &mut Repeat, salt: impl std::hash::Hash + Copy) -> bool {
    let mut ch = false;
    ui.horizontal_wrapped(|ui| {
        label(ui, pal, "Every");
        ch |= ui.add(egui::DragValue::new(&mut rep.every).range(1..=99)).changed();
        let items: Vec<(String, Freq)> = Freq::ALL.iter().map(|f| (format!("{}{}", f.label().to_lowercase(), if rep.every > 1 { "s" } else { "" }), *f)).collect();
        if let Some(f) = combo(ui, (salt, "freq"), 90.0, &items, &rep.freq) {
            rep.freq = f;
            ch = true;
        }
    });
    if rep.freq == Freq::Weekly {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for (i, n) in dates::WEEKDAYS.iter().enumerate() {
                let on = rep.weekdays & (1 << i) != 0;
                let (r, resp) = ui.allocate_exact_size(vec2(if roomy() { 42.0 } else { 38.0 }, if roomy() { 34.0 } else { 26.0 }), Sense::click());
                ui.painter().rect_filled(r, 7.0, if on { pal.accent } else { pal.hover });
                ui.painter().text(r.center(), Align2::CENTER_CENTER, n, bold(12.0), if on { Color32::WHITE } else { pal.muted });
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    rep.weekdays ^= 1 << i;
                    ch = true;
                }
            }
        });
        label(ui, pal, if rep.weekdays & 0x7f == 0 { "No day chosen: repeats on the same weekday as the date." } else { "" });
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        label(ui, pal, "Ends");
        let kind = match rep.end {
            End::Never => 0,
            End::On(_) => 1,
            End::After(_) => 2,
        };
        let items = vec![("never".to_owned(), 0), ("on a date".to_owned(), 1), ("after N times".to_owned(), 2)];
        if let Some(k) = combo(ui, (salt, "end"), 120.0, &items, &kind) {
            rep.end = match k {
                0 => End::Never,
                1 => End::On(dates::today_day() + 90),
                _ => End::After(10),
            };
            ch = true;
        }
        match &mut rep.end {
            End::On(d) => ch |= date_input(ui, pal, (salt, "endd"), d),
            End::After(n) => {
                ch |= ui.add(egui::DragValue::new(n).range(1..=999).suffix(" times")).changed();
            }
            End::Never => {}
        }
    });
    ch
}

fn setter_ui(ui: &mut Ui, pal: &Palette, ws: &Workspace, db: u64, prop: u64, s: &mut Setter, salt: impl std::hash::Hash + Copy) -> bool {
    let Some(d) = ws.db(db) else { return false };
    let kind = d.kind_of(prop);
    let mut ch = false;
    match kind {
        PropKind::Select => {
            let items: Vec<(String, u64)> = d.prop(prop).map(|p| p.opts.iter().map(|o| (o.name.clone(), o.id)).collect()).unwrap_or_default();
            let cur = match s {
                Setter::Sel(o) => *o,
                _ => 0,
            };
            if let Some(o) = combo(ui, (salt, "sel"), 140.0, &items, &cur) {
                *s = Setter::Sel(o);
                ch = true;
            } else if cur == 0 {
                if let Some((_, o)) = items.first() {
                    *s = Setter::Sel(*o);
                    ch = true;
                }
            }
        }
        PropKind::MultiSelect => {
            let items: Vec<(String, u64)> = d.prop(prop).map(|p| p.opts.iter().map(|o| (format!("add “{}”", o.name), o.id)).collect()).unwrap_or_default();
            let cur = match s {
                Setter::AddOpt(o) => *o,
                _ => 0,
            };
            if let Some(o) = combo(ui, (salt, "multi"), 150.0, &items, &cur) {
                *s = Setter::AddOpt(o);
                ch = true;
            } else if cur == 0 {
                if let Some((_, o)) = items.first() {
                    *s = Setter::AddOpt(*o);
                    ch = true;
                }
            }
        }
        PropKind::Checkbox => {
            let cur = matches!(s, Setter::Check(true));
            let items = vec![("checked".to_owned(), true), ("unchecked".to_owned(), false)];
            if let Some(b) = combo(ui, (salt, "chk"), 110.0, &items, &cur) {
                *s = Setter::Check(b);
                ch = true;
            } else if !matches!(s, Setter::Check(_)) {
                *s = Setter::Check(true);
                ch = true;
            }
        }
        PropKind::Date => {
            let which = match s {
                Setter::Today => 0,
                Setter::InDays(_) => 1,
                Setter::ShiftDays(_) => 2,
                Setter::Clear => 3,
                _ => 0,
            };
            let items = vec![("today".to_owned(), 0), ("today + N days".to_owned(), 1), ("shift by N days".to_owned(), 2), ("clear".to_owned(), 3)];
            if let Some(k) = combo(ui, (salt, "date"), 140.0, &items, &which) {
                *s = match k {
                    0 => Setter::Today,
                    1 => Setter::InDays(1),
                    2 => Setter::ShiftDays(1),
                    _ => Setter::Clear,
                };
                ch = true;
            } else if !matches!(s, Setter::Today | Setter::InDays(_) | Setter::ShiftDays(_) | Setter::Clear) {
                *s = Setter::Today;
                ch = true;
            }
            match s {
                Setter::InDays(n) | Setter::ShiftDays(n) => ch |= ui.add(egui::DragValue::new(n).range(-3650..=3650).suffix(" d")).changed(),
                _ => {}
            }
        }
        PropKind::Number => {
            let mut v = match s {
                Setter::Num(n) => *n,
                _ => 0.0,
            };
            if ui.add(egui::DragValue::new(&mut v).speed(0.5)).changed() || !matches!(s, Setter::Num(_)) {
                *s = Setter::Num(v);
                ch = true;
            }
        }
        _ => {
            let mut t = match s {
                Setter::Text(t) => t.clone(),
                _ => String::new(),
            };
            if ui.add(TextEdit::singleline(&mut t).desired_width((190.0_f32).min((ui.available_width() - 12.0).max(90.0))).hint_text("text, {title}, {today}…").margin(Margin::symmetric(8, 6))).changed() || !matches!(s, Setter::Text(_)) {
                *s = Setter::Text(t);
                ch = true;
            }
        }
    }
    ch
}

fn default_setter(ws: &Workspace, db: u64, prop: u64) -> Setter {
    let Some(d) = ws.db(db) else { return Setter::Text(String::new()) };
    match d.kind_of(prop) {
        PropKind::Select => Setter::Sel(d.prop(prop).and_then(|p| p.opts.first().map(|o| o.id)).unwrap_or(0)),
        PropKind::MultiSelect => Setter::AddOpt(d.prop(prop).and_then(|p| p.opts.first().map(|o| o.id)).unwrap_or(0)),
        PropKind::Checkbox => Setter::Check(true),
        PropKind::Date => Setter::Today,
        PropKind::Number => Setter::Num(0.0),
        _ => Setter::Text(String::new()),
    }
}

const KINDS_ROW: [&str; 9] = ["Set property", "Create row", "Move row", "Rename", "Add to page", "Notify", "Duplicate", "Move to trash", "Run script"];

fn kind_index(a: &Action) -> usize {
    match a {
        Action::Set { .. } => 0,
        Action::CreateRow { .. } => 1,
        Action::MoveRow { .. } => 2,
        Action::Rename { .. } => 3,
        Action::Append { .. } => 4,
        Action::Notify { .. } => 5,
        Action::Duplicate => 6,
        Action::Trash => 7,
        Action::Script { .. } => 8,
    }
}

fn new_action(ws: &Workspace, db: u64, kind: usize) -> Action {
    let first_db = databases(ws).first().map(|x| x.1).unwrap_or(0);
    match kind {
        0 => {
            let prop = ws.db(db).and_then(|d| d.props.iter().find(|p| settable(p.kind)).map(|p| p.id)).unwrap_or(0);
            Action::Set { prop, value: default_setter(ws, db, prop) }
        }
        1 => Action::CreateRow { db: if db != 0 { db } else { first_db }, title: "New task".into(), sets: vec![] },
        2 => Action::MoveRow { db: databases(ws).iter().map(|x| x.1).find(|d| *d != db).unwrap_or(first_db) },
        3 => Action::Rename { title: "{title}".into() },
        4 => Action::Append { text: "Done on {today}".into(), todo: false },
        5 => Action::Notify { message: "{title}".into() },
        6 => Action::Duplicate,
        8 => Action::Script { code: crate::scripthost::EXAMPLES[2].code.to_owned() },
        _ => Action::Trash,
    }
}

/// Editor for a list of actions. `row_ctx`: the actions run on a database row (rule) / on a row page (button) — otherwise only the
/// row-independent actions are offered.
pub fn actions_editor(ui: &mut Ui, pal: &Palette, ws: &Workspace, db: u64, acts: &mut Vec<Action>, row_ctx: bool, salt: impl std::hash::Hash + Copy) -> bool {
    let mut ch = false;
    let mut remove: Option<usize> = None;
    let mut swap: Option<usize> = None;
    let allowed: Vec<usize> = if row_ctx { (0..9).collect() } else { vec![1, 4, 5, 8] };
    let dbs = databases(ws);
    for i in 0..acts.len() {
        let idp = (salt, "act", i);
        Frame::new().fill(pal.hover).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let items: Vec<(String, usize)> = allowed.iter().map(|k| (KINDS_ROW[*k].to_owned(), *k)).collect();
                if let Some(k) = combo(ui, (idp, "kind"), 140.0, &items, &kind_index(&acts[i])) {
                    acts[i] = new_action(ws, db, k);
                    ch = true;
                }
                if i > 0 && icon_btn(ui, pal, l::ARROW_UP_FROM_LINE, "Move up", false).clicked() {
                    swap = Some(i);
                }
                if icon_btn(ui, pal, l::TRASH_2, "Remove action", true).clicked() {
                    remove = Some(i);
                }
            });
            ui.add_space(4.0);
            let mut a = acts[i].clone();
            match &mut a {
                Action::Set { prop, value } => {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(d) = ws.db(db) {
                            let items = prop_items(d, settable);
                            if let Some(p) = combo(ui, (idp, "prop"), 130.0, &items, prop) {
                                *prop = p;
                                *value = default_setter(ws, db, p);
                            }
                            label(ui, pal, "to");
                            setter_ui(ui, pal, ws, db, *prop, value, (idp, "set"));
                        }
                    });
                }
                Action::CreateRow { db: t, title, sets } => {
                    ui.horizontal_wrapped(|ui| {
                        label(ui, pal, "In");
                        if let Some(n) = combo(ui, (idp, "cdb"), 160.0, &dbs.iter().map(|(n, i)| (n.clone(), *i)).collect::<Vec<_>>(), t) {
                            *t = n;
                            sets.clear();
                        }
                    });
                    ui.add(TextEdit::singleline(title).desired_width(f32::INFINITY).hint_text("Title — {title}, {today}, {prop:Name}").margin(Margin::symmetric(8, 6)));
                    let mut rm = None;
                    for (si, (p, s)) in sets.iter_mut().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            if let Some(d) = ws.db(*t) {
                                if let Some(np) = combo(ui, (idp, "sp", si), 120.0, &prop_items(d, settable), p) {
                                    *p = np;
                                    *s = default_setter(ws, *t, np);
                                }
                                label(ui, pal, "=");
                                setter_ui(ui, pal, ws, *t, *p, s, (idp, "ss", si));
                            }
                            if icon_btn(ui, pal, l::X, "Remove", true).clicked() {
                                rm = Some(si);
                            }
                        });
                    }
                    if let Some(si) = rm {
                        sets.remove(si);
                    }
                    if let Some(d) = ws.db(*t) {
                        if widgets::ghost_button(ui, pal, "+ Set a property").clicked() {
                            if let Some(p) = d.props.iter().find(|p| settable(p.kind)) {
                                sets.push((p.id, default_setter(ws, *t, p.id)));
                            }
                        }
                    }
                }
                Action::MoveRow { db: t } => {
                    ui.horizontal_wrapped(|ui| {
                        label(ui, pal, "Move the row to");
                        if let Some(n) = combo(ui, (idp, "mdb"), 170.0, &dbs.iter().map(|(n, i)| (n.clone(), *i)).collect::<Vec<_>>(), t) {
                            *t = n;
                        }
                    });
                    label(ui, pal, "Properties with the same name and type are carried over.");
                }
                Action::Rename { title } => {
                    ui.add(TextEdit::singleline(title).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 6)));
                }
                Action::Append { text, todo } => {
                    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 6)));
                    ui.checkbox(todo, "As a to-do");
                }
                Action::Notify { message } => {
                    ui.add(TextEdit::singleline(message).desired_width(f32::INFINITY).hint_text("Message — {title}, {date}, {today}").margin(Margin::symmetric(8, 6)));
                }
                Action::Duplicate | Action::Trash => {}
                Action::Script { code } => {
                    crate::scriptui::inline_editor(ui, pal, ws, db, code, (idp, "script"));
                }
            }
            if a != acts[i] {
                acts[i] = a;
                ch = true;
            }
        });
        ui.add_space(6.0);
    }
    if let Some(i) = remove {
        acts.remove(i);
        ch = true;
    }
    if let Some(i) = swap {
        acts.swap(i, i - 1);
        ch = true;
    }
    ui.horizontal_wrapped(|ui| {
        let items: Vec<(String, usize)> = allowed.iter().map(|k| (format!("+ {}", KINDS_ROW[*k]), *k)).collect();
        ComboBox::from_id_salt(Id::new((salt, "add"))).selected_text(RichText::new("+ Add an action").font(bold(13.0))).width((150.0_f32).min((ui.available_width() - 12.0).max(90.0))).show_ui(ui, |ui| {
            for (n, k) in &items {
                if ui.selectable_label(false, RichText::new(n).font(body(13.5))).clicked() {
                    acts.push(new_action(ws, db, *k));
                    ch = true;
                }
            }
        });
    });
    ch
}

// ───────────────────────────── button blocks ─────────────────────────────

/// Draws a one-click button block. Click → run (or open the editor when it has no actions yet); ⚙ / right-click → edit.
pub fn button_block(ui: &mut Ui, b: &mut Block, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let ctx = ui.ctx().clone();
    let h = if roomy() { 44.0 } else { 36.0 };
    let text = if b.text.trim().is_empty() { "Button".to_owned() } else { b.text.clone() };
    let g = ui.painter().layout_no_wrap(text.clone(), bold(14.5), Color32::WHITE);
    let bw = (g.size().x + 62.0).clamp(110.0, (w - 48.0).max(110.0));
    let flash_id = Id::new(("btn_flash", b.id));
    let now = ctx.input(|i| i.time);
    let flash: f64 = ctx.data(|d| d.get_temp(flash_id)).unwrap_or(-10.0);
    let ran = now - flash < 1.3;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(bw, h), Sense::click());
        let t = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        let down = resp.is_pointer_button_down_on();
        let r = if down { rect.shrink(1.5) } else { rect };
        let base = if ran { Color32::from_rgb(46, 160, 100) } else { pal.accent };
        ui.painter().rect_filled(r.translate(vec2(0.0, 1.5)), 10.0, Color32::from_black_alpha(22));
        ui.painter().rect_filled(r, 10.0, lerp_color(base, Color32::BLACK, 0.12 * t));
        let gl = if ran { l::CHECK } else { l::ZAP };
        icons::glyph(ui.painter(), pos2(r.left() + 20.0, r.center().y), 15.0, gl, Color32::WHITE);
        ui.painter().with_clip_rect(r.shrink2(vec2(34.0, 0.0)).expand2(vec2(0.0, 4.0))).text(pos2(r.left() + 36.0, r.center().y), Align2::LEFT_CENTER, &text, bold(14.5), Color32::WHITE);
        let right = widgets::right_clicked(ui, rect);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if b.acts.is_empty() {
                ed.edit_button = Some(b.id);
            } else {
                ed.press_button = Some(b.id);
                ctx.data_mut(|d| d.insert_temp(flash_id, now));
            }
        }
        if right.is_some() {
            ed.edit_button = Some(b.id);
        }
        if icon_btn(ui, pal, l::SETTINGS_2, "Edit button", false).clicked() {
            ed.edit_button = Some(b.id);
        }
        if b.acts.is_empty() {
            ui.label(RichText::new("No actions yet — click to set up").font(body(12.5)).color(pal.muted));
        } else if w > 420.0 {
            ui.label(RichText::new(format!("{} action{}", b.acts.len(), if b.acts.len() == 1 { "" } else { "s" })).font(body(12.5)).color(pal.muted));
        }
    });
    if ran {
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }
    false
}

/// Modal editor for a button block (label + actions). `page` is the page that holds the button.
pub fn button_editor(ctx: &egui::Context, ws: &mut Workspace, rt: &mut Runtime, page: u64, block: u64, pal: &Palette) -> bool {
    let Some(pg) = ws.page(page) else { return false };
    let Some(mut b) = pg.blocks.iter().find(|b| b.id == block).cloned() else { return false };
    let row_ctx = pg.row;
    let db = if pg.row { pg.parent.unwrap_or(0) } else { 0 };
    let before = b.clone();
    let mut run = false;
    let wsr: &Workspace = ws;
    let open = crate::app::modal(ctx, Id::new("btn_editor"), pal, 520.0, 56.0, |ui| {
        ScrollArea::vertical().max_height((ctx.screen_rect().height() - 120.0).max(240.0)).auto_shrink([false, true]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                ui.painter().rect_filled(r, 10.0, with_alpha(pal.accent, 0.14));
                icons::glyph(ui.painter(), r.center(), 18.0, l::ZAP, pal.accent);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Button").font(bold(17.0)).color(pal.text));
                    ui.label(RichText::new("One click runs every action below, in order.").font(body(12.5)).color(pal.muted));
                });
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 8.0);
                    caps(ui, pal, "Label");
                    ui.add(TextEdit::singleline(&mut b.text).desired_width(f32::INFINITY).hint_text("e.g. Start sprint").margin(Margin::symmetric(8, 7)));
                    caps(ui, pal, "Actions");
                    if !row_ctx {
                        label(ui, pal, "This page is not a database row, so only “Create row”, “Add to page” and “Notify” are available.");
                        ui.add_space(4.0);
                    }
                    actions_editor(ui, pal, wsr, db, &mut b.acts, row_ctx, ("btn", block));
                    label(ui, pal, "Placeholders: {title}  {date}  {today}  {now}  {prop:Name}");
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if widgets::primary_button(ui, pal, "Done", 110.0).clicked() {
                            run = false;
                            ui.ctx().data_mut(|d| d.insert_temp(Id::new("btn_editor_done"), true));
                        }
                        if !b.acts.is_empty() && widgets::ghost_button(ui, pal, "Test run").clicked() {
                            run = true;
                        }
                    });
                    ui.add_space(8.0);
                });
            });
        });
    });
    let done: bool = ctx.data_mut(|d| d.remove_temp::<bool>(Id::new("btn_editor_done"))).unwrap_or(false);
    if b.text != before.text || b.acts != before.acts {
        if let Some(blk) = ws.page_mut(page).and_then(|p| p.blocks.iter_mut().find(|x| x.id == block)) {
            blk.text = b.text.clone();
            blk.acts = b.acts.clone();
        }
        ws.touch(page);
    }
    if run {
        let out = au::press_button(ws, rt, page, block, crate::auth::now_ms());
        st(|s| s.msg = Some(format!("Ran “{}” — {} notification(s)", b.text, out.notices.len())));
    }
    open && !done
}

// ───────────────────────────── row date popover: repeat + remind ─────────────────────────────

/// Small "🔁 Repeat · 🔔 Remind" line under a date property of a row page.
pub fn row_date_chips(ui: &mut Ui, pal: &Palette, ws: &Workspace, row: u64, prop: u64, indent: f32) {
    let Some(p) = ws.page(row) else { return };
    if !matches!(p.props.get(&prop), Some(Val::Text(t)) if !t.is_empty()) {
        return;
    }
    let rep = p.repeats.get(&prop);
    let rem = ws.auto.reminders.iter().find(|r| r.page == row && r.prop == prop && !r.done);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_space(indent + 150.0);
        let items = [
            (l::ROTATE_CW, rep.map(|r| r.label()).unwrap_or_else(|| "Repeat".into()), rep.is_some()),
            (l::BELL, rem.map(|r| format!("{}{}", dates::hhmm(r.minute), if r.lead_min > 0 { format!(" · {} before", lead_label(r.lead_min)) } else { String::new() })).unwrap_or_else(|| "Remind".into()), rem.is_some()),
        ];
        for (i, (g, text, on)) in items.iter().enumerate() {
            let galley = ui.painter().layout_no_wrap(text.clone(), body(12.0), pal.muted);
            let hh = if roomy() { 30.0 } else { 22.0 };
            let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 32.0, hh), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            let bg = if *on { with_alpha(pal.accent, 0.12 + 0.08 * t) } else { with_alpha(pal.hover, 0.5 + 0.5 * t) };
            ui.painter().rect_filled(r, hh / 2.0, bg);
            let fg = if *on { pal.accent } else { pal.muted };
            icons::glyph(ui.painter(), pos2(r.left() + 13.0, r.center().y), 12.0, g, fg);
            ui.painter().text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, text, body(12.0), fg);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                st(|s| {
                    s.row_pop = Some((row, prop, r.left_bottom() + vec2(0.0, 6.0)));
                    s.row_pop_fresh = true;
                });
                let _ = i;
            }
        }
    });
}

fn lc_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn lead_label(m: u32) -> String {
    match m {
        0 => "at time".into(),
        1..=59 => format!("{m} min"),
        60..=1439 => format!("{} h", m / 60),
        _ => format!("{} d", m / 1440),
    }
}

const LEADS: [u32; 7] = [0, 5, 15, 30, 60, 1440, 2880];

fn lead_items() -> Vec<(String, u32)> {
    LEADS.iter().map(|m| (if *m == 0 { "At time of date".to_owned() } else { format!("{} before", lead_label(*m)) }, *m)).collect()
}

/// The popover itself; call once per frame from the app. Returns true when the workspace changed.
pub fn row_popover(ctx: &egui::Context, ws: &mut Workspace, rt: &mut Runtime, pal: &Palette) -> bool {
    let Some((row, prop, pos)) = st(|s| s.row_pop) else { return false };
    let Some(p) = ws.page(row) else {
        st(|s| s.row_pop = None);
        return false;
    };
    let Some(db) = p.parent else { return false };
    let mut rep = p.repeats.get(&prop).cloned();
    let mut rem = ws.auto.reminders.iter().find(|r| r.page == row && r.prop == prop).cloned();
    let (rep0, rem0) = (rep.clone(), rem.clone());
    let mut fresh = st(|s| std::mem::take(&mut s.row_pop_fresh));
    let default_lead = ws.auto.settings.default_lead;
    let wsr: &Workspace = ws;
    let statuses: Vec<(String, u64)> = std::iter::once(("Automatic".to_owned(), 0u64)).chain(wsr.db(db).map(|d| prop_items(d, |k| matches!(k, PropKind::Select | PropKind::Checkbox))).unwrap_or_default()).collect();
    let open = widgets::popup_area(ctx, Id::new("row_date_pop"), pos, pal, &mut fresh, 340.0, |ui| {
        ui.add_space(4.0);
        // ── repeat
        ui.horizontal_wrapped(|ui| {
            ui.add_space(6.0);
            icons::glyph(ui.painter(), ui.cursor().min + vec2(8.0, 10.0), 14.0, l::ROTATE_CW, pal.accent);
            ui.add_space(22.0);
            ui.label(RichText::new("Repeat").font(bold(14.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                let mut on = rep.is_some();
                if switch(ui, pal, &mut on) {
                    rep = if on { Some(Repeat::default()) } else { None };
                }
            });
        });
        if let Some(r) = rep.as_mut() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    repeat_editor(ui, pal, r, ("rowrep", row, prop));
                    ui.add_space(6.0);
                    let mode = if r.spawn { 2 } else if r.roll && !r.on_complete { 1 } else { 0 };
                    let items = vec![
                        ("When completed: move the date forward".to_owned(), 0),
                        ("When the date passes: move it forward".to_owned(), 1),
                        ("When completed: keep it and add the next one".to_owned(), 2),
                    ];
                    if let Some(m) = combo(ui, ("rowrepmode", row, prop), 300.0, &items, &mode) {
                        r.on_complete = m != 1;
                        r.roll = m == 1;
                        r.spawn = m == 2;
                    }
                    if r.on_complete {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            label(ui, pal, "Completed =");
                            if let Some(s) = combo(ui, ("rowrepst", row, prop), 130.0, &statuses, &r.status_prop) {
                                r.status_prop = s;
                                r.done_opt = 0;
                                r.reset_opt = 0;
                            }
                        });
                        ui.checkbox(&mut r.from_completion, "Count from the day it is completed");
                        label(ui, pal, "Done-like statuses (Done, Complete …) and checkboxes are detected automatically.");
                    }
                });
            });
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        // ── reminder
        ui.horizontal_wrapped(|ui| {
            ui.add_space(6.0);
            icons::glyph(ui.painter(), ui.cursor().min + vec2(8.0, 10.0), 14.0, l::BELL, pal.accent);
            ui.add_space(22.0);
            ui.label(RichText::new("Reminder").font(bold(14.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                let mut on = rem.is_some();
                if switch(ui, pal, &mut on) {
                    rem = if on { Some(Reminder { id: 0, page: row, prop, day: 0, minute: 9 * 60, lead_min: default_lead, message: String::new(), repeat: None, fired_for: 0, done: false }) } else { None };
                }
            });
        });
        if let Some(r) = rem.as_mut() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        label(ui, pal, "At");
                        hm_picker(ui, ("remhm", row, prop), &mut r.minute);
                        if let Some(m) = combo(ui, ("remlead", row, prop), 130.0, &lead_items(), &r.lead_min) {
                            r.lead_min = m;
                        }
                    });
                    ui.add_space(4.0);
                    ui.add(TextEdit::singleline(&mut r.message).desired_width((280.0_f32).min((ui.available_width() - 12.0).max(90.0))).hint_text("Custom message (optional)").margin(Margin::symmetric(8, 6)));
                    label(ui, pal, "You get a desktop notification on Windows and a push notification in the browser.");
                });
            });
        }
        ui.add_space(6.0);
    });
    // write back
    let mut changed = false;
    if rep != rep0 {
        if let Some(p) = ws.page_mut(row) {
            match &rep {
                Some(r) => {
                    p.repeats.insert(prop, r.clone());
                }
                None => {
                    p.repeats.remove(&prop);
                }
            }
        }
        changed = true;
    }
    if rem != rem0 {
        match rem {
            Some(mut r) => {
                if r.id == 0 {
                    r.id = ws.id();
                    ws.auto.reminders.push(r);
                } else if let Some(x) = ws.auto.reminders.iter_mut().find(|x| x.id == r.id) {
                    *x = r;
                }
            }
            None => ws.auto.reminders.retain(|x| !(x.page == row && x.prop == prop)),
        }
        changed = true;
    }
    if changed {
        rt.dirty = true;
    }
    st(|s| {
        s.row_pop_fresh = fresh;
        if !open {
            s.row_pop = None;
        }
    });
    changed
}

// ───────────────────────────── the Automations workspace ─────────────────────────────

pub fn show(ui: &mut Ui, ws: &mut Workspace, rt: &mut Runtime, pal: &Palette) -> Out {
    let mut out = Out::default();
    let avail = ui.available_width();
    let side = if avail > 900.0 { (avail - 820.0) / 2.0 } else { 14.0_f32.min(avail * 0.04) };
    let narrow = avail < 560.0;
    ScrollArea::vertical().id_salt("auto_scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.add_space(side);
            ui.vertical(|ui| {
                let page_w = (avail - 2.0 * side - 8.0).clamp(240.0, 820.0);
                ui.set_width(page_w);
                // long labels wrap inside their column instead of running over the next widget (phones)
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                ui.add_space(18.0);
                // header
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    ui.painter().rect_filled(r, 12.0, with_alpha(pal.accent, 0.14));
                    icons::glyph(ui.painter(), r.center(), 22.0, l::ZAP, pal.accent);
                    ui.vertical(|ui| {
                        ui.set_max_width((page_w - 44.0 - roomy_w() - 3.0 * ui.spacing().item_spacing.x).max(120.0));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("Automations").font(bold(if narrow { 22.0 } else { 26.0 })).color(pal.text));
                        ui.label(RichText::new("Rules, scripts, buttons, recurring tasks and reminders").font(body(13.0)).color(pal.muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_btn(ui, pal, l::X, "Close", false).clicked() {
                            out.close = true;
                        }
                    });
                });
                ui.add_space(14.0);
                let mut tab = st(|s| s.tab) as usize;
                let unread = ws.auto.unread();
                let inbox_label = if unread > 0 { format!("Inbox · {unread}") } else { "Inbox".to_owned() };
                let labels: Vec<&str> = vec!["Rules", "Reminders", &inbox_label, "Activity", "Scripts", "Roadmap"];
                ScrollArea::horizontal().id_salt("auto_tabs").auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    crate::uploader::segmented(ui, pal, Id::new("auto_seg"), &labels, &mut tab);
                });
                st(|s| s.tab = tab as u8);
                ui.add_space(14.0);
                if let Some(m) = st(|s| s.msg.take()) {
                    out.toast = Some(m);
                }
                match tab {
                    0 => rules_tab(ui, ws, rt, pal, &mut out),
                    1 => reminders_tab(ui, ws, rt, pal, &mut out),
                    2 => inbox_tab(ui, ws, pal, &mut out),
                    3 => activity_tab(ui, ws, pal),
                    4 => crate::scriptui::scripts_tab(ui, ws, rt, pal, &mut out),
                    _ => crate::scriptui::roadmap_tab(ui, pal),
                }
                ui.add_space(60.0);
            });
        });
    });
    out
}

fn rules_tab(ui: &mut Ui, ws: &mut Workspace, rt: &mut Runtime, pal: &Palette, out: &mut Out) {
    let dbs = databases(ws);
    // master switch + new rule
    card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut on = ws.auto.settings.rules_on;
            if switch(ui, pal, &mut on) {
                ws.auto.settings.rules_on = on;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(if ws.auto.settings.rules_on { "Automations are running" } else { "Automations are paused" }).font(bold(14.5)));
                ui.label(RichText::new(format!("{} rule(s) · {} run(s) so far", ws.auto.rules.len(), ws.auto.rules.iter().map(|r| r.runs).sum::<u32>())).font(body(12.5)).color(pal.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::primary_button(ui, pal, "+ New rule", 112.0).clicked() {
                    let db = dbs.first().map(|x| x.1).unwrap_or(0);
                    let prop = ws.db(db).and_then(|d| d.props.iter().find(|p| p.kind == PropKind::Select).map(|p| p.id)).unwrap_or(0);
                    let id = ws.id();
                    let trig = if prop != 0 { Trigger::PropChanged { prop, to: ChangeTo::Any } } else { Trigger::RowAdded };
                    let r = Rule::new(id, "New rule", db, trig, vec![Action::Notify { message: "{title} changed".into() }]);
                    ws.auto.rules.push(r);
                    st(|s| s.open_rule = Some(id));
                    rt.dirty = true;
                }
            });
        });
    });
    ui.add_space(12.0);

    // rules
    if ws.auto.rules.is_empty() {
        card(ui, pal, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(14.0);
                icons::glyph(ui.painter(), ui.cursor().center_top() + vec2(0.0, 14.0), 28.0, l::WAND_SPARKLES, pal.muted);
                ui.add_space(36.0);
                ui.label(RichText::new("No rules yet").font(bold(15.0)));
                ui.label(RichText::new("Start from a recipe below, or build your own with “New rule”.").font(body(13.0)).color(pal.muted));
                ui.add_space(12.0);
            });
        });
    }
    let ids: Vec<u64> = ws.auto.rules.iter().map(|r| r.id).collect();
    let mut delete: Option<u64> = None;
    let mut run_now: Option<u64> = None;
    for id in ids {
        let Some(idx) = ws.auto.rules.iter().position(|r| r.id == id) else { continue };
        let mut rule = ws.auto.rules[idx].clone();
        let before = rule.clone();
        let open = st(|s| s.open_rule) == Some(id);
        let summary_t = describe_trigger(ws, &rule);
        let summary_a: Vec<String> = rule.actions.iter().map(|a| describe_action(ws, rule.db, a)).collect();
        let mut toggle_open = false;
        card(ui, pal, |ui| {
            // phones: the title gets the whole row, the three buttons move to their own row (nothing overlaps)
            let stacked = ui.available_width() < 460.0;
            let btn_w = roomy_w() * 3.0 + ui.spacing().item_spacing.x * 3.0;
            ui.horizontal(|ui| {
                let mut on = rule.on;
                if switch(ui, pal, &mut on) {
                    rule.on = on;
                }
                ui.vertical(|ui| {
                    let room = ui.available_width() - if stacked { 0.0 } else { btn_w };
                    ui.set_max_width(room.max(110.0));
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let r = ui.add(egui::Label::new(RichText::new(&rule.name).font(bold(15.0)).color(if rule.on { pal.text } else { pal.muted })).sense(Sense::click()).wrap());
                    if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        toggle_open = true;
                    }
                    ui.add(egui::Label::new(RichText::new(format!("When {}", lc_first(&summary_t))).font(body(12.5)).color(pal.muted)).wrap());
                });
                if !stacked {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_btn(ui, pal, l::TRASH_2, "Delete rule", true).clicked() {
                            delete = Some(id);
                        }
                        if icon_btn(ui, pal, if open { l::CHEVRONS_UP_DOWN } else { l::SETTINGS_2 }, if open { "Collapse" } else { "Edit rule" }, false).clicked() {
                            toggle_open = true;
                        }
                        if icon_btn(ui, pal, l::PLAY, "Run now (all matching rows)", false).clicked() {
                            run_now = Some(id);
                        }
                    });
                }
            });
            if stacked {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if icon_btn(ui, pal, l::PLAY, "Run now (all matching rows)", false).clicked() {
                        run_now = Some(id);
                    }
                    if icon_btn(ui, pal, if open { l::CHEVRONS_UP_DOWN } else { l::SETTINGS_2 }, if open { "Collapse" } else { "Edit rule" }, false).clicked() {
                        toggle_open = true;
                    }
                    if icon_btn(ui, pal, l::TRASH_2, "Delete rule", true).clicked() {
                        delete = Some(id);
                    }
                });
            }
            if !open {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                    for a in &summary_a {
                        pill(ui, pal, a, pal.muted, pal.hover);
                    }
                });
                ui.add_space(2.0);
                let last = if rule.last_run > 0.0 { format!("last run {} ago", crate::app::ago(rule.last_run)) } else { "never run".into() };
                label(ui, pal, &format!("{} run(s) · {last}", rule.runs));
            } else {
                rule_editor(ui, pal, ws, &mut rule, id);
            }
        });
        ui.add_space(8.0);
        if toggle_open {
            st(|s| s.open_rule = if open { None } else { Some(id) });
        }
        if rule != before {
            au::drop_dead_conds_in(ws, &mut rule);
            if before.trigger != rule.trigger || before.db != rule.db {
                rule.next_min = 0;
                rule.fired.clear();
            }
            if let Some(slot) = ws.auto.rules.get_mut(idx) {
                *slot = rule;
            }
            rt.dirty = true;
        }
    }
    if let Some(id) = delete {
        ws.auto.rules.retain(|r| r.id != id);
        out.toast = Some("Rule deleted".into());
    }
    if let Some(id) = run_now {
        let o = au::run_rule_now(ws, rt, id, crate::auth::now_ms());
        out.toast = Some(format!("Rule ran · {} notification(s)", o.notices.len()));
    }

    // recipes
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Recipes").font(bold(17.0)));
        label(ui, pal, "one-click automations for");
        let mut cur = st(|s| s.recipe_db);
        if !dbs.iter().any(|d| d.1 == cur) {
            cur = dbs.first().map(|d| d.1).unwrap_or(0);
        }
        if let Some(n) = combo(ui, "recipe_db", 190.0, &dbs.iter().map(|(n, i)| (n.clone(), *i)).collect::<Vec<_>>(), &cur) {
            cur = n;
        }
        st(|s| s.recipe_db = cur);
    });
    ui.add_space(8.0);
    let db = st(|s| s.recipe_db);
    let db = if dbs.iter().any(|d| d.1 == db) { db } else { dbs.first().map(|d| d.1).unwrap_or(0) };
    let avail = ui.available_width();
    let gap = 10.0;
    let cols = if avail > 640.0 { 2 } else { 1 };
    let cw = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
    let (pad, icon, btn_h) = (14.0_f32, 34.0_f32, if roomy() { 40.0_f32 } else { 32.0_f32 });
    let tx = pad + icon + 10.0;
    let tw = (cw - tx - pad).max(80.0);
    let recipes = au::recipes();
    let mut created: Option<String> = None;
    for chunk in recipes.chunks(cols) {
        // measure every tile of the row first, so all tiles of a row get the same height and the buttons line up
        let mut galleys = vec![];
        let mut h_max = 0.0_f32;
        for rc in chunk {
            let gt = ui.painter().layout(rc.title.to_owned(), bold(14.0), pal.text, tw);
            let gd = ui.painter().layout(rc.desc.to_owned(), body(12.5), pal.muted, tw);
            h_max = h_max.max(pad + gt.size().y + 3.0 + gd.size().y + 10.0 + btn_h + pad);
            galleys.push((gt, gd));
        }
        let h_max = h_max.max(pad * 2.0 + icon);
        let (row_rect, _) = ui.allocate_exact_size(vec2(avail, h_max), Sense::hover());
        for (i, rc) in chunk.iter().enumerate() {
            let rect = Rect::from_min_size(pos2(row_rect.left() + i as f32 * (cw + gap), row_rect.top()), vec2(cw, h_max));
            let usable = !rc.needs_db || db != 0;
            let p = ui.painter();
            p.rect_filled(rect, 12.0, pal.hover);
            p.rect_stroke(rect, 12.0, Stroke::new(1.0_f32, with_alpha(pal.border, 0.6)), egui::StrokeKind::Inside);
            let ir = Rect::from_min_size(rect.min + vec2(pad, pad), vec2(icon, icon));
            p.rect_filled(ir, 9.0, with_alpha(pal.accent, 0.14));
            icons::glyph(p, ir.center(), 16.0, rc.icon, pal.accent);
            let (gt, gd) = &galleys[i];
            let (tpos, dpos) = (pos2(rect.left() + tx, rect.top() + pad), pos2(rect.left() + tx, rect.top() + pad + gt.size().y + 3.0));
            p.galley(tpos, gt.clone(), pal.text);
            p.galley(dpos, gd.clone(), pal.muted);
            let br = Rect::from_min_size(pos2(rect.left() + tx, rect.bottom() - pad - btn_h), vec2(tw, btn_h));
            ui.scope_builder(egui::UiBuilder::new().max_rect(br).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.add_enabled_ui(usable, |ui| {
                    if widgets::ghost_button(ui, pal, "+ Add this rule").clicked() {
                        if let Some(id) = rc.create(ws, if rc.needs_db { db } else { 0 }) {
                            st(|s| s.open_rule = Some(id));
                            created = Some(format!("Added “{}”", rc.title));
                            rt.dirty = true;
                        } else {
                            created = Some("This database lacks the properties this recipe needs (e.g. a Status select or a Date).".into());
                        }
                    }
                });
            });
        }
        ui.add_space(gap);
    }
    if created.is_some() {
        out.toast = created;
    }
}

fn rule_editor(ui: &mut Ui, pal: &Palette, ws: &Workspace, rule: &mut Rule, id: u64) {
    ui.add_space(8.0);
    caps(ui, pal, "Name");
    ui.add(TextEdit::singleline(&mut rule.name).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 7)));
    let dbs = databases(ws);
    let schedule = matches!(rule.trigger, Trigger::Schedule { .. });
    caps(ui, pal, "Database");
    ui.horizontal_wrapped(|ui| {
        let mut items: Vec<(String, u64)> = dbs.iter().map(|(n, i)| (n.clone(), *i)).collect();
        if schedule {
            items.insert(0, ("— none —".to_owned(), 0));
        }
        if let Some(d) = combo(ui, ("rdb", id), 220.0, &items, &rule.db) {
            rule.db = d;
            // keep the trigger valid for the new database
            let d = ws.db(d);
            let first_sel = d.and_then(|d| d.props.iter().find(|p| p.kind == PropKind::Select).map(|p| p.id)).unwrap_or(0);
            let first_date = d.and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
            match &mut rule.trigger {
                Trigger::PropChanged { prop, to } => {
                    *prop = first_sel;
                    *to = ChangeTo::Any;
                }
                Trigger::DateReached { prop, .. } => *prop = first_date,
                _ => {}
            }
            rule.actions.clear();
            rule.conds.clear();
        }
    });
    // trigger
    caps(ui, pal, "When");
    let kinds = vec![("A row is added".to_owned(), 0), ("A property changes".to_owned(), 1), ("A date arrives".to_owned(), 2), ("On a schedule".to_owned(), 3), ("I press “Run now”".to_owned(), 4)];
    let cur_kind = match rule.trigger {
        Trigger::RowAdded => 0,
        Trigger::PropChanged { .. } => 1,
        Trigger::DateReached { .. } => 2,
        Trigger::Schedule { .. } => 3,
        Trigger::Manual => 4,
    };
    let d = ws.db(rule.db);
    if let Some(k) = combo(ui, ("rtrig", id), 190.0, &kinds, &cur_kind) {
        let first_prop = |kind: PropKind| d.and_then(|d| d.props.iter().find(|p| p.kind == kind).map(|p| p.id)).unwrap_or(0);
        rule.trigger = match k {
            0 => Trigger::RowAdded,
            1 => {
                let p = first_prop(PropKind::Select);
                let p = if p != 0 { p } else { d.and_then(|d| d.props.iter().find(|p| settable(p.kind)).map(|p| p.id)).unwrap_or(0) };
                Trigger::PropChanged { prop: p, to: ChangeTo::Any }
            }
            2 => Trigger::DateReached { prop: first_prop(PropKind::Date), offset_days: -1, minute: 9 * 60 },
            3 => Trigger::Schedule { repeat: Repeat::daily(), start_day: dates::today_day(), minute: 9 * 60 },
            _ => Trigger::Manual,
        };
    }
    ui.add_space(4.0);
    match &mut rule.trigger {
        Trigger::RowAdded => label(ui, pal, "Runs whenever a new row appears in the database (also rows created by other rules)."),
        Trigger::PropChanged { prop, to } => {
            if let Some(d) = d {
                ui.horizontal_wrapped(|ui| {
                    if let Some(p) = combo(ui, ("rprop", id), 150.0, &prop_items(d, settable), prop) {
                        *prop = p;
                        *to = ChangeTo::Any;
                    }
                    match d.kind_of(*prop) {
                        PropKind::Select => {
                            let mut items = vec![("changes to anything".to_owned(), ChangeTo::Any)];
                            items.extend(d.prop(*prop).map(|p| p.opts.iter().map(|o| (format!("becomes “{}”", o.name), ChangeTo::Sel(o.id))).collect::<Vec<_>>()).unwrap_or_default());
                            if let Some(t) = combo(ui, ("rto", id), 190.0, &items, to) {
                                *to = t;
                            }
                        }
                        PropKind::MultiSelect => {
                            let mut items = vec![("changes".to_owned(), ChangeTo::Any)];
                            items.extend(d.prop(*prop).map(|p| p.opts.iter().map(|o| (format!("gets “{}”", o.name), ChangeTo::Has(o.id))).collect::<Vec<_>>()).unwrap_or_default());
                            if let Some(t) = combo(ui, ("rto", id), 190.0, &items, to) {
                                *to = t;
                            }
                        }
                        PropKind::Checkbox => {
                            let items = vec![("changes".to_owned(), ChangeTo::Any), ("is checked".to_owned(), ChangeTo::Check(true)), ("is unchecked".to_owned(), ChangeTo::Check(false))];
                            if let Some(t) = combo(ui, ("rto", id), 150.0, &items, to) {
                                *to = t;
                            }
                        }
                        _ => {
                            *to = ChangeTo::Any;
                            label(ui, pal, "changes (after you stop typing)");
                        }
                    }
                });
            } else {
                label(ui, pal, "Choose a database first.");
            }
        }
        Trigger::DateReached { prop, offset_days, minute } => {
            if let Some(d) = d {
                ui.horizontal_wrapped(|ui| {
                    let mut n = offset_days.abs();
                    let mut dir = match (*offset_days).cmp(&0) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    };
                    if n != 0 && ui.add(egui::DragValue::new(&mut n).range(1..=365).suffix(" d")).changed() {
                        *offset_days = dir * n;
                    }
                    let items = vec![("before".to_owned(), -1), ("on the day of".to_owned(), 0), ("after".to_owned(), 1)];
                    if let Some(k) = combo(ui, ("rdir", id), 120.0, &items, &dir) {
                        dir = k;
                        *offset_days = dir * n.max(1);
                    }
                    if let Some(p) = combo(ui, ("rdp", id), 140.0, &prop_items(d, |k| k == PropKind::Date), prop) {
                        *prop = p;
                    }
                    label(ui, pal, "at");
                    hm_picker(ui, ("rdhm", id), minute);
                });
                ui.checkbox(&mut rule.skip_done, "Skip rows that are already done");
            } else {
                label(ui, pal, "Choose a database first.");
            }
        }
        Trigger::Schedule { repeat, start_day, minute } => {
            repeat_editor(ui, pal, repeat, ("rsch", id));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                label(ui, pal, "At");
                hm_picker(ui, ("rschhm", id), minute);
                label(ui, pal, "starting");
                date_input(ui, pal, ("rschd", id), start_day);
            });
        }
        Trigger::Manual => label(ui, pal, "Nothing runs by itself. Press ▶ on the rule to apply it to every row that matches the conditions."),
    }
    // conditions
    if let (Some(d), true) = (d, !matches!(rule.trigger, Trigger::Schedule { .. })) {
        caps(ui, pal, "Only if (optional)");
        let mut rm = None;
        for (i, f) in rule.conds.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let mut items = vec![("Title".to_owned(), 0u64)];
                items.extend(prop_items(d, |_| true));
                if let Some(p) = combo(ui, ("cp", id, i), 120.0, &items, &f.prop) {
                    f.prop = p;
                    f.op = *FilterOp::for_kind(d.kind_of(p)).first().unwrap_or(&FilterOp::Is);
                    f.value.clear();
                }
                let kind = d.kind_of(f.prop);
                let ops: Vec<(String, FilterOp)> = FilterOp::for_kind(kind).iter().map(|o| (o.label(kind).to_owned(), *o)).collect();
                if let Some(o) = combo(ui, ("co", id, i), 120.0, &ops, &f.op) {
                    f.op = o;
                }
                if f.op.needs_value() {
                    if kind == PropKind::Select {
                        let items: Vec<(String, String)> = d.prop(f.prop).map(|p| p.opts.iter().map(|o| (o.name.clone(), o.name.clone())).collect()).unwrap_or_default();
                        if let Some(v) = combo(ui, ("cv", id, i), 120.0, &items, &f.value) {
                            f.value = v;
                        } else if f.value.is_empty() {
                            if let Some((_, v)) = items.first() {
                                f.value = v.clone();
                            }
                        }
                    } else {
                        ui.add(TextEdit::singleline(&mut f.value).desired_width((120.0_f32).min((ui.available_width() - 12.0).max(90.0))).margin(Margin::symmetric(8, 6)));
                    }
                }
                if icon_btn(ui, pal, l::X, "Remove condition", true).clicked() {
                    rm = Some(i);
                }
            });
        }
        if let Some(i) = rm {
            rule.conds.remove(i);
        }
        if widgets::ghost_button(ui, pal, "+ Add condition").clicked() {
            if let Some(f) = au::default_cond(ws, rule.db) {
                rule.conds.push(f);
            }
        }
    }
    // actions
    caps(ui, pal, "Then");
    let row_ctx = rule.db != 0 && !matches!(rule.trigger, Trigger::Schedule { .. } | Trigger::Manual);
    let row_ctx = row_ctx || matches!(rule.trigger, Trigger::Manual) && rule.db != 0;
    actions_editor(ui, pal, ws, rule.db, &mut rule.actions, row_ctx, ("ra", id));
    label(ui, pal, "Placeholders: {title}  {date}  {today}  {now}  {db}  {prop:Name}");
    ui.add_space(4.0);
}

fn reminders_tab(ui: &mut Ui, ws: &mut Workspace, rt: &mut Runtime, pal: &Palette, out: &mut Out) {
    // notification settings
    card(ui, pal, |ui| {
        ui.label(RichText::new("Notifications").font(bold(15.0)));
        ui.add_space(6.0);
        let perm = crate::platform::notify_permission();
        ui.horizontal_wrapped(|ui| {
            let mut on = ws.auto.settings.os_notify;
            if switch(ui, pal, &mut on) {
                ws.auto.settings.os_notify = on;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(if cfg!(target_arch = "wasm32") { "Browser push notifications" } else { "Desktop notifications" }).font(bold(13.5)));
                let status = match perm.as_str() {
                    "granted" => if cfg!(target_arch = "wasm32") { "Allowed in this browser" } else { "Windows toast notifications" },
                    "denied" => "Blocked — allow notifications for this site in the browser settings",
                    "default" => "Not allowed yet",
                    _ => "Not supported by this browser",
                };
                label(ui, pal, status);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if perm == "default" && widgets::primary_button(ui, pal, "Allow", 84.0).clicked() {
                    crate::platform::notify_request();
                }
                if widgets::ghost_button(ui, pal, "Send test").clicked() {
                    crate::platform::notify_os("Lumen", "Notifications are working 🎉", "lumen-test");
                    out.toast = Some("Test notification sent".into());
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let mut q = ws.auto.settings.quiet;
            if switch(ui, pal, &mut q) {
                ws.auto.settings.quiet = q;
            }
            ui.label(RichText::new("Quiet hours").font(bold(13.5)));
            if ws.auto.settings.quiet {
                label(ui, pal, "from");
                hm_picker(ui, "qf", &mut ws.auto.settings.quiet_from);
                label(ui, pal, "to");
                hm_picker(ui, "qt", &mut ws.auto.settings.quiet_to);
            } else {
                label(ui, pal, "silence system notifications at night (they still arrive in the inbox)");
            }
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            label(ui, pal, "Default lead time for date reminders");
            if let Some(m) = combo(ui, "deflead", 150.0, &lead_items(), &ws.auto.settings.default_lead) {
                ws.auto.settings.default_lead = m;
            }
        });
    });
    ui.add_space(12.0);

    // new reminder
    let mut create = false;
    card(ui, pal, |ui| {
        ui.label(RichText::new("New reminder").font(bold(15.0)));
        ui.add_space(6.0);
        st(|s| {
            if s.rem.day == 0 {
                s.rem.day = dates::today_day();
                s.rem.minute = ((dates::now_local_min().rem_euclid(1440) as u16 / 15) * 15 + 30) % 1440;
            }
        });
        let mut r = st(|s| std::mem::take(&mut s.rem));
        ui.add(TextEdit::singleline(&mut r.msg).desired_width(f32::INFINITY).hint_text("Remind me to…").margin(Margin::symmetric(8, 7)));
        ui.add_space(6.0);
        date_input(ui, pal, "newrem_date", &mut r.day);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            label(ui, pal, "at");
            hm_picker(ui, "newrem_hm", &mut r.minute);
            if let Some(m) = combo(ui, "newrem_lead", 130.0, &lead_items(), &r.lead) {
                r.lead = m;
            }
            let reps = vec![("doesn't repeat".to_owned(), 0u8), ("daily".to_owned(), 1), ("weekdays".to_owned(), 2), ("weekly".to_owned(), 3), ("monthly".to_owned(), 4), ("yearly".to_owned(), 5)];
            if let Some(m) = combo(ui, "newrem_rep", 130.0, &reps, &r.rep) {
                r.rep = m;
            }
        });
        ui.add_space(8.0);
        if widgets::primary_button(ui, pal, "Add reminder", 140.0).clicked() && !r.msg.trim().is_empty() {
            let repeat = match r.rep {
                1 => Some(Repeat::daily()),
                2 => Some(Repeat::weekly_on(&[0, 1, 2, 3, 4])),
                3 => Some(Repeat { freq: Freq::Weekly, weekdays: 1 << dates::weekday_of_day(r.day), ..Repeat::default() }),
                4 => Some(Repeat { freq: Freq::Monthly, ..Repeat::default() }),
                5 => Some(Repeat { freq: Freq::Yearly, ..Repeat::default() }),
                _ => None,
            };
            let id = ws.id();
            ws.auto.reminders.push(Reminder { id, page: 0, prop: 0, day: r.day, minute: r.minute, lead_min: r.lead, message: r.msg.trim().to_owned(), repeat, fired_for: 0, done: false });
            r.msg.clear();
            create = true;
        }
        st(|s| s.rem = r);
    });
    if create {
        rt.dirty = true;
        out.toast = Some("Reminder added".into());
    }
    ui.add_space(12.0);

    // list
    ui.label(RichText::new("Upcoming & recent").font(bold(15.0)));
    ui.add_space(6.0);
    let mut list: Vec<(i64, Reminder)> = ws.auto.reminders.iter().filter_map(|r| r.due_min(ws).map(|d| (d, r.clone()))).collect();
    list.sort_by_key(|(d, r)| (r.done, *d));
    if list.is_empty() {
        label(ui, pal, "No reminders yet. Add one above, or open a database row with a date and tap “Remind”.");
    }
    let mut remove: Option<u64> = None;
    let mut toggle_done: Option<u64> = None;
    for (due, r) in list {
        card(ui, pal, |ui| {
            ui.horizontal_wrapped(|ui| {
                icons::glyph(ui.painter(), ui.cursor().min + vec2(10.0, 12.0), 16.0, if r.done { l::CIRCLE_CHECK } else { l::BELL }, if r.done { pal.muted } else { pal.accent });
                ui.add_space(26.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let title = if !r.message.is_empty() { r.message.clone() } else if r.page != 0 { ws.display_title(r.page) } else { "Reminder".into() };
                    ui.add(egui::Label::new(RichText::new(title).font(bold(14.0)).color(if r.done { pal.muted } else { pal.text })).wrap());
                    let when = format!("{} · {}{}{}", dates::pretty(&dates::iso_of_day(due.div_euclid(1440))), dates::hhmm(r.minute), if r.lead_min > 0 { format!(" · {} before", lead_label(r.lead_min)) } else { String::new() }, r.repeat.as_ref().map(|x| format!(" · {}", x.label().to_lowercase())).unwrap_or_default());
                    label(ui, pal, &when);
                    if r.page != 0 && !r.message.is_empty() {
                        label(ui, pal, &format!("on “{}”", ws.display_title(r.page)));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_btn(ui, pal, l::TRASH_2, "Delete", true).clicked() {
                        remove = Some(r.id);
                    }
                    if r.page != 0 && icon_btn(ui, pal, l::ARROW_RIGHT, "Open page", false).clicked() {
                        out.open = Some(r.page);
                    }
                    if r.prop == 0 && icon_btn(ui, pal, if r.done { l::ROTATE_CW } else { l::CHECK }, if r.done { "Re-activate" } else { "Mark done" }, false).clicked() {
                        toggle_done = Some(r.id);
                    }
                });
            });
        });
        ui.add_space(6.0);
    }
    if let Some(id) = remove {
        ws.auto.reminders.retain(|r| r.id != id);
    }
    if let Some(id) = toggle_done {
        if let Some(r) = ws.auto.reminders.iter_mut().find(|r| r.id == id) {
            r.done = !r.done;
            if !r.done {
                r.fired_for = 0;
            }
        }
        rt.dirty = true;
    }
}

fn kind_color(k: NoticeKind, pal: &Palette) -> Color32 {
    match k {
        NoticeKind::Reminder => Color32::from_rgb(217, 115, 13),
        NoticeKind::Rule => pal.accent,
        NoticeKind::Button => Color32::from_rgb(46, 160, 100),
        NoticeKind::Repeat => Color32::from_rgb(144, 101, 176),
        NoticeKind::System => pal.muted,
    }
}

fn inbox_tab(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, out: &mut Out) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Inbox").font(bold(15.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::ghost_button(ui, pal, "Clear all").clicked() {
                ws.auto.inbox.clear();
            }
            if widgets::ghost_button(ui, pal, "Mark all read").clicked() {
                ws.auto.mark_all_read();
            }
        });
    });
    ui.add_space(6.0);
    if ws.auto.inbox.is_empty() {
        card(ui, pal, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(l::INBOX).font(icons::font(28.0)).color(pal.muted));
                ui.label(RichText::new("You're all caught up").font(bold(14.5)));
                label(ui, pal, "Reminders and automation notifications land here.");
                ui.add_space(8.0);
            });
        });
        return;
    }
    let items: Vec<Notice> = ws.auto.inbox.iter().rev().cloned().collect();
    for n in items {
        let (rect, resp) = {
            let r = Frame::new().fill(if n.read { pal.bg } else { with_alpha(pal.accent, 0.06) }).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let c = kind_color(n.kind, pal);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        if !n.read {
                            ui.painter().circle_filled(dot.center(), 4.0, pal.accent);
                        }
                    });
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal_wrapped(|ui| {
                            pill(ui, pal, n.kind.label(), c, with_alpha(c, 0.14));
                            label(ui, pal, &format!("{} ago", crate::app::ago(n.ts)));
                        });
                        ui.add(egui::Label::new(RichText::new(&n.title).font(bold(14.0)).color(pal.text)).wrap());
                        if !n.body.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&n.body).font(body(13.0)).color(pal.muted)).wrap());
                        }
                    });
                });
            });
            let rect = r.response.rect;
            (rect, ui.interact(rect, Id::new(("notice", n.id)), Sense::click()))
        };
        let _ = rect;
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if let Some(x) = ws.auto.inbox.iter_mut().find(|x| x.id == n.id) {
                x.read = true;
            }
            if let Some(p) = n.page.filter(|p| ws.page(*p).map_or(false, |x| !x.trashed)) {
                out.open = Some(p);
            }
        }
        ui.add_space(6.0);
    }
}

fn activity_tab(ui: &mut Ui, ws: &mut Workspace, pal: &Palette) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Activity").font(bold(15.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::ghost_button(ui, pal, "Clear").clicked() {
                ws.auto.log.clear();
            }
        });
    });
    ui.add_space(6.0);
    if ws.auto.log.is_empty() {
        label(ui, pal, "Nothing has run yet. Every rule, button press and recurring-task step is recorded here.");
        return;
    }
    for e in ws.auto.log.iter().rev().take(80) {
        card(ui, pal, |ui| {
            ui.horizontal_wrapped(|ui| {
                icons::glyph(ui.painter(), ui.cursor().min + vec2(9.0, 11.0), 14.0, if e.ok { l::ZAP } else { l::TRIANGLE_ALERT }, if e.ok { pal.accent } else { pal.danger });
                ui.add_space(24.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&e.rule).font(bold(13.5)));
                        label(ui, pal, &format!("{} ago", crate::app::ago(e.ts)));
                    });
                    ui.add(egui::Label::new(RichText::new(&e.msg).font(body(12.5)).color(pal.muted)).wrap());
                });
            });
        });
        ui.add_space(5.0);
    }
}

#[allow(dead_code)]
fn unused(_: Rect, _: Filter) {}
