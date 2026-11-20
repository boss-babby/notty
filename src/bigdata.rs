//! Big-data engine of the dashboards: results are computed **off the UI thread**, cached by data revision, and the UI only ever
//! paints finished numbers.
//!
//! * Native / Android: a small pool of worker threads (`lumen-data-N`, at most `cores − 1`, 1‥3 threads) runs the aggregation; the UI thread
//!   only copies rows into a compact snapshot — in slices of [`SLICE`] pages per frame (≈ 1–2 ms), so a million rows never freeze a frame.
//! * WebAssembly has no threads without cross-origin isolation + shared memory; there the finished job runs on the UI thread in an idle
//!   frame (no pointer down, no key pressed) — everything else (snapshot slicing, caching, sampling, drawing caps) is identical.
//! * Stale-while-revalidate: while a new result is computed the previous one stays on screen (with an "Updating…" chip).
//! * Revision key: [`bump`] is called by every `Workspace` mutation entry point; together with the page count it identifies a data state.
//!   Nothing is hashed per frame any more (the old row-fingerprint of `Workspace::query` was O(rows) per frame).
//! * Memory: a snapshot holds only the columns the widget uses (group / stack / value / x / y / date / filter / sort properties), and more than
//!   [`SAMPLE_CAP`] rows are sampled systematically (counts and sums are scaled back up, the widget shows "sampled"). The previous snapshot is
//!   dropped as soon as the worker owns the next one.
//! Small data (≤ [`small_rows`] rows) is computed synchronously like before: zero latency, zero flicker.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use egui::Context;

use crate::dashcfg::{Agg, Cfg, Range};
use crate::dashdata::{self as dd, Dataset, Num, Scatter};
use crate::model::{Page, PropKind, Workspace};

static REV: AtomicU64 = AtomicU64::new(1);
static SMALL: AtomicUsize = AtomicUsize::new(6_000);
static CAP: AtomicUsize = AtomicUsize::new(250_000);

/// Pages copied per frame while a snapshot is taken.
pub const SLICE: usize = 20_000;
/// Rows above which a widget switches to the background engine.
pub fn small_rows() -> usize {
    SMALL.load(Ordering::Relaxed)
}
/// Maximum rows of a snapshot (more are sampled).
pub fn sample_cap() -> usize {
    CAP.load(Ordering::Relaxed)
}
#[cfg(test)]
pub fn set_limits(small: usize, cap: usize) {
    SMALL.store(small, Ordering::Relaxed);
    CAP.store(cap, Ordering::Relaxed);
}

/// Marks the workspace data as changed (cheap; call from every mutation entry point).
#[inline]
pub fn bump() {
    REV.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn rev() -> u64 {
    #[cfg(test)]
    if FREEZE.with(|f| f.get()) {
        return 0;
    }
    REV.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    static FREEZE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Once per frame: the data revision moves on when something that can edit data happened (key, text, paste, click / tap, or the app
/// reports changes) — and once more in the frame after it, because the edit itself is applied late in the frame that received the event.
/// Pure scrolling / pointer movement never invalidates a cache.
pub fn frame_tick(ctx: &Context, dirty: bool) {
    use std::sync::atomic::AtomicBool;
    static LAST: AtomicBool = AtomicBool::new(false);
    let ev = dirty
        || ctx.input(|i| {
            i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Cut | egui::Event::PointerButton { .. } | egui::Event::Ime(_)))
        });
    let prev = LAST.swap(ev, Ordering::Relaxed);
    if ev || prev {
        bump();
    }
}

// ───────────────────────────── worker pool ─────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod pool {
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex, OnceLock};

    type Job = Box<dyn FnOnce() + Send + 'static>;
    static TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

    pub fn run(job: Job) {
        let tx = TX.get_or_init(|| {
            let (tx, rx): (Sender<Job>, Receiver<Job>) = channel();
            let rx = Arc::new(Mutex::new(rx));
            let n = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).clamp(1, 3);
            for i in 0..n {
                let rx = rx.clone();
                let _ = std::thread::Builder::new().name(format!("lumen-data-{i}")).stack_size(8 << 20).spawn(move || loop {
                    let job = { rx.lock().map(|r| r.recv()).ok().and_then(|r| r.ok()) };
                    match job {
                        Some(j) => {
                            // a panicking job must never take the worker (or the app) down
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(j));
                        }
                        None => break,
                    }
                });
            }
            Mutex::new(tx)
        });
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(job);
        }
    }
}

/// A job running somewhere else. `poll` returns the result once.
struct Task<T> {
    slot: Arc<Mutex<Option<T>>>,
    #[cfg(target_arch = "wasm32")]
    deferred: Option<Box<dyn FnOnce() -> T>>,
}

impl<T: Send + 'static> Task<T> {
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(ctx: &Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        let ctx = ctx.clone();
        pool::run(Box::new(move || {
            let v = f();
            if let Ok(mut g) = s2.lock() {
                *g = Some(v);
            }
            ctx.request_repaint();
        }));
        Task { slot }
    }
    #[cfg(target_arch = "wasm32")]
    fn spawn(_ctx: &Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        Task { slot: Arc::new(Mutex::new(None)), deferred: Some(Box::new(f)) }
    }
    fn poll(&mut self, idle: bool) -> Option<T> {
        #[cfg(target_arch = "wasm32")]
        if idle {
            if let Some(f) = self.deferred.take() {
                let v = f();
                if let Ok(mut g) = self.slot.lock() {
                    *g = Some(v);
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = idle;
        self.slot.lock().ok().and_then(|mut g| g.take())
    }
}

trait Pending {
    fn poll(&mut self, idle: bool) -> Option<Arc<dyn Any + Send + Sync>>;
}
struct PendingT<T: Send + Sync + 'static>(Task<T>);
impl<T: Send + Sync + 'static> Pending for PendingT<T> {
    fn poll(&mut self, idle: bool) -> Option<Arc<dyn Any + Send + Sync>> {
        self.0.poll(idle).map(|v| Arc::new(v) as Arc<dyn Any + Send + Sync>)
    }
}

// ───────────────────────────── snapshot ─────────────────────────────

/// Compact copy of the rows a widget needs (only the used properties; no blocks, history, icons …).
struct Snap {
    rev: u64,
    len: usize,
    idx: usize,
    db: u64,
    keep: BTreeSet<u64>,
    title: bool,
    stride: usize,
    seen: usize,
    total: usize,
    rows: Vec<Page>,
}

impl Snap {
    fn new(ws: &Workspace, cfg: &Cfg, total: usize) -> Snap {
        let mut keep: BTreeSet<u64> = [cfg.group, cfg.stack, cfg.value, cfg.x, cfg.y, cfg.date, cfg.sort].into_iter().filter(|p| *p != 0 && *p != dd::TITLE_SORT).collect();
        keep.extend(cfg.filters.iter().map(|f| f.prop));
        keep.extend(cfg.cols.iter().copied());
        if let Some(d) = ws.db(cfg.db) {
            if let Some(v) = d.views.iter().find(|v| v.id == cfg.view && cfg.view != 0) {
                keep.extend(v.filters.iter().map(|f| f.prop));
                keep.extend(v.sorts.iter().map(|s| s.prop));
            }
        }
        let title = keep.contains(&0) || cfg.sort == dd::TITLE_SORT;
        keep.remove(&0);
        let stride = total.div_ceil(sample_cap().max(1)).max(1);
        Snap { rev: rev(), len: ws.pages.len(), idx: 0, db: cfg.db, keep, title, stride, seen: 0, total, rows: Vec::with_capacity(total / stride + 1) }
    }

    /// Copies up to `n` pages; true when the whole workspace was scanned.
    fn step(&mut self, ws: &Workspace, n: usize) -> bool {
        let end = (self.idx + n).min(ws.pages.len());
        for p in &ws.pages[self.idx..end] {
            if p.row && p.parent == Some(self.db) && !p.trashed {
                let take = self.seen % self.stride == 0;
                self.seen += 1;
                if take {
                    self.rows.push(lite(p, &self.keep, self.title));
                }
            }
        }
        self.idx = end;
        self.idx >= ws.pages.len()
    }

    fn valid(&self, ws: &Workspace) -> bool {
        self.rev == rev() && self.len == ws.pages.len()
    }
}

fn lite(p: &Page, keep: &BTreeSet<u64>, title: bool) -> Page {
    let mut q = crate::model::blank_page(p.id, p.parent, p.kind);
    q.row = true;
    q.updated = p.updated;
    if title {
        q.title = p.title.clone();
    }
    for (k, v) in &p.props {
        if keep.contains(k) {
            q.props.insert(*k, v.clone());
        }
    }
    q
}

// ───────────────────────────── cache ─────────────────────────────

#[derive(Default)]
struct Entry {
    key: u64,
    val: Option<Arc<dyn Any + Send + Sync>>,
    task: Option<(u64, Box<dyn Pending>, Option<(usize, usize)>)>,
    snap: Option<Snap>,
    sampled: Option<(usize, usize)>,
    first_seen: f64,
    seen_key: u64,
    count_key: u64,
    count: usize,
}

thread_local! {
    static CACHE: RefCell<HashMap<(u64, TypeId), Entry>> = RefCell::new(HashMap::new());
}

/// A computed result plus how fresh it is.
pub struct Big<T> {
    pub v: Arc<T>,
    /// A newer result is being computed (the one shown is from the previous data state).
    pub busy: bool,
    /// `(rows used, rows total)` when the result is based on a sample.
    pub sampled: Option<(usize, usize)>,
}

impl<T> Deref for Big<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.v
    }
}

impl<T> Big<T> {
    /// "Updating…" / "Sampled 250,000 of 1,200,000" — a short status line, or empty.
    pub fn status(&self) -> String {
        match (self.busy, self.sampled) {
            (_, Some((a, b))) if !self.busy => format!("sampled {} of {} rows", dd::fmt_num(a as f64), dd::fmt_num(b as f64)),
            (true, _) => "Updating…".to_owned(),
            _ => String::new(),
        }
    }
}

fn key_of(ws: &Workspace, cfg: &Cfg, global: Range, tag: u8) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rev().hash(&mut h);
    ws.pages.len().hash(&mut h);
    tag.hash(&mut h);
    serde_json::to_string(cfg).unwrap_or_default().hash(&mut h);
    format!("{global:?}").hash(&mut h);
    dd::today().hash(&mut h);
    if let Some(d) = ws.db(cfg.db) {
        // property definitions (options, kinds) are part of the data state
        d.props.len().hash(&mut h);
    }
    h.finish()
}

fn rows_in_db(ws: &Workspace, db: u64) -> usize {
    ws.pages.iter().filter(|p| p.row && p.parent == Some(db) && !p.trashed).count()
}

fn derived(ws: &Workspace, cfg: &Cfg) -> bool {
    let Some(d) = ws.db(cfg.db) else { return false };
    let mut props: Vec<u64> = vec![cfg.group, cfg.stack, cfg.value, cfg.x, cfg.y, cfg.date];
    props.extend(cfg.filters.iter().map(|f| f.prop));
    if let Some(v) = d.views.iter().find(|v| v.id == cfg.view && cfg.view != 0) {
        props.extend(v.filters.iter().map(|f| f.prop));
        props.extend(v.sorts.iter().map(|s| s.prop));
    }
    props.iter().any(|p| *p != 0 && matches!(d.kind_of(*p), PropKind::Formula | PropKind::Rollup | PropKind::Relation))
}

type Compute<T> = fn(&Workspace, &Cfg, Range, f64) -> T;

/// The generic cached / background evaluation. `slot` identifies the consumer (widget id + salt).
pub fn cached<T: Default + Send + Sync + 'static>(ctx: &Context, slot: u64, tag: u8, ws: &Workspace, cfg: &Cfg, global: Range, f: Compute<T>) -> Big<T> {
    let key = key_of(ws, cfg, global, tag);
    let now = ctx.input(|i| i.time);
    let idle = ctx.input(|i| i.events.is_empty() && !i.pointer.any_down() && i.pointer.velocity().length() < 1.0);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let e = c.entry((slot, TypeId::of::<T>())).or_default();
        let fresh = |e: &Entry, busy: bool| -> Big<T> {
            let v = e.val.clone().and_then(|a| a.downcast::<T>().ok()).unwrap_or_else(|| Arc::new(T::default()));
            Big { v, busy, sampled: e.sampled }
        };
        if e.seen_key != key {
            e.seen_key = key;
            e.first_seen = now;
        }
        if e.key == key && e.val.is_some() {
            return fresh(e, false);
        }
        // a running job?
        if let Some((tk, p, smp)) = e.task.as_mut() {
            if let Some(v) = p.poll(idle) {
                let (tk, smp) = (*tk, *smp);
                e.task = None;
                e.val = Some(v);
                e.key = tk;
                e.sampled = smp;
                if tk == key {
                    return fresh(e, false);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(if cfg!(target_arch = "wasm32") { 60 } else { 120 }));
                return fresh(e, true);
            }
        }
        // new data state: small → synchronous, large → snapshot + worker
        if e.count_key != key {
            e.count = rows_in_db(ws, cfg.db);
            e.count_key = key;
        }
        if e.count <= small_rows() || derived(ws, cfg) {
            e.val = Some(Arc::new(f(ws, cfg, global, 1.0)));
            e.key = key;
            e.sampled = None;
            e.snap = None;
            return fresh(e, false);
        }
        // debounce a burst of edits (typing in a row) — but never leave the widget empty
        if e.val.is_some() && now - e.first_seen < 0.4 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return fresh(e, true);
        }
        if e.snap.as_ref().map_or(false, |s| !s.valid(ws)) {
            e.snap = None;
        }
        let total = e.count;
        let snap = e.snap.get_or_insert_with(|| Snap::new(ws, cfg, total));
        if !snap.step(ws, SLICE) {
            ctx.request_repaint();
            return fresh(e, true);
        }
        let snap = e.snap.take().expect("snapshot");
        let factor = if snap.stride > 1 { snap.total as f64 / snap.rows.len().max(1) as f64 } else { 1.0 };
        let sampled = (snap.stride > 1).then_some((snap.rows.len(), snap.total));
        let db_page = ws.page(cfg.db).map(|p| {
            let mut q = crate::model::blank_page(p.id, p.parent, p.kind);
            q.title = p.title.clone();
            q.db = p.db.clone();
            q
        });
        // every non-row page (titles for relation labels, the database definitions) — small compared with the rows
        let others: Vec<Page> = ws.pages.iter().filter(|p| !p.row && p.id != cfg.db).map(|p| {
            let mut q = crate::model::blank_page(p.id, p.parent, p.kind);
            q.title = p.title.clone();
            q.db = p.db.clone();
            q
        }).collect();
        let cfg2 = Cfg { limit: if cfg.limit == 0 { 300 } else { cfg.limit }, ..cfg.clone() };
        let (cfg2, global) = (cfg2, global);
        let name = ws.name.clone();
        let task = Task::spawn(ctx, move || {
            let mut m = Workspace::seed();
            m.name = name;
            m.pages = db_page.into_iter().chain(others).chain(snap.rows).collect();
            let out = f(&m, &cfg2, global, factor);
            drop(m);
            out
        });
        e.task = Some((key, Box::new(PendingT(task)), sampled));
        ctx.request_repaint();
        fresh(e, true)
    })
}

/// Drops every cached result (memory pressure).
pub fn clear_cache() {
    CACHE.with(|c| c.borrow_mut().clear());
}

/// Runs a job off the UI thread (native / Android: worker pool; WebAssembly: immediately, there are no threads).
pub fn run_bg(job: Box<dyn FnOnce() + Send + 'static>) {
    #[cfg(not(target_arch = "wasm32"))]
    pool::run(job);
    #[cfg(target_arch = "wasm32")]
    job();
}

fn fresh_of<T: Default + Send + Sync + 'static>(e: &Entry, busy: bool) -> Big<T> {
    let v = e.val.clone().and_then(|a| a.downcast::<T>().ok()).unwrap_or_else(|| Arc::new(T::default()));
    Big { v, busy, sampled: e.sampled }
}

/// Like [`cached`], for results computed straight from a large dataset's store (no snapshot, no sampling — the numbers are exact).
/// The key is the dataset's own version, so edits elsewhere never invalidate it.
pub fn cached_job<T: Default + Send + Sync + 'static>(ctx: &Context, slot: u64, tag: u8, cfg: &Cfg, global: Range, ver: u64, make: impl FnOnce() -> Box<dyn FnOnce() -> T + Send>) -> Big<T> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (tag, ver, serde_json::to_string(cfg).unwrap_or_default(), format!("{global:?}")).hash(&mut h);
    let key = h.finish();
    let now = ctx.input(|i| i.time);
    let idle = ctx.input(|i| i.events.is_empty() && !i.pointer.any_down());
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let e = c.entry((slot, TypeId::of::<T>())).or_default();
        if e.seen_key != key {
            e.seen_key = key;
            e.first_seen = now;
        }
        if e.key == key && e.val.is_some() {
            return fresh_of::<T>(e, false);
        }
        if let Some((tk, p, _)) = e.task.as_mut() {
            if let Some(v) = p.poll(idle) {
                let tk = *tk;
                e.task = None;
                e.val = Some(v);
                e.key = tk;
                e.sampled = None;
                if tk == key {
                    return fresh_of::<T>(e, false);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return fresh_of::<T>(e, true);
            }
        }
        if e.val.is_some() && now - e.first_seen < 0.3 {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return fresh_of::<T>(e, true);
        }
        let task = Task::spawn(ctx, make());
        e.task = Some((key, Box::new(PendingT(task)), None));
        ctx.request_repaint();
        fresh_of::<T>(e, true)
    })
}

fn big_of(ws: &Workspace, cfg: &Cfg) -> Option<(crate::bigtable::Shared, u64)> {
    let m = ws.bigs.get(&cfg.db)?;
    Some((crate::bigtable::open_store(m, false)?, crate::bigtable::version(&m.dir)))
}

/// Drops the cached results of a widget (it was deleted).
pub fn forget(slot: u64) {
    CACHE.with(|c| c.borrow_mut().retain(|k, _| k.0 != slot));
}

// ───────────────────────────── typed entry points ─────────────────────────────

fn scale(ds: &mut Dataset, agg: Agg, factor: f64) {
    if factor > 1.0 && matches!(agg, Agg::Count | Agg::Sum) {
        for s in ds.series.iter_mut() {
            for v in s.vals.iter_mut() {
                *v = (*v * factor).round();
            }
        }
    }
}

fn c_grouped(ws: &Workspace, cfg: &Cfg, g: Range, f: f64) -> Dataset {
    let mut d = dd::grouped(ws, cfg, g);
    scale(&mut d, cfg.agg, f);
    d
}
fn c_histogram(ws: &Workspace, cfg: &Cfg, g: Range, f: f64) -> Dataset {
    let mut d = dd::histogram(ws, cfg, g);
    scale(&mut d, Agg::Count, f);
    d
}
fn c_number(ws: &Workspace, cfg: &Cfg, g: Range, f: f64) -> Num {
    let mut n = dd::number(ws, cfg, g);
    if f > 1.0 && matches!(cfg.agg, Agg::Count | Agg::Sum) {
        n.value *= f;
        n.prev = n.prev.map(|p| p * f);
    }
    n.rows.truncate(5_000);
    n
}
/// Scatter plots keep at most [`MAX_POINTS`] points (stride sampling), a canvas cannot show more than that anyway.
pub const MAX_POINTS: usize = 6_000;
fn c_scatter(ws: &Workspace, cfg: &Cfg, g: Range, _f: f64) -> Scatter {
    let mut s = dd::scatter(ws, cfg, g);
    if s.pts.len() > MAX_POINTS {
        let stride = s.pts.len().div_ceil(MAX_POINTS);
        s.pts = s.pts.into_iter().step_by(stride).collect();
    }
    s
}
fn c_heat(ws: &Workspace, cfg: &Cfg, g: Range, _f: f64) -> BTreeMap<i64, Vec<u64>> {
    let mut m = dd::heat(ws, cfg, g);
    for v in m.values_mut() {
        v.truncate(2_000);
    }
    m
}
fn c_rows(ws: &Workspace, cfg: &Cfg, g: Range, _f: f64) -> Vec<u64> {
    dd::rows(ws, cfg, g)
}
fn c_sorted(ws: &Workspace, cfg: &Cfg, g: Range, _f: f64) -> Vec<u64> {
    dd::sorted_rows(ws, cfg, g)
}

const fn salted(slot: u64, salt: u64) -> u64 {
    slot.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt
}

pub fn grouped(ctx: &Context, slot: u64, salt: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Dataset> {
    if let Some((st, ver)) = big_of(ws, cfg) {
        return cached_job(ctx, salted(slot, salt), 1, cfg, g, ver, || {
            let (db, cfg) = (ws.db(cfg.db).cloned().unwrap_or_else(|| crate::model::Database { props: vec![], views: vec![], active: 0 }), cfg.clone());
            Box::new(move || crate::bigtable::big_grouped(&st, &db, &cfg))
        });
    }
    cached(ctx, salted(slot, salt), 1, ws, cfg, g, c_grouped)
}
pub fn histogram(ctx: &Context, slot: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Dataset> {
    if ws.bigs.contains_key(&cfg.db) {
        return Big { v: Arc::new(Dataset::default()), busy: false, sampled: None };
    }
    cached(ctx, salted(slot, 2), 2, ws, cfg, g, c_histogram)
}
pub fn number(ctx: &Context, slot: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Num> {
    if let Some((st, ver)) = big_of(ws, cfg) {
        return cached_job(ctx, salted(slot, 3), 3, cfg, g, ver, || {
            let cfg = cfg.clone();
            Box::new(move || crate::bigtable::big_number(&st, &cfg))
        });
    }
    cached(ctx, salted(slot, 3), 3, ws, cfg, g, c_number)
}
pub fn scatter(ctx: &Context, slot: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Scatter> {
    if let Some((st, ver)) = big_of(ws, cfg) {
        return cached_job(ctx, salted(slot, 4), 4, cfg, g, ver, || {
            let cfg = cfg.clone();
            Box::new(move || crate::bigtable::big_scatter(&st, &cfg, MAX_POINTS))
        });
    }
    cached(ctx, salted(slot, 4), 4, ws, cfg, g, c_scatter)
}
pub fn heat(ctx: &Context, slot: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<BTreeMap<i64, Vec<u64>>> {
    cached(ctx, salted(slot, 5), 5, ws, cfg, g, c_heat)
}
pub fn rows(ctx: &Context, slot: u64, salt: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Vec<u64>> {
    cached(ctx, salted(slot, 6 + salt), 6, ws, cfg, g, c_rows)
}
pub fn sorted_rows(ctx: &Context, slot: u64, ws: &Workspace, cfg: &Cfg, g: Range) -> Big<Vec<u64>> {
    cached(ctx, salted(slot, 20), 7, ws, cfg, g, c_sorted)
}

#[cfg(test)]
mod tests {
    use super::*;
    static LOCK: Mutex<()> = Mutex::new(());

    fn big_ws(n: usize) -> (Workspace, Cfg) {
        let mut ws = Workspace::seed();
        let db = ws.pages.iter().find(|p| p.db.is_some()).map(|p| p.id).expect("a database");
        ws.pages.retain(|p| !(p.row && p.parent == Some(db)));
        let (sel, num) = {
            let d = ws.db(db).unwrap();
            (d.props.iter().find(|p| p.kind == PropKind::Select).map(|p| p.id), d.props.iter().find(|p| p.kind == PropKind::Number).map(|p| p.id))
        };
        let opts: Vec<u64> = sel.map(|s| ws.db(db).unwrap().prop(s).unwrap().opts.iter().map(|o| o.id).collect()).unwrap_or_default();
        let base = ws.next_id + 10;
        for i in 0..n as u64 {
            let mut p = crate::model::blank_page(base + i, Some(db), crate::model::PageKind::Doc);
            p.row = true;
            p.title = format!("row {i}");
            if let (Some(s), false) = (sel, opts.is_empty()) {
                p.props.insert(s, crate::model::Val::Sel(opts[i as usize % opts.len()]));
            }
            if let Some(nn) = num {
                p.props.insert(nn, crate::model::Val::Num((i % 100) as f64));
            }
            ws.pages.push(p);
        }
        ws.next_id = base + n as u64 + 1;
        let cfg = Cfg { db, group: sel.unwrap_or(0), agg: Agg::Count, ..Default::default() };
        (ws, cfg)
    }

    #[test]
    fn background_result_equals_direct_result() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        FREEZE.with(|f| f.set(true));
        set_limits(100, 1_000_000);
        let (ws, cfg) = big_ws(2_000);
        let direct = dd::grouped(&ws, &cfg, Range::All);
        let ctx = Context::default();
        let mut got = None;
        for _ in 0..400 {
            let _ = ctx.run(Default::default(), |ctx| {
                let r = grouped(ctx, 7, 0, &ws, &cfg, Range::All);
                if !r.busy {
                    got = Some(r.v.clone());
                }
            });
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let got = got.expect("the worker finished");
        assert_eq!(got.cats, direct.cats);
        assert_eq!(got.series[0].vals, direct.series[0].vals);
        assert_eq!(got.total(), 2_000.0);
        set_limits(6_000, 250_000);
    }

    #[test]
    fn sampling_scales_counts_back() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        FREEZE.with(|f| f.set(true));
        set_limits(100, 500);
        let (ws, cfg) = big_ws(5_000);
        let ctx = Context::default();
        let mut got = None;
        for _ in 0..600 {
            let _ = ctx.run(Default::default(), |ctx| {
                let r = grouped(ctx, 9, 0, &ws, &cfg, Range::All);
                if !r.busy {
                    got = Some((r.v.total(), r.sampled));
                }
            });
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let (total, smp) = got.expect("finished");
        assert_eq!(smp, Some((500, 5_000)));
        assert!((total - 5_000.0).abs() <= 60.0, "scaled total {total}");
        set_limits(6_000, 250_000);
    }

    #[test]
    fn snapshots_are_taken_in_slices() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        FREEZE.with(|f| f.set(true));
        set_limits(6_000, 250_000);
        let (ws, cfg) = big_ws(3_000);
        let mut s = Snap::new(&ws, &cfg, 3_000);
        let mut steps = 0;
        while !s.step(&ws, 700) {
            steps += 1;
        }
        assert!(steps >= 1);
        assert_eq!(s.rows.len(), 3_000);
        assert!(s.rows.iter().all(|r| r.blocks.is_empty() && r.history.is_empty()));
    }
}
