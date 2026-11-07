//! Automation engine: rules, one-click buttons, repeat rules and reminders.
//!
//! Design goals (the engine is the foundation for the cloud / multi-user features that come later):
//!
//! * **Pure data + pure functions.** Nothing in here touches egui, the clock or the OS: `tick` takes the current time as an argument and
//!   returns an [`Output`] describing what happened (notices to show / send to the OS). That makes every rule unit-testable and lets a future
//!   server run the very same code against a synced workspace.
//! * **Change detection by snapshot diff.** The UI edits `Page::props` from many places (table cells, board drags, the timeline, bulk
//!   actions …). Instead of hooking every one of them, the engine keeps a snapshot of all rows and diffs it a few times per second, which
//!   yields `Event`s (`Added`, `Changed`). A remote change that arrives from a sync layer produces exactly the same events.
//! * **Open vocabulary.** `Trigger`, `Action` and `Setter` are enums with `#[serde(default)]` everywhere, so new variants / fields never
//!   break an old workspace file.
//! * **Safe by default.** Cascades are depth-limited, every rule is rate-limited per row, scheduled triggers ignore stale occurrences
//!   (enabling a rule never replays the past) and text edits are debounced (no rule fires per keystroke).
//!
//! Concepts
//! * [`Rule`]       — *when* (trigger) + *if* (conditions, the same `Filter`s database views use) + *then* (actions).
//! * [`Repeat`]     — recurrence of a date property (daily / weekly on chosen weekdays / monthly / yearly, end rules).
//!                    Completing a recurring task rolls the date forward and resets the status; or spawns the next copy.
//! * [`Reminder`]   — a date + time (+ lead time, + optional repeat) that raises a notification; bound to a row's date property or standalone.
//! * Buttons        — `BlockKind::Button` blocks carry a list of [`Action`]s that run on click.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::dates;
use crate::model::{row_matches, Block, BlockKind, Filter, FilterOp, Opt, Page, PropKind, Val, Workspace};

// ───────────────────────────── repeat rules ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Freq {
    #[default]
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    pub const ALL: [Freq; 4] = [Freq::Daily, Freq::Weekly, Freq::Monthly, Freq::Yearly];

    pub fn label(self) -> &'static str {
        match self {
            Freq::Daily => "Day",
            Freq::Weekly => "Week",
            Freq::Monthly => "Month",
            Freq::Yearly => "Year",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum End {
    #[default]
    Never,
    /// Last day (day number) an occurrence may fall on.
    On(i64),
    /// Total number of occurrences, the first one included.
    After(u32),
}

fn one() -> u32 {
    1
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Repeat {
    pub freq: Freq,
    #[serde(default = "one")]
    pub every: u32,
    /// Weekly: bit 0 = Monday … bit 6 = Sunday (0 = the weekday of the current date).
    #[serde(default)]
    pub weekdays: u8,
    #[serde(default)]
    pub end: End,
    /// Occurrences completed so far (used by `End::After`).
    #[serde(default)]
    pub done: u32,
    /// Rows: when the task is completed, roll the date forward and reset the status.
    #[serde(default)]
    pub on_complete: bool,
    /// Rows: when the date has passed, roll it forward by itself (habits, "take vitamins", weekly reminders).
    #[serde(default)]
    pub roll: bool,
    /// On completion keep the finished row as a record and create the next occurrence as a copy.
    #[serde(default)]
    pub spawn: bool,
    /// The next date counts from the completion day instead of the old due date ("2 weeks after I finish").
    #[serde(default)]
    pub from_completion: bool,
    /// Status property that decides "completed" (0 = automatic: any checkbox / a "Done"-like select option).
    #[serde(default)]
    pub status_prop: u64,
    /// Option that means completed (0 = a "Done"-like name).
    #[serde(default)]
    pub done_opt: u64,
    /// Option the status is reset to (0 = the first option that is not "Done"-like).
    #[serde(default)]
    pub reset_opt: u64,
}

impl Default for Repeat {
    fn default() -> Self {
        Repeat { freq: Freq::Weekly, every: 1, weekdays: 0, end: End::Never, done: 0, on_complete: true, roll: false, spawn: false, from_completion: false, status_prop: 0, done_opt: 0, reset_opt: 0 }
    }
}

/// Adds `n` months to a day number, clamping the day of month ("Jan 31 + 1 month" = Feb 28/29).
fn add_months(day: i64, n: i64) -> i64 {
    let (y, m, d) = dates::civil_from_days(day);
    let total = y as i64 * 12 + (m as i64 - 1) + n;
    let (y2, m2) = (total.div_euclid(12) as i32, total.rem_euclid(12) as u32 + 1);
    dates::days_from_civil(y2, m2, d.min(dates::days_in_month(y2, m2)))
}

fn week_index(day: i64) -> i64 {
    (day + 3).div_euclid(7)
}

fn weekday_of(day: i64) -> u32 {
    (day + 3).rem_euclid(7) as u32
}

impl Repeat {
    pub fn daily() -> Repeat {
        Repeat { freq: Freq::Daily, ..Repeat::default() }
    }

    pub fn weekly_on(days: &[u32]) -> Repeat {
        Repeat { freq: Freq::Weekly, weekdays: days.iter().fold(0u8, |m, d| m | (1 << (d % 7))), ..Repeat::default() }
    }

    fn every(&self) -> i64 {
        self.every.clamp(1, 999) as i64
    }

    /// First occurrence strictly after `day`.
    pub fn next_after(&self, day: i64) -> i64 {
        let n = self.every();
        match self.freq {
            Freq::Daily => day + n,
            Freq::Weekly => {
                let mask = if self.weekdays & 0x7f == 0 { 1u8 << weekday_of(day) } else { self.weekdays & 0x7f };
                let base = week_index(day);
                for k in 1..=(7 * n + 7) {
                    let d = day + k;
                    if mask & (1 << weekday_of(d)) != 0 && (week_index(d) - base).rem_euclid(n) == 0 {
                        return d;
                    }
                }
                day + 7 * n
            }
            Freq::Monthly => add_months(day, n),
            Freq::Yearly => add_months(day, 12 * n),
        }
    }

    /// The next date value after `value` (a `YYYY-MM-DD` or range value). `None` when the series is over or the value is not a date.
    /// Occurrences in the past are skipped: a task that is three weeks overdue and repeats weekly lands on the next occurrence ≥ today.
    pub fn advance(&self, value: &str, today: i64) -> Option<String> {
        let (a, b) = dates::span(value)?;
        let len = b - a;
        let mut next = if self.from_completion { self.next_after(today) } else { self.next_after(a) };
        let mut guard = 0;
        while next < today && guard < 2000 {
            next = self.next_after(next);
            guard += 1;
        }
        match self.end {
            End::On(last) if next > last => return None,
            End::After(k) if self.done + 1 >= k => return None,
            _ => {}
        }
        Some(dates::span_value(next, next + len))
    }

    /// "Every 2 weeks on Mon, Wed · 5 times" style summary.
    pub fn label(&self) -> String {
        let n = self.every();
        let mut s = match (self.freq, n) {
            (Freq::Daily, 1) => "Every day".to_owned(),
            (Freq::Weekly, 1) => "Every week".to_owned(),
            (Freq::Monthly, 1) => "Every month".to_owned(),
            (Freq::Yearly, 1) => "Every year".to_owned(),
            (f, n) => format!("Every {n} {}s", f.label().to_lowercase()),
        };
        if self.freq == Freq::Weekly && self.weekdays & 0x7f != 0 {
            let names: Vec<&str> = (0..7).filter(|i| self.weekdays & (1 << i) != 0).map(|i| dates::WEEKDAYS[i]).collect();
            s.push_str(&format!(" on {}", names.join(", ")));
        }
        match self.end {
            End::Never => {}
            End::On(d) => s.push_str(&format!(" until {}", dates::pretty(&dates::iso_of_day(d)))),
            End::After(k) => s.push_str(&format!(" · {k} times")),
        }
        s
    }
}

// ───────────────────────────── rules ─────────────────────────────

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum ChangeTo {
    /// Any change of the property.
    Any,
    /// A select property became this option.
    Sel(u64),
    /// A multi-select property gained this option.
    Has(u64),
    Check(bool),
}

impl ChangeTo {
    fn matches(&self, old: &Option<Val>, new: &Option<Val>) -> bool {
        match self {
            ChangeTo::Any => true,
            ChangeTo::Sel(o) => matches!(new, Some(Val::Sel(n)) if n == o),
            ChangeTo::Has(o) => {
                let had = matches!(old, Some(Val::Multi(m)) if m.contains(o));
                matches!(new, Some(Val::Multi(m)) if m.contains(o)) && !had
            }
            ChangeTo::Check(b) => matches!(new, Some(Val::Check(n)) if n == b) || (!*b && new.is_none() && matches!(old, Some(Val::Check(true)))),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Trigger {
    /// A row is added to the database.
    RowAdded,
    /// A property of a row changed (optionally: to a given value).
    PropChanged { prop: u64, to: ChangeTo },
    /// The day of a date property (± `offset_days`) at `minute` after midnight is reached — "1 day before it is due", "when overdue".
    DateReached { prop: u64, offset_days: i32, minute: u16 },
    /// On a schedule: every day / week / month … at `minute`. Runs once per occurrence, without a row.
    Schedule { repeat: Repeat, start_day: i64, minute: u16 },
    /// Only when someone presses "Run now" (applies to every row that matches the conditions).
    Manual,
}

impl Trigger {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Trigger::RowAdded => "Row added",
            Trigger::PropChanged { .. } => "Property changes",
            Trigger::DateReached { .. } => "Date arrives",
            Trigger::Schedule { .. } => "On a schedule",
            Trigger::Manual => "Manually",
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Setter {
    Text(String),
    Num(f64),
    Sel(u64),
    /// Add an option to a multi-select.
    AddOpt(u64),
    Check(bool),
    /// Date = today.
    Today,
    /// Date = today + n days.
    InDays(i32),
    /// Move the current date by n days (negative = earlier).
    ShiftDays(i32),
    Clear,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Action {
    /// Set a property of the row that triggered the rule (or the row the button sits on).
    Set { prop: u64, value: Setter },
    /// Create a row ("create a task"). `{title}`, `{today}`, `{prop:Name}` are replaced from the triggering row.
    CreateRow { db: u64, title: String, sets: Vec<(u64, Setter)> },
    /// Move the triggering row into another database (matching properties are carried over by name).
    MoveRow { db: u64 },
    /// Rename the row.
    Rename { title: String },
    /// Add a text / to-do block to the end of the page.
    Append { text: String, todo: bool },
    /// Raise a notification (inbox + desktop / push).
    Notify { message: String },
    /// Duplicate the row.
    Duplicate,
    /// Move the row to the trash.
    Trash,
}

impl Action {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Action::Set { .. } => "Set property",
            Action::CreateRow { .. } => "Create row",
            Action::MoveRow { .. } => "Move row",
            Action::Rename { .. } => "Rename",
            Action::Append { .. } => "Add to page",
            Action::Notify { .. } => "Notify",
            Action::Duplicate => "Duplicate",
            Action::Trash => "Move to trash",
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    #[serde(default = "yes")]
    pub on: bool,
    /// Database the rule watches (0 = none: only for scheduled rules without rows).
    #[serde(default)]
    pub db: u64,
    pub trigger: Trigger,
    #[serde(default)]
    pub conds: Vec<Filter>,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Date triggers ignore rows that are completed.
    #[serde(default = "yes")]
    pub skip_done: bool,
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub last_run: f64,
    /// Scheduled rules: next occurrence (local minutes since epoch; 0 = not computed yet).
    #[serde(default)]
    pub next_min: i64,
    /// Date triggers: row → occurrence (local minute) that already fired.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fired: BTreeMap<u64, i64>,
}

fn yes() -> bool {
    true
}

impl Rule {
    pub fn new(id: u64, name: &str, db: u64, trigger: Trigger, actions: Vec<Action>) -> Rule {
        Rule { id, name: name.into(), on: true, db, trigger, conds: vec![], actions, skip_done: true, runs: 0, last_run: 0.0, next_min: 0, fired: BTreeMap::new() }
    }
}

// ───────────────────────────── reminders, notices, log, settings ─────────────────────────────

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Reminder {
    pub id: u64,
    /// The page the reminder belongs to (0 = none).
    #[serde(default)]
    pub page: u64,
    /// Date property of the row that gives the day (0 = standalone: `day` is used).
    #[serde(default)]
    pub prop: u64,
    #[serde(default)]
    pub day: i64,
    /// Time of day, minutes after midnight.
    #[serde(default = "nine")]
    pub minute: u16,
    /// Fire this many minutes before.
    #[serde(default)]
    pub lead_min: u32,
    #[serde(default)]
    pub message: String,
    /// Standalone reminders only: repeat after firing.
    #[serde(default)]
    pub repeat: Option<Repeat>,
    /// Occurrence (local minute) that already fired.
    #[serde(default)]
    pub fired_for: i64,
    #[serde(default)]
    pub done: bool,
}

fn nine() -> u16 {
    9 * 60
}

impl Reminder {
    /// Local minute of the due time, `None` when the bound date vanished.
    pub fn due_min(&self, ws: &Workspace) -> Option<i64> {
        let day = if self.prop != 0 {
            match ws.page(self.page)?.props.get(&self.prop)? {
                Val::Text(s) => dates::span(s)?.0,
                _ => return None,
            }
        } else {
            self.day
        };
        Some(day * 1440 + self.minute as i64)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum NoticeKind {
    #[default]
    System,
    Reminder,
    Rule,
    Button,
    Repeat,
}

impl NoticeKind {
    pub fn label(self) -> &'static str {
        match self {
            NoticeKind::System => "System",
            NoticeKind::Reminder => "Reminder",
            NoticeKind::Rule => "Automation",
            NoticeKind::Button => "Button",
            NoticeKind::Repeat => "Recurring",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Notice {
    pub id: u64,
    pub ts: f64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub kind: NoticeKind,
    #[serde(default)]
    pub read: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct LogEntry {
    pub ts: f64,
    pub rule: String,
    pub msg: String,
    #[serde(default)]
    pub ok: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Settings {
    /// Master switch: when off no rule runs (reminders and repeats keep working).
    pub rules_on: bool,
    /// Desktop notifications (Windows toast) and web notifications.
    pub os_notify: bool,
    /// Default lead time of new reminders in minutes.
    pub default_lead: u32,
    /// Quiet hours (minutes after midnight): no OS notification between `quiet_from` and `quiet_to`; they still land in the inbox.
    pub quiet: bool,
    pub quiet_from: u16,
    pub quiet_to: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { rules_on: true, os_notify: true, default_lead: 0, quiet: false, quiet_from: 22 * 60, quiet_to: 7 * 60 }
    }
}

impl Settings {
    /// Is `minute` (after midnight) inside the quiet hours?
    pub fn is_quiet(&self, minute: u16) -> bool {
        if !self.quiet || self.quiet_from == self.quiet_to {
            return false;
        }
        if self.quiet_from < self.quiet_to {
            minute >= self.quiet_from && minute < self.quiet_to
        } else {
            minute >= self.quiet_from || minute < self.quiet_to
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct AutoState {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
    #[serde(default)]
    pub inbox: Vec<Notice>,
    #[serde(default)]
    pub log: Vec<LogEntry>,
    #[serde(default)]
    pub settings: Settings,
}

const MAX_INBOX: usize = 120;
const MAX_LOG: usize = 150;

impl AutoState {
    pub fn unread(&self) -> usize {
        self.inbox.iter().filter(|n| !n.read).count()
    }

    pub fn mark_all_read(&mut self) {
        for n in self.inbox.iter_mut() {
            n.read = true;
        }
    }
}

// ───────────────────────────── runtime (not saved) ─────────────────────────────

struct RowSnap {
    title: String,
    props: BTreeMap<u64, Val>,
}

#[derive(Clone, Debug)]
pub enum Ev {
    Added { row: u64, db: u64 },
    Changed { row: u64, db: u64, prop: u64, old: Option<Val>, new: Option<Val> },
}

impl Ev {
    fn row(&self) -> u64 {
        match self {
            Ev::Added { row, .. } | Ev::Changed { row, .. } => *row,
        }
    }

    fn db(&self) -> u64 {
        match self {
            Ev::Added { db, .. } | Ev::Changed { db, .. } => *db,
        }
    }
}

/// Everything the engine needs between two ticks. Lives in the app, never in the workspace file.
#[derive(Default)]
pub struct Runtime {
    primed: bool,
    snap: HashMap<u64, RowSnap>,
    /// (row, prop) → (value being typed, first seen at) — text / number edits fire after the value rested for a moment.
    settling: HashMap<(u64, u64), (Val, f64)>,
    /// (rule, row) → (window start ms, runs in the window).
    guard: HashMap<(u64, u64), (f64, u32)>,
    last_sched_min: i64,
    last_scan: f64,
    /// Set when something changed that the scheduler should look at right away (rule edited, reminder added …).
    pub dirty: bool,
    last_push_hash: u64,
}

/// What a tick produced.
#[derive(Default)]
pub struct Output {
    /// New notices (already stored in the inbox) — the app shows toasts and OS notifications for them.
    pub notices: Vec<Notice>,
    /// Page data changed (the app marks the workspace dirty / recomputes formulas).
    pub changed: bool,
}

const MAX_DEPTH: u8 = 3;
const SETTLE_MS: f64 = 1200.0;
/// Scheduled occurrences older than this are skipped silently (the app was closed): enabling a rule never replays the past.
const STALE_MIN: i64 = 3 * 1440;

impl Runtime {
    pub fn new() -> Runtime {
        Runtime::default()
    }

    /// Re-reads the workspace without producing events (after the engine's own edits, a backup import …).
    pub fn resync(&mut self, ws: &Workspace) {
        self.snap.clear();
        self.settling.clear();
        self.primed = false;
        let _ = self.detect(ws, 0.0);
    }

    fn detect(&mut self, ws: &Workspace, now: f64) -> Vec<Ev> {
        let mut evs = vec![];
        let mut seen: HashSet<u64> = HashSet::new();
        let primed = self.primed;
        for p in ws.pages.iter().filter(|p| p.row) {
            let Some(db) = p.parent else { continue };
            seen.insert(p.id);
            let Some(old) = self.snap.get_mut(&p.id) else {
                if primed && !p.trashed {
                    evs.push(Ev::Added { row: p.id, db });
                }
                self.snap.insert(p.id, RowSnap { title: p.title.clone(), props: p.props.clone() });
                continue;
            };
            if old.props == p.props && old.title == p.title {
                continue;
            }
            let d = ws.db(db);
            let computed = |k: u64| d.and_then(|d| d.prop(k)).map_or(false, |pp| pp.kind.computed());
            let mut diffs: Vec<(u64, Option<Val>, Option<Val>)> = vec![];
            for (k, v) in &p.props {
                if old.props.get(k) != Some(v) && !computed(*k) {
                    diffs.push((*k, old.props.get(k).cloned(), Some(v.clone())));
                }
            }
            for (k, v) in &old.props {
                if !p.props.contains_key(k) && !computed(*k) {
                    diffs.push((*k, Some(v.clone()), None));
                }
            }
            if old.title != p.title {
                diffs.push((0, Some(Val::Text(old.title.clone())), Some(Val::Text(p.title.clone()))));
            }
            // refresh the snapshot of everything that is not an unsettled text / number edit
            let mut unsettled: HashSet<u64> = HashSet::new();
            for (k, o, n) in diffs {
                let typing = matches!(n, Some(Val::Text(_)) | Some(Val::Num(_))) && (k == 0 || d.map_or(true, |d| d.kind_of(k) != PropKind::Date));
                if typing && primed {
                    let key = (p.id, k);
                    let same = matches!(self.settling.get(&key), Some((v, _)) if Some(v) == n.as_ref());
                    if same && now - self.settling[&key].1 >= SETTLE_MS {
                        self.settling.remove(&key);
                    } else {
                        if !same {
                            self.settling.insert(key, (n.clone().unwrap_or(Val::Text(String::new())), now));
                        }
                        unsettled.insert(k);
                        continue;
                    }
                }
                if primed && !p.trashed {
                    evs.push(Ev::Changed { row: p.id, db, prop: k, old: o, new: n });
                }
            }
            if unsettled.is_empty() {
                old.props = p.props.clone();
                old.title = p.title.clone();
            } else {
                for (k, v) in &p.props {
                    if !unsettled.contains(k) {
                        old.props.insert(*k, v.clone());
                    }
                }
                old.props.retain(|k, _| p.props.contains_key(k) || unsettled.contains(k));
                if !unsettled.contains(&0) {
                    old.title = p.title.clone();
                }
            }
        }
        self.snap.retain(|id, _| seen.contains(id));
        self.settling.retain(|(r, _), _| seen.contains(r));
        self.primed = true;
        evs
    }

    /// Rate limit: a rule may run at most 12 times per minute for the same row (protects against A→B→A loops).
    fn allow(&mut self, rule: u64, row: u64, now: f64) -> bool {
        let e = self.guard.entry((rule, row)).or_insert((now, 0));
        if now - e.0 > 60_000.0 {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= 12
    }
}

// ───────────────────────────── engine entry points ─────────────────────────────

/// Drives everything: change detection → rules, recurring tasks, scheduled triggers, reminders. Cheap enough to call every ~250 ms.
/// `now_ms` is epoch milliseconds, `now_min` is [`dates::now_local_min`].
pub fn tick(ws: &mut Workspace, rt: &mut Runtime, now_ms: f64, now_min: i64) -> Output {
    let mut out = Output::default();
    let evs = rt.detect(ws, now_ms);
    let mut acted = false;
    if !evs.is_empty() {
        acted |= process(ws, rt, evs, now_ms, &mut out);
    }
    let sched_due = rt.dirty || now_min != rt.last_sched_min || now_ms - rt.last_scan > 20_000.0;
    if sched_due && rt.primed {
        rt.dirty = false;
        rt.last_sched_min = now_min;
        rt.last_scan = now_ms;
        acted |= run_schedule(ws, rt, now_ms, now_min, &mut out);
    }
    if acted {
        // the engine's own edits must not come back as new events
        rt.resync(ws);
        out.changed = true;
    }
    out
}

/// Runs the actions of a button block. The page holding the button is the "current row" when it is a database row.
pub fn press_button(ws: &mut Workspace, rt: &mut Runtime, page: u64, block: u64, now_ms: f64) -> Output {
    let mut out = Output::default();
    let Some(b) = ws.page(page).and_then(|p| p.blocks.iter().find(|b| b.id == block)).cloned() else { return out };
    let label = if b.text.is_empty() { "Button".to_owned() } else { b.text.clone() };
    let row = ws.page(page).filter(|p| p.row).map(|p| p.id);
    let db = ws.page(page).filter(|p| p.row).and_then(|p| p.parent).unwrap_or(0);
    let ctx = Ctx { row, page: Some(page), db, source: label.clone(), kind: NoticeKind::Button };
    let mut follow = vec![];
    let notes = run_actions(ws, &b.acts, &ctx, now_ms, &mut out, &mut follow);
    log(ws, &label, &notes.join(" · "), true, now_ms);
    let _ = process(ws, rt, follow, now_ms, &mut out);
    rt.resync(ws);
    out.changed = true;
    out
}

/// "Run now" for a rule: every row of its database that matches the conditions (or once without a row for scheduled rules).
pub fn run_rule_now(ws: &mut Workspace, rt: &mut Runtime, rule_id: u64, now_ms: f64) -> Output {
    let mut out = Output::default();
    let Some(rule) = ws.auto.rules.iter().find(|r| r.id == rule_id).cloned() else { return out };
    let rows: Vec<u64> = if rule.db != 0 && !matches!(rule.trigger, Trigger::Schedule { .. }) {
        ws.rows(rule.db).into_iter().filter(|r| conds_pass(ws, &rule, *r)).take(500).collect()
    } else {
        vec![]
    };
    let mut follow = vec![];
    let mut n = 0;
    if rows.is_empty() {
        let ctx = Ctx { row: None, page: None, db: rule.db, source: rule.name.clone(), kind: NoticeKind::Rule };
        let notes = run_actions(ws, &rule.actions, &ctx, now_ms, &mut out, &mut follow);
        log(ws, &rule.name, &format!("run now · {}", notes.join(" · ")), true, now_ms);
        n += 1;
    } else {
        for r in rows {
            let ctx = Ctx { row: Some(r), page: Some(r), db: rule.db, source: rule.name.clone(), kind: NoticeKind::Rule };
            run_actions(ws, &rule.actions, &ctx, now_ms, &mut out, &mut follow);
            n += 1;
        }
        log(ws, &rule.name, &format!("run now · {n} row(s)"), true, now_ms);
    }
    if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule_id) {
        r.runs += n;
        r.last_run = now_ms;
    }
    let _ = process(ws, rt, follow, now_ms, &mut out);
    rt.resync(ws);
    out.changed = true;
    out
}

// ───────────────────────────── event processing ─────────────────────────────

struct Ctx {
    row: Option<u64>,
    /// Page that receives `Append` (the row, or the page holding a button).
    page: Option<u64>,
    db: u64,
    source: String,
    kind: NoticeKind,
}

fn log(ws: &mut Workspace, rule: &str, msg: &str, ok: bool, now: f64) {
    ws.auto.log.push(LogEntry { ts: now, rule: rule.into(), msg: msg.into(), ok });
    let n = ws.auto.log.len();
    if n > MAX_LOG {
        ws.auto.log.drain(0..n - MAX_LOG);
    }
}

fn push_notice(ws: &mut Workspace, out: &mut Output, title: &str, body: &str, page: Option<u64>, kind: NoticeKind, now: f64) {
    let id = ws.id();
    let n = Notice { id, ts: now, title: title.into(), body: body.into(), page, kind, read: false };
    ws.auto.inbox.push(n.clone());
    let len = ws.auto.inbox.len();
    if len > MAX_INBOX {
        ws.auto.inbox.drain(0..len - MAX_INBOX);
    }
    out.notices.push(n);
}

fn conds_pass(ws: &Workspace, rule: &Rule, row: u64) -> bool {
    let (Some(p), Some(d)) = (ws.page(row), ws.db(rule.db)) else { return rule.conds.is_empty() };
    rule.conds.iter().all(|f| row_matches(p, d, f))
}

fn title_or_untitled(ws: &Workspace, row: u64) -> String {
    match ws.page(row) {
        Some(p) if !p.title.is_empty() => p.title.clone(),
        _ => "Untitled".into(),
    }
}

/// Handles events, cascades included. Returns true when anything ran.
fn process(ws: &mut Workspace, rt: &mut Runtime, first: Vec<Ev>, now: f64, out: &mut Output) -> bool {
    let mut queue: VecDeque<(Ev, u8)> = first.into_iter().map(|e| (e, 0)).collect();
    let mut acted = false;
    let mut budget = 400; // hard stop for runaway cascades
    while let Some((ev, depth)) = queue.pop_front() {
        budget -= 1;
        if budget == 0 {
            log(ws, "engine", "stopped: too many chained automations", false, now);
            break;
        }
        let mut follow: Vec<Ev> = vec![];
        // 1) recurring tasks
        if let Ev::Changed { row, prop, .. } = &ev {
            acted |= handle_repeats(ws, *row, *prop, now, out, &mut follow);
        }
        // 2) rules
        if ws.auto.settings.rules_on {
            let rules: Vec<Rule> = ws.auto.rules.iter().filter(|r| r.on && r.db == ev.db()).cloned().collect();
            for rule in rules {
                let hit = match (&rule.trigger, &ev) {
                    (Trigger::RowAdded, Ev::Added { .. }) => true,
                    (Trigger::PropChanged { prop, to }, Ev::Changed { prop: p, old, new, .. }) => prop == p && to.matches(old, new),
                    _ => false,
                };
                if !hit || !conds_pass(ws, &rule, ev.row()) || !rt.allow(rule.id, ev.row(), now) {
                    continue;
                }
                let ctx = Ctx { row: Some(ev.row()), page: Some(ev.row()), db: ev.db(), source: rule.name.clone(), kind: NoticeKind::Rule };
                let notes = run_actions(ws, &rule.actions, &ctx, now, out, &mut follow);
                let t = title_or_untitled(ws, ev.row());
                log(ws, &rule.name, &format!("“{t}” · {}", notes.join(" · ")), true, now);
                if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule.id) {
                    r.runs += 1;
                    r.last_run = now;
                }
                acted = true;
            }
        }
        if depth < MAX_DEPTH {
            for e in follow {
                queue.push_back((e, depth + 1));
            }
        }
    }
    acted
}

// ───────────────────────────── recurring tasks ─────────────────────────────

pub fn doneish(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "done" | "complete" | "completed" | "finished" | "shipped" | "closed" | "resolved" | "won" | "published" | "paid" | "archived" | "cancelled" | "canceled"
    )
}

/// Is this row completed (according to `rep`'s status settings, or automatically)?
pub fn row_done(ws: &Workspace, row: u64, rep: Option<&Repeat>) -> bool {
    let Some(p) = ws.page(row) else { return false };
    let Some(d) = p.parent.and_then(|db| ws.db(db)) else { return false };
    if let Some(r) = rep.filter(|r| r.status_prop != 0) {
        return match p.props.get(&r.status_prop) {
            Some(Val::Sel(o)) => {
                if r.done_opt != 0 {
                    *o == r.done_opt
                } else {
                    d.opt(r.status_prop, *o).map_or(false, |o| doneish(&o.name))
                }
            }
            Some(Val::Check(b)) => *b,
            _ => false,
        };
    }
    d.props.iter().any(|pr| match (pr.kind, p.props.get(&pr.id)) {
        (PropKind::Checkbox, Some(Val::Check(true))) => true,
        (PropKind::Select, Some(Val::Sel(o))) => pr.opts.iter().find(|x| x.id == *o).map_or(false, |x| doneish(&x.name)),
        _ => false,
    })
}

fn reset_status(ws: &mut Workspace, row: u64, rep: &Repeat, follow: &mut Vec<Ev>) {
    let Some(db) = ws.page(row).and_then(|p| p.parent) else { return };
    let Some(d) = ws.db(db).cloned() else { return };
    let mut sets: Vec<(u64, Option<Val>)> = vec![];
    for pr in d.props.iter().filter(|p| rep.status_prop == 0 || p.id == rep.status_prop) {
        match pr.kind {
            PropKind::Checkbox => sets.push((pr.id, Some(Val::Check(false)))),
            PropKind::Select => {
                let cur = ws.page(row).and_then(|p| p.props.get(&pr.id)).cloned();
                let is_done = match &cur {
                    Some(Val::Sel(o)) => {
                        if rep.status_prop != 0 && rep.done_opt != 0 {
                            *o == rep.done_opt
                        } else {
                            pr.opts.iter().find(|x| x.id == *o).map_or(false, |x| doneish(&x.name))
                        }
                    }
                    _ => false,
                };
                if is_done {
                    let target = if rep.reset_opt != 0 { Some(rep.reset_opt) } else { pr.opts.iter().find(|o| !doneish(&o.name)).map(|o| o.id) };
                    sets.push((pr.id, target.map(Val::Sel)));
                }
            }
            _ => {}
        }
    }
    for (k, v) in sets {
        set_val(ws, row, db, k, v, follow);
    }
}

fn handle_repeats(ws: &mut Workspace, row: u64, prop: u64, now: f64, out: &mut Output, follow: &mut Vec<Ev>) -> bool {
    let Some(p) = ws.page(row) else { return false };
    if p.repeats.is_empty() {
        return false;
    }
    let Some(db) = p.parent else { return false };
    let kind = ws.db(db).map_or(PropKind::Text, |d| d.kind_of(prop));
    let list: Vec<(u64, Repeat)> = p.repeats.iter().filter(|(_, r)| r.on_complete).map(|(k, r)| (*k, r.clone())).collect();
    let mut acted = false;
    for (date_prop, rep) in list {
        let relevant = if rep.status_prop != 0 { prop == rep.status_prop } else { matches!(kind, PropKind::Select | PropKind::Checkbox) };
        if relevant && row_done(ws, row, Some(&rep)) {
            acted |= advance_row(ws, row, db, date_prop, rep, now, out, follow);
        }
    }
    acted
}

/// Rolls the date `date_prop` of `row` to its next occurrence (or spawns the next copy) and resets the status.
#[allow(clippy::too_many_arguments)]
fn advance_row(ws: &mut Workspace, row: u64, db: u64, date_prop: u64, mut rep: Repeat, now: f64, out: &mut Output, follow: &mut Vec<Ev>) -> bool {
    let Some(Val::Text(cur)) = ws.page(row).and_then(|p| p.props.get(&date_prop)).cloned() else { return false };
    let title = title_or_untitled(ws, row);
    let today = dates::today_day();
    let Some(next) = rep.advance(&cur, today) else {
        if let Some(p) = ws.page_mut(row) {
            p.repeats.remove(&date_prop);
        }
        log(ws, "Recurring", &format!("“{title}” — series finished"), true, now);
        push_notice(ws, out, "Recurring task finished", &format!("“{title}” completed its last occurrence."), Some(row), NoticeKind::Repeat, now);
        return true;
    };
    rep.done += 1;
    if rep.spawn {
        if let Some(mut copy) = ws.page(row).cloned() {
            let nid = ws.id();
            copy.id = nid;
            copy.favorite = false;
            copy.history.clear();
            copy.updated = now;
            copy.props.insert(date_prop, Val::Text(next.clone()));
            copy.repeats.insert(date_prop, rep.clone());
            for b in copy.blocks.iter_mut() {
                b.id = ws.id();
            }
            ws.pages.push(copy);
            reset_status(ws, nid, &rep, &mut vec![]);
            follow.push(Ev::Added { row: nid, db });
        }
        if let Some(p) = ws.page_mut(row) {
            p.repeats.remove(&date_prop);
        }
    } else {
        set_val(ws, row, db, date_prop, Some(Val::Text(next.clone())), follow);
        if let Some(p) = ws.page_mut(row) {
            p.repeats.insert(date_prop, rep.clone());
        }
        reset_status(ws, row, &rep, follow);
    }
    log(ws, "Recurring", &format!("“{title}” → {}", dates::pretty(&next)), true, now);
    true
}

// ───────────────────────────── scheduler: date triggers, schedules, reminders ─────────────────────────────

fn row_date_start(p: &Page, prop: u64) -> Option<i64> {
    match p.props.get(&prop)? {
        Val::Text(s) => dates::span(s).map(|x| x.0),
        _ => None,
    }
}

fn run_schedule(ws: &mut Workspace, rt: &mut Runtime, now: f64, now_min: i64, out: &mut Output) -> bool {
    let mut acted = false;
    let mut follow: Vec<Ev> = vec![];
    let today = now_min.div_euclid(1440);

    // ── auto-rolling repeats (habits): the date passed → next occurrence
    let rolls: Vec<(u64, u64, u64, Repeat)> = ws
        .pages
        .iter()
        .filter(|p| p.row && !p.trashed && !p.repeats.is_empty())
        .flat_map(|p| p.repeats.iter().filter(|(_, r)| r.roll).map(move |(k, r)| (p.id, p.parent.unwrap_or(0), *k, r.clone())))
        .collect();
    for (row, db, prop, rep) in rolls {
        let past = ws.page(row).and_then(|p| match p.props.get(&prop) {
            Some(Val::Text(s)) => dates::span(s).map(|x| x.1 < today),
            _ => None,
        });
        if past == Some(true) && !(rep.on_complete && row_done(ws, row, Some(&rep))) {
            acted |= advance_row(ws, row, db, prop, rep, now, out, &mut follow);
        }
    }

    // ── rules with time triggers
    if ws.auto.settings.rules_on {
        let rules: Vec<Rule> = ws.auto.rules.iter().filter(|r| r.on).cloned().collect();
        for rule in rules {
            match &rule.trigger {
                Trigger::DateReached { prop, offset_days, minute } => {
                    let rows: Vec<(u64, i64)> = ws
                        .pages
                        .iter()
                        .filter(|p| p.row && !p.trashed && p.parent == Some(rule.db))
                        .filter_map(|p| row_date_start(p, *prop).map(|d| (p.id, (d + *offset_days as i64) * 1440 + *minute as i64)))
                        .collect();
                    let mut fired = rule.fired.clone();
                    fired.retain(|r, _| rows.iter().any(|(id, _)| id == r));
                    let mut changed_fired = fired.len() != rule.fired.len();
                    for (row, at) in rows {
                        if now_min < at || fired.get(&row) == Some(&at) {
                            continue;
                        }
                        fired.insert(row, at);
                        changed_fired = true;
                        if now_min - at > STALE_MIN {
                            continue;
                        }
                        if (rule.skip_done && row_done(ws, row, None)) || !conds_pass(ws, &rule, row) || !rt.allow(rule.id, row, now) {
                            continue;
                        }
                        let ctx = Ctx { row: Some(row), page: Some(row), db: rule.db, source: rule.name.clone(), kind: NoticeKind::Rule };
                        let notes = run_actions(ws, &rule.actions, &ctx, now, out, &mut follow);
                        let t = title_or_untitled(ws, row);
                        log(ws, &rule.name, &format!("“{t}” · {}", notes.join(" · ")), true, now);
                        if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule.id) {
                            r.runs += 1;
                            r.last_run = now;
                        }
                        acted = true;
                    }
                    if changed_fired {
                        if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule.id) {
                            r.fired = fired;
                        }
                    }
                }
                Trigger::Schedule { repeat, start_day, minute } => {
                    let mut next = rule.next_min;
                    if next == 0 {
                        // first occurrence at or after now (never in the past)
                        let mut d = *start_day;
                        let mut guard = 0;
                        while d * 1440 + (*minute as i64) < now_min && guard < 5000 {
                            d = repeat.next_after(d);
                            guard += 1;
                        }
                        next = d * 1440 + *minute as i64;
                        if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule.id) {
                            r.next_min = next;
                        }
                    }
                    if now_min >= next {
                        let fresh = now_min - next <= STALE_MIN;
                        let mut d = next.div_euclid(1440);
                        let mut guard = 0;
                        let mut nn = next;
                        let mut ended = false;
                        while nn <= now_min && guard < 5000 {
                            let nd = repeat.next_after(d);
                            if let End::On(last) = repeat.end {
                                if nd > last {
                                    ended = true;
                                }
                            }
                            d = nd;
                            nn = d * 1440 + *minute as i64;
                            guard += 1;
                        }
                        if fresh && rt.allow(rule.id, 0, now) {
                            let ctx = Ctx { row: None, page: None, db: rule.db, source: rule.name.clone(), kind: NoticeKind::Rule };
                            let notes = run_actions(ws, &rule.actions, &ctx, now, out, &mut follow);
                            log(ws, &rule.name, &format!("scheduled · {}", notes.join(" · ")), true, now);
                            acted = true;
                        }
                        if let Some(r) = ws.auto.rules.iter_mut().find(|r| r.id == rule.id) {
                            if fresh {
                                r.runs += 1;
                                r.last_run = now;
                            }
                            r.next_min = nn;
                            if ended {
                                r.on = false;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── reminders
    let mut remove: Vec<u64> = vec![];
    let reminders = ws.auto.reminders.clone();
    for rem in reminders {
        if rem.done {
            continue;
        }
        if rem.prop != 0 && ws.page(rem.page).map_or(true, |p| p.trashed) {
            remove.push(rem.id);
            continue;
        }
        let Some(due) = rem.due_min(ws) else { continue };
        let trig = due - rem.lead_min as i64;
        if now_min < trig || rem.fired_for == due {
            continue;
        }
        let mut upd = rem.clone();
        upd.fired_for = due;
        let skip = now_min - trig > 7 * 1440 || (rem.prop != 0 && row_done(ws, rem.page, None));
        if !skip {
            let page = (rem.page != 0).then_some(rem.page);
            let title = page.map(|p| ws.display_title(p)).unwrap_or_else(|| "Reminder".into());
            let day = dates::pretty(&dates::iso_of_day(due.div_euclid(1440)));
            let when = if rem.lead_min > 0 { format!("Due {day} at {}", dates::hhmm(rem.minute)) } else { format!("{day} · {}", dates::hhmm(rem.minute)) };
            let (t, b) = if rem.message.is_empty() { (title, when) } else { (rem.message.clone(), format!("{title} · {when}")) };
            push_notice(ws, out, &t, &b, page, NoticeKind::Reminder, now);
            acted = true;
        }
        if rem.prop == 0 {
            if let Some(rp) = &rem.repeat {
                match rp.advance(&dates::iso_of_day(rem.day), today + 1) {
                    Some(n) => {
                        upd.day = dates::parse(&n).map(dates::day_of).unwrap_or(rem.day + 1);
                        let mut r2 = rp.clone();
                        r2.done += 1;
                        upd.repeat = Some(r2);
                    }
                    None => upd.done = true,
                }
            } else {
                upd.done = true;
            }
        }
        if let Some(r) = ws.auto.reminders.iter_mut().find(|r| r.id == rem.id) {
            *r = upd;
        }
    }
    if !remove.is_empty() {
        ws.auto.reminders.retain(|r| !remove.contains(&r.id));
    }

    if !follow.is_empty() {
        acted |= process(ws, rt, follow, now, out);
    }
    acted
}

/// Reminders firing in the next `hours`, as JSON for the browser's own timers (`platform::schedule_push`). `None` when unchanged since last call.
pub fn push_schedule(ws: &Workspace, rt: &mut Runtime, now_min: i64, hours: i64) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let tz = dates::tz_offset_min();
    let mut items = vec![];
    for r in ws.auto.reminders.iter().filter(|r| !r.done) {
        let Some(due) = r.due_min(ws) else { continue };
        let trig = due - r.lead_min as i64;
        if r.fired_for == due || trig < now_min - 1 || trig > now_min + hours * 60 {
            continue;
        }
        let page = (r.page != 0).then_some(r.page);
        let title = page.map(|p| ws.display_title(p)).unwrap_or_else(|| "Reminder".into());
        let (t, b) = if r.message.is_empty() { (title, dates::hhmm(r.minute)) } else { (r.message.clone(), title) };
        items.push(serde_json::json!({"at": ((trig - tz) as f64) * 60_000.0, "title": t, "body": b, "tag": format!("lumen-rem-{}-{}", r.id, due)}));
    }
    let json = serde_json::Value::Array(items).to_string();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    let hv = h.finish();
    if hv == rt.last_push_hash {
        return None;
    }
    rt.last_push_hash = hv;
    Some(json)
}

// ───────────────────────────── executing actions ─────────────────────────────

/// Replaces `{title}`, `{today}`, `{now}`, `{db}`, `{date}` and `{prop:Name}` using the triggering row.
pub fn render(ws: &Workspace, row: Option<u64>, tpl: &str) -> String {
    if !tpl.contains('{') {
        return tpl.to_owned();
    }
    let now_clock = dates::hhmm(dates::now_local_min().rem_euclid(1440) as u16);
    let mut s = tpl.replace("{today}", &dates::pretty(&dates::today_iso())).replace("{now}", &format!("{} {now_clock}", dates::pretty(&dates::today_iso())));
    if let Some(p) = row.and_then(|r| ws.page(r)) {
        s = s.replace("{title}", if p.title.is_empty() { "Untitled" } else { &p.title });
        if let Some(db) = p.parent.and_then(|d| ws.db(d)) {
            if s.contains("{db}") {
                s = s.replace("{db}", &p.parent.map(|d| ws.display_title(d)).unwrap_or_default());
            }
            if let Some(dp) = db.first_of(PropKind::Date) {
                if let Some(Val::Text(v)) = p.props.get(&dp) {
                    s = s.replace("{date}", &dates::pretty(v));
                }
            }
            while let Some(a) = s.find("{prop:") {
                let Some(len) = s[a..].find('}') else { break };
                let name = s[a + 6..a + len].to_owned();
                let val = db.props.iter().find(|pr| pr.name.eq_ignore_ascii_case(&name)).and_then(|pr| p.props.get(&pr.id).map(|v| db.val_text(pr.id, v))).unwrap_or_default();
                s.replace_range(a..a + len + 1, &val);
            }
        }
    }
    s.replace("{date}", "").replace("{title}", "")
}

/// Sets (or clears) a property and queues a `Changed` event when the value really changed.
pub fn set_val(ws: &mut Workspace, row: u64, db: u64, prop: u64, v: Option<Val>, follow: &mut Vec<Ev>) -> bool {
    let Some(p) = ws.page_mut(row) else { return false };
    let old = if prop == 0 { Some(Val::Text(p.title.clone())) } else { p.props.get(&prop).cloned() };
    if old == v {
        return false;
    }
    if prop == 0 {
        if let Some(Val::Text(t)) = &v {
            p.title = t.clone();
        }
    } else {
        match &v {
            Some(x) => {
                p.props.insert(prop, x.clone());
            }
            None => {
                p.props.remove(&prop);
            }
        }
    }
    p.updated = crate::auth::now_ms();
    follow.push(Ev::Changed { row, db, prop, old, new: v });
    true
}

fn setter_val(ws: &Workspace, row: Option<u64>, prop: u64, s: &Setter) -> Option<Option<Val>> {
    let cur = row.and_then(|r| ws.page(r)).and_then(|p| p.props.get(&prop)).cloned();
    Some(match s {
        Setter::Text(t) => Some(Val::Text(render(ws, row, t))),
        Setter::Num(n) => Some(Val::Num(*n)),
        Setter::Sel(o) => Some(Val::Sel(*o)),
        Setter::AddOpt(o) => {
            let mut m = match cur {
                Some(Val::Multi(m)) => m,
                _ => vec![],
            };
            if !m.contains(o) {
                m.push(*o);
            }
            Some(Val::Multi(m))
        }
        Setter::Check(b) => Some(Val::Check(*b)),
        Setter::Today => Some(Val::Text(dates::today_iso())),
        Setter::InDays(n) => Some(Val::Text(dates::iso_of_day(dates::today_day() + *n as i64))),
        Setter::ShiftDays(n) => match cur {
            Some(Val::Text(t)) => {
                let (a, b) = dates::span(&t)?;
                Some(Val::Text(dates::span_value(a + *n as i64, b + *n as i64)))
            }
            _ => Some(Val::Text(dates::iso_of_day(dates::today_day() + *n as i64))),
        },
        Setter::Clear => None,
    })
}

/// Short, human description of a setter ("Done", "today", "+3 days" …).
pub fn setter_text(ws: &Workspace, db: u64, prop: u64, s: &Setter) -> String {
    match s {
        Setter::Text(t) => format!("“{t}”"),
        Setter::Num(n) => format!("{n}"),
        Setter::Sel(o) | Setter::AddOpt(o) => ws.db(db).and_then(|d| d.opt(prop, *o)).map(|o| o.name.clone()).unwrap_or_else(|| "?".into()),
        Setter::Check(b) => (if *b { "checked" } else { "unchecked" }).into(),
        Setter::Today => "today".into(),
        Setter::InDays(n) => format!("today {} {} day(s)", if *n < 0 { "−" } else { "+" }, n.abs()),
        Setter::ShiftDays(n) => format!("shifted {n} day(s)"),
        Setter::Clear => "empty".into(),
    }
}

fn run_actions(ws: &mut Workspace, acts: &[Action], ctx: &Ctx, now: f64, out: &mut Output, follow: &mut Vec<Ev>) -> Vec<String> {
    let mut notes = vec![];
    // the triggering row keeps its identity even after "Move row" (its parent changes)
    let mut db = ctx.db;
    for a in acts {
        match a {
            Action::Set { prop, value } => {
                let Some(row) = ctx.row else {
                    notes.push("Set property skipped (no row)".into());
                    continue;
                };
                if let Some(v) = setter_val(ws, Some(row), *prop, value) {
                    set_val(ws, row, db, *prop, v, follow);
                    let name = ws.db(db).and_then(|d| d.prop(*prop)).map(|p| p.name.clone()).unwrap_or_else(|| "property".into());
                    notes.push(format!("{name} → {}", setter_text(ws, db, *prop, value)));
                }
            }
            Action::Rename { title } => {
                if let Some(row) = ctx.row {
                    let t = render(ws, Some(row), title);
                    set_val(ws, row, db, 0, Some(Val::Text(t.clone())), follow);
                    notes.push(format!("renamed to “{t}”"));
                }
            }
            Action::CreateRow { db: target, title, sets } => {
                if ws.db(*target).is_none() {
                    notes.push("Create row skipped (database is gone)".into());
                    continue;
                }
                let t = render(ws, ctx.row, title);
                let id = ws.add_row(*target, None);
                let vals: Vec<(u64, Option<Val>)> = sets.iter().filter_map(|(p, s)| setter_val(ws, Some(id), *p, s).map(|v| (*p, v))).collect();
                if let Some(p) = ws.page_mut(id) {
                    p.title = t.clone();
                    for (k, v) in vals {
                        if let Some(v) = v {
                            p.props.insert(k, v);
                        }
                    }
                }
                follow.push(Ev::Added { row: id, db: *target });
                notes.push(format!("created “{}”", if t.is_empty() { "Untitled" } else { &t }));
            }
            Action::MoveRow { db: target } => {
                if let Some(row) = ctx.row {
                    if move_row(ws, row, *target) {
                        let name = ws.display_title(*target);
                        notes.push(format!("moved to {name}"));
                        db = *target;
                        follow.push(Ev::Added { row, db: *target });
                    } else {
                        notes.push("Move skipped".into());
                    }
                }
            }
            Action::Append { text, todo } => {
                if let Some(pg) = ctx.page {
                    let t = render(ws, ctx.row, text);
                    let kind = if *todo { BlockKind::Todo } else { BlockKind::Text };
                    let b: Block = ws.blk(kind, &t, 0);
                    if let Some(p) = ws.page_mut(pg) {
                        // an untouched empty trailing block is reused
                        if let Some(last) = p.blocks.last_mut().filter(|l| l.kind == BlockKind::Text && l.text.is_empty()) {
                            last.kind = kind;
                            last.text = t;
                        } else {
                            p.blocks.push(b);
                        }
                        p.updated = now;
                    }
                    notes.push("added to page".into());
                }
            }
            Action::Notify { message } => {
                let m = render(ws, ctx.row, message);
                let page = ctx.row.or(ctx.page);
                push_notice(ws, out, &ctx.source, &m, page, ctx.kind, now);
                notes.push(format!("notified “{m}”"));
            }
            Action::Duplicate => {
                if let Some(row) = ctx.row {
                    if let Some(mut copy) = ws.page(row).cloned() {
                        let nid = ws.id();
                        copy.id = nid;
                        copy.title = if copy.title.is_empty() { "Untitled copy".into() } else { format!("{} (copy)", copy.title) };
                        copy.favorite = false;
                        copy.history.clear();
                        copy.updated = now;
                        copy.repeats.clear();
                        for b in copy.blocks.iter_mut() {
                            b.id = ws.id();
                        }
                        let pdb = copy.parent.unwrap_or(0);
                        ws.pages.push(copy);
                        follow.push(Ev::Added { row: nid, db: pdb });
                        notes.push("duplicated".into());
                    }
                }
            }
            Action::Trash => {
                if let Some(row) = ctx.row {
                    ws.set_trashed(row, true);
                    notes.push("moved to trash".into());
                }
            }
        }
    }
    if notes.is_empty() {
        notes.push("nothing to do".into());
    }
    out.changed = true;
    notes
}

/// Moves a row into another database. Properties are matched by name and kind; select options by name (missing options are created).
pub fn move_row(ws: &mut Workspace, row: u64, target: u64) -> bool {
    let Some(src_db) = ws.page(row).filter(|p| p.row).and_then(|p| p.parent) else { return false };
    if src_db == target {
        return false;
    }
    let (Some(sd), Some(td)) = (ws.db(src_db).cloned(), ws.db(target).cloned()) else { return false };
    let old = ws.page(row).map(|p| p.props.clone()).unwrap_or_default();
    let mut new_props: BTreeMap<u64, Val> = BTreeMap::new();
    let mut created: Vec<(u64, u64, String)> = vec![]; // (target prop, new option id, name)
    for sp in &sd.props {
        let Some(tp) = td.props.iter().find(|t| t.kind == sp.kind && t.name.eq_ignore_ascii_case(&sp.name)) else { continue };
        let Some(v) = old.get(&sp.id) else { continue };
        let mut map_opt = |o: u64, ws: &mut Workspace| -> Option<u64> {
            let name = sp.opts.iter().find(|x| x.id == o)?.name.clone();
            if let Some(t) = tp.opts.iter().find(|x| x.name.eq_ignore_ascii_case(&name)) {
                return Some(t.id);
            }
            if let Some((_, id, _)) = created.iter().find(|(p, _, n)| *p == tp.id && *n == name) {
                return Some(*id);
            }
            let id = ws.id();
            created.push((tp.id, id, name.clone()));
            if let Some(p) = ws.db_mut(target).and_then(|d| d.props.iter_mut().find(|p| p.id == tp.id)) {
                let color = p.opts.len() % 9;
                p.opts.push(Opt { id, name, color });
            }
            Some(id)
        };
        let nv = match v {
            Val::Sel(o) => map_opt(*o, ws).map(Val::Sel),
            Val::Multi(m) => Some(Val::Multi(m.iter().filter_map(|o| map_opt(*o, ws)).collect())),
            other if !sp.kind.computed() && sp.kind != PropKind::Relation => Some(other.clone()),
            _ => None,
        };
        if let Some(nv) = nv {
            new_props.insert(tp.id, nv);
        }
    }
    if let Some(p) = ws.page_mut(row) {
        p.parent = Some(target);
        p.props = new_props;
        p.repeats.clear();
        p.updated = crate::auth::now_ms();
    }
    true
}

// ───────────────────────────── recipes ─────────────────────────────

pub struct Recipe {
    pub title: &'static str,
    pub desc: &'static str,
    pub icon: &'static str,
    /// Needs a database (otherwise it is a workspace-level schedule).
    pub needs_db: bool,
    build: fn(&mut Workspace, u64) -> Option<Rule>,
}

impl Recipe {
    pub fn create(&self, ws: &mut Workspace, db: u64) -> Option<u64> {
        let rule = (self.build)(ws, db)?;
        let id = rule.id;
        ws.auto.rules.push(rule);
        Some(id)
    }
}

fn status_of(ws: &Workspace, db: u64) -> Option<(u64, Vec<Opt>)> {
    let d = ws.db(db)?;
    d.props
        .iter()
        .filter(|p| p.kind == PropKind::Select)
        .find(|p| p.opts.iter().any(|o| doneish(&o.name)))
        .or_else(|| d.props.iter().find(|p| p.kind == PropKind::Select))
        .map(|p| (p.id, p.opts.clone()))
}

fn done_opt(opts: &[Opt]) -> Option<u64> {
    opts.iter().find(|o| doneish(&o.name)).map(|o| o.id)
}

fn first_open_opt(opts: &[Opt]) -> Option<u64> {
    opts.iter().find(|o| !doneish(&o.name)).map(|o| o.id)
}

fn ensure_prop(ws: &mut Workspace, db: u64, name: &str, kind: PropKind) -> u64 {
    if let Some(p) = ws.db(db).and_then(|d| d.props.iter().find(|p| p.kind == kind && p.name.eq_ignore_ascii_case(name))) {
        return p.id;
    }
    ws.add_prop(db, name, kind, &[])
}

pub fn recipes() -> Vec<Recipe> {
    use crate::lucide as l;
    vec![
        Recipe {
            title: "Stamp the completion date",
            desc: "When Status becomes Done, fill a “Completed” date with today.",
            icon: l::CALENDAR_DAYS,
            needs_db: true,
            build: |ws, db| {
                let (sp, opts) = status_of(ws, db)?;
                let done = done_opt(&opts)?;
                let dp = ensure_prop(ws, db, "Completed", PropKind::Date);
                let id = ws.id();
                Some(Rule::new(id, "Stamp completion date", db, Trigger::PropChanged { prop: sp, to: ChangeTo::Sel(done) }, vec![Action::Set { prop: dp, value: Setter::Today }]))
            },
        },
        Recipe {
            title: "Stamp the start date",
            desc: "When Status moves to “In progress”, fill a “Started” date.",
            icon: l::PLAY,
            needs_db: true,
            build: |ws, db| {
                let (sp, opts) = status_of(ws, db)?;
                let prog = opts
                    .iter()
                    .find(|o| {
                        let n = o.name.to_lowercase();
                        n.contains("progress") || n.contains("doing")
                    })
                    .map(|o| o.id)
                    .or_else(|| opts.get(1).map(|o| o.id))?;
                let dp = ensure_prop(ws, db, "Started", PropKind::Date);
                let id = ws.id();
                Some(Rule::new(id, "Stamp start date", db, Trigger::PropChanged { prop: sp, to: ChangeTo::Sel(prog) }, vec![Action::Set { prop: dp, value: Setter::Today }]))
            },
        },
        Recipe {
            title: "Create a follow-up task",
            desc: "When a row is Done, create “Follow up: …” due in 7 days.",
            icon: l::COPY_PLUS,
            needs_db: true,
            build: |ws, db| {
                let (sp, opts) = status_of(ws, db)?;
                let done = done_opt(&opts)?;
                let open = first_open_opt(&opts)?;
                let dp = ws.db(db)?.first_of(PropKind::Date);
                let mut sets = vec![(sp, Setter::Sel(open))];
                if let Some(dp) = dp {
                    sets.push((dp, Setter::InDays(7)));
                }
                let id = ws.id();
                Some(Rule::new(id, "Follow-up task", db, Trigger::PropChanged { prop: sp, to: ChangeTo::Sel(done) }, vec![Action::CreateRow { db, title: "Follow up: {title}".into(), sets }]))
            },
        },
        Recipe {
            title: "Notify when done",
            desc: "Raise a notification whenever a row is marked Done.",
            icon: l::BELL,
            needs_db: true,
            build: |ws, db| {
                let (sp, opts) = status_of(ws, db)?;
                let done = done_opt(&opts)?;
                let id = ws.id();
                Some(Rule::new(id, "Notify when done", db, Trigger::PropChanged { prop: sp, to: ChangeTo::Sel(done) }, vec![Action::Notify { message: "“{title}” is done 🎉".into() }]))
            },
        },
        Recipe {
            title: "Defaults for new rows",
            desc: "Give every new row the first status option.",
            icon: l::CIRCLE_PLUS,
            needs_db: true,
            build: |ws, db| {
                let (sp, opts) = status_of(ws, db)?;
                let open = first_open_opt(&opts)?;
                let id = ws.id();
                Some(Rule::new(id, "Defaults for new rows", db, Trigger::RowAdded, vec![Action::Set { prop: sp, value: Setter::Sel(open) }]))
            },
        },
        Recipe {
            title: "Due tomorrow reminder",
            desc: "At 09:00 the day before the date, notify (skips finished rows).",
            icon: l::ALARM_CLOCK,
            needs_db: true,
            build: |ws, db| {
                let dp = ws.db(db)?.first_of(PropKind::Date)?;
                let id = ws.id();
                Some(Rule::new(id, "Due tomorrow", db, Trigger::DateReached { prop: dp, offset_days: -1, minute: 9 * 60 }, vec![Action::Notify { message: "“{title}” is due tomorrow ({date})".into() }]))
            },
        },
        Recipe {
            title: "Overdue alert",
            desc: "The morning after the date passes, notify about unfinished rows.",
            icon: l::TRIANGLE_ALERT,
            needs_db: true,
            build: |ws, db| {
                let dp = ws.db(db)?.first_of(PropKind::Date)?;
                let id = ws.id();
                Some(Rule::new(id, "Overdue alert", db, Trigger::DateReached { prop: dp, offset_days: 1, minute: 9 * 60 }, vec![Action::Notify { message: "“{title}” is overdue (was {date})".into() }]))
            },
        },
        Recipe {
            title: "Daily stand-up task",
            desc: "Every weekday at 09:00 create a “Stand-up” row.",
            icon: l::CALENDAR_PLUS,
            needs_db: true,
            build: |ws, db| {
                let dp = ws.db(db)?.first_of(PropKind::Date);
                let mut sets = vec![];
                if let Some((sp, opts)) = status_of(ws, db) {
                    if let Some(o) = first_open_opt(&opts) {
                        sets.push((sp, Setter::Sel(o)));
                    }
                }
                if let Some(dp) = dp {
                    sets.push((dp, Setter::Today));
                }
                let id = ws.id();
                Some(Rule::new(id, "Daily stand-up", db, Trigger::Schedule { repeat: Repeat::weekly_on(&[0, 1, 2, 3, 4]), start_day: dates::today_day(), minute: 9 * 60 }, vec![Action::CreateRow { db, title: "Stand-up · {today}".into(), sets }]))
            },
        },
        Recipe {
            title: "Weekly review",
            desc: "Every Friday at 16:00 create a review row and notify.",
            icon: l::LIST_CHECKS,
            needs_db: true,
            build: |ws, db| {
                let dp = ws.db(db)?.first_of(PropKind::Date);
                let mut sets = vec![];
                if let Some(dp) = dp {
                    sets.push((dp, Setter::Today));
                }
                let id = ws.id();
                Some(Rule::new(
                    id,
                    "Weekly review",
                    db,
                    Trigger::Schedule { repeat: Repeat::weekly_on(&[4]), start_day: dates::today_day(), minute: 16 * 60 },
                    vec![Action::CreateRow { db, title: "Weekly review · {today}".into(), sets }, Action::Notify { message: "Time for your weekly review".into() }],
                ))
            },
        },
        Recipe {
            title: "Morning briefing",
            desc: "Every day at 08:30 raise a notification (no database needed).",
            icon: l::SPARKLES,
            needs_db: false,
            build: |ws, _| {
                let id = ws.id();
                Some(Rule::new(id, "Morning briefing", 0, Trigger::Schedule { repeat: Repeat::daily(), start_day: dates::today_day(), minute: 8 * 60 + 30 }, vec![Action::Notify { message: "Good morning — it is {today}. Open Lumen to plan your day.".into() }]))
            },
        },
    ]
}

/// Drops conditions that point at properties which no longer exist.
pub fn drop_dead_conds_in(ws: &Workspace, rule: &mut Rule) {
    if let Some(d) = ws.db(rule.db) {
        rule.conds.retain(|f| f.prop == 0 || d.prop(f.prop).is_some());
    }
}

/// A default condition for the "add condition" button.
pub fn default_cond(ws: &Workspace, db: u64) -> Option<Filter> {
    let d = ws.db(db)?;
    let p = d.props.iter().find(|p| !p.kind.computed() && p.kind != PropKind::Relation)?;
    let op = *FilterOp::for_kind(p.kind).first()?;
    Some(Filter { prop: p.id, op, value: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageKind;

    fn setup() -> (Workspace, u64, u64, u64, u64) {
        let mut ws = Workspace::seed();
        let db = ws.add_page(None, PageKind::Database);
        let d = ws.db(db).unwrap();
        let status = d.first_of(PropKind::Select).unwrap();
        let date = d.first_of(PropKind::Date).unwrap();
        let done = ws.opt_id(db, status, "Done").unwrap();
        (ws, db, status, date, done)
    }

    #[test]
    fn weekly_repeat_respects_weekdays_and_interval() {
        let mon = dates::days_from_civil(2026, 1, 5);
        assert_eq!(weekday_of(mon), 0);
        let r = Repeat::weekly_on(&[0, 2]);
        assert_eq!(r.next_after(mon), mon + 2); // Mon → Wed
        assert_eq!(r.next_after(mon + 2), mon + 7); // Wed → next Mon
        let mut every2 = Repeat::weekly_on(&[0]);
        every2.every = 2;
        assert_eq!(every2.next_after(mon), mon + 14);
        let plain = Repeat { freq: Freq::Weekly, ..Repeat::default() };
        assert_eq!(plain.next_after(mon + 3), mon + 10);
    }

    #[test]
    fn monthly_and_yearly_clamp() {
        let jan31 = dates::days_from_civil(2026, 1, 31);
        let r = Repeat { freq: Freq::Monthly, ..Repeat::default() };
        assert_eq!(dates::civil_from_days(r.next_after(jan31)), (2026, 2, 28));
        let leap = dates::days_from_civil(2024, 2, 29);
        let y = Repeat { freq: Freq::Yearly, ..Repeat::default() };
        assert_eq!(dates::civil_from_days(y.next_after(leap)), (2025, 2, 28));
    }

    #[test]
    fn advance_skips_the_past_keeps_ranges_and_ends() {
        let today = dates::days_from_civil(2026, 3, 18);
        let r = Repeat { freq: Freq::Weekly, weekdays: 1, ..Repeat::default() }; // Mondays
        let v = r.advance("2026-01-05/2026-01-06", today).unwrap();
        let (a, b) = dates::span(&v).unwrap();
        assert!(a >= today && weekday_of(a) == 0 && b == a + 1);
        let mut e = Repeat::daily();
        e.end = End::After(3);
        e.done = 2;
        assert!(e.advance("2026-03-18", today).is_none());
        let mut u = Repeat::daily();
        u.end = End::On(today + 1);
        assert!(u.advance(&dates::iso_of_day(today - 1), today).is_some());
        assert!(u.advance(&dates::iso_of_day(today + 1), today).is_none());
        assert_eq!(Repeat::weekly_on(&[0, 2]).label(), "Every week on Mon, Wed");
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let s = Settings { quiet: true, quiet_from: 22 * 60, quiet_to: 7 * 60, ..Settings::default() };
        assert!(s.is_quiet(23 * 60) && s.is_quiet(60) && !s.is_quiet(12 * 60));
    }

    #[test]
    fn status_rule_sets_date_and_does_not_retrigger() {
        let (mut ws, db, status, _date, done) = setup();
        let completed = ws.add_prop(db, "Completed", PropKind::Date, &[]);
        let id = ws.id();
        ws.auto.rules.push(Rule::new(id, "stamp", db, Trigger::PropChanged { prop: status, to: ChangeTo::Sel(done) }, vec![Action::Set { prop: completed, value: Setter::Today }]));
        let row = ws.rows(db)[0];
        let mut rt = Runtime::new();
        let m = dates::now_local_min();
        let _ = tick(&mut ws, &mut rt, 1000.0, m); // prime
        ws.page_mut(row).unwrap().props.insert(status, Val::Sel(done));
        let o = tick(&mut ws, &mut rt, 2000.0, m);
        assert!(o.changed);
        assert_eq!(ws.page(row).unwrap().props.get(&completed), Some(&Val::Text(dates::today_iso())));
        assert_eq!(ws.auto.rules[0].runs, 1);
        let _ = tick(&mut ws, &mut rt, 3000.0, m);
        assert_eq!(ws.auto.rules[0].runs, 1);
    }

    #[test]
    fn loops_are_bounded() {
        let (mut ws, db, status, _date, done) = setup();
        let open = ws.opt_id(db, status, "Not started").unwrap();
        for (name, trig, act) in [
            ("A", Trigger::RowAdded, Setter::Sel(open)),
            ("B", Trigger::PropChanged { prop: status, to: ChangeTo::Sel(open) }, Setter::Sel(done)),
            ("C", Trigger::PropChanged { prop: status, to: ChangeTo::Sel(done) }, Setter::Sel(open)),
        ] {
            let id = ws.id();
            ws.auto.rules.push(Rule::new(id, name, db, trig, vec![Action::Set { prop: status, value: act }]));
        }
        let mut rt = Runtime::new();
        let m = dates::now_local_min();
        let _ = tick(&mut ws, &mut rt, 1000.0, m);
        let n = ws.add_row(db, None);
        let _ = tick(&mut ws, &mut rt, 2000.0, m);
        let total: u32 = ws.auto.rules.iter().map(|r| r.runs).sum();
        assert!((2..=10).contains(&total), "cascade must stop (ran {total})");
        assert!(ws.page(n).unwrap().props.contains_key(&status));
    }

    #[test]
    fn completing_a_recurring_task_rolls_the_date_and_resets_status() {
        let (mut ws, db, status, date, done) = setup();
        let row = ws.rows(db)[0];
        let today = dates::today_day();
        {
            let p = ws.page_mut(row).unwrap();
            p.props.insert(date, Val::Text(dates::iso_of_day(today)));
            p.repeats.insert(date, Repeat::daily());
        }
        let mut rt = Runtime::new();
        let m = dates::now_local_min();
        let _ = tick(&mut ws, &mut rt, 1000.0, m);
        ws.page_mut(row).unwrap().props.insert(status, Val::Sel(done));
        let _ = tick(&mut ws, &mut rt, 2000.0, m);
        let p = ws.page(row).unwrap();
        assert_eq!(p.props.get(&date), Some(&Val::Text(dates::iso_of_day(today + 1))));
        let open = ws.opt_id(db, status, "Not started").unwrap();
        assert_eq!(p.props.get(&status), Some(&Val::Sel(open)));
        assert_eq!(p.repeats.get(&date).unwrap().done, 1);
    }

    #[test]
    fn spawn_mode_keeps_the_finished_row() {
        let (mut ws, db, status, date, done) = setup();
        let row = ws.rows(db)[0];
        let today = dates::today_day();
        let before = ws.rows(db).len();
        {
            let p = ws.page_mut(row).unwrap();
            p.props.insert(date, Val::Text(dates::iso_of_day(today)));
            p.repeats.insert(date, Repeat { spawn: true, ..Repeat::weekly_on(&[]) });
        }
        let mut rt = Runtime::new();
        let m = dates::now_local_min();
        let _ = tick(&mut ws, &mut rt, 1000.0, m);
        ws.page_mut(row).unwrap().props.insert(status, Val::Sel(done));
        let _ = tick(&mut ws, &mut rt, 2000.0, m);
        assert_eq!(ws.rows(db).len(), before + 1);
        assert_eq!(ws.page(row).unwrap().props.get(&status), Some(&Val::Sel(done)));
        assert!(ws.page(row).unwrap().repeats.is_empty());
    }

    #[test]
    fn date_trigger_fires_once_and_never_replays_old_dates() {
        let (mut ws, db, _status, date, _done) = setup();
        let now_min = dates::now_local_min();
        let today = now_min.div_euclid(1440);
        let rows = ws.rows(db);
        ws.page_mut(rows[0]).unwrap().props.insert(date, Val::Text(dates::iso_of_day(today + 1)));
        ws.page_mut(rows[1]).unwrap().props.insert(date, Val::Text(dates::iso_of_day(today - 30)));
        let id = ws.id();
        ws.auto.rules.push(Rule::new(id, "due", db, Trigger::DateReached { prop: date, offset_days: -1, minute: 0 }, vec![Action::Notify { message: "{title} due".into() }]));
        let mut rt = Runtime::new();
        let o1 = tick(&mut ws, &mut rt, 1000.0, now_min);
        assert_eq!(o1.notices.len(), 1, "only the fresh occurrence fires");
        let o2 = tick(&mut ws, &mut rt, 2000.0, now_min + 1);
        assert!(o2.notices.is_empty(), "fires once");
    }

    #[test]
    fn reminders_fire_then_repeat_or_finish() {
        let (mut ws, _db, _s, _d, _o) = setup();
        let now_min = dates::now_local_min();
        let day = now_min.div_euclid(1440);
        let minute = now_min.rem_euclid(1440) as u16;
        let id = ws.id();
        ws.auto.reminders.push(Reminder { id, page: 0, prop: 0, day, minute, lead_min: 0, message: "Stretch".into(), repeat: Some(Repeat::daily()), fired_for: 0, done: false });
        let id2 = ws.id();
        ws.auto.reminders.push(Reminder { id: id2, page: 0, prop: 0, day, minute, lead_min: 0, message: "Once".into(), repeat: None, fired_for: 0, done: false });
        let mut rt = Runtime::new();
        let o = tick(&mut ws, &mut rt, 1000.0, now_min);
        assert_eq!(o.notices.len(), 2);
        assert_eq!(ws.auto.reminders[0].day, day + 1);
        assert!(!ws.auto.reminders[0].done);
        assert!(ws.auto.reminders[1].done);
        assert!(push_schedule(&ws, &mut rt, now_min, 48).is_some());
    }

    #[test]
    fn schedule_rule_creates_rows_and_buttons_work() {
        let (mut ws, db, _s, _d, _o) = setup();
        let now_min = dates::now_local_min();
        let day = now_min.div_euclid(1440);
        let id = ws.id();
        let minute = now_min.rem_euclid(1440) as u16;
        ws.auto.rules.push(Rule::new(id, "daily", db, Trigger::Schedule { repeat: Repeat::daily(), start_day: day, minute }, vec![Action::CreateRow { db, title: "Daily {today}".into(), sets: vec![] }]));
        let before = ws.rows(db).len();
        let mut rt = Runtime::new();
        let _ = tick(&mut ws, &mut rt, 1000.0, now_min);
        assert_eq!(ws.rows(db).len(), before + 1);
        assert_eq!(ws.auto.rules[0].next_min, (day + 1) * 1440 + minute as i64);
        let row = ws.rows(db)[0];
        let mut b = ws.blk(BlockKind::Button, "Go", 0);
        b.acts = vec![Action::Append { text: "clicked {title}".into(), todo: true }, Action::Notify { message: "hi".into() }];
        let bid = b.id;
        ws.page_mut(row).unwrap().blocks.push(b);
        let o = press_button(&mut ws, &mut rt, row, bid, 5000.0);
        assert_eq!(o.notices.len(), 1);
        assert!(ws.page(row).unwrap().blocks.iter().any(|b| b.kind == BlockKind::Todo && b.text.starts_with("clicked")));
    }

    #[test]
    fn move_row_maps_properties_by_name() {
        let (mut ws, db, status, _d, done) = setup();
        let db2 = ws.add_page(None, PageKind::Database);
        let st2 = ws.db(db2).unwrap().first_of(PropKind::Select).unwrap();
        let row = ws.rows(db)[0];
        ws.page_mut(row).unwrap().props.insert(status, Val::Sel(done));
        assert!(move_row(&mut ws, row, db2));
        assert_eq!(ws.page(row).unwrap().parent, Some(db2));
        let o = ws.opt_id(db2, st2, "Done").unwrap();
        assert_eq!(ws.page(row).unwrap().props.get(&st2), Some(&Val::Sel(o)));
    }

    #[test]
    fn typing_is_debounced_and_recipes_build() {
        let (mut ws, db, _s, _d, _o) = setup();
        let id = ws.id();
        ws.auto.rules.push(Rule::new(id, "title", db, Trigger::PropChanged { prop: 0, to: ChangeTo::Any }, vec![Action::Notify { message: "renamed".into() }]));
        let row = ws.rows(db)[0];
        let mut rt = Runtime::new();
        let m = dates::now_local_min();
        let _ = tick(&mut ws, &mut rt, 1000.0, m);
        ws.page_mut(row).unwrap().title = "H".into();
        assert!(tick(&mut ws, &mut rt, 1100.0, m).notices.is_empty());
        ws.page_mut(row).unwrap().title = "He".into();
        assert!(tick(&mut ws, &mut rt, 1500.0, m).notices.is_empty());
        assert!(tick(&mut ws, &mut rt, 2000.0, m).notices.is_empty());
        assert_eq!(tick(&mut ws, &mut rt, 3000.0, m).notices.len(), 1);
        for r in recipes() {
            assert!(r.create(&mut ws, if r.needs_db { db } else { 0 }).is_some(), "{}", r.title);
        }
        let s = serde_json::to_string(&ws.auto).unwrap();
        let back: AutoState = serde_json::from_str(&s).unwrap();
        assert_eq!(back.rules.len(), ws.auto.rules.len());
    }
}
