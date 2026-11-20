//! Large datasets: a database whose rows live in a disk-backed [`lumen_store::Store`] instead of `Workspace.pages`.
//!
//! The page (schema, views, title) stays a normal database page; `Workspace.bigs[page id]` points at the store directory. Only the
//! windowed table (`bigui.rs`) and the dashboards (`bigdata.rs`) read the rows, so nothing about a million rows is in RAM
//! except the chunk cache (budget: `memgov`). Native / Android keep the data in `<data dir>/Lumen/tables/<dir>`; in the browser
//! the store is memory-backed for the session (`BigMeta.volatile`) — export CSV to keep it.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lumen_store::{chunk_ids, chunk_titles, col_check, col_num, col_sel, col_text, shared_scan, Row, Store, Value};

use crate::dashcfg::{Agg, Bucket, Cfg};
use crate::dashdata::{Dataset, Num, Scatter, Ser};
use crate::model::{BigMeta, Database, PropKind, ViewKind, Workspace};
use crate::theme::tag_color;

/// A CSV with at least this many lines is imported as a large dataset.
pub const BIG_ROWS: usize = 20_000;

pub type Shared = Arc<Mutex<Store>>;

static REG: Mutex<Option<HashMap<String, Shared>>> = Mutex::new(None);
static VERSIONS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(not(target_arch = "wasm32"))]
fn root() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = std::env::var_os("LUMEN_TABLES_DIR").map(PathBuf::from).or_else(|| {
        let b = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))?;
        Some(b.join("Lumen").join("tables"))
    })?;
    std::fs::create_dir_all(&base).ok()?;
    Some(base)
}

#[cfg(not(target_arch = "wasm32"))]
fn open_new(dir: &str, budget: usize, _create: bool) -> Option<Store> {
    Store::open_dir(root()?.join(dir), budget).ok()
}

#[cfg(target_arch = "wasm32")]
fn open_new(_dir: &str, budget: usize, create: bool) -> Option<Store> {
    if create {
        Store::open(Box::new(lumen_store::MemSource::default()), budget).ok()
    } else {
        None
    }
}

/// The store behind a dataset (opened once, shared by the UI and the dashboard workers).
pub fn open_store(meta: &BigMeta, create: bool) -> Option<Shared> {
    let mut g = REG.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(s) = map.get(&meta.dir) {
        return Some(s.clone());
    }
    let st = open_new(&meta.dir, crate::memgov::row_cache_bytes(), create)?;
    let a = Arc::new(Mutex::new(st));
    map.insert(meta.dir.clone(), a.clone());
    Some(a)
}

pub fn store(ws: &Workspace, db: u64) -> Option<Shared> {
    ws.bigs.get(&db).and_then(|m| open_store(m, false))
}

/// Data version of a dataset (changes with every edit): the cache key of dashboard results.
pub fn version(dir: &str) -> u64 {
    VERSIONS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(dir).copied())).unwrap_or(0)
}

pub fn bump_version(dir: &str) {
    if let Ok(mut g) = VERSIONS.lock() {
        *g.get_or_insert_with(HashMap::new).entry(dir.to_owned()).or_default() += 1;
    }
    crate::bigdata::bump();
}

pub fn set_budget_all(bytes: usize) {
    if let Ok(g) = REG.lock() {
        if let Some(m) = g.as_ref() {
            for s in m.values() {
                let mut st = lock(s);
                if st.budget() != bytes {
                    st.set_budget(bytes);
                }
            }
        }
    }
}

/// Folds pending edits of every open table into the chunks (called when the workspace is saved).
pub fn flush_all() {
    if let Ok(g) = REG.lock() {
        if let Some(m) = g.as_ref() {
            for s in m.values() {
                let _ = lock(s).checkpoint();
            }
        }
    }
}

// ───────────────────────────── CSV ─────────────────────────────

/// Streaming CSV reader: one record at a time, never the whole file as a `Vec<Vec<String>>`.
pub struct Csv<'a> {
    it: std::iter::Peekable<std::str::Chars<'a>>,
    delim: char,
}

impl<'a> Csv<'a> {
    pub fn new(text: &'a str) -> Csv<'a> {
        let text = text.trim_start_matches('\u{feff}');
        let first = text.lines().next().unwrap_or("");
        let delim = if first.matches('\t').count() > first.matches(',').count() { '\t' } else if first.matches(';').count() > first.matches(',').count() { ';' } else { ',' };
        Csv { it: text.chars().peekable(), delim }
    }
}

impl Iterator for Csv<'_> {
    type Item = Vec<String>;
    fn next(&mut self) -> Option<Vec<String>> {
        let mut row: Vec<String> = vec![];
        let mut cell = String::new();
        let mut q = false;
        while let Some(c) = self.it.next() {
            if q {
                if c == '"' {
                    if self.it.peek() == Some(&'"') {
                        cell.push('"');
                        self.it.next();
                    } else {
                        q = false;
                    }
                } else {
                    cell.push(c);
                }
            } else if c == '"' {
                q = true;
            } else if c == self.delim {
                row.push(std::mem::take(&mut cell));
            } else if c == '\n' || c == '\r' {
                if c == '\r' && self.it.peek() == Some(&'\n') {
                    self.it.next();
                }
                row.push(std::mem::take(&mut cell));
                if row.iter().any(|c| !c.is_empty()) {
                    return Some(row);
                }
                row.clear();
            } else {
                cell.push(c);
            }
        }
        if !cell.is_empty() || !row.is_empty() {
            row.push(cell);
            if row.iter().any(|c| !c.is_empty()) {
                return Some(row);
            }
        }
        None
    }
}

struct ColStat {
    nonempty: usize,
    num: bool,
    boolean: bool,
    date: bool,
    url: bool,
    distinct: BTreeSet<String>,
    overflow: bool,
}

fn is_bool(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "x" | "✓")
}

fn kind_of(s: &ColStat) -> PropKind {
    if s.nonempty == 0 {
        PropKind::Text
    } else if s.num {
        PropKind::Number
    } else if s.boolean {
        PropKind::Checkbox
    } else if s.date {
        PropKind::Date
    } else if s.url {
        PropKind::Url
    } else if !s.overflow && s.distinct.len() <= 24 && s.nonempty >= s.distinct.len() * 4 {
        PropKind::Select
    } else {
        PropKind::Text
    }
}

fn new_meta(id: u64) -> BigMeta {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    BigMeta { dir: format!("t{id}-{:x}-{n}", crate::auth::now_ms() as u64), volatile: cfg!(target_arch = "wasm32") }
}

fn finish(ws: &mut Workspace, id: u64, name: &str, meta: BigMeta) {
    ws.bigs.insert(id, meta);
    if let Some(p) = ws.page_mut(id) {
        p.title = name.to_owned();
        p.icon = "📊".into();
    }
    crate::bigdata::bump();
}

/// Imports a CSV as a large dataset (two streaming passes: infer column types, then write chunks). Column 0 is the title.
pub fn import_csv(ws: &mut Workspace, name: &str, text: &str, parent: Option<u64>) -> Option<u64> {
    let mut header_it = Csv::new(text);
    let header = header_it.next()?;
    if header.len() < 2 {
        return None;
    }
    let nc = header.len();
    let mut stats: Vec<ColStat> = (1..nc).map(|_| ColStat { nonempty: 0, num: true, boolean: true, date: true, url: true, distinct: BTreeSet::new(), overflow: false }).collect();
    let mut total = 0usize;
    for rec in Csv::new(text).skip(1) {
        total += 1;
        for (k, st) in stats.iter_mut().enumerate() {
            let v = rec.get(k + 1).map(|s| s.trim()).unwrap_or("");
            if v.is_empty() {
                continue;
            }
            st.nonempty += 1;
            if st.num && v.parse::<f64>().is_err() {
                st.num = false;
            }
            if st.boolean && !is_bool(v) {
                st.boolean = false;
            }
            if st.date && crate::dates::parse(v).is_none() {
                st.date = false;
            }
            if st.url && !(v.starts_with("http://") || v.starts_with("https://")) {
                st.url = false;
            }
            if !st.overflow && !st.distinct.contains(v) {
                if st.distinct.len() >= 25 {
                    st.overflow = true;
                    st.distinct.clear();
                } else {
                    st.distinct.insert(v.to_owned());
                }
            }
        }
    }
    if total == 0 {
        return None;
    }
    let id = ws.add_database(parent, ViewKind::Table);
    ws.pages.retain(|p| !(p.row && p.parent == Some(id)));
    if let Some(d) = ws.db_mut(id) {
        d.props.clear();
    }
    let mut cols: Vec<(u32, PropKind, HashMap<String, u64>)> = vec![];
    for (k, st) in stats.iter().enumerate() {
        let kind = kind_of(st);
        let opts: Vec<&str> = if kind == PropKind::Select { st.distinct.iter().map(|s| s.as_str()).collect() } else { vec![] };
        let h = header[k + 1].trim();
        let pid = ws.add_prop(id, if h.is_empty() { "Column" } else { h }, kind, &opts);
        let mut map = HashMap::new();
        if kind == PropKind::Select {
            for o in &opts {
                if let Some(oid) = ws.opt_id(id, pid, o) {
                    map.insert((*o).to_owned(), oid);
                }
            }
        }
        cols.push((pid as u32, kind, map));
    }
    let meta = new_meta(id);
    let shared = open_store(&meta, true)?;
    let rows = Csv::new(text).skip(1).enumerate().map(|(i, rec)| {
        let mut props: Vec<(u32, Value)> = Vec::with_capacity(cols.len());
        for (k, (pid, kind, map)) in cols.iter().enumerate() {
            let s = rec.get(k + 1).map(|s| s.trim()).unwrap_or("");
            if s.is_empty() {
                continue;
            }
            let v = match kind {
                PropKind::Number => s.parse::<f64>().ok().map(Value::Num),
                PropKind::Checkbox => Some(Value::Check(matches!(s.to_lowercase().as_str(), "true" | "yes" | "x" | "✓"))),
                PropKind::Select => map.get(s).map(|o| Value::Sel(*o)),
                _ => Some(Value::Text(s.to_owned())),
            };
            if let Some(v) = v {
                props.push((*pid, v));
            }
        }
        props.sort_by_key(|p| p.0);
        Row { id: i as u64 + 1, title: rec.first().cloned().unwrap_or_default(), updated: crate::auth::now_ms() as u64, props }
    });
    lock(&shared).bulk_append(rows).ok()?;
    finish(ws, id, name, meta);
    Some(id)
}

/// A deterministic demo dataset (statuses, owners, amounts, dates, notes) for trying the large-data engine.
pub fn create_sample(ws: &mut Workspace, name: &str, n: u64, parent: Option<u64>) -> Option<u64> {
    let id = ws.add_database(parent, ViewKind::Table);
    ws.pages.retain(|p| !(p.row && p.parent == Some(id)));
    if let Some(d) = ws.db_mut(id) {
        d.props.clear();
    }
    let status = ws.add_prop(id, "Status", PropKind::Select, &["Backlog", "Todo", "In progress", "Review", "Done"]) as u32;
    let owners = ["Ava", "Noor", "Ravi", "Mei", "Tariq", "Lena", "Omar", "Sofia"];
    let owner = ws.add_prop(id, "Owner", PropKind::Select, &owners) as u32;
    let amount = ws.add_prop(id, "Amount", PropKind::Number, &[]) as u32;
    let score = ws.add_prop(id, "Score", PropKind::Number, &[]) as u32;
    let due = ws.add_prop(id, "Due", PropKind::Date, &[]) as u32;
    let done = ws.add_prop(id, "Done", PropKind::Checkbox, &[]) as u32;
    let note = ws.add_prop(id, "Notes", PropKind::Text, &[]) as u32;
    let opt = |p: u32, name: &str| ws.opt_id(id, p as u64, name).unwrap_or(0);
    let st_ids: Vec<u64> = ["Backlog", "Todo", "In progress", "Review", "Done"].iter().map(|s| opt(status, s)).collect();
    let ow_ids: Vec<u64> = owners.iter().map(|s| opt(owner, s)).collect();
    let base = crate::dates::today_day() - 700;
    let words = ["billing", "login", "export", "sync", "search", "mobile", "chart", "import", "report", "emoji", "pdf", "canvas"];
    let meta = new_meta(id);
    let shared = open_store(&meta, true)?;
    let rows = (1..=n).map(|i| {
        let h = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i >> 7);
        let s = (h % 5) as usize;
        let mut props = vec![
            (status, Value::Sel(st_ids[s])),
            (owner, Value::Sel(ow_ids[((h >> 8) % 8) as usize])),
            (amount, Value::Num(((h >> 16) % 100_000) as f64 / 10.0)),
            (score, Value::Num(((h >> 24) % 101) as f64)),
            (due, Value::Text(crate::dates::iso_of_day(base + ((h >> 32) % 1400) as i64))),
            (done, Value::Check(s == 4)),
        ];
        if h % 3 != 0 {
            props.push((note, Value::Text(format!("{} {} follow-up", words[((h >> 40) % 12) as usize], words[((h >> 48) % 12) as usize]))));
        }
        props.sort_by_key(|p| p.0);
        Row { id: i, title: format!("Task {i} — {}", words[(h % 12) as usize]), updated: 1_700_000_000_000 + i, props }
    });
    lock(&shared).bulk_append(rows).ok()?;
    finish(ws, id, name, meta);
    Some(id)
}

pub fn cell_text(db: &Database, prop: u32, v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Num(x) => crate::dashdata::fmt_num(*x),
        Value::Sel(o) => db.opt(prop as u64, *o).map(|o| o.name.clone()).unwrap_or_default(),
        Value::Multi(m) => m.iter().filter_map(|o| db.opt(prop as u64, *o)).map(|o| o.name.clone()).collect::<Vec<_>>().join("; "),
        Value::Check(b) => if *b { "true" } else { "false" }.to_owned(),
    }
}

/// The whole table as CSV (streamed chunk by chunk).
pub fn export_csv(ws: &Workspace, id: u64) -> Option<String> {
    let st = store(ws, id)?;
    let db = ws.db(id)?;
    let props: Vec<&crate::model::Prop> = db.props.iter().filter(|p| !matches!(p.kind, PropKind::Formula | PropKind::Relation | PropKind::Rollup)).collect();
    let mut out = String::new();
    out.push_str("Name");
    for p in &props {
        out.push(',');
        out.push_str(&crate::io::csv_escape(&p.name));
    }
    out.push('\n');
    shared_scan(&st, |b| {
        for r in lumen_store::decode_chunk(b)? {
            out.push_str(&crate::io::csv_escape(&r.title));
            for p in &props {
                out.push(',');
                if let Some(v) = r.get(p.id as u32) {
                    out.push_str(&crate::io::csv_escape(&cell_text(db, p.id as u32, v)));
                }
            }
            out.push('\n');
        }
        Ok(())
    })
    .ok()?;
    Some(out)
}

// ───────────────────────────── sorted / searched view (worker) ─────────────────────────────

fn nan_last(a: f64, b: f64, asc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Equal,
        (true, false) => Greater,
        (false, true) => Less,
        _ => {
            let o = a.partial_cmp(&b).unwrap_or(Equal);
            if asc { o } else { o.reverse() }
        }
    }
}

/// Row ids matching `query` (title and text columns, case-insensitive) in the order of `sort` (`(property or 0 = title, ascending)`).
pub fn build_view(st: &Mutex<Store>, db: &Database, sort: Option<(u64, bool)>, query: &str, limit: usize) -> io::Result<Vec<u64>> {
    let q = query.trim().to_lowercase();
    let text_props: Vec<u32> = if q.is_empty() { vec![] } else { db.props.iter().filter(|p| matches!(p.kind, PropKind::Text | PropKind::Url | PropKind::Date)).map(|p| p.id as u32).collect() };
    let skind = sort.map(|(p, _)| if p == 0 { PropKind::Text } else { db.kind_of(p) });
    let by_title = matches!(sort, Some((0, _)));
    let ranks: HashMap<u64, usize> = match sort {
        Some((p, _)) if p != 0 => db.prop(p).map(|pp| pp.opts.iter().enumerate().map(|(i, o)| (o.id, i)).collect()).unwrap_or_default(),
        _ => HashMap::new(),
    };
    let (mut plain, mut nums, mut strs): (Vec<u64>, Vec<(f64, u64)>, Vec<(String, u64)>) = (vec![], vec![], vec![]);
    shared_scan(st, |b| {
        let ids = chunk_ids(b)?;
        let titles = if !q.is_empty() || by_title { Some(chunk_titles(b)?) } else { None };
        let mut keep = vec![true; ids.len()];
        if !q.is_empty() {
            let t = titles.as_ref().expect("titles");
            for i in 0..ids.len() {
                keep[i] = t[i].to_lowercase().contains(&q);
            }
            for p in &text_props {
                let (dict, codes) = col_text(b, *p)?;
                let hit: Vec<bool> = dict.iter().map(|s| s.to_lowercase().contains(&q)).collect();
                for i in 0..ids.len() {
                    if !keep[i] && codes[i].map_or(false, |c| hit[c as usize]) {
                        keep[i] = true;
                    }
                }
            }
        }
        match (sort, skind) {
            (None, _) => plain.extend(ids.iter().zip(&keep).filter(|(_, k)| **k).map(|(i, _)| *i)),
            (Some((p, _)), Some(kind)) => {
                if by_title {
                    let t = titles.as_ref().expect("titles");
                    for i in 0..ids.len() {
                        if keep[i] {
                            strs.push((t[i].chars().take(32).collect::<String>().to_lowercase(), ids[i]));
                        }
                    }
                } else {
                    match kind {
                        PropKind::Number => {
                            let c = col_num(b, p as u32)?;
                            for i in 0..ids.len() {
                                if keep[i] {
                                    nums.push((c[i].unwrap_or(f64::NAN), ids[i]));
                                }
                            }
                        }
                        PropKind::Select => {
                            let c = col_sel(b, p as u32)?;
                            for i in 0..ids.len() {
                                if keep[i] {
                                    nums.push((c[i].and_then(|o| ranks.get(&o)).map_or(f64::NAN, |r| *r as f64), ids[i]));
                                }
                            }
                        }
                        PropKind::Checkbox => {
                            let c = col_check(b, p as u32)?;
                            for i in 0..ids.len() {
                                if keep[i] {
                                    nums.push((c[i].map_or(f64::NAN, |x| x as u8 as f64), ids[i]));
                                }
                            }
                        }
                        _ => {
                            let (dict, codes) = col_text(b, p as u32)?;
                            for i in 0..ids.len() {
                                if keep[i] {
                                    strs.push((codes[i].map(|c| dict[c as usize].chars().take(32).collect::<String>().to_lowercase()).unwrap_or_default(), ids[i]));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    let mut out: Vec<u64> = if let Some((_, asc)) = sort {
        if !strs.is_empty() || (nums.is_empty() && skind.map_or(false, |k| !matches!(k, PropKind::Number | PropKind::Select | PropKind::Checkbox) || by_title)) {
            strs.sort_by(|a, b| match (a.0.is_empty(), b.0.is_empty()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => if asc { a.0.cmp(&b.0) } else { b.0.cmp(&a.0) },
            });
            strs.into_iter().map(|x| x.1).collect()
        } else {
            nums.sort_by(|a, b| nan_last(a.0, b.0, asc));
            nums.into_iter().map(|x| x.1).collect()
        }
    } else {
        plain
    };
    out.truncate(limit);
    Ok(out)
}

// ───────────────────────────── dashboards: exact aggregates straight from the chunks ─────────────────────────────

#[derive(Clone, Copy)]
struct Acc {
    count: f64,
    sum: f64,
    min: f64,
    max: f64,
    nvals: f64,
}

impl Acc {
    fn new() -> Acc {
        Acc { count: 0.0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY, nvals: 0.0 }
    }
    fn row(&mut self, v: Option<f64>) {
        self.count += 1.0;
        if let Some(x) = v {
            self.sum += x;
            self.nvals += 1.0;
            self.min = self.min.min(x);
            self.max = self.max.max(x);
        }
    }
    fn merge(&mut self, o: &Acc) {
        self.count += o.count;
        self.sum += o.sum;
        self.nvals += o.nvals;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }
    fn value(&self, agg: Agg) -> f64 {
        match agg {
            Agg::Sum => self.sum,
            Agg::Avg => if self.nvals > 0.0 { self.sum / self.nvals } else { 0.0 },
            Agg::Min => if self.nvals > 0.0 { self.min } else { 0.0 },
            Agg::Max => if self.nvals > 0.0 { self.max } else { 0.0 },
            Agg::Filled => self.nvals,
            _ => self.count,
        }
    }
}

fn needs_values(cfg: &Cfg) -> bool {
    cfg.value != 0 && matches!(cfg.agg, Agg::Sum | Agg::Avg | Agg::Min | Agg::Max | Agg::Filled)
}

/// One number (count / sum / avg / min / max). Straight from the zone maps: no chunk is read.
pub fn big_number(st: &Mutex<Store>, cfg: &Cfg) -> Num {
    let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
    let len = s.len().unwrap_or(0) as f64;
    let value = if needs_values(cfg) {
        match s.aggregate_num(cfg.value as u32, None) {
            Ok(a) if a.count > 0 => match cfg.agg {
                Agg::Sum => a.sum,
                Agg::Avg => a.sum / a.count as f64,
                Agg::Min => a.min,
                Agg::Max => a.max,
                _ => a.count as f64,
            },
            _ => 0.0,
        }
    } else {
        len
    };
    Num { value, prev: None, rows: vec![] }
}

fn bucket_key(iso: &str, b: Bucket) -> String {
    let n = |k: usize| iso.chars().take(k).collect::<String>();
    match b {
        Bucket::Day => n(10),
        Bucket::Year => n(4),
        Bucket::Quarter => {
            let y = n(4);
            let m: u32 = iso.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
            format!("{y}-Q{}", (m.max(1) - 1) / 3 + 1)
        }
        _ => n(7),
    }
}

/// Group-by chart data over the whole table, exact: select / checkbox / date (bucketed by ISO prefix) / text groups; one series.
pub fn big_grouped(st: &Mutex<Store>, db: &Database, cfg: &Cfg) -> Dataset {
    let g = cfg.group as u32;
    let kind = if cfg.group == 0 { PropKind::Text } else { db.kind_of(cfg.group) };
    let vals = needs_values(cfg);
    let mut sel: HashMap<Option<u64>, Acc> = HashMap::new();
    let mut chk: [Acc; 3] = [Acc::new(); 3];
    let mut txt: HashMap<String, Acc> = HashMap::new();
    let mut all = Acc::new();
    let r = shared_scan(st, |b| {
        let v: Vec<Option<f64>> = if vals { col_num(b, cfg.value as u32)? } else { vec![] };
        let val = |i: usize| if vals { v[i] } else { None };
        if cfg.group == 0 {
            let n = chunk_ids(b)?.len();
            for i in 0..n {
                all.row(val(i));
            }
            return Ok(());
        }
        match kind {
            PropKind::Select => {
                for (i, o) in col_sel(b, g)?.into_iter().enumerate() {
                    sel.entry(o).or_insert_with(Acc::new).row(val(i));
                }
            }
            PropKind::Checkbox => {
                for (i, o) in col_check(b, g)?.into_iter().enumerate() {
                    chk[match o { Some(true) => 0, Some(false) => 1, None => 2 }].row(val(i));
                }
            }
            _ => {
                let (dict, codes) = col_text(b, g)?;
                let keys: Vec<String> = dict.iter().map(|s| if kind == PropKind::Date { bucket_key(s, cfg.bucket) } else { s.clone() }).collect();
                let mut local: HashMap<u32, Acc> = HashMap::new();
                let mut none = Acc::new();
                for (i, c) in codes.into_iter().enumerate() {
                    match c {
                        Some(c) => local.entry(c).or_insert_with(Acc::new).row(val(i)),
                        None => none.row(val(i)),
                    }
                }
                for (c, a) in local {
                    txt.entry(keys[c as usize].clone()).or_insert_with(Acc::new).merge(&a);
                }
                if none.count > 0.0 {
                    txt.entry(String::new()).or_insert_with(Acc::new).merge(&none);
                }
            }
        }
        Ok(())
    });
    let _ = r;
    let (mut cats, mut colors, mut accs): (Vec<String>, Vec<Option<egui::Color32>>, Vec<Acc>) = (vec![], vec![], vec![]);
    let mut dated = false;
    if cfg.group == 0 {
        cats.push("All".into());
        colors.push(None);
        accs.push(all);
    } else {
        match kind {
            PropKind::Select => {
                if let Some(p) = db.prop(cfg.group) {
                    for o in &p.opts {
                        let a = sel.get(&Some(o.id)).copied().unwrap_or_else(Acc::new);
                        cats.push(o.name.clone());
                        colors.push(Some(tag_color(o.color)));
                        accs.push(a);
                    }
                }
                if let Some(a) = sel.get(&None) {
                    cats.push("No value".into());
                    colors.push(None);
                    accs.push(*a);
                }
            }
            PropKind::Checkbox => {
                for (i, (n, c)) in [("Yes", Some(tag_color(2))), ("No", Some(tag_color(0))), ("No value", None)].into_iter().enumerate() {
                    if i < 2 || chk[2].count > 0.0 {
                        cats.push(n.into());
                        colors.push(c);
                        accs.push(chk[i]);
                    }
                }
            }
            _ => {
                let mut v: Vec<(String, Acc)> = txt.into_iter().collect();
                if kind == PropKind::Date {
                    dated = true;
                    v.retain(|(k, _)| !k.is_empty());
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                } else {
                    v.sort_by(|a, b| b.1.count.partial_cmp(&a.1.count).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
                    let limit = if cfg.limit == 0 { 300 } else { cfg.limit as usize };
                    if v.len() > limit {
                        let mut other = Acc::new();
                        for (_, a) in v.drain(limit..) {
                            other.merge(&a);
                        }
                        v.push(("Other".into(), other));
                    }
                }
                for (k, a) in v {
                    cats.push(if k.is_empty() { "No value".into() } else { k });
                    colors.push(None);
                    accs.push(a);
                }
            }
        }
    }
    if cfg.hide_empty {
        let keep: Vec<usize> = (0..cats.len()).filter(|i| accs[*i].count > 0.0).collect();
        cats = keep.iter().map(|i| cats[*i].clone()).collect();
        colors = keep.iter().map(|i| colors[*i]).collect();
        accs = keep.iter().map(|i| accs[*i]).collect();
    }
    let name = match cfg.agg {
        Agg::Sum => "Sum",
        Agg::Avg => "Average",
        Agg::Min => "Min",
        Agg::Max => "Max",
        _ => "Count",
    };
    let n = cats.len();
    Dataset { cats, colors, series: vec![Ser { name: name.into(), color: None, vals: accs.iter().map(|a| a.value(cfg.agg)).collect(), rows: vec![vec![]; n] }], dated, percent: false }
}

/// Scatter points (x, y numeric columns), thinned to ~`max_points` while scanning.
pub fn big_scatter(st: &Mutex<Store>, cfg: &Cfg, max_points: usize) -> Scatter {
    let total = st.lock().unwrap_or_else(|e| e.into_inner()).len().unwrap_or(0) as usize;
    let every = (total / (max_points * 2).max(1)).max(1);
    let mut pts: Vec<(f64, f64, u64, usize)> = vec![];
    let mut k = 0usize;
    let _ = shared_scan(st, |b| {
        let ids = chunk_ids(b)?;
        let (xs, ys) = (col_num(b, cfg.x as u32)?, col_num(b, cfg.y as u32)?);
        for i in 0..ids.len() {
            k += 1;
            if k % every != 0 {
                continue;
            }
            if let (Some(x), Some(y)) = (xs[i], ys[i]) {
                pts.push((x, y, ids[i], 0));
            }
        }
        Ok(())
    });
    if pts.len() > max_points {
        let s = pts.len().div_ceil(max_points);
        pts = pts.into_iter().step_by(s).collect();
    }
    Scatter { pts, cats: vec![("All".into(), None)] }
}

#[cfg(test)]
mod tests {
    use super::*;
    // the tests share one process-wide environment variable (the tables directory): run them one at a time
    static T_LOCK: Mutex<()> = Mutex::new(());

    fn csv(n: usize) -> String {
        let mut s = String::from("Name,Status,Amount,Done,Due\n");
        for i in 0..n {
            s.push_str(&format!("Task {i},{},{},{},2024-{:02}-{:02}\n", ["todo", "doing", "done"][i % 3], i % 100, if i % 2 == 0 { "true" } else { "false" }, i % 12 + 1, i % 27 + 1));
        }
        s
    }

    #[test]
    fn csv_import_streams_into_the_store_and_aggregates_exactly() {
        let _g = T_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lumen-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("LUMEN_TABLES_DIR", &dir);
        let mut ws = Workspace::seed();
        let id = import_csv(&mut ws, "Big", &csv(30_000), None).expect("imported");
        let st = store(&ws, id).expect("store");
        assert_eq!(lock(&st).len().unwrap(), 30_000);
        // no page was created per row
        assert!(!ws.pages.iter().any(|p| p.row && p.parent == Some(id)));
        let db = ws.db(id).unwrap().clone();
        let status = db.props.iter().find(|p| p.name == "Status").unwrap();
        assert_eq!(status.kind, PropKind::Select);
        let amount = db.props.iter().find(|p| p.name == "Amount").unwrap();
        // group by status, sum of amount
        let cfg = Cfg { db: id, group: status.id, value: amount.id, agg: Agg::Sum, ..Default::default() };
        let ds = big_grouped(&st, &db, &cfg);
        assert_eq!(ds.cats.len(), 3);
        let expect: f64 = (0..30_000usize).map(|i| (i % 100) as f64).sum();
        assert_eq!(ds.series[0].vals.iter().sum::<f64>(), expect);
        // group by date month, count
        let due = db.props.iter().find(|p| p.name == "Due").unwrap();
        let ds = big_grouped(&st, &db, &Cfg { db: id, group: due.id, agg: Agg::Count, bucket: Bucket::Month, ..Default::default() });
        assert!(ds.dated && ds.cats.len() == 12 && ds.series[0].vals.iter().sum::<f64>() == 30_000.0);
        // number card from zone maps
        let n = big_number(&st, &Cfg { db: id, value: amount.id, agg: Agg::Max, ..Default::default() });
        assert_eq!(n.value, 99.0);
        // sorted + searched view
        let ids = build_view(&st, &db, Some((amount.id, false)), "", 100).unwrap();
        assert_eq!(ids.len(), 100);
        assert_eq!(lock(&st).get(ids[0]).unwrap().unwrap().get(amount.id as u32), Some(&Value::Num(99.0)));
        let found = build_view(&st, &db, None, "task 2999", 1000).unwrap();
        assert_eq!(found.len(), 11); // Task 2999, Task 29990..29999
        let by_title = build_view(&st, &db, Some((0, true)), "", 3).unwrap();
        assert_eq!(lock(&st).get(by_title[0]).unwrap().unwrap().title, "Task 0");
        // export round trip
        let out = export_csv(&ws, id).unwrap();
        assert_eq!(out.lines().count(), 30_001);
        assert!(out.lines().nth(1).unwrap().starts_with("Task 0,"));
        // an edit survives a re-open of the registry entry
        {
            let mut s = lock(&st);
            let mut r = s.get(5).unwrap().unwrap();
            r.title = "edited".into();
            s.upsert(r).unwrap();
            s.checkpoint().unwrap();
        }
        assert_eq!(lock(&st).get(5).unwrap().unwrap().title, "edited");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sample_dataset_is_deterministic() {
        let _g = T_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lumen-big-sample-{}", std::process::id()));
        std::env::set_var("LUMEN_TABLES_DIR", &dir);
        let mut ws = Workspace::seed();
        let id = create_sample(&mut ws, "Sample", 25_000, None).unwrap();
        let st = store(&ws, id).unwrap();
        assert_eq!(lock(&st).len().unwrap(), 25_000);
        let r = lock(&st).get(777).unwrap().unwrap();
        assert!(r.title.starts_with("Task 777"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_reader_handles_quotes_and_newlines() {
        let rows: Vec<Vec<String>> = Csv::new("a,b\n\"x,1\",\"he said \"\"hi\"\"\nsecond line\"\n\n,\nlast,1").collect();
        assert_eq!(rows[0], vec!["a", "b"]);
        assert_eq!(rows[1], vec!["x,1", "he said \"hi\"\nsecond line"]);
        assert_eq!(rows[2], vec!["last", "1"]);
        assert_eq!(rows.len(), 3);
    }
}
