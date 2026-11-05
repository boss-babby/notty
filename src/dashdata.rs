//! Dashboard data engine — turns a widget's [`Cfg`] into numbers.
//!
//! Pure functions over the [`Workspace`] (no UI), so every chart, number card and list is computed the same way and the
//! results are unit-tested.  Records are selected by *source database → saved view (filters + sorts) → extra filters →
//! time window*, grouped by a property (Select, Multi-select, Checkbox, Date buckets, Text, Number, Relation) and
//! optionally split into series by a second property, then aggregated (`count`, `sum`, `avg`, …).  Every cell keeps the ids
//! of its records so that a click on a bar / slice / number can open a drill-down list.

use std::collections::{BTreeMap, HashMap, HashSet};

use egui::Color32;

use crate::dashcfg::{Agg, Bucket, Cfg, Range};
use crate::dates;
use crate::model::{cmp_vals, row_matches, row_val, Database, Page, PropKind, Val, Workspace};
use crate::theme::tag_color;

/// Sort id that means "by title" in `Cfg::sort` (0 = keep the saved view's order).
pub const TITLE_SORT: u64 = u64::MAX;

#[derive(Clone, Debug, Default)]
pub struct Ser {
    pub name: String,
    pub color: Option<Color32>,
    pub vals: Vec<f64>,
    /// Records behind every value (for drill-down).
    pub rows: Vec<Vec<u64>>,
}

#[derive(Clone, Debug, Default)]
pub struct Dataset {
    pub cats: Vec<String>,
    /// Colour of a category when it comes from a Select option.
    pub colors: Vec<Option<Color32>>,
    pub series: Vec<Ser>,
    /// The categories are consecutive date buckets.
    pub dated: bool,
    pub percent: bool,
}

impl Dataset {
    pub fn is_empty(&self) -> bool {
        self.cats.is_empty() || self.series.iter().all(|s| s.vals.iter().all(|v| *v == 0.0))
    }

    pub fn total(&self) -> f64 {
        self.series.iter().flat_map(|s| s.vals.iter()).sum()
    }

    /// Sum over series for category `i`.
    pub fn cat_total(&self, i: usize) -> f64 {
        self.series.iter().map(|s| s.vals.get(i).copied().unwrap_or(0.0)).sum()
    }

    pub fn cat_rows(&self, i: usize) -> Vec<u64> {
        let mut v: Vec<u64> = self.series.iter().flat_map(|s| s.rows.get(i).cloned().unwrap_or_default()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

// ───────────────────────────── selecting records ─────────────────────────────

pub fn today() -> i64 {
    dates::today_day()
}

/// Day span of a record for date property `date` (0 = the day it was last edited).
pub fn row_span(row: &Page, date: u64) -> Option<(i64, i64)> {
    if date == 0 {
        let d = (row.updated / 86_400_000.0).floor() as i64;
        Some((d, d))
    } else if let Some(Val::Text(s)) = row.props.get(&date) {
        dates::span(s)
    } else {
        None
    }
}

/// Records of the widget's database: saved view → extra filters → optional day window.
pub fn rows_in(ws: &Workspace, cfg: &Cfg, bounds: Option<(i64, i64)>) -> Vec<u64> {
    let Some(d) = ws.db(cfg.db) else { return vec![] };
    let ids = match d.views.iter().position(|v| v.id == cfg.view).filter(|_| cfg.view != 0) {
        Some(vi) => ws.query(cfg.db, vi),
        None => ws.rows(cfg.db),
    };
    ids.into_iter()
        .filter(|id| {
            ws.page(*id).map_or(false, |r| {
                cfg.filters.iter().all(|f| row_matches(r, d, f)) && bounds.map_or(true, |(a, b)| row_span(r, cfg.date).map_or(false, |(s, e)| e >= a && s <= b))
            })
        })
        .collect()
}

pub fn effective_range(cfg: &Cfg, global: Range) -> Range {
    if cfg.range != Range::All {
        cfg.range
    } else {
        global
    }
}

pub fn rows(ws: &Workspace, cfg: &Cfg, global: Range) -> Vec<u64> {
    rows_in(ws, cfg, effective_range(cfg, global).bounds(today()))
}

/// Records for list-like widgets: [`rows`] re-sorted by `cfg.sort` when one is set.
pub fn sorted_rows(ws: &Workspace, cfg: &Cfg, global: Range) -> Vec<u64> {
    let mut ids = rows(ws, cfg, global);
    if cfg.sort == 0 {
        return ids;
    }
    let Some(d) = ws.db(cfg.db) else { return ids };
    ids.sort_by(|a, b| {
        let (Some(pa), Some(pb)) = (ws.page(*a), ws.page(*b)) else { return std::cmp::Ordering::Equal };
        let o = if cfg.sort == TITLE_SORT { pa.title.to_lowercase().cmp(&pb.title.to_lowercase()) } else { cmp_vals(d, cfg.sort, pa, pb) };
        if cfg.asc {
            o
        } else {
            o.reverse()
        }
    });
    ids
}

// ───────────────────────────── values ─────────────────────────────

pub fn num_of(row: &Page, prop: u64) -> Option<f64> {
    match row_val(row, prop)? {
        Val::Num(n) => Some(n),
        Val::Check(b) => Some(if b { 1.0 } else { 0.0 }),
        Val::Text(s) => s.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

fn is_filled(row: &Page, prop: u64) -> bool {
    match row_val(row, prop) {
        None => false,
        Some(Val::Text(s)) => !s.trim().is_empty(),
        Some(Val::Multi(m)) => !m.is_empty(),
        Some(Val::Check(b)) => b,
        Some(_) => true,
    }
}

/// Combines the values of property `prop` over records `ids`.
pub fn aggregate(ws: &Workspace, db: u64, ids: &[u64], prop: u64, agg: Agg) -> f64 {
    let rows: Vec<&Page> = ids.iter().filter_map(|i| ws.page(*i)).collect();
    if rows.is_empty() {
        return 0.0;
    }
    let numeric = matches!(agg, Agg::Sum | Agg::Avg | Agg::Min | Agg::Max | Agg::Median);
    if agg == Agg::Count || (prop == 0 && numeric) {
        return rows.len() as f64;
    }
    match agg {
        Agg::Filled => rows.iter().filter(|r| is_filled(r, prop)).count() as f64,
        Agg::Unique => {
            let Some(d) = ws.db(db) else { return 0.0 };
            let mut set = HashSet::new();
            for r in &rows {
                if let Some(v) = row_val(r, prop) {
                    let t = d.val_text(prop, &v);
                    if !t.trim().is_empty() {
                        set.insert(t);
                    }
                }
            }
            set.len() as f64
        }
        Agg::Percent => {
            let hit = rows.iter().filter(|r| num_of(r, prop).map_or(false, |n| n > 0.0)).count();
            hit as f64 * 100.0 / rows.len() as f64
        }
        _ => {
            let mut nums: Vec<f64> = rows.iter().filter_map(|r| num_of(r, prop)).collect();
            if nums.is_empty() {
                return 0.0;
            }
            match agg {
                Agg::Sum => nums.iter().sum(),
                Agg::Avg => nums.iter().sum::<f64>() / nums.len() as f64,
                Agg::Min => nums.iter().copied().fold(f64::INFINITY, f64::min),
                Agg::Max => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                _ => {
                    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let m = nums.len() / 2;
                    if nums.len() % 2 == 1 {
                        nums[m]
                    } else {
                        (nums[m - 1] + nums[m]) / 2.0
                    }
                }
            }
        }
    }
}

// ───────────────────────────── grouping ─────────────────────────────

/// (sort key, label, colour)
type Key = (String, String, Option<Color32>);

const LAST: &str = "~~~~~~";

fn date_key(start: i64) -> String {
    format!("{:010}", start + 100_000_000)
}

fn keys_of(ws: &Workspace, d: &Database, row: &Page, prop: u64, bucket: Bucket) -> Vec<Key> {
    let none = || vec![(LAST.to_owned(), "No value".to_owned(), Some(tag_color(0).gamma_multiply(0.7)))];
    if prop == 0 {
        return vec![("0".into(), "All".into(), None)];
    }
    let val = row.props.get(&prop);
    match d.kind_of(prop) {
        PropKind::Select => match val {
            Some(Val::Sel(oid)) => match d.prop(prop).and_then(|p| p.opts.iter().position(|o| o.id == *oid).map(|i| (i, &p.opts[i]))) {
                Some((i, o)) => vec![(format!("{i:06}"), o.name.clone(), Some(tag_color(o.color)))],
                None => none(),
            },
            _ => none(),
        },
        PropKind::MultiSelect => match val {
            Some(Val::Multi(m)) if !m.is_empty() => m
                .iter()
                .filter_map(|oid| d.prop(prop).and_then(|p| p.opts.iter().position(|o| o.id == *oid).map(|i| (i, &p.opts[i]))))
                .map(|(i, o)| (format!("{i:06}"), o.name.clone(), Some(tag_color(o.color))))
                .collect(),
            _ => none(),
        },
        PropKind::Checkbox => {
            if matches!(val, Some(Val::Check(true))) {
                vec![("0".into(), "Yes".into(), Some(tag_color(2)))]
            } else {
                vec![("1".into(), "No".into(), Some(tag_color(0)))]
            }
        }
        PropKind::Date => match val {
            Some(Val::Text(s)) => match dates::span(s) {
                Some((a, _)) => {
                    let st = bucket.start(a);
                    vec![(date_key(st), bucket.label_of(st), None)]
                }
                None => vec![(LAST.to_owned(), "No date".to_owned(), Some(tag_color(0).gamma_multiply(0.7)))],
            },
            _ => vec![(LAST.to_owned(), "No date".to_owned(), Some(tag_color(0).gamma_multiply(0.7)))],
        },
        PropKind::Relation => match val {
            Some(Val::Multi(m)) if !m.is_empty() => m
                .iter()
                .map(|id| {
                    let t = ws.page(*id).map(|p| p.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled".into());
                    (t.to_lowercase(), t, None)
                })
                .collect(),
            _ => none(),
        },
        PropKind::Number | PropKind::Formula | PropKind::Rollup => match num_of(row, prop) {
            Some(n) if val.is_some() => {
                let label = d.val_text(prop, &Val::Num(n));
                (vec![(format!("{:020.4}", n + 1e12), label, None)])
            }
            _ => none(),
        },
        _ => match val {
            Some(v) => {
                let t = d.val_text(prop, v);
                if t.trim().is_empty() {
                    none()
                } else {
                    vec![(t.to_lowercase(), t, None)]
                }
            }
            None => none(),
        },
    }
}

fn series_name(ws: &Workspace, cfg: &Cfg) -> String {
    if cfg.agg == Agg::Count || cfg.value == 0 {
        return if cfg.agg == Agg::Count || cfg.agg == Agg::Percent { "Records".into() } else { cfg.agg.short().into() };
    }
    let p = ws.db(cfg.db).and_then(|d| d.prop(cfg.value)).map(|p| p.name.clone()).unwrap_or_default();
    format!("{} of {}", cfg.agg.short(), p)
}

/// Groups `ids` by `cfg.group` (and `cfg.stack`) and aggregates `cfg.value`.
pub fn build(ws: &Workspace, cfg: &Cfg, ids: &[u64]) -> Dataset {
    let Some(d) = ws.db(cfg.db) else { return Dataset::default() };
    let group_kind = d.kind_of(cfg.group);
    let dated = cfg.group != 0 && group_kind == PropKind::Date;

    // pass 1: keys
    let mut cats: BTreeMap<String, (String, Option<Color32>)> = BTreeMap::new();
    let mut sers: BTreeMap<String, (String, Option<Color32>)> = BTreeMap::new();
    let mut hits: Vec<(Vec<String>, Vec<String>, u64)> = vec![];
    for id in ids {
        let Some(r) = ws.page(*id) else { continue };
        let ck = keys_of(ws, d, r, cfg.group, cfg.bucket);
        let sk = if cfg.stack != 0 { keys_of(ws, d, r, cfg.stack, cfg.bucket) } else { vec![("0".into(), series_name(ws, cfg), None)] };
        for (k, l, c) in &ck {
            cats.entry(k.clone()).or_insert((l.clone(), *c));
        }
        for (k, l, c) in &sk {
            sers.entry(k.clone()).or_insert((l.clone(), *c));
        }
        hits.push((ck.into_iter().map(|k| k.0).collect(), sk.into_iter().map(|k| k.0).collect(), *id));
    }
    // select options without records still get their (empty) bar
    if cfg.group != 0 && !cfg.hide_empty {
        if let Some(p) = d.prop(cfg.group).filter(|p| matches!(p.kind, PropKind::Select | PropKind::MultiSelect)) {
            for (i, o) in p.opts.iter().enumerate() {
                cats.entry(format!("{i:06}")).or_insert((o.name.clone(), Some(tag_color(o.color))));
            }
        }
        if group_kind == PropKind::Checkbox {
            cats.entry("0".into()).or_insert(("Yes".into(), Some(tag_color(2))));
            cats.entry("1".into()).or_insert(("No".into(), Some(tag_color(0))));
        }
    }
    if cats.is_empty() {
        cats.insert("0".into(), ("All".into(), None));
    }
    if sers.is_empty() {
        sers.insert("0".into(), (series_name(ws, cfg), None));
    }
    // continuous date axis: fill the gaps between the first and last bucket
    if dated {
        let nums: Vec<i64> = cats.keys().filter(|k| *k != LAST).filter_map(|k| k.parse::<i64>().ok()).map(|n| n - 100_000_000).collect();
        if let (Some(lo), Some(hi)) = (nums.iter().min().copied(), nums.iter().max().copied()) {
            let mut s = lo;
            let mut n = 0;
            while s <= hi && n < 400 {
                cats.entry(date_key(s)).or_insert((cfg.bucket.label_of(s), None));
                s = cfg.bucket.next(s);
                n += 1;
            }
        }
    }

    let cat_keys: Vec<String> = cats.keys().cloned().collect();
    let ser_keys: Vec<String> = sers.keys().cloned().collect();
    let mut cells: HashMap<(usize, usize), Vec<u64>> = HashMap::new();
    for (ck, sk, id) in &hits {
        for c in ck {
            for s in sk {
                if let (Some(ci), Some(si)) = (cat_keys.iter().position(|k| k == c), ser_keys.iter().position(|k| k == s)) {
                    cells.entry((si, ci)).or_default().push(*id);
                }
            }
        }
    }
    let mut out = Dataset { dated, percent: cfg.agg.is_percent(), ..Default::default() };
    for k in &cat_keys {
        let (l, c) = cats[k].clone();
        out.cats.push(l);
        out.colors.push(c);
    }
    for (si, sk) in ser_keys.iter().enumerate() {
        let (name, color) = sers[sk].clone();
        let mut s = Ser { name, color, ..Default::default() };
        for ci in 0..cat_keys.len() {
            let rows = cells.remove(&(si, ci)).unwrap_or_default();
            s.vals.push(aggregate(ws, cfg.db, &rows, cfg.value, cfg.agg));
            s.rows.push(rows);
        }
        if cfg.cumulative {
            let mut run = 0.0;
            for v in &mut s.vals {
                run += *v;
                *v = run;
            }
        }
        out.series.push(s);
    }
    // drop empty categories when asked, keep the top N otherwise
    let keep: Vec<usize> = {
        let mut idx: Vec<usize> = (0..out.cats.len()).collect();
        if cfg.hide_empty {
            idx.retain(|i| out.series.iter().any(|s| !s.rows[*i].is_empty()));
        }
        if cfg.limit > 0 && idx.len() > cfg.limit as usize {
            if dated {
                // newest buckets win
                idx = idx.split_off(idx.len() - cfg.limit as usize);
            } else {
                let mut by: Vec<usize> = idx.clone();
                by.sort_by(|a, b| out.cat_total(*b).partial_cmp(&out.cat_total(*a)).unwrap_or(std::cmp::Ordering::Equal));
                by.truncate(cfg.limit as usize);
                by.sort_unstable();
                idx = by;
            }
        }
        idx
    };
    if keep.len() != out.cats.len() {
        out.cats = keep.iter().map(|i| out.cats[*i].clone()).collect();
        out.colors = keep.iter().map(|i| out.colors[*i]).collect();
        for s in &mut out.series {
            s.vals = keep.iter().map(|i| s.vals[*i]).collect();
            s.rows = keep.iter().map(|i| std::mem::take(&mut s.rows[*i])).collect();
        }
    }
    out
}

pub fn grouped(ws: &Workspace, cfg: &Cfg, global: Range) -> Dataset {
    let ids = rows(ws, cfg, global);
    build(ws, cfg, &ids)
}

/// Histogram of a numeric property (`cfg.value`, or `cfg.x` when no value is set).
pub fn histogram(ws: &Workspace, cfg: &Cfg, global: Range) -> Dataset {
    let ids = rows(ws, cfg, global);
    let prop = if cfg.value != 0 { cfg.value } else { cfg.x };
    let vals: Vec<(f64, u64)> = ids.iter().filter_map(|id| ws.page(*id).and_then(|r| num_of(r, prop)).map(|n| (n, *id))).collect();
    let mut ds = Dataset::default();
    if vals.is_empty() {
        return ds;
    }
    let lo = vals.iter().map(|v| v.0).fold(f64::INFINITY, f64::min);
    let hi = vals.iter().map(|v| v.0).fold(f64::NEG_INFINITY, f64::max);
    let n = if cfg.limit > 0 { cfg.limit as usize } else { ((vals.len() as f64).sqrt().ceil() as usize).clamp(5, 12) }.clamp(2, 40);
    let w = ((hi - lo) / n as f64).max(1e-9);
    let n = if hi == lo { 1 } else { n };
    let mut ser = Ser { name: "Records".into(), ..Default::default() };
    ser.vals = vec![0.0; n];
    ser.rows = vec![vec![]; n];
    for (v, id) in &vals {
        let i = (((*v - lo) / w).floor() as usize).min(n - 1);
        ser.vals[i] += 1.0;
        ser.rows[i].push(*id);
    }
    for i in 0..n {
        ds.cats.push(if hi == lo { fmt_num(lo) } else { format!("{}–{}", fmt_num(lo + w * i as f64), fmt_num(lo + w * (i + 1) as f64)) });
        ds.colors.push(None);
    }
    ds.series.push(ser);
    ds
}

pub struct Scatter {
    /// (x, y, record, category index)
    pub pts: Vec<(f64, f64, u64, usize)>,
    pub cats: Vec<(String, Option<Color32>)>,
}

/// Points of a scatter plot: x / y numeric properties, coloured by the grouping property.
pub fn scatter(ws: &Workspace, cfg: &Cfg, global: Range) -> Scatter {
    let ids = rows(ws, cfg, global);
    let mut out = Scatter { pts: vec![], cats: vec![] };
    let Some(d) = ws.db(cfg.db) else { return out };
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut tmp: Vec<(String, String, Option<Color32>)> = vec![];
    let mut raw = vec![];
    for id in ids {
        let Some(r) = ws.page(id) else { continue };
        let (Some(x), Some(y)) = (num_of(r, cfg.x), num_of(r, cfg.y)) else { continue };
        let k = keys_of(ws, d, r, cfg.group, cfg.bucket).into_iter().next().unwrap_or(("0".into(), "All".into(), None));
        if !idx.contains_key(&k.0) {
            idx.insert(k.0.clone(), 0);
            tmp.push(k.clone());
        }
        raw.push((x, y, id, k.0));
    }
    tmp.sort_by(|a, b| a.0.cmp(&b.0));
    for (i, t) in tmp.iter().enumerate() {
        idx.insert(t.0.clone(), i);
        out.cats.push((t.1.clone(), t.2));
    }
    out.pts = raw.into_iter().map(|(x, y, id, k)| (x, y, id, idx[&k])).collect();
    out
}

/// Records per day for the heat-map (the day comes from `cfg.date`, or the last edit).
pub fn heat(ws: &Workspace, cfg: &Cfg, global: Range) -> BTreeMap<i64, Vec<u64>> {
    let mut m: BTreeMap<i64, Vec<u64>> = BTreeMap::new();
    // without a database the heat-map shows page edits of the whole workspace
    if ws.db(cfg.db).is_none() {
        for p in ws.pages.iter().filter(|p| !p.trashed) {
            m.entry((p.updated / 86_400_000.0).floor() as i64).or_default().push(p.id);
        }
        return m;
    }
    for id in rows(ws, cfg, global) {
        if let Some((a, _)) = ws.page(id).and_then(|r| row_span(r, cfg.date)) {
            m.entry(a).or_default().push(id);
        }
    }
    m
}

pub struct Num {
    pub value: f64,
    pub prev: Option<f64>,
    pub rows: Vec<u64>,
}

/// One aggregated number, plus the same number for the previous period when the widget has a time window.
pub fn number(ws: &Workspace, cfg: &Cfg, global: Range) -> Num {
    let r = effective_range(cfg, global);
    let rows = rows_in(ws, cfg, r.bounds(today()));
    let value = aggregate(ws, cfg.db, &rows, cfg.value, cfg.agg);
    let prev = r.previous(today()).map(|b| aggregate(ws, cfg.db, &rows_in(ws, cfg, Some(b)), cfg.value, cfg.agg));
    Num { value, prev, rows }
}

// ───────────────────────────── formatting ─────────────────────────────

/// Compact: 1.2M, 12k, 3.5, 7.
pub fn fmt_num(v: f64) -> String {
    if v.abs() >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v.abs() >= 10_000.0 {
        format!("{:.0}k", v / 1000.0)
    } else if (v - v.round()).abs() < 0.005 {
        format!("{:.0}", v)
    } else {
        format!("{v:.1}")
    }
}

/// Full precision with thousands separators: 12,345.6
pub fn fmt_full(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    let neg = r < 0.0;
    let s = format!("{:.2}", r.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let frac = frac.trim_end_matches('0');
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    if neg && r != 0.0 {
        out.insert(0, '-');
    }
    out
}

pub fn fmt_agg(v: f64, percent: bool) -> String {
    if percent {
        format!("{}%", fmt_num(v))
    } else {
        fmt_num(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ViewKind;

    pub fn empty_ws() -> Workspace {
        let mut ws = Workspace::seed();
        ws.pages.clear();
        ws.current = None;
        ws.dash = Default::default();
        ws
    }

    /// Deals: Stage (select), Value (number), Closed (date), Won (checkbox).
    pub fn deals() -> (Workspace, u64, [u64; 4]) {
        let mut ws = empty_ws();
        let db = ws.add_database(None, ViewKind::Table);
        ws.pages.retain(|p| !(p.row && p.parent == Some(db))); // a new database starts with sample rows
        let stage = ws.add_prop(db, "Stage", PropKind::Select, &["Lead", "Won", "Lost"]);
        let value = ws.add_prop(db, "Value", PropKind::Number, &[]);
        let closed = ws.add_prop(db, "Closed", PropKind::Date, &[]);
        let won = ws.add_prop(db, "Won", PropKind::Checkbox, &[]);
        let data = [("A", "Lead", 100.0, "2025-01-10", false), ("B", "Won", 300.0, "2025-01-20", true), ("C", "Won", 50.0, "2025-03-05", true), ("D", "Lost", 10.0, "2025-03-06", false)];
        for (t, s, v, d, w) in data {
            let r = ws.add_row(db, None);
            let oid = ws.opt_id(db, stage, s).unwrap();
            let p = ws.page_mut(r).unwrap();
            p.title = t.into();
            p.props.insert(stage, Val::Sel(oid));
            p.props.insert(value, Val::Num(v));
            p.props.insert(closed, Val::Text(d.into()));
            p.props.insert(won, Val::Check(w));
        }
        (ws, db, [stage, value, closed, won])
    }

    #[test]
    fn groups_and_aggregates_by_select() {
        let (ws, db, [stage, value, ..]) = deals();
        let mut cfg = Cfg { db, group: stage, ..Cfg::default() };
        let ds = grouped(&ws, &cfg, Range::All);
        assert_eq!(ds.cats, vec!["Lead", "Won", "Lost"]);
        assert_eq!(ds.series[0].vals, vec![1.0, 2.0, 1.0]);
        cfg.value = value;
        cfg.agg = Agg::Sum;
        let ds = grouped(&ws, &cfg, Range::All);
        assert_eq!(ds.series[0].vals, vec![100.0, 350.0, 10.0]);
        assert_eq!(ds.series[0].rows[1].len(), 2, "drill-down keeps the records");
        cfg.agg = Agg::Avg;
        assert_eq!(grouped(&ws, &cfg, Range::All).series[0].vals[1], 175.0);
        cfg.agg = Agg::Max;
        assert_eq!(grouped(&ws, &cfg, Range::All).series[0].vals[1], 300.0);
        cfg.agg = Agg::Median;
        assert_eq!(grouped(&ws, &cfg, Range::All).series[0].vals[1], 175.0);
    }

    #[test]
    fn filters_views_and_limits() {
        let (mut ws, db, [stage, value, ..]) = deals();
        let mut cfg = Cfg { db, group: stage, value, agg: Agg::Sum, ..Cfg::default() };
        cfg.filters.push(crate::model::Filter { prop: value, op: crate::model::FilterOp::Gt, value: "40".into() });
        let ds = grouped(&ws, &cfg, Range::All);
        assert_eq!(ds.series[0].vals, vec![100.0, 350.0, 0.0]);
        // top-N + hide empty
        cfg.hide_empty = true;
        cfg.limit = 1;
        let ds = grouped(&ws, &cfg, Range::All);
        assert_eq!(ds.cats, vec!["Won"]);
        // a pinned view carries its own filter
        let vid = ws.db(db).unwrap().views[0].id;
        ws.db_mut(db).unwrap().views[0].filters.push(crate::model::Filter { prop: stage, op: crate::model::FilterOp::Is, value: "Won".into() });
        let cfg = Cfg { db, view: vid, ..Cfg::default() };
        assert_eq!(rows(&ws, &cfg, Range::All).len(), 2);
    }

    #[test]
    fn date_axis_is_continuous_and_stackable() {
        let (ws, db, [stage, value, closed, _]) = deals();
        let cfg = Cfg { db, group: closed, bucket: Bucket::Month, value, agg: Agg::Sum, ..Cfg::default() };
        let ds = grouped(&ws, &cfg, Range::All);
        assert!(ds.dated);
        assert_eq!(ds.cats, vec!["Jan ’25", "Feb ’25", "Mar ’25"], "the empty February is kept");
        assert_eq!(ds.series[0].vals, vec![400.0, 0.0, 60.0]);
        let cfg = Cfg { stack: stage, cumulative: true, ..cfg };
        let ds = grouped(&ws, &cfg, Range::All);
        assert_eq!(ds.series.len(), 3);
        let won = ds.series.iter().find(|s| s.name == "Won").unwrap();
        assert_eq!(won.vals, vec![300.0, 300.0, 350.0], "cumulative");
    }

    #[test]
    fn numbers_percent_and_trend() {
        let (ws, db, [_, value, closed, won]) = deals();
        let n = number(&ws, &Cfg { db, value, agg: Agg::Sum, ..Cfg::default() }, Range::All);
        assert_eq!(n.value, 460.0);
        assert!(n.prev.is_none());
        let n = number(&ws, &Cfg { db, value: won, agg: Agg::Percent, ..Cfg::default() }, Range::All);
        assert_eq!(n.value, 50.0);
        let n = number(&ws, &Cfg { db, agg: Agg::Count, ..Cfg::default() }, Range::All);
        assert_eq!(n.value, 4.0);
        // a window on a date property: rows dated far in the past are excluded and a previous period exists
        let n = number(&ws, &Cfg { db, date: closed, range: Range::Last30, ..Cfg::default() }, Range::All);
        assert_eq!(n.value, 0.0);
        assert_eq!(n.prev, Some(0.0));
        // the dashboard-wide range applies to widgets without their own
        let n = number(&ws, &Cfg { db, date: closed, ..Cfg::default() }, Range::Last7);
        assert_eq!(n.value, 0.0);
    }

    #[test]
    fn histogram_scatter_heat() {
        let (ws, db, [stage, value, closed, _]) = deals();
        let h = histogram(&ws, &Cfg { db, value, ..Cfg::default() }, Range::All);
        assert_eq!(h.series[0].vals.iter().sum::<f64>(), 4.0);
        let s = scatter(&ws, &Cfg { db, x: value, y: value, group: stage, ..Cfg::default() }, Range::All);
        assert_eq!(s.pts.len(), 4);
        assert_eq!(s.cats.len(), 3);
        let m = heat(&ws, &Cfg { db, date: closed, ..Cfg::default() }, Range::All);
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt_num(1_260_000.0), "1.3M");
        assert_eq!(fmt_num(12_000.0), "12k");
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(3.46), "3.5");
        assert_eq!(fmt_full(1234567.891), "1,234,567.89");
        assert_eq!(fmt_full(-1500.0), "-1,500");
        assert_eq!(fmt_full(0.5), "0.5");
        assert_eq!(fmt_agg(42.0, true), "42%");
    }
}
