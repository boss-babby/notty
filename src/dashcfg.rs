//! Dashboard model — everything that is *saved* about the customisable dashboard.
//!
//! A workspace owns a [`Dash`]: several **boards** (tabs), each an ordered list of **widgets** laid out on a 12-column grid.
//! A widget is `kind` (a stable string id such as `"bar"`, `"table"`, `"tasks"`) plus a [`Cfg`] — one flat, forward
//! compatible settings struct (every field is `#[serde(default)]`, unknown JSON is ignored, `extra` carries free-form
//! settings for future widget types).  What a widget *does* is registered in `dashwidgets.rs` (`Def`), so a new feature of
//! the app only has to declare which settings it needs ([`Opt`]) and supply a draw function — the gallery, the settings
//! inspector, drag & drop, resizing, persistence and pinning then work for it automatically.  See `DASHBOARD.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dates;
use crate::model::{Filter, PropKind, Workspace};

// ───────────────────────────── enums used by the settings ─────────────────────────────

/// How the values of the records in one group are combined into a single number.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Agg {
    #[default]
    Count,
    /// Records where the property is not empty.
    Filled,
    /// Distinct values.
    Unique,
    Sum,
    Avg,
    Min,
    Max,
    Median,
    /// Share of records (in %) whose checkbox is ticked / number is above zero.
    Percent,
}

impl Agg {
    pub const ALL: [Agg; 9] = [Agg::Count, Agg::Filled, Agg::Unique, Agg::Sum, Agg::Avg, Agg::Min, Agg::Max, Agg::Median, Agg::Percent];

    pub fn label(self) -> &'static str {
        match self {
            Agg::Count => "Count of records",
            Agg::Filled => "Count filled",
            Agg::Unique => "Count unique",
            Agg::Sum => "Sum",
            Agg::Avg => "Average",
            Agg::Min => "Minimum",
            Agg::Max => "Maximum",
            Agg::Median => "Median",
            Agg::Percent => "Percent checked",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Agg::Count => "Count",
            Agg::Filled => "Filled",
            Agg::Unique => "Unique",
            Agg::Sum => "Sum",
            Agg::Avg => "Average",
            Agg::Min => "Min",
            Agg::Max => "Max",
            Agg::Median => "Median",
            Agg::Percent => "Percent",
        }
    }

    /// True when the aggregation reads a property (everything except a plain record count).
    pub fn needs_prop(self) -> bool {
        !matches!(self, Agg::Count)
    }

    pub fn is_percent(self) -> bool {
        matches!(self, Agg::Percent)
    }
}

/// Granularity used when a Date property is the grouping axis.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Bucket {
    Day,
    Week,
    #[default]
    Month,
    Quarter,
    Year,
}

impl Bucket {
    pub const ALL: [Bucket; 5] = [Bucket::Day, Bucket::Week, Bucket::Month, Bucket::Quarter, Bucket::Year];

    pub fn label(self) -> &'static str {
        match self {
            Bucket::Day => "Day",
            Bucket::Week => "Week",
            Bucket::Month => "Month",
            Bucket::Quarter => "Quarter",
            Bucket::Year => "Year",
        }
    }

    /// First day (day number) of the bucket that contains `day`.
    pub fn start(self, day: i64) -> i64 {
        match self {
            Bucket::Day => day,
            Bucket::Week => day - (day + 3).rem_euclid(7),
            Bucket::Month => {
                let (y, m, _) = dates::civil_from_days(day);
                dates::days_from_civil(y, m, 1)
            }
            Bucket::Quarter => {
                let (y, m, _) = dates::civil_from_days(day);
                dates::days_from_civil(y, (m - 1) / 3 * 3 + 1, 1)
            }
            Bucket::Year => {
                let (y, _, _) = dates::civil_from_days(day);
                dates::days_from_civil(y, 1, 1)
            }
        }
    }

    /// First day of the bucket after the one starting at `start`.
    pub fn next(self, start: i64) -> i64 {
        let (y, m, _) = dates::civil_from_days(start);
        match self {
            Bucket::Day => start + 1,
            Bucket::Week => start + 7,
            Bucket::Month => {
                if m == 12 {
                    dates::days_from_civil(y + 1, 1, 1)
                } else {
                    dates::days_from_civil(y, m + 1, 1)
                }
            }
            Bucket::Quarter => {
                if m >= 10 {
                    dates::days_from_civil(y + 1, 1, 1)
                } else {
                    dates::days_from_civil(y, m + 3, 1)
                }
            }
            Bucket::Year => dates::days_from_civil(y + 1, 1, 1),
        }
    }

    pub fn label_of(self, start: i64) -> String {
        let (y, m, d) = dates::civil_from_days(start);
        let mon = &dates::MONTHS[m as usize - 1][..3];
        match self {
            Bucket::Day | Bucket::Week => format!("{mon} {d}"),
            Bucket::Month => format!("{mon} ’{:02}", y.rem_euclid(100)),
            Bucket::Quarter => format!("Q{} ’{:02}", (m - 1) / 3 + 1, y.rem_euclid(100)),
            Bucket::Year => format!("{y}"),
        }
    }
}

/// A time window applied to a widget's records (through its date property, or "last edited").
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Range {
    #[default]
    All,
    Today,
    Last7,
    Last30,
    Last90,
    Last365,
    ThisWeek,
    ThisMonth,
    ThisQuarter,
    ThisYear,
    Next7,
    Next30,
}

impl Range {
    pub const ALL: [Range; 12] = [
        Range::All,
        Range::Today,
        Range::Last7,
        Range::Last30,
        Range::Last90,
        Range::Last365,
        Range::ThisWeek,
        Range::ThisMonth,
        Range::ThisQuarter,
        Range::ThisYear,
        Range::Next7,
        Range::Next30,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Range::All => "All time",
            Range::Today => "Today",
            Range::Last7 => "Last 7 days",
            Range::Last30 => "Last 30 days",
            Range::Last90 => "Last 90 days",
            Range::Last365 => "Last 12 months",
            Range::ThisWeek => "This week",
            Range::ThisMonth => "This month",
            Range::ThisQuarter => "This quarter",
            Range::ThisYear => "This year",
            Range::Next7 => "Next 7 days",
            Range::Next30 => "Next 30 days",
        }
    }

    /// Inclusive `(first, last)` day numbers, `None` for [`Range::All`].
    pub fn bounds(self, today: i64) -> Option<(i64, i64)> {
        Some(match self {
            Range::All => return None,
            Range::Today => (today, today),
            Range::Last7 => (today - 6, today),
            Range::Last30 => (today - 29, today),
            Range::Last90 => (today - 89, today),
            Range::Last365 => (today - 364, today),
            Range::ThisWeek => {
                let s = Bucket::Week.start(today);
                (s, s + 6)
            }
            Range::ThisMonth => (Bucket::Month.start(today), Bucket::Month.next(Bucket::Month.start(today)) - 1),
            Range::ThisQuarter => (Bucket::Quarter.start(today), Bucket::Quarter.next(Bucket::Quarter.start(today)) - 1),
            Range::ThisYear => (Bucket::Year.start(today), Bucket::Year.next(Bucket::Year.start(today)) - 1),
            Range::Next7 => (today, today + 6),
            Range::Next30 => (today, today + 29),
        })
    }

    /// The window of the same length that ends right before this one (for "vs previous period" trends).
    pub fn previous(self, today: i64) -> Option<(i64, i64)> {
        let (a, b) = self.bounds(today)?;
        let len = b - a + 1;
        match self {
            Range::ThisMonth => Some((Bucket::Month.start(a - 1), a - 1)),
            Range::ThisQuarter => Some((Bucket::Quarter.start(a - 1), a - 1)),
            Range::ThisYear => Some((Bucket::Year.start(a - 1), a - 1)),
            _ => Some((a - len, a - 1)),
        }
    }
}

// ───────────────────────────── widget settings ─────────────────────────────

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Item {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

fn yes() -> bool {
    true
}

/// Flat settings of a widget. Which fields a widget kind uses is declared by its `Def::opts`.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Cfg {
    // ── data source ──
    /// Source database page id (0 = none).
    #[serde(default)]
    pub db: u64,
    /// Saved view of that database whose filters + sorts are applied (view id, 0 = none) — "pin a filtered view".
    #[serde(default)]
    pub view: u64,
    /// Extra filters on top of the view.
    #[serde(default)]
    pub filters: Vec<Filter>,
    // ── analysis ──
    /// Property that makes the categories / x axis (0 = one single group).
    #[serde(default)]
    pub group: u64,
    /// Property that splits every category into series (stacked / multi-line); 0 = none.
    #[serde(default)]
    pub stack: u64,
    /// Property that is aggregated (0 with `Agg::Count` = number of records).
    #[serde(default)]
    pub value: u64,
    #[serde(default)]
    pub agg: Agg,
    #[serde(default)]
    pub bucket: Bucket,
    /// Scatter axes.
    #[serde(default)]
    pub x: u64,
    #[serde(default)]
    pub y: u64,
    // ── time ──
    /// Date property used by `range` / heat-map / calendar (0 = "last edited").
    #[serde(default)]
    pub date: u64,
    #[serde(default)]
    pub range: Range,
    // ── lists & tables ──
    #[serde(default)]
    pub cols: Vec<u64>,
    #[serde(default)]
    pub sort: u64,
    #[serde(default)]
    pub asc: bool,
    /// Max categories / rows (0 = automatic).
    #[serde(default)]
    pub limit: u32,
    /// Target for gauges, goal bars and number cards (0 = none).
    #[serde(default)]
    pub goal: f64,
    // ── personal widgets ──
    #[serde(default)]
    pub pages: Vec<u64>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub items: Vec<Item>,
    /// Countdown target (`YYYY-MM-DD`).
    #[serde(default)]
    pub target: String,
    // ── look ──
    #[serde(default = "yes")]
    pub legend: bool,
    #[serde(default = "yes")]
    pub labels: bool,
    #[serde(default)]
    pub palette: u8,
    /// Line / area: running total instead of per-bucket values.
    #[serde(default)]
    pub cumulative: bool,
    /// Hide groups without records.
    #[serde(default)]
    pub hide_empty: bool,
    /// Free-form settings for widgets added by future features.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            db: 0,
            view: 0,
            filters: vec![],
            group: 0,
            stack: 0,
            value: 0,
            agg: Agg::Count,
            bucket: Bucket::Month,
            x: 0,
            y: 0,
            date: 0,
            range: Range::All,
            cols: vec![],
            sort: 0,
            asc: false,
            limit: 0,
            goal: 0.0,
            pages: vec![],
            text: String::new(),
            items: vec![],
            target: String::new(),
            legend: true,
            labels: true,
            palette: 0,
            cumulative: false,
            hide_empty: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Widget {
    pub id: u64,
    /// Registry id of the widget type (`dashwidgets::defs`). Unknown kinds are kept and shown as a placeholder.
    pub kind: String,
    /// Empty = automatic title.
    #[serde(default)]
    pub title: String,
    /// Width in grid columns (1–12).
    #[serde(default = "four")]
    pub span: u8,
    /// Height in row units (1–8).
    #[serde(default = "three")]
    pub rows: u8,
    #[serde(default)]
    pub cfg: Cfg,
}

fn four() -> u8 {
    4
}
fn three() -> u8 {
    3
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Board {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub widgets: Vec<Widget>,
    /// 0 = a tab of the Home dashboard. Otherwise the id of the **page** that hosts this board: in Lumen (as in Notion) *any*
    /// page can be a dashboard — the page keeps its blocks and gains a grid of widgets.
    #[serde(default)]
    pub page: u64,
    /// Time window of this board (Today / This week / …) — used by dashboards that live on a page; Home tabs share `Dash::range`.
    #[serde(default)]
    pub range: Range,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Dash {
    #[serde(default)]
    pub boards: Vec<Board>,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub next_id: u64,
    /// Dashboard-wide time window: overrides the window of every widget that has none of its own.
    #[serde(default)]
    pub range: Range,
}

impl Dash {
    /// The time window that applies to the board `board` (page dashboards have their own, Home tabs share one).
    pub fn scope_range(&self, board: u64) -> Range {
        match self.boards.iter().find(|b| b.id == board) {
            Some(b) if b.page != 0 => b.range,
            _ => self.range,
        }
    }

    pub fn set_scope_range(&mut self, board: u64, r: Range) {
        match self.boards.iter_mut().find(|b| b.id == board) {
            Some(b) if b.page != 0 => b.range = r,
            _ => self.range = r,
        }
    }
}

thread_local! {
    static TOAST: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Queues a message for the app's toast area (database menus pin widgets without access to the app).
pub fn set_toast(msg: &str) {
    TOAST.with(|t| *t.borrow_mut() = Some(msg.to_owned()));
}

pub fn take_toast() -> Option<String> {
    TOAST.with(|t| t.borrow_mut().take())
}

pub const MAX_SPAN: u8 = 12;
pub const MAX_ROWS: u8 = 8;

/// Settings an inspector section edits. A widget type lists the ones it understands (`Def::opts`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opt {
    Db,
    View,
    Filters,
    Group,
    Stack,
    Value,
    Agg,
    Bucket,
    Date,
    Range,
    Cols,
    Sort,
    Limit,
    Goal,
    Axes,
    Pages,
    Text,
    Items,
    Target,
    Legend,
    Labels,
    Palette,
    Cumulative,
    HideEmpty,
}

/// Where a widget type shows up in the gallery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    Overview,
    Charts,
    Data,
    Personal,
}

impl Cat {
    pub const ALL: [Cat; 4] = [Cat::Overview, Cat::Charts, Cat::Data, Cat::Personal];

    pub fn label(self) -> &'static str {
        match self {
            Cat::Overview => "Workspace",
            Cat::Charts => "Charts",
            Cat::Data => "Records",
            Cat::Personal => "Personal",
        }
    }
}

// ───────────────────────────── operations ─────────────────────────────

impl Dash {
    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn board(&self) -> Option<&Board> {
        self.boards.get(self.active)
    }

    pub fn board_mut(&mut self) -> Option<&mut Board> {
        let i = self.active;
        self.boards.get_mut(i)
    }

    pub fn widget(&self, id: u64) -> Option<&Widget> {
        self.boards.iter().flat_map(|b| b.widgets.iter()).find(|w| w.id == id)
    }

    pub fn widget_mut(&mut self, id: u64) -> Option<&mut Widget> {
        self.boards.iter_mut().flat_map(|b| b.widgets.iter_mut()).find(|w| w.id == id)
    }

    pub fn add_board(&mut self, name: &str, widgets: Vec<Widget>) -> usize {
        let id = self.new_id();
        self.boards.push(Board { id, name: name.to_owned(), widgets, page: 0, range: Range::All });
        self.active = self.boards.len() - 1;
        self.active
    }

    /// Adds a widget to the active board (a board is created when there is none). Returns the widget id.
    pub fn add_widget(&mut self, mut w: Widget) -> u64 {
        if self.home_count() == 0 {
            self.add_board("Overview", vec![]);
        }
        self.fix_active();
        w.id = self.new_id();
        let id = w.id;
        let i = self.active;
        self.boards[i].widgets.push(w);
        id
    }

    pub fn remove_widget(&mut self, id: u64) {
        for b in &mut self.boards {
            b.widgets.retain(|w| w.id != id);
        }
    }

    /// Copies a widget right behind the original.
    pub fn duplicate_widget(&mut self, id: u64) -> Option<u64> {
        let nid = self.new_id();
        for b in &mut self.boards {
            if let Some(i) = b.widgets.iter().position(|w| w.id == id) {
                let mut c = b.widgets[i].clone();
                c.id = nid;
                if !c.title.is_empty() {
                    c.title.push_str(" (copy)");
                }
                b.widgets.insert(i + 1, c);
                return Some(nid);
            }
        }
        None
    }

    /// Moves widget `id` so that it sits before (`after == false`) or after the widget `target`.
    pub fn move_widget(&mut self, id: u64, target: u64, after: bool) {
        if id == target {
            return;
        }
        let Some(b) = self.boards.iter_mut().find(|b| b.widgets.iter().any(|w| w.id == id) && b.widgets.iter().any(|w| w.id == target)) else { return };
        let from = b.widgets.iter().position(|w| w.id == id).unwrap();
        let w = b.widgets.remove(from);
        let at = b.widgets.iter().position(|w| w.id == target).unwrap();
        b.widgets.insert(if after { at + 1 } else { at }, w);
    }

    /// Number of boards shown as tabs on the Home dashboard.
    pub fn home_count(&self) -> usize {
        self.boards.iter().filter(|b| b.page == 0).count()
    }

    pub fn remove_board(&mut self, i: usize) {
        if self.home_count() > 1 && i < self.boards.len() && self.boards[i].page == 0 {
            self.boards.remove(i);
            self.fix_active();
        }
    }

    /// Keeps `active` pointing at a Home tab (page boards are never tabs).
    pub fn fix_active(&mut self) {
        self.active = self.active.min(self.boards.len().saturating_sub(1));
        if self.boards.get(self.active).map_or(true, |b| b.page != 0) {
            self.active = self.boards.iter().position(|b| b.page == 0).unwrap_or(0);
        }
    }

    pub fn board_index(&self, id: u64) -> Option<usize> {
        self.boards.iter().position(|b| b.id == id)
    }

    /// The board that turns page `page` into a dashboard, if it has one.
    pub fn page_board(&self, page: u64) -> Option<u64> {
        self.boards.iter().find(|b| b.page == page && page != 0).map(|b| b.id)
    }

    /// Gives `page` a dashboard (idempotent) and returns the board id.
    pub fn ensure_page_board(&mut self, page: u64, name: &str) -> u64 {
        if let Some(b) = self.page_board(page) {
            return b;
        }
        let id = self.new_id();
        self.boards.push(Board { id, name: name.to_owned(), widgets: vec![], page, range: Range::All });
        id
    }

    /// Adds a widget to board `board` (0 = the active Home board). Returns the widget id.
    pub fn add_widget_to(&mut self, board: u64, mut w: Widget) -> u64 {
        if board == 0 {
            return self.add_widget(w);
        }
        w.id = self.new_id();
        let id = w.id;
        if let Some(i) = self.board_index(board) {
            self.boards[i].widgets.push(w);
        }
        id
    }

    /// Removes the dashboard of a page (the page and its blocks stay).
    pub fn drop_page_board(&mut self, page: u64) {
        self.boards.retain(|b| b.page != page || page == 0);
    }
}

pub fn new_widget(kind: &str, span: u8, rows: u8, mut cfg: Cfg) -> Widget {
    let mut kind = kind.to_owned();
    if PLOT_KINDS.contains(&kind.as_str()) {
        cfg.extra.insert("plot".into(), kind);
        kind = "chart".into();
    }
    Widget { id: 0, kind, title: String::new(), span, rows, cfg }
}

// ───────────────────────────── smart defaults ─────────────────────────────

/// Live databases (not trashed), in sidebar order.
pub fn databases(ws: &Workspace) -> Vec<u64> {
    ws.pages.iter().filter(|p| p.db.is_some() && !p.trashed).map(|p| p.id).collect()
}

/// Fills `cfg` with sensible choices for database `db`: first Select (or multi-select / checkbox / date) as grouping,
/// first Number as value, first Date as date, first two Numbers as scatter axes, the first properties as columns.
pub fn autofill(ws: &Workspace, cfg: &mut Cfg, db: u64) {
    let Some(d) = ws.db(db) else { return };
    cfg.db = db;
    cfg.view = 0;
    cfg.filters.clear();
    let first = |k: PropKind| d.props.iter().find(|p| p.kind == k).map(|p| p.id).unwrap_or(0);
    cfg.group = [PropKind::Select, PropKind::MultiSelect, PropKind::Checkbox].iter().map(|k| first(*k)).find(|id| *id != 0).unwrap_or_else(|| first(PropKind::Date));
    cfg.bucket = Bucket::Month;
    cfg.stack = 0;
    cfg.value = first(PropKind::Number);
    cfg.agg = if cfg.value != 0 { Agg::Sum } else { Agg::Count };
    cfg.date = first(PropKind::Date);
    let nums: Vec<u64> = d.props.iter().filter(|p| p.kind == PropKind::Number).map(|p| p.id).collect();
    cfg.x = nums.first().copied().unwrap_or(0);
    cfg.y = nums.get(1).copied().or(nums.first().copied()).unwrap_or(0);
    cfg.cols = d.props.iter().take(4).map(|p| p.id).collect();
    cfg.sort = 0;
    cfg.asc = true;
}

/// Best database for a new data widget: one with a Select property and records, then any with records, then any.
pub fn best_db(ws: &Workspace) -> Option<u64> {
    let dbs = databases(ws);
    let score = |id: &u64| {
        let has_sel = ws.db(*id).map_or(false, |d| d.props.iter().any(|p| p.kind == PropKind::Select));
        let rows = ws.rows(*id).len();
        (has_sel && rows > 0, rows > 0, has_sel)
    };
    dbs.iter().copied().max_by_key(|id| (score(id), std::cmp::Reverse(*id)))
}

/// Pins a saved view of a database to the active board as widget `kind`.
/// Called from the database toolbar ("Pin to dashboard") — filters and sorts of the view travel with the widget.
pub fn pin(ws: &mut Workspace, db: u64, view_index: usize, kind: &str) -> u64 {
    let mut cfg = Cfg::default();
    autofill(ws, &mut cfg, db);
    let name = ws.page(db).map(|p| p.title.clone()).unwrap_or_default();
    let mut vname = String::new();
    if let Some(v) = ws.db(db).and_then(|d| d.views.get(view_index)) {
        cfg.view = v.id;
        vname = v.name.clone();
    }
    let (span, rows) = match kind {
        "number" => (3, 1),
        "table" | "board" | "calendar" => (6, 4),
        "list" | "progress" => (4, 3),
        _ => (6, 3),
    };
    let mut w = new_widget(kind, span, rows, cfg);
    let name = if name.is_empty() { "Untitled".to_owned() } else { name };
    w.title = if vname.is_empty() || vname == "Table" || vname == "Default view" { name } else { format!("{name} · {vname}") };
    ws.dash.add_widget(w)
}

/// Pins a chart (or number) of one property, e.g. from a column menu: groups by `prop` (or aggregates it when it is a Number).
pub fn pin_prop(ws: &mut Workspace, db: u64, prop: u64, kind: &str) -> u64 {
    let mut cfg = Cfg::default();
    autofill(ws, &mut cfg, db);
    let k = ws.db(db).map(|d| d.kind_of(prop)).unwrap_or(PropKind::Text);
    let pname = ws.db(db).and_then(|d| d.prop(prop)).map(|p| p.name.clone()).unwrap_or_default();
    match k {
        PropKind::Number | PropKind::Formula | PropKind::Rollup => {
            cfg.value = prop;
            cfg.agg = Agg::Sum;
        }
        _ => {
            cfg.group = prop;
            cfg.value = 0;
            cfg.agg = Agg::Count;
        }
    }
    let title = ws.page(db).map(|p| p.title.clone()).unwrap_or_default();
    let mut w = new_widget(kind, if kind == "number" { 3 } else { 6 }, if kind == "number" { 1 } else { 3 }, cfg);
    w.title = format!("{title} · {pname}");
    ws.dash.add_widget(w)
}

// ───────────────────────────── preset boards ─────────────────────────────

fn with_title(mut w: Widget, t: &str) -> Widget {
    w.title = t.to_owned();
    w
}

/// The classic "home" board: KPIs, tasks, deadlines, one chart per database with a Select property, recents, activity.
pub fn overview_widgets(ws: &Workspace) -> Vec<Widget> {
    let mut v = vec![new_widget("stats", 12, 1, Cfg::default()), new_widget("tasks", 6, 3, Cfg::default()), new_widget("upcoming", 6, 3, Cfg::default())];
    let mut n = 0;
    for db in databases(ws) {
        let Some(d) = ws.db(db) else { continue };
        if !d.props.iter().any(|p| p.kind == PropKind::Select) || n >= 3 {
            continue;
        }
        let mut cfg = Cfg::default();
        autofill(ws, &mut cfg, db);
        let title = ws.page(db).map(|p| p.title.clone()).unwrap_or_default();
        let sel = d.prop(cfg.group).map(|p| p.name.clone()).unwrap_or_default();
        v.push(with_title(new_widget("bar", 6, 3, Cfg { value: 0, agg: Agg::Count, ..cfg.clone() }), &format!("{title} by {sel}")));
        if cfg.value != 0 {
            let vn = d.prop(cfg.value).map(|p| p.name.clone()).unwrap_or_default();
            v.push(with_title(new_widget("donut", 6, 3, cfg), &format!("{vn} by {sel}")));
        }
        n += 1;
    }
    v.push(new_widget("recent", 4, 3, Cfg::default()));
    v.push(new_widget("favorites", 4, 3, Cfg::default()));
    v.push(new_widget("activity", 4, 3, Cfg::default()));
    v
}

/// A block of widgets per database: record count, a distribution chart, a trend over time and the records themselves.
pub fn analytics_widgets(ws: &Workspace) -> Vec<Widget> {
    let mut v = vec![];
    for db in databases(ws).into_iter().take(4) {
        let Some(d) = ws.db(db) else { continue };
        let title = ws.page(db).map(|p| p.title.clone()).unwrap_or_default();
        let mut cfg = Cfg::default();
        autofill(ws, &mut cfg, db);
        v.push(with_title(new_widget("number", 3, 1, Cfg { value: 0, agg: Agg::Count, ..cfg.clone() }), &format!("{title} · records")));
        if cfg.value != 0 {
            let vn = d.prop(cfg.value).map(|p| p.name.clone()).unwrap_or_default();
            v.push(with_title(new_widget("number", 3, 1, cfg.clone()), &format!("{title} · total {vn}")));
        }
        if d.props.iter().any(|p| matches!(p.kind, PropKind::Select | PropKind::MultiSelect | PropKind::Checkbox)) {
            v.push(with_title(new_widget("donut", 6, 3, Cfg { value: 0, agg: Agg::Count, ..cfg.clone() }), &format!("{title} · breakdown")));
        }
        if let Some(dp) = d.first_of(PropKind::Date) {
            v.push(with_title(new_widget("area", 6, 3, Cfg { group: dp, bucket: Bucket::Month, value: 0, agg: Agg::Count, ..cfg.clone() }), &format!("{title} · over time")));
        }
        v.push(with_title(new_widget("table", 6, 3, cfg), &title));
    }
    if v.is_empty() {
        v.push(new_widget("stats", 12, 1, Cfg::default()));
        v.push(new_widget("activity", 6, 3, Cfg::default()));
    }
    v
}

pub fn personal_widgets() -> Vec<Widget> {
    vec![
        new_widget("today", 4, 2, Cfg::default()),
        new_widget("timer", 4, 2, Cfg::default()),
        new_widget("quickadd", 4, 2, Cfg::default()),
        new_widget("tasks", 6, 3, Cfg::default()),
        new_widget("checklist", 3, 3, Cfg { items: vec![Item { text: "Plan the week".into(), done: false }, Item { text: "Review inbox".into(), done: false }], ..Cfg::default() }),
        new_widget("notes", 3, 3, Cfg { text: "Jot things down here…".into(), ..Cfg::default() }),
    ]
}

/// Creates the first board for a workspace that has none (also upgrades workspaces saved before dashboards were customisable).
pub fn ensure_seeded(ws: &mut Workspace) {
    migrate_plots(&mut ws.dash);
    if ws.dash.home_count() == 0 {
        let w = overview_widgets(ws);
        ws.dash.add_board("Overview", vec![]);
        for x in w {
            ws.dash.add_widget(x);
        }
    }
    ws.dash.fix_active();
}

/// Every plot type that now lives inside the single unified **Chart** widget.
pub const PLOT_KINDS: [&str; 14] = ["bar", "hbar", "stacked", "line", "area", "pie", "donut", "funnel", "radar", "histogram", "scatter", "heatmap", "gauge", "number"];

/// The plot type of a widget: `Some(kind)` for a Chart widget (or a legacy per-plot widget), `None` for everything else.
pub fn plot_of(w: &Widget) -> Option<String> {
    if w.kind == "chart" {
        return Some(w.cfg.extra.get("plot").cloned().filter(|k| PLOT_KINDS.contains(&k.as_str())).unwrap_or_else(|| "bar".into()));
    }
    PLOT_KINDS.contains(&w.kind.as_str()).then(|| w.kind.clone())
}

/// Old workspaces stored one widget kind per plot type; they become Chart widgets whose plot type is a setting.
pub fn migrate_plots(d: &mut Dash) {
    for b in &mut d.boards {
        for w in &mut b.widgets {
            if w.kind != "chart" && PLOT_KINDS.contains(&w.kind.as_str()) {
                w.cfg.extra.insert("plot".into(), w.kind.clone());
                w.kind = "chart".into();
            }
        }
    }
}

/// Creates a page that is a dashboard: a normal page (icon, cover, sub-pages, blocks below the widgets) whose body is a widget grid.
/// `preset` is `blank`, `starter`, `overview`, `analytics` or `personal`. Returns the page id.
pub fn new_dashboard_page(ws: &mut Workspace, parent: Option<u64>, preset: &str) -> u64 {
    let id = ws.add_page(parent, crate::model::PageKind::Doc);
    if let Some(p) = ws.page_mut(id) {
        p.title = "New dashboard".into();
        p.icon = crate::lucide::LAYOUT_DASHBOARD.into();
        p.full_width = true;
    }
    let b = ws.dash.ensure_page_board(id, "Dashboard");
    let key = if preset == "starter" { "overview" } else { preset };
    fill_board(ws, b, key);
    id
}

/// Fills board `board` with the widgets of a preset (`overview`, `analytics`, `personal`).
pub fn fill_board(ws: &mut Workspace, board: u64, preset: &str) {
    let widgets = match preset {
        "overview" => overview_widgets(ws),
        "analytics" => analytics_widgets(ws),
        "personal" => personal_widgets(),
        _ => vec![],
    };
    for w in widgets {
        ws.dash.add_widget_to(board, w);
    }
}

pub fn add_preset(ws: &mut Workspace, preset: &str) {
    let (name, widgets) = match preset {
        "overview" => ("Overview", overview_widgets(ws)),
        "analytics" => ("Analytics", analytics_widgets(ws)),
        "personal" => ("Personal", personal_widgets()),
        _ => ("New board", vec![]),
    };
    ws.dash.add_board(name, vec![]);
    for w in widgets {
        ws.dash.add_widget(w);
    }
}

// ───────────────────────────── layout ─────────────────────────────

/// Greedy row packing on the 12-column grid. Returns, per widget, `(row index, column offset, columns)`; on narrow
/// screens (`cols_avail` < 12) spans are scaled so that a widget never gets narrower than a readable minimum.
pub fn pack(spans: &[u8], cols_avail: u8) -> Vec<(usize, u8, u8)> {
    let total = cols_avail.max(1);
    let mut out = Vec::with_capacity(spans.len());
    let (mut row, mut x) = (0usize, 0u8);
    for s in spans {
        let s = (*s).clamp(1, MAX_SPAN);
        let s = if total >= MAX_SPAN { s } else { ((s as u32 * total as u32 + MAX_SPAN as u32 - 1) / MAX_SPAN as u32).clamp(1, total as u32) as u8 };
        if x + s > total {
            row += 1;
            x = 0;
        }
        out.push((row, x, s));
        x += s;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_walk_the_calendar() {
        let d = dates::days_from_civil(2025, 11, 18);
        assert_eq!(dates::civil_from_days(Bucket::Month.start(d)), (2025, 11, 1));
        assert_eq!(dates::civil_from_days(Bucket::Quarter.start(d)), (2025, 10, 1));
        assert_eq!(dates::civil_from_days(Bucket::Year.start(d)), (2025, 1, 1));
        assert_eq!(dates::civil_from_days(Bucket::Month.next(Bucket::Month.start(d))), (2025, 12, 1));
        assert_eq!(dates::civil_from_days(Bucket::Quarter.next(Bucket::Quarter.start(d))), (2026, 1, 1));
        // 2025-11-18 is a Tuesday: the week starts on Monday the 17th
        assert_eq!(dates::civil_from_days(Bucket::Week.start(d)), (2025, 11, 17));
        assert_eq!(Bucket::Month.label_of(Bucket::Month.start(d)), "Nov ’25");
        assert_eq!(Bucket::Quarter.label_of(Bucket::Quarter.start(d)), "Q4 ’25");
    }

    #[test]
    fn ranges() {
        let today = dates::days_from_civil(2025, 11, 18);
        assert_eq!(Range::All.bounds(today), None);
        assert_eq!(Range::Last7.bounds(today), Some((today - 6, today)));
        let (a, b) = Range::ThisMonth.bounds(today).unwrap();
        assert_eq!(dates::civil_from_days(a), (2025, 11, 1));
        assert_eq!(dates::civil_from_days(b), (2025, 11, 30));
        let (pa, pb) = Range::ThisMonth.previous(today).unwrap();
        assert_eq!((dates::civil_from_days(pa), dates::civil_from_days(pb)), ((2025, 10, 1), (2025, 10, 31)));
        let (pa, pb) = Range::Last30.previous(today).unwrap();
        assert_eq!((pb, pa), (today - 30, today - 59));
    }

    #[test]
    fn old_and_future_json_loads() {
        // a widget saved by an older version, with unknown fields from a newer one
        let w: Widget = serde_json::from_str(r#"{"id":4,"kind":"bar","future_field":[1,2]}"#).unwrap();
        assert_eq!((w.span, w.rows, w.cfg.legend, w.cfg.labels), (4, 3, true, true));
        let d: Dash = serde_json::from_str("{}").unwrap();
        assert!(d.boards.is_empty());
        let back: Widget = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn move_duplicate_remove() {
        let mut d = Dash::default();
        let a = d.add_widget(new_widget("a", 4, 2, Cfg::default()));
        let b = d.add_widget(new_widget("b", 4, 2, Cfg::default()));
        let c = d.add_widget(new_widget("c", 4, 2, Cfg::default()));
        let order = |d: &Dash| d.board().unwrap().widgets.iter().map(|w| w.kind.clone()).collect::<Vec<_>>().join("");
        d.move_widget(c, a, false);
        assert_eq!(order(&d), "cab");
        d.move_widget(c, b, true);
        assert_eq!(order(&d), "abc");
        d.move_widget(a, a, true);
        assert_eq!(order(&d), "abc");
        let e = d.duplicate_widget(b).unwrap();
        assert_eq!(order(&d), "abbc");
        assert_ne!(e, b);
        d.remove_widget(b);
        assert_eq!(order(&d), "abc");
    }

    #[test]
    fn packing_wraps_and_scales() {
        let p = pack(&[6, 6, 4, 8, 12], 12);
        assert_eq!(p, vec![(0, 0, 6), (0, 6, 6), (1, 0, 4), (1, 4, 8), (2, 0, 12)]);
        // phone: 2 columns available → narrow widgets get 1 column, full rows stay full
        let p = pack(&[3, 3, 12, 6], 2);
        assert_eq!(p[0].2, 1);
        assert_eq!(p[2].2, 2);
        assert!(p.iter().all(|(_, x, s)| x + s <= 2));
    }
}
