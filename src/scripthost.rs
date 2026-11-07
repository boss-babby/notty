//! The bridge between LumenScript (`script.rs`) and the workspace: rows, databases, notifications, dates.
//! Everything a script can do is listed here — nothing else is reachable from a script (see `Host` in `script.rs`).
//!
//! Also holds the documentation shown in the editor (`API`), the example scripts and `api_markdown()`, a compact
//! machine-readable description that is handed to an AI assistant so it can write scripts for the user.

use std::collections::BTreeMap;

use crate::automation::{self as au, Ev, NoticeKind, Output};
use crate::dates;
use crate::model::{BlockKind, Opt, PageKind, PropKind, Val, Workspace};
use crate::script::{self, Host, HostRef, ScriptError, Value};

const ROW: u8 = 1;
const DB: u8 = 2;

const MAX_WRITES: u32 = 3000;
const MAX_CREATE: u32 = 200;
const MAX_NOTIFY: u32 = 25;
const MAX_EFFECTS: usize = 300;

/// What a script runs against.
#[derive(Clone, Debug, Default)]
pub struct ScriptCtx {
    /// The row that triggered the rule / the row holding the button (`row` in scripts).
    pub row: Option<u64>,
    /// The page that holds a button block, or the row itself (`page` in scripts).
    pub page: Option<u64>,
    pub db: u64,
    /// Name shown on notifications and in the log.
    pub source: String,
    pub kind: NoticeKind,
    pub now: f64,
}

pub struct Report {
    pub output: Vec<String>,
    pub error: Option<ScriptError>,
    pub steps: u64,
    /// Human-readable list of what the script changed (shown after a test run).
    pub effects: Vec<String>,
}

/// Runs `code` on the live workspace. Row changes are reported to the automation engine through `follow`.
pub fn run_script(ws: &mut Workspace, code: &str, ctx: &ScriptCtx, out: &mut Output, follow: &mut Vec<Ev>) -> Report {
    let mut h = SH { ws, ctx, out, follow, effects: vec![], writes: 0, created: 0, notified: 0 };
    let o = script::run(code, &mut h);
    let effects = std::mem::take(&mut h.effects);
    if !effects.is_empty() {
        h.out.changed = true;
    }
    Report { output: o.output, error: o.error, steps: o.steps, effects }
}

/// Runs a script on a throw-away copy of the workspace: nothing is saved, nothing is notified.
pub fn dry_run(ws: &Workspace, code: &str, ctx: &ScriptCtx) -> Report {
    let mut copy = ws.clone();
    let mut out = Output::default();
    let mut follow = vec![];
    run_script(&mut copy, code, ctx, &mut out, &mut follow)
}

struct SH<'a> {
    ws: &'a mut Workspace,
    ctx: &'a ScriptCtx,
    out: &'a mut Output,
    follow: &'a mut Vec<Ev>,
    effects: Vec<String>,
    writes: u32,
    created: u32,
    notified: u32,
}

fn row_ref(id: u64) -> Value {
    Value::Host(HostRef { kind: ROW, id })
}
fn db_ref(id: u64) -> Value {
    Value::Host(HostRef { kind: DB, id })
}

fn num_text(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

fn weekday_name(day: i64) -> &'static str {
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][(day + 4).rem_euclid(7) as usize]
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Num(n) => num_text(*n),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.type_name().to_owned(),
    }
}

impl<'a> SH<'a> {
    fn effect(&mut self, s: String) {
        if self.effects.len() < MAX_EFFECTS {
            self.effects.push(s);
        }
    }

    fn write_budget(&mut self) -> Result<(), String> {
        self.writes += 1;
        if self.writes > MAX_WRITES {
            return Err(format!("too many changes in one run (limit {MAX_WRITES})"));
        }
        Ok(())
    }

    fn databases(&self) -> Vec<(String, u64)> {
        self.ws.pages.iter().filter(|p| p.kind == PageKind::Database && !p.trashed && p.db.is_some()).map(|p| (self.ws.display_title(p.id), p.id)).collect()
    }

    fn find_db(&self, name: &str) -> Result<u64, String> {
        let dbs = self.databases();
        let n = name.trim().to_lowercase();
        if let Some((_, id)) = dbs.iter().find(|(t, _)| t.trim().to_lowercase() == n) {
            return Ok(*id);
        }
        if let Some((_, id)) = dbs.iter().find(|(t, _)| t.to_lowercase().contains(&n) && !n.is_empty()) {
            return Ok(*id);
        }
        Err(format!("no database named “{name}” (you have: {})", if dbs.is_empty() { "none".into() } else { dbs.iter().map(|d| d.0.clone()).collect::<Vec<_>>().join(", ") }))
    }

    fn db_of(&self, row: u64) -> Option<u64> {
        self.ws.page(row).filter(|p| p.row).and_then(|p| p.parent).filter(|d| self.ws.db(*d).is_some())
    }

    /// `Ok(None)` = the title; `Ok(Some((id, kind)))` = a property.
    fn prop_of(&self, db: u64, name: &str) -> Result<Option<(u64, PropKind)>, String> {
        let d = self.ws.db(db).ok_or("the database is gone")?;
        let n = name.trim().to_lowercase();
        if let Some(p) = d.props.iter().find(|p| p.name.trim().to_lowercase() == n) {
            return Ok(Some((p.id, p.kind)));
        }
        if n == "title" || n == "name" {
            return Ok(None);
        }
        Err(format!("no property “{name}” (available: Title, {})", d.props.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ")))
    }

    fn to_script(&self, db: u64, prop: u64, v: &Val) -> Value {
        let Some(p) = self.ws.db(db).and_then(|d| d.prop(prop)) else { return Value::Null };
        match v {
            Val::Text(t) => {
                if t.is_empty() { Value::Null } else { Value::str(t) }
            }
            Val::Num(n) => Value::Num(*n),
            Val::Check(b) => Value::Bool(*b),
            Val::Sel(o) => p.opts.iter().find(|x| x.id == *o).map(|x| Value::str(&x.name)).unwrap_or(Value::Null),
            Val::Multi(m) => {
                if p.kind == PropKind::Relation {
                    Value::list(m.iter().filter(|r| self.ws.page(**r).is_some()).map(|r| row_ref(*r)).collect())
                } else {
                    Value::list(m.iter().filter_map(|o| p.opts.iter().find(|x| x.id == *o)).map(|x| Value::str(&x.name)).collect())
                }
            }
        }
    }

    fn from_script(&mut self, db: u64, prop: u64, kind: PropKind, v: &Value) -> Result<Option<Val>, String> {
        if matches!(v, Value::Null) {
            return Ok(None);
        }
        let name = self.ws.db(db).and_then(|d| d.prop(prop)).map(|p| p.name.clone()).unwrap_or_default();
        Ok(Some(match kind {
            PropKind::Text | PropKind::Url => Val::Text(text_of(v)),
            PropKind::Number => match v {
                Value::Num(n) => Val::Num(*n),
                Value::Str(s) => Val::Num(s.trim().replace(',', ".").parse::<f64>().map_err(|_| format!("“{s}” is not a number (property “{name}”)"))?),
                Value::Bool(b) => Val::Num(*b as u8 as f64),
                _ => return Err(format!("property “{name}” needs a number")),
            },
            PropKind::Checkbox => Val::Check(v.truthy()),
            PropKind::Date => {
                let s = text_of(v);
                if s.is_empty() {
                    return Ok(None);
                }
                if dates::parse(&s).is_none() {
                    return Err(format!("“{s}” is not a date — use YYYY-MM-DD (property “{name}”)"));
                }
                Val::Text(s)
            }
            PropKind::Select => {
                let n = text_of(v);
                if n.is_empty() {
                    return Ok(None);
                }
                Val::Sel(self.opt_id(db, prop, &n))
            }
            PropKind::MultiSelect => {
                let names: Vec<String> = match v {
                    Value::List(l) => l.borrow().iter().map(text_of).collect(),
                    other => vec![text_of(other)],
                };
                let mut ids = vec![];
                for n in names.into_iter().filter(|n| !n.is_empty()) {
                    let id = self.opt_id(db, prop, &n);
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                Val::Multi(ids)
            }
            PropKind::Formula | PropKind::Rollup => return Err(format!("property “{name}” is computed and cannot be set")),
            PropKind::Relation => return Err(format!("relation “{name}” cannot be set from a script yet")),
        }))
    }

    /// Option id by name (case-insensitive); the option is created when it does not exist yet.
    fn opt_id(&mut self, db: u64, prop: u64, name: &str) -> u64 {
        let found = self.ws.db(db).and_then(|d| d.prop(prop)).and_then(|p| p.opts.iter().find(|o| o.name.trim().eq_ignore_ascii_case(name.trim())).map(|o| o.id));
        if let Some(id) = found {
            return id;
        }
        let id = self.ws.id();
        if let Some(p) = self.ws.db_mut(db).and_then(|d| d.props.iter_mut().find(|p| p.id == prop)) {
            let color = p.opts.len() % 9;
            p.opts.push(Opt { id, name: name.trim().to_owned(), color });
        }
        id
    }

    fn row_get(&mut self, row: u64, key: &str) -> Result<Value, String> {
        let Some(page) = self.ws.page(row) else { return Err("this page no longer exists".into()) };
        match key {
            "id" => return Ok(Value::Num(row as f64)),
            "trashed" => return Ok(Value::Bool(page.trashed)),
            "icon" => return Ok(Value::str(&page.icon)),
            "text" | "content" => {
                let t: Vec<&str> = page.blocks.iter().map(|b| b.text.as_str()).collect();
                return Ok(Value::str(t.join("\n").trim_end()));
            }
            "db" | "database" => return Ok(self.db_of(row).map(db_ref).unwrap_or(Value::Null)),
            _ => {}
        }
        let Some(db) = self.db_of(row) else {
            return if key.eq_ignore_ascii_case("title") { Ok(Value::str(&page.title)) } else { Err(format!("“{key}” is not available: this page is not a database row")) };
        };
        match self.prop_of(db, key)? {
            None => Ok(Value::str(&page.title)),
            Some((pid, _)) => Ok(page.props.get(&pid).map(|v| self.to_script(db, pid, v)).unwrap_or_else(|| {
                if self.ws.db(db).and_then(|d| d.prop(pid)).map_or(false, |p| p.kind == PropKind::Checkbox) { Value::Bool(false) } else { Value::Null }
            })),
        }
    }

    fn row_set(&mut self, row: u64, key: &str, v: &Value) -> Result<(), String> {
        self.write_budget()?;
        if self.ws.page(row).is_none() {
            return Err("this page no longer exists".into());
        }
        if key == "icon" {
            let t = text_of(v);
            if let Some(p) = self.ws.page_mut(row) {
                p.icon = t;
            }
            return Ok(());
        }
        let db = self.db_of(row);
        let title = self.ws.display_title(row);
        let (pid, kind) = match db {
            Some(db) => match self.prop_of(db, key)? {
                Some(x) => x,
                None => (0, PropKind::Text),
            },
            None if key.eq_ignore_ascii_case("title") => (0, PropKind::Text),
            None => return Err(format!("“{key}” cannot be set: this page is not a database row")),
        };
        let newv = if pid == 0 { Some(Val::Text(text_of(v))) } else { self.from_script(db.unwrap_or(0), pid, kind, v)? };
        let dbid = db.unwrap_or(0);
        let shown = match &newv {
            Some(Val::Text(t)) => t.clone(),
            _ => text_of(v),
        };
        if au::set_val(self.ws, row, dbid, pid, newv, self.follow) {
            self.effect(format!("set {key} of “{title}” → {}", if shown.is_empty() { "(empty)".into() } else { shown }));
        }
        Ok(())
    }

    fn row_map(&mut self, row: u64) -> Result<Value, String> {
        let mut m = BTreeMap::new();
        m.insert("id".to_owned(), Value::Num(row as f64));
        m.insert("title".to_owned(), self.row_get(row, "title")?);
        if let Some(db) = self.db_of(row) {
            let names: Vec<(u64, String)> = self.ws.db(db).map(|d| d.props.iter().map(|p| (p.id, p.name.clone())).collect()).unwrap_or_default();
            for (pid, name) in names {
                let v = self.ws.page(row).and_then(|p| p.props.get(&pid)).map(|v| self.to_script(db, pid, v)).unwrap_or(Value::Null);
                m.insert(name, v);
            }
        }
        Ok(Value::map(m))
    }

    fn new_row(&mut self, db: u64, init: &Value) -> Result<Value, String> {
        self.created += 1;
        if self.created > MAX_CREATE {
            return Err(format!("a script can create at most {MAX_CREATE} rows per run"));
        }
        self.write_budget()?;
        let id = self.ws.add_row(db, None);
        let mut shown = String::new();
        let res: Result<(), String> = (|| {
            match init {
                Value::Str(s) => {
                    if let Some(p) = self.ws.page_mut(id) {
                        p.title = s.to_string();
                    }
                    shown = s.to_string();
                }
                Value::Map(m) => {
                    let entries: Vec<(String, Value)> = m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    for (k, v) in entries {
                        match self.prop_of(db, &k)? {
                            None => {
                                let t = text_of(&v);
                                shown = t.clone();
                                if let Some(p) = self.ws.page_mut(id) {
                                    p.title = t;
                                }
                            }
                            Some((pid, kind)) => {
                                if let Some(val) = self.from_script(db, pid, kind, &v)? {
                                    if let Some(p) = self.ws.page_mut(id) {
                                        p.props.insert(pid, val);
                                    }
                                }
                            }
                        }
                    }
                }
                Value::Null => {}
                _ => return Err("add() takes a title or a map like {title: \"…\", Status: \"Done\"}".into()),
            }
            Ok(())
        })();
        if let Err(e) = res {
            self.ws.pages.retain(|p| p.id != id); // never leave a half-built row behind
            return Err(e);
        }
        self.follow.push(Ev::Added { row: id, db });
        let dn = self.ws.display_title(db);
        self.effect(format!("created “{}” in {dn}", if shown.is_empty() { "Untitled" } else { &shown }));
        Ok(row_ref(id))
    }

    fn append(&mut self, page: u64, text: &str, todo: bool) -> Result<(), String> {
        self.write_budget()?;
        let kind = if todo { BlockKind::Todo } else { BlockKind::Text };
        let b = self.ws.blk(kind, text, 0);
        let now = self.ctx.now;
        let Some(p) = self.ws.page_mut(page) else { return Err("this page no longer exists".into()) };
        if let Some(last) = p.blocks.last_mut().filter(|l| l.kind == BlockKind::Text && l.text.is_empty()) {
            last.kind = kind;
            last.text = text.to_owned();
        } else {
            p.blocks.push(b);
        }
        p.updated = now;
        let t = self.ws.display_title(page);
        self.effect(format!("added to “{t}”: {text}"));
        Ok(())
    }

    fn arg_str(args: &[Value], i: usize, what: &str) -> Result<String, String> {
        match args.get(i) {
            Some(Value::Str(s)) => Ok(s.to_string()),
            Some(Value::Num(n)) => Ok(num_text(*n)),
            _ => Err(format!("{what} is required (text)")),
        }
    }

    fn iso_arg(args: &[Value], i: usize) -> Result<i64, String> {
        let s = Self::arg_str(args, i, "a date")?;
        dates::parse(&s).map(dates::day_of).ok_or_else(|| format!("“{s}” is not a date — use YYYY-MM-DD"))
    }

    fn row_arg(&self, v: Option<&Value>) -> Result<u64, String> {
        match v {
            Some(Value::Host(h)) if h.kind == ROW => Ok(h.id),
            _ => Err("expected a row".into()),
        }
    }
}

const FUNCS: [&str; 13] = ["notify", "db", "dbs", "today", "now", "date_add", "date_diff", "date_format", "weekday", "days_until", "create_page", "search", "is_date"];

impl<'a> Host for SH<'a> {
    fn has_fn(&self, name: &str) -> bool {
        FUNCS.contains(&name)
    }

    fn global(&mut self, name: &str) -> Option<Value> {
        match name {
            "row" => Some(self.ctx.row.map(row_ref).unwrap_or(Value::Null)),
            "page" => Some(self.ctx.page.or(self.ctx.row).map(row_ref).unwrap_or(Value::Null)),
            "trigger" => {
                let mut m = BTreeMap::new();
                m.insert("source".to_owned(), Value::str(&self.ctx.source));
                m.insert(
                    "kind".to_owned(),
                    Value::str(match self.ctx.kind {
                        NoticeKind::Button => "button",
                        NoticeKind::Rule => "rule",
                        NoticeKind::Reminder => "reminder",
                        NoticeKind::Repeat => "repeat",
                        NoticeKind::System => "manual",
                    }),
                );
                m.insert("has_row".to_owned(), Value::Bool(self.ctx.row.is_some()));
                Some(Value::map(m))
            }
            _ => None,
        }
    }

    fn call(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "notify" => {
                self.notified += 1;
                if self.notified > MAX_NOTIFY {
                    return Err(format!("a script can send at most {MAX_NOTIFY} notifications per run"));
                }
                let m = Self::arg_str(args, 0, "notify() needs a message")?;
                let page = self.ctx.row.or(self.ctx.page);
                au::push_notice(self.ws, self.out, &self.ctx.source.clone(), &m, page, self.ctx.kind, self.ctx.now);
                self.effect(format!("notified “{m}”"));
                Ok(Value::Null)
            }
            "db" => {
                let n = Self::arg_str(args, 0, "db() needs the database name")?;
                Ok(db_ref(self.find_db(&n)?))
            }
            "dbs" => Ok(Value::list(self.databases().into_iter().map(|(_, id)| db_ref(id)).collect())),
            "today" => Ok(Value::str(dates::today_iso())),
            "now" => {
                let m = dates::now_local_min().rem_euclid(1440) as u16;
                Ok(Value::str(format!("{} {}", dates::today_iso(), dates::hhmm(m))))
            }
            "date_add" => {
                let d = Self::iso_arg(args, 0)?;
                let n = args.get(1).and_then(|v| v.as_f64()).ok_or("date_add(date, days) needs a number of days")?;
                Ok(Value::str(dates::iso_of_day(d + n as i64)))
            }
            "date_diff" => Ok(Value::Num((Self::iso_arg(args, 1)? - Self::iso_arg(args, 0)?) as f64)),
            "days_until" => Ok(Value::Num((Self::iso_arg(args, 0)? - dates::today_day()) as f64)),
            "weekday" => {
                let d = if args.is_empty() { dates::today_day() } else { Self::iso_arg(args, 0)? };
                Ok(Value::str(weekday_name(d)))
            }
            "date_format" => {
                let s = Self::arg_str(args, 0, "a date")?;
                Ok(Value::str(dates::pretty(&s)))
            }
            "is_date" => Ok(Value::Bool(args.first().and_then(|v| v.as_str()).map_or(false, |s| dates::parse(s).is_some()))),
            "create_page" => {
                self.created += 1;
                if self.created > MAX_CREATE {
                    return Err(format!("a script can create at most {MAX_CREATE} pages per run"));
                }
                self.write_budget()?;
                let title = Self::arg_str(args, 0, "create_page() needs a title")?;
                let cur = self.ws.current;
                let id = self.ws.add_page(None, PageKind::Doc);
                self.ws.current = cur;
                let body = args.get(1).map(text_of).unwrap_or_default();
                let blocks: Vec<_> = body.lines().take(400).map(|l| self.ws.blk(BlockKind::Text, l, 0)).collect();
                if let Some(p) = self.ws.page_mut(id) {
                    p.title = title.clone();
                    if !blocks.is_empty() {
                        p.blocks = blocks;
                    }
                }
                self.effect(format!("created page “{title}”"));
                Ok(row_ref(id))
            }
            "search" => {
                let q = Self::arg_str(args, 0, "search() needs text")?.to_lowercase();
                let hits: Vec<Value> = self.ws.pages.iter().filter(|p| !p.trashed && !q.is_empty() && p.title.to_lowercase().contains(&q)).take(100).map(|p| row_ref(p.id)).collect();
                Ok(Value::list(hits))
            }
            _ => Err(format!("unknown function {name}")),
        }
    }

    fn get(&mut self, o: HostRef, key: &str) -> Result<Value, String> {
        match o.kind {
            ROW => self.row_get(o.id, key),
            _ => {
                let Some(d) = self.ws.db(o.id) else { return Err("this database no longer exists".into()) };
                match key {
                    "id" => Ok(Value::Num(o.id as f64)),
                    "name" | "title" => Ok(Value::str(self.ws.display_title(o.id))),
                    "count" => Ok(Value::Num(self.ws.rows(o.id).len() as f64)),
                    "props" | "columns" | "properties" => Ok(Value::list(d.props.iter().map(|p| Value::str(&p.name)).collect())),
                    _ => Err(format!("a database has no property “{key}” (try .rows(), .add(), .find(), .name, .count)")),
                }
            }
        }
    }

    fn set(&mut self, o: HostRef, key: &str, v: Value) -> Result<(), String> {
        match o.kind {
            ROW => self.row_set(o.id, key, &v),
            _ => Err("databases are read-only (use .add() to create rows)".into()),
        }
    }

    fn method(&mut self, o: HostRef, name: &str, args: &[Value]) -> Result<Value, String> {
        if o.kind == ROW {
            let row = o.id;
            if self.ws.page(row).is_none() {
                return Err("this page no longer exists".into());
            }
            return match name {
                "get" => {
                    let k = Self::arg_str(args, 0, "get() needs a property name")?;
                    match self.row_get(row, &k) {
                        Ok(Value::Null) | Err(_) if args.len() > 1 => Ok(args[1].clone()),
                        other => other,
                    }
                }
                "set" => {
                    let k = Self::arg_str(args, 0, "set() needs a property name")?;
                    self.row_set(row, &k, args.get(1).unwrap_or(&Value::Null))?;
                    Ok(Value::Null)
                }
                "set_all" | "update" => {
                    if let Some(Value::Map(m)) = args.first() {
                        let entries: Vec<(String, Value)> = m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in entries {
                            self.row_set(row, &k, &v)?;
                        }
                    }
                    Ok(Value::Null)
                }
                "props" | "to_map" => self.row_map(row),
                "has" => {
                    let k = Self::arg_str(args, 0, "has() needs a property name")?;
                    Ok(Value::Bool(self.db_of(row).map_or(false, |db| self.prop_of(db, &k).is_ok())))
                }
                "trash" | "delete" => {
                    self.write_budget()?;
                    let t = self.ws.display_title(row);
                    self.ws.set_trashed(row, true);
                    self.effect(format!("moved “{t}” to the trash"));
                    Ok(Value::Null)
                }
                "restore" => {
                    self.ws.set_trashed(row, false);
                    Ok(Value::Null)
                }
                "append" | "append_todo" => {
                    let t = Self::arg_str(args, 0, "append() needs text")?;
                    self.append(row, &t, name == "append_todo")?;
                    Ok(Value::Null)
                }
                "duplicate" => {
                    self.created += 1;
                    if self.created > MAX_CREATE {
                        return Err(format!("a script can create at most {MAX_CREATE} rows per run"));
                    }
                    self.write_budget()?;
                    let Some(mut copy) = self.ws.page(row).cloned() else { return Err("this page no longer exists".into()) };
                    let nid = self.ws.id();
                    copy.id = nid;
                    copy.title = if copy.title.is_empty() { "Untitled copy".into() } else { format!("{} (copy)", copy.title) };
                    copy.favorite = false;
                    copy.history.clear();
                    copy.updated = self.ctx.now;
                    copy.repeats.clear();
                    for b in copy.blocks.iter_mut() {
                        b.id = self.ws.id();
                    }
                    let pdb = copy.parent.unwrap_or(0);
                    self.ws.pages.push(copy);
                    self.follow.push(Ev::Added { row: nid, db: pdb });
                    self.effect(format!("duplicated “{}”", self.ws.display_title(row)));
                    Ok(row_ref(nid))
                }
                "move_to" => {
                    let n = Self::arg_str(args, 0, "move_to() needs a database name")?;
                    let target = self.find_db(&n)?;
                    self.write_budget()?;
                    if au::move_row(self.ws, row, target) {
                        self.follow.push(Ev::Added { row, db: target });
                        let t = self.ws.display_title(row);
                        self.effect(format!("moved “{t}” to {}", self.ws.display_title(target)));
                        Ok(Value::Bool(true))
                    } else {
                        Ok(Value::Bool(false))
                    }
                }
                _ => Err(format!("a row has no method “{name}” (try get, set, props, append, trash, duplicate, move_to)")),
            };
        }
        // database
        let db = o.id;
        if self.ws.db(db).is_none() {
            return Err("this database no longer exists".into());
        }
        match name {
            "rows" | "all" => Ok(Value::list(self.ws.rows(db).into_iter().map(row_ref).collect())),
            "count" => Ok(Value::Num(self.ws.rows(db).len() as f64)),
            "add" | "create" | "new_row" => self.new_row(db, args.first().unwrap_or(&Value::Null)),
            "find" | "where" => {
                let k = Self::arg_str(args, 0, &format!("{name}() needs a property name"))?;
                let want = args.get(1).cloned().unwrap_or(Value::Null);
                let mut hits = vec![];
                for r in self.ws.rows(db) {
                    let v = self.row_get(r, &k)?;
                    let eq = match (&v, &want) {
                        (Value::Str(a), Value::Str(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
                        (Value::Num(a), Value::Num(b)) => a == b,
                        (Value::Bool(a), Value::Bool(b)) => a == b,
                        (Value::Null, Value::Null) => true,
                        (Value::List(l), w) => l.borrow().iter().any(|x| matches!((x, w), (Value::Str(a), Value::Str(b)) if a.eq_ignore_ascii_case(b))),
                        _ => false,
                    };
                    if eq {
                        hits.push(row_ref(r));
                        if name == "find" {
                            return Ok(hits.remove(0));
                        }
                    }
                }
                Ok(if name == "find" { Value::Null } else { Value::list(hits) })
            }
            "options" => {
                let k = Self::arg_str(args, 0, "options() needs a property name")?;
                let (pid, _) = self.prop_of(db, &k)?.ok_or("the title has no options")?;
                Ok(Value::list(self.ws.db(db).and_then(|d| d.prop(pid)).map(|p| p.opts.iter().map(|o| Value::str(&o.name)).collect()).unwrap_or_default()))
            }
            "contains_row" => {
                let r = self.row_arg(args.first())?;
                Ok(Value::Bool(self.db_of(r) == Some(db)))
            }
            _ => Err(format!("a database has no method “{name}” (try rows, add, find, where, options, count)")),
        }
    }

    fn display(&mut self, o: HostRef) -> String {
        match o.kind {
            ROW => self.ws.display_title(o.id),
            _ => format!("database {}", self.ws.display_title(o.id)),
        }
    }
}

// ───────────────────────────── documentation & examples ─────────────────────────────

pub struct Doc {
    pub title: &'static str,
    pub items: &'static [(&'static str, &'static str)],
}

pub const API: &[Doc] = &[
    Doc {
        title: "Language",
        items: &[
            ("let x = 1   const y = \"a\"", "Variables. Statements end at a line break (or ;). Comments: // … and /* … */"),
            ("\"Hi {row.title}, {1 + 2}\"", "Double-quoted strings interpolate {expressions}. Single quotes are literal. \\{ writes a brace."),
            ("if a { … } else if b { … } else { … }", "Conditions need no parentheses. Falsy: null, false, 0, \"\". Operators: && || ! ?? ?: == != < <= > >= + - * / % **"),
            ("for x in list { … }   for i, x in list { … }   for k, v in map { … }", "Loops. 1..5 is the list 1,2,3,4. while cond { … }, break, continue."),
            ("fn add(a, b) { return a + b }   let f = |x| x * 2", "Functions and lambdas (closures). Recursion depth is limited."),
            ("try { … } catch e { … }   throw \"msg\"", "Error handling. Runtime errors can be caught; the step budget cannot."),
            ("[1, 2]   {a: 1, \"b c\": 2}   m.a   m[\"b c\"]   x?.y", "Lists, maps, optional chaining. Negative list indexes count from the end."),
        ],
    },
    Doc {
        title: "Workspace",
        items: &[
            ("row", "The row that triggered the rule / holds the button (null in schedules and manual runs)."),
            ("page", "The page holding the button, or the row."),
            ("trigger", "{source, kind, has_row} — what started the script."),
            ("db(\"Tasks\")", "A database by name (case-insensitive). Throws a helpful error listing your databases."),
            ("dbs()", "All databases."),
            ("database.rows()", "All rows (not trashed). Also .count(), .name, .props, .options(\"Status\")."),
            ("database.add({title: \"x\", Status: \"To do\"})", "Create a row; missing select options are created. Also .add(\"title\")."),
            ("database.find(\"Status\", \"Done\")", "First row whose property equals the value; .where(…) returns all of them."),
            ("row.title   row.Status   row[\"Due date\"]", "Read properties by name. Select → option name, multi-select → list, checkbox → bool, date → \"YYYY-MM-DD\"."),
            ("row.Status = \"Done\"   row.set(\"Done\", true)", "Write properties. Dates must be YYYY-MM-DD, numbers numeric. null clears the value."),
            ("row.update({Status: \"Done\", Priority: 2})", "Write several properties at once."),
            ("row.props()", "All properties as a map. row.get(\"x\", default) reads with a fallback."),
            ("row.append(\"text\")   row.append_todo(\"text\")", "Add a text / to-do block to the page."),
            ("row.duplicate()   row.move_to(\"Archive\")   row.trash()", "Duplicate (returns the copy), move to another database, move to the trash."),
            ("create_page(\"Title\", \"line1\\nline2\")", "New top-level page with optional body text. search(\"text\") finds pages by title."),
            ("notify(\"message\")", "Send a notification (inbox + desktop / push). At most 25 per run."),
        ],
    },
    Doc {
        title: "Dates",
        items: &[
            ("today()   now()", "\"2025-01-31\"  /  \"2025-01-31 14:05\" (local time)."),
            ("date_add(\"2025-01-31\", 7)", "Add days (negative subtracts)."),
            ("date_diff(a, b)   days_until(d)", "Days from a to b / from today to d (negative = in the past)."),
            ("weekday(d)   date_format(d)   is_date(s)", "\"Monday\"; \"Jan 31, 2025\"; validity check."),
        ],
    },
    Doc {
        title: "Lists, maps, text, numbers",
        items: &[
            ("list.map(f) .filter(f) .find(f) .some(f) .every(f) .reduce(f, init) .for_each(f)", "Higher-order helpers; f receives (item, index)."),
            ("list.sort() .sort(|a, b| a - b) .sort_by(|x| x.Due) .reverse() .unique() .group_by(f)", "Sorting and grouping (sorting returns a new list). Empty values sort last."),
            ("list.push(x) .pop() .shift() .remove(i) .slice(a, b) .concat(l) .flat() .join(\", \") .contains(x) .index_of(x)", "List editing and queries. .sum() .min() .max() .count(f) .sum_by(f) .take(n) .skip(n)."),
            ("map.keys() .values() .entries() .has(k) .get(k, d) .set(k, v) .remove(k) .merge(m)", "Maps. Also keys(m), values(m), entries(m)."),
            ("s.upper() .lower() .trim() .split(\",\") .replace(a, b) .contains(x) .starts_with(x) .slice(a, b)", "Text. Also .pad_left(n, \"0\") .repeat(n) .index_of(x) .capitalize() .to_num() .lines()."),
            ("len(x) str(x) num(x) int(x) bool(x) type(x) range(n) json(x)", "Conversions. Math: abs floor ceil round(x, digits) sqrt pow min max clamp sum."),
            ("print(x, …)   assert(cond, \"msg\")   error(\"msg\")", "print writes to the console of a test run and to the Activity log."),
        ],
    },
    Doc {
        title: "Safety limits",
        items: &[
            ("400 000 steps per run", "Endless loops are stopped with a clear error. Recursion depth 40."),
            ("200 rows / 3 000 changes / 25 notifications", "Per run. A script cannot flood your workspace."),
            ("No files, network, clipboard or OS access", "The only way out of the sandbox is the functions listed here."),
            ("Test button", "Runs the script on a copy of your workspace and lists what it WOULD change — nothing is saved."),
        ],
    },
];

pub struct Example {
    pub name: &'static str,
    pub desc: &'static str,
    pub code: &'static str,
}

pub const EXAMPLES: &[Example] = &[
    Example {
        name: "Weekly review page",
        desc: "Creates a page that lists everything overdue.",
        code: "// Lists every unfinished task that is past its date\nlet lines = []\nfor d in dbs() {\n    if !d.props.contains(\"Date\") || !d.props.contains(\"Status\") { continue }\n    for r in d.rows() {\n        if r.Date != null && r.Date < today() && r.Status != \"Done\" {\n            lines.push(\"[ ] {r.title} — due {date_format(r.Date)} ({d.name})\")\n        }\n    }\n}\ncreate_page(\"Review {today()}\", \"Overdue items:\\n\" + lines.join(\"\\n\"))\nnotify(\"{lines.len()} overdue item(s) collected\")\n",
    },
    Example {
        name: "Status report",
        desc: "Counts rows per status and notifies the summary.",
        code: "// Change \"Tasks\" to the name of your database\nlet tasks = db(\"Tasks\")\nlet counts = tasks.rows().group_by(|r| r.Status ?? \"No status\")\nlet parts = []\nfor status, rows in counts { parts.push(\"{status}: {rows.len()}\") }\nprint(parts.join(\"\\n\"))\nnotify(parts.join(\" · \"))\n",
    },
    Example {
        name: "Follow-up for a finished row",
        desc: "Use as a rule action: when Status becomes Done, schedule a check-in a week later.",
        code: "// Rule action — `row` is the row that changed\nif row.Status == \"Done\" {\n    row.Completed = today()\n    row.db.add({title: \"Check-in: {row.title}\", Date: date_add(today(), 7)})\n    notify(\"Follow-up created for {row.title}\")\n}\n",
    },
    Example {
        name: "Bulk clean-up",
        desc: "Trims titles and fills an empty Status for every row of a database.",
        code: "let d = db(\"Tasks\")\nlet fixed = 0\nfor r in d.rows() {\n    let t = r.title.trim()\n    if t != r.title { r.title = t; fixed += 1 }\n    if r.Status == null { r.Status = \"Not started\"; fixed += 1 }\n}\nprint(\"fixed {fixed} value(s)\")\n",
    },
    Example {
        name: "Checklist from a list",
        desc: "Appends a to-do block per item to the current page or row.",
        code: "let steps = [\"Draft\", \"Review\", \"Publish\"]\nfor s in steps { page.append_todo(s) }\n",
    },
];

/// Compact description of the language and its API — meant to be pasted into (or sent to) an AI assistant that writes scripts.
pub fn api_markdown() -> String {
    let mut s = String::from(
        "You write automation scripts for Lumen (a Notion-like workspace) in LumenScript, a small sandboxed JavaScript/Python-like language.\n\
Reply with ONLY the script in one code block, no explanations. Use the exact property and database names the user gives you.\n\
Scripts run inside a rule (a `row` exists), from a button (`row` / `page` exist) or manually (no row).\n\n",
    );
    for d in API {
        s.push_str(&format!("## {}\n", d.title));
        for (sig, desc) in d.items {
            s.push_str(&format!("- `{sig}` — {desc}\n"));
        }
        s.push('\n');
    }
    s.push_str("## Example\n```\n");
    s.push_str(EXAMPLES[2].code);
    s.push_str("```\n");
    s
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::model::Workspace;

    fn ws_with_tasks() -> (Workspace, u64) {
        let mut ws = Workspace::seed();
        let db = ws.add_database(None, crate::model::ViewKind::Table);
        if let Some(p) = ws.page_mut(db) {
            p.title = "Tasks".into();
        }
        (ws, db)
    }

    fn run(ws: &mut Workspace, code: &str, row: Option<u64>) -> Report {
        let ctx = ScriptCtx { row, page: row, db: 0, source: "test".into(), kind: NoticeKind::Rule, now: 1000.0 };
        let mut out = Output::default();
        let mut follow = vec![];
        run_script(ws, code, &ctx, &mut out, &mut follow)
    }

    #[test]
    fn scripts_create_read_and_write_rows() {
        let (mut ws, db) = ws_with_tasks();
        let before = ws.rows(db).len();
        let r = run(&mut ws, "let t = db(\"tasks\")\nlet n = t.add({title: \"Write docs\", Status: \"In progress\", Date: date_add(today(), 3)})\nprint(n.title, n.Status, t.count())\nn.Status = \"Done\"\nprint(db(\"Tasks\").find(\"Status\", \"Done\").title)", None);
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.output[1], "Write docs");
        assert_eq!(ws.rows(db).len(), before + 1);
        assert!(r.effects.iter().any(|e| e.contains("created")));
    }

    #[test]
    fn errors_are_helpful_and_atomic() {
        let (mut ws, db) = ws_with_tasks();
        let before = ws.rows(db).len();
        let r = run(&mut ws, "db(\"Tasks\").add({title: \"x\", Date: \"not a date\"})", None);
        assert!(r.error.as_ref().map_or(false, |e| e.msg.contains("not a date")), "{:?}", r.error);
        assert_eq!(ws.rows(db).len(), before, "failed add must not leave a row behind");
        let r = run(&mut ws, "db(\"Nope\")", None);
        assert!(r.error.unwrap().msg.contains("Tasks"));
        let r = run(&mut ws, "let r = db(\"Tasks\").rows()[0]\nr.Missing = 1", None);
        assert!(r.error.unwrap().msg.contains("available"));
    }

    #[test]
    fn limits_hold() {
        let (mut ws, db) = ws_with_tasks();
        let r = run(&mut ws, "for i in 0..500 { db(\"Tasks\").add(\"r{i}\") }", None);
        assert!(r.error.unwrap().msg.contains("at most"));
        assert!(ws.rows(db).len() <= 200 + 3);
        let r = run(&mut ws, "while true { }", None);
        assert!(r.error.unwrap().msg.contains("steps"));
    }

    #[test]
    fn dry_run_changes_nothing() {
        let (ws, db) = ws_with_tasks();
        let ctx = ScriptCtx::default();
        let r = dry_run(&ws, "db(\"Tasks\").add(\"ghost\")", &ctx);
        assert!(r.error.is_none());
        assert!(!r.effects.is_empty());
        assert!(ws.rows(db).iter().all(|id| ws.page(*id).map_or(true, |p| p.title != "ghost")));
    }

    #[test]
    fn docs_examples_parse() {
        for e in EXAMPLES {
            script::check(e.code).unwrap_or_else(|er| panic!("{}: {er}", e.name));
        }
        assert!(api_markdown().contains("LumenScript"));
    }
}
