//! Workspace data model: pages, blocks, databases. Everything is serde-serializable.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::auth::now_ms;
use crate::dates;

// ───────────────────────────── blocks ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum BlockKind {
    Text,
    H1,
    H2,
    H3,
    Todo,
    Bullet,
    Numbered,
    Toggle,
    Quote,
    Callout,
    Code,
    Divider,
    /// Auto-generated table of contents.
    Toc,
    /// Link to another page (or database).
    PageLink,
}

impl BlockKind {
    /// Kinds that carry editable text and can be chosen in "Turn into".
    pub const TEXTY: [BlockKind; 11] = [
        BlockKind::Text,
        BlockKind::H1,
        BlockKind::H2,
        BlockKind::H3,
        BlockKind::Todo,
        BlockKind::Bullet,
        BlockKind::Numbered,
        BlockKind::Toggle,
        BlockKind::Quote,
        BlockKind::Callout,
        BlockKind::Code,
    ];

    pub fn has_text(self) -> bool {
        !matches!(self, BlockKind::Divider | BlockKind::Toc | BlockKind::PageLink)
    }

    pub fn label(self) -> &'static str {
        match self {
            BlockKind::Text => "Text",
            BlockKind::H1 => "Heading 1",
            BlockKind::H2 => "Heading 2",
            BlockKind::H3 => "Heading 3",
            BlockKind::Todo => "To-do list",
            BlockKind::Bullet => "Bulleted list",
            BlockKind::Numbered => "Numbered list",
            BlockKind::Toggle => "Toggle list",
            BlockKind::Quote => "Quote",
            BlockKind::Callout => "Callout",
            BlockKind::Code => "Code",
            BlockKind::Divider => "Divider",
            BlockKind::Toc => "Table of contents",
            BlockKind::PageLink => "Link to page",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            BlockKind::Text => "Just start writing with plain text.",
            BlockKind::H1 => "Big section heading.",
            BlockKind::H2 => "Medium section heading.",
            BlockKind::H3 => "Small section heading.",
            BlockKind::Todo => "Track tasks with a to-do list.",
            BlockKind::Bullet => "Create a simple bulleted list.",
            BlockKind::Numbered => "Create a list with numbering.",
            BlockKind::Toggle => "Hide and show nested content.",
            BlockKind::Quote => "Capture a quote.",
            BlockKind::Callout => "Make writing stand out.",
            BlockKind::Code => "Capture a code snippet.",
            BlockKind::Divider => "Visually divide blocks.",
            BlockKind::Toc => "Jump links to every heading.",
            BlockKind::PageLink => "Link to an existing page.",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            BlockKind::Text => "T",
            BlockKind::H1 => "H1",
            BlockKind::H2 => "H2",
            BlockKind::H3 => "H3",
            BlockKind::Todo => "☑",
            BlockKind::Bullet => "•",
            BlockKind::Numbered => "1.",
            BlockKind::Toggle => "▸",
            BlockKind::Quote => "“",
            BlockKind::Callout => "💡",
            BlockKind::Code => "</>",
            BlockKind::Divider => "—",
            BlockKind::Toc => "≡",
            BlockKind::PageLink => "↗",
        }
    }

    pub fn keywords(self) -> &'static str {
        match self {
            BlockKind::Text => "text paragraph plain",
            BlockKind::H1 => "heading h1 title big",
            BlockKind::H2 => "heading h2 subtitle",
            BlockKind::H3 => "heading h3 small",
            BlockKind::Todo => "todo to-do task checkbox check",
            BlockKind::Bullet => "bullet list unordered",
            BlockKind::Numbered => "number ordered list",
            BlockKind::Toggle => "toggle collapse fold details",
            BlockKind::Quote => "quote blockquote cite",
            BlockKind::Callout => "callout note info tip",
            BlockKind::Code => "code snippet pre",
            BlockKind::Divider => "divider line hr separator",
            BlockKind::Toc => "table of contents toc outline",
            BlockKind::PageLink => "link page mention reference",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: u64,
    pub kind: BlockKind,
    pub text: String,
    #[serde(default)]
    pub checked: bool,
    /// Nesting level (0..=6). Blocks nest under the closest previous block with a lower level.
    #[serde(default)]
    pub indent: u8,
    #[serde(default)]
    pub collapsed: bool,
    /// Text colour (0 = default, otherwise tag colour index + 1).
    #[serde(default)]
    pub color: u8,
    /// Background colour (0 = none, otherwise tag colour index + 1).
    #[serde(default)]
    pub bg: u8,
    #[serde(default)]
    pub link: Option<u64>,
}

// ───────────────────────────── databases ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum PropKind {
    Text,
    Number,
    Select,
    MultiSelect,
    Checkbox,
    Date,
    Url,
}

impl PropKind {
    pub const ALL: [PropKind; 7] =
        [PropKind::Text, PropKind::Number, PropKind::Select, PropKind::MultiSelect, PropKind::Checkbox, PropKind::Date, PropKind::Url];

    pub fn label(self) -> &'static str {
        match self {
            PropKind::Text => "Text",
            PropKind::Number => "Number",
            PropKind::Select => "Select",
            PropKind::MultiSelect => "Multi-select",
            PropKind::Checkbox => "Checkbox",
            PropKind::Date => "Date",
            PropKind::Url => "URL",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            PropKind::Text => "≡",
            PropKind::Number => "#",
            PropKind::Select => "◉",
            PropKind::MultiSelect => "☰",
            PropKind::Checkbox => "☑",
            PropKind::Date => "▦",
            PropKind::Url => "↗",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Opt {
    pub id: u64,
    pub name: String,
    pub color: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Prop {
    pub id: u64,
    pub name: String,
    pub kind: PropKind,
    #[serde(default)]
    pub opts: Vec<Opt>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Val {
    Text(String),
    Num(f64),
    Sel(u64),
    Multi(Vec<u64>),
    Check(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ViewKind {
    Table,
    Board,
    List,
    Gallery,
    Calendar,
}

impl ViewKind {
    pub const ALL: [ViewKind; 5] = [ViewKind::Table, ViewKind::Board, ViewKind::List, ViewKind::Gallery, ViewKind::Calendar];

    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Table => "Table",
            ViewKind::Board => "Board",
            ViewKind::List => "List",
            ViewKind::Gallery => "Gallery",
            ViewKind::Calendar => "Calendar",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            ViewKind::Table => "⊞",
            ViewKind::Board => "▥",
            ViewKind::List => "☰",
            ViewKind::Gallery => "▦",
            ViewKind::Calendar => "📅",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum FilterOp {
    Contains,
    Is,
    IsNot,
    Gt,
    Lt,
    IsEmpty,
    NotEmpty,
    Checked,
    Unchecked,
}

impl FilterOp {
    pub fn label(self, kind: PropKind) -> &'static str {
        match (self, kind) {
            (FilterOp::Contains, _) => "contains",
            (FilterOp::Is, PropKind::Number) => "=",
            (FilterOp::Is, _) => "is",
            (FilterOp::IsNot, _) => "is not",
            (FilterOp::Gt, PropKind::Number) => ">",
            (FilterOp::Gt, _) => "after",
            (FilterOp::Lt, PropKind::Number) => "<",
            (FilterOp::Lt, _) => "before",
            (FilterOp::IsEmpty, _) => "is empty",
            (FilterOp::NotEmpty, _) => "is not empty",
            (FilterOp::Checked, _) => "is checked",
            (FilterOp::Unchecked, _) => "is unchecked",
        }
    }

    pub fn needs_value(self) -> bool {
        matches!(self, FilterOp::Contains | FilterOp::Is | FilterOp::IsNot | FilterOp::Gt | FilterOp::Lt)
    }

    pub fn for_kind(kind: PropKind) -> &'static [FilterOp] {
        use FilterOp::*;
        match kind {
            PropKind::Text | PropKind::Url => &[Contains, Is, IsNot, IsEmpty, NotEmpty],
            PropKind::Number => &[Is, Gt, Lt, IsEmpty, NotEmpty],
            PropKind::Select => &[Is, IsNot, IsEmpty, NotEmpty],
            PropKind::MultiSelect => &[Contains, IsEmpty, NotEmpty],
            PropKind::Checkbox => &[Checked, Unchecked],
            PropKind::Date => &[Is, Gt, Lt, IsEmpty, NotEmpty],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Filter {
    /// 0 = the title column.
    pub prop: u64,
    pub op: FilterOp,
    pub value: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sort {
    pub prop: u64,
    pub asc: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct View {
    pub id: u64,
    pub name: String,
    pub kind: ViewKind,
    /// Board grouping property (0 = auto: first select property).
    #[serde(default)]
    pub group: u64,
    /// Calendar date property (0 = auto: first date property).
    #[serde(default)]
    pub date: u64,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub sorts: Vec<Sort>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Database {
    pub props: Vec<Prop>,
    pub views: Vec<View>,
    #[serde(default)]
    pub active: usize,
}

impl Database {
    pub fn prop(&self, id: u64) -> Option<&Prop> {
        self.props.iter().find(|p| p.id == id)
    }

    pub fn first_of(&self, kind: PropKind) -> Option<u64> {
        self.props.iter().find(|p| p.kind == kind).map(|p| p.id)
    }

    pub fn group_prop(&self, v: &View) -> Option<u64> {
        self.prop(v.group).filter(|p| p.kind == PropKind::Select).map(|p| p.id).or_else(|| self.first_of(PropKind::Select))
    }

    pub fn date_prop(&self, v: &View) -> Option<u64> {
        self.prop(v.date).filter(|p| p.kind == PropKind::Date).map(|p| p.id).or_else(|| self.first_of(PropKind::Date))
    }

    pub fn kind_of(&self, prop: u64) -> PropKind {
        if prop == 0 {
            PropKind::Text
        } else {
            self.prop(prop).map(|p| p.kind).unwrap_or(PropKind::Text)
        }
    }

    pub fn opt(&self, prop: u64, oid: u64) -> Option<&Opt> {
        self.prop(prop)?.opts.iter().find(|o| o.id == oid)
    }

    /// Plain-text rendering of a value (used by CSV export, filters and search).
    pub fn val_text(&self, prop: u64, v: &Val) -> String {
        match v {
            Val::Text(s) => s.clone(),
            Val::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Val::Sel(o) => self.opt(prop, *o).map(|o| o.name.clone()).unwrap_or_default(),
            Val::Multi(m) => m.iter().filter_map(|o| self.opt(prop, *o)).map(|o| o.name.clone()).collect::<Vec<_>>().join(", "),
            Val::Check(b) => (if *b { "Yes" } else { "No" }).to_owned(),
        }
    }
}

// ───────────────────────────── pages ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum PageKind {
    Doc,
    /// Legacy kanban page — migrated to a database on load.
    Board,
    Database,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: u64,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Column {
    pub id: u64,
    pub title: String,
    pub color: usize,
    pub cards: Vec<Card>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: u64,
    pub parent: Option<u64>,
    pub title: String,
    pub icon: String,
    pub cover: Option<usize>,
    pub kind: PageKind,
    pub blocks: Vec<Block>,
    /// Legacy kanban columns; read for migration only.
    #[serde(default, skip_serializing)]
    pub columns: Vec<Column>,
    pub favorite: bool,
    pub trashed: bool,
    pub expanded: bool,
    pub updated: f64,
    /// True when this page is a row of a database.
    #[serde(default)]
    pub row: bool,
    #[serde(default)]
    pub props: BTreeMap<u64, Val>,
    #[serde(default)]
    pub db: Option<Database>,
    #[serde(default)]
    pub small_text: bool,
    #[serde(default)]
    pub full_width: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub pages: Vec<Page>,
    pub next_id: u64,
    pub current: Option<u64>,
    pub dark: bool,
    pub sidebar_open: bool,
    #[serde(default)]
    pub accent: usize,
}

pub const ICONS: [&str; 36] = [
    "📄", "📝", "✅", "📚", "💡", "🎯", "🚀", "⭐", "🔥", "🌱", "🎨", "📅", "🏠", "💼", "📎", "❤", "☀", "☕", "⚡", "✈", "🔒", "🔔",
    "🎵", "🌍", "📊", "📁", "🎬", "🛠", "📌", "💬", "🏆", "🎓", "💰", "🔍", "📖", "📷",
];

pub const TEMPLATES: [(&str, &str, &str); 8] = [
    ("Meeting notes", "📝", "Agenda, notes and action items."),
    ("Weekly planner", "📅", "Plan every day of the week."),
    ("Project tracker", "🎯", "Database with table, board and calendar views."),
    ("Task list", "✅", "Tasks with due dates and tags."),
    ("Reading list", "📚", "Books with status, author and rating."),
    ("Journal entry", "📖", "Daily reflection prompts."),
    ("Team wiki", "🏠", "Docs home with contents and FAQ."),
    ("Product spec", "📋", "Problem, goals, requirements, launch plan."),
];

fn blank_page(id: u64, parent: Option<u64>, kind: PageKind) -> Page {
    Page {
        id,
        parent,
        title: String::new(),
        icon: String::new(),
        cover: None,
        kind,
        blocks: vec![],
        columns: vec![],
        favorite: false,
        trashed: false,
        expanded: false,
        updated: now_ms(),
        row: false,
        props: BTreeMap::new(),
        db: None,
        small_text: false,
        full_width: false,
    }
}

impl Workspace {
    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn blk(&mut self, kind: BlockKind, text: &str, indent: u8) -> Block {
        Block { id: self.id(), kind, text: text.into(), checked: false, indent, collapsed: false, color: 0, bg: 0, link: None }
    }

    pub fn page(&self, id: u64) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn page_mut(&mut self, id: u64) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub fn db(&self, id: u64) -> Option<&Database> {
        self.page(id).and_then(|p| p.db.as_ref())
    }

    pub fn db_mut(&mut self, id: u64) -> Option<&mut Database> {
        self.page_mut(id).and_then(|p| p.db.as_mut())
    }

    /// Sidebar children (database rows are not shown in the sidebar).
    pub fn children(&self, parent: Option<u64>) -> Vec<u64> {
        self.pages.iter().filter(|p| p.parent == parent && !p.trashed && !p.row).map(|p| p.id).collect()
    }

    pub fn has_children(&self, id: u64) -> bool {
        self.pages.iter().any(|p| p.parent == Some(id) && !p.trashed && !p.row)
    }

    pub fn add_page(&mut self, parent: Option<u64>, kind: PageKind) -> u64 {
        if kind != PageKind::Doc {
            return self.add_database(parent, ViewKind::Table);
        }
        let id = self.id();
        let b = self.blk(BlockKind::Text, "", 0);
        let mut page = blank_page(id, parent, PageKind::Doc);
        page.icon = "📄".into();
        page.blocks = vec![b];
        self.push_page(page, parent);
        id
    }

    fn push_page(&mut self, page: Page, parent: Option<u64>) {
        let id = page.id;
        if let Some(p) = parent {
            if let Some(pp) = self.page_mut(p) {
                pp.expanded = true;
            }
        }
        self.pages.push(page);
        self.current = Some(id);
    }

    // ───────── databases ─────────

    /// New database with a Status select, a Date and a Tags multi-select.
    pub fn add_database(&mut self, parent: Option<u64>, view: ViewKind) -> u64 {
        let id = self.id();
        let mut page = blank_page(id, parent, PageKind::Database);
        page.icon = "📁".into();
        page.db = Some(Database::default());
        page.full_width = true;
        self.push_page(page, parent);
        self.add_prop(id, "Status", PropKind::Select, &["Not started", "In progress", "Done"]);
        self.add_prop(id, "Date", PropKind::Date, &[]);
        self.add_prop(id, "Tags", PropKind::MultiSelect, &["Idea", "Urgent"]);
        self.add_view(id, view);
        if view != ViewKind::Board {
            for _ in 0..3 {
                self.add_row(id, None);
            }
        }
        id
    }

    pub fn add_prop(&mut self, db: u64, name: &str, kind: PropKind, opts: &[&str]) -> u64 {
        let pid = self.id();
        let opts: Vec<Opt> = opts.iter().enumerate().map(|(i, n)| Opt { id: self.id(), name: (*n).into(), color: i % 9 }).collect();
        if let Some(d) = self.db_mut(db) {
            d.props.push(Prop { id: pid, name: name.into(), kind, opts });
        }
        pid
    }

    pub fn add_view(&mut self, db: u64, kind: ViewKind) -> u64 {
        let vid = self.id();
        if let Some(d) = self.db_mut(db) {
            let same = d.views.iter().filter(|v| v.kind == kind).count();
            let name = if same == 0 { format!("{} view", kind.label()) } else { format!("{} view {}", kind.label(), same + 1) };
            d.views.push(View { id: vid, name, kind, group: 0, date: 0, filters: vec![], sorts: vec![] });
            d.active = d.views.len() - 1;
        }
        vid
    }

    pub fn add_row(&mut self, db: u64, preset: Option<(u64, Val)>) -> u64 {
        let id = self.id();
        let b = self.blk(BlockKind::Text, "", 0);
        let mut page = blank_page(id, Some(db), PageKind::Doc);
        page.blocks = vec![b];
        page.row = true;
        if let Some((k, v)) = preset {
            page.props.insert(k, v);
        }
        self.pages.push(page);
        id
    }

    pub fn opt_id(&self, db: u64, prop: u64, name: &str) -> Option<u64> {
        self.db(db)?.prop(prop)?.opts.iter().find(|o| o.name == name).map(|o| o.id)
    }

    fn titled_row(&mut self, db: u64, title: &str, vals: Vec<(u64, Val)>) -> u64 {
        let r = self.add_row(db, None);
        if let Some(p) = self.page_mut(r) {
            p.title = title.into();
            for (k, v) in vals {
                p.props.insert(k, v);
            }
        }
        r
    }

    fn clear_rows(&mut self, db: u64) {
        self.pages.retain(|p| !(p.row && p.parent == Some(db)));
    }

    pub fn rows(&self, db: u64) -> Vec<u64> {
        self.pages.iter().filter(|p| p.row && p.parent == Some(db) && !p.trashed).map(|p| p.id).collect()
    }

    /// Rows of a database after applying the filters and sorts of view `vi`.
    pub fn query(&self, db: u64, vi: usize) -> Vec<u64> {
        let Some(d) = self.db(db) else { return vec![] };
        let mut rows: Vec<&Page> = self.pages.iter().filter(|p| p.row && p.parent == Some(db) && !p.trashed).collect();
        let Some(view) = d.views.get(vi) else { return rows.into_iter().map(|r| r.id).collect() };
        rows.retain(|r| view.filters.iter().all(|f| row_matches(r, d, f)));
        for s in view.sorts.iter().rev() {
            rows.sort_by(|a, b| {
                let o = cmp_vals(d, s.prop, a, b);
                if s.asc {
                    o
                } else {
                    o.reverse()
                }
            });
        }
        rows.into_iter().map(|r| r.id).collect()
    }

    // ───────── tree operations ─────────

    pub fn descendants(&self, id: u64) -> Vec<u64> {
        let mut out = vec![];
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            for p in self.pages.iter().filter(|p| p.parent == Some(x)) {
                out.push(p.id);
                stack.push(p.id);
            }
        }
        out
    }

    fn fallback_page(&self) -> Option<u64> {
        self.pages.iter().find(|p| !p.trashed && !p.row).map(|p| p.id)
    }

    pub fn set_trashed(&mut self, id: u64, trashed: bool) {
        let mut ids = self.descendants(id);
        ids.push(id);
        for p in self.pages.iter_mut().filter(|p| ids.contains(&p.id)) {
            p.trashed = trashed;
        }
        if trashed {
            if let Some(cur) = self.current {
                if ids.contains(&cur) {
                    let back = self.page(id).and_then(|p| p.parent).filter(|p| self.page(*p).map_or(false, |x| !x.trashed));
                    self.current = back.or_else(|| self.fallback_page());
                }
            }
        } else if let Some(parent) = self.page(id).and_then(|p| p.parent) {
            if self.page(parent).map(|p| p.trashed).unwrap_or(true) {
                if let Some(p) = self.page_mut(id) {
                    p.parent = None;
                    p.row = false;
                }
            }
        }
    }

    pub fn delete_forever(&mut self, id: u64) {
        let mut ids = self.descendants(id);
        ids.push(id);
        self.pages.retain(|p| !ids.contains(&p.id));
        for p in self.pages.iter_mut() {
            for b in p.blocks.iter_mut() {
                if b.link.map_or(false, |l| ids.contains(&l)) {
                    b.link = None;
                }
            }
        }
        if let Some(cur) = self.current {
            if ids.contains(&cur) {
                self.current = self.fallback_page();
            }
        }
    }

    pub fn duplicate(&mut self, id: u64) -> Option<u64> {
        let src = self.page(id)?.clone();
        let rows: Vec<Page> = if src.kind == PageKind::Database { self.rows(id).iter().filter_map(|r| self.page(*r).cloned()).collect() } else { vec![] };
        let nid = self.id();
        let mut copy = src;
        copy.id = nid;
        copy.title = if copy.title.is_empty() { "Untitled copy".into() } else { format!("{} (copy)", copy.title) };
        copy.favorite = false;
        copy.updated = now_ms();
        for b in copy.blocks.iter_mut() {
            b.id = self.id();
        }
        self.pages.push(copy);
        for mut r in rows {
            r.id = self.id();
            r.parent = Some(nid);
            for b in r.blocks.iter_mut() {
                b.id = self.id();
            }
            self.pages.push(r);
        }
        self.current = Some(nid);
        Some(nid)
    }

    pub fn breadcrumb(&self, id: u64) -> Vec<u64> {
        let mut out = vec![];
        let mut cur = Some(id);
        while let Some(c) = cur {
            out.push(c);
            cur = self.page(c).and_then(|p| p.parent);
            if out.len() > 32 {
                break;
            }
        }
        out.reverse();
        out
    }

    pub fn touch(&mut self, id: u64) {
        if let Some(p) = self.page_mut(id) {
            p.updated = now_ms();
        }
    }

    /// Pages that link to `id` through a page-link block.
    pub fn backlinks(&self, id: u64) -> Vec<u64> {
        self.pages
            .iter()
            .filter(|p| !p.trashed && p.id != id && p.blocks.iter().any(|b| b.kind == BlockKind::PageLink && b.link == Some(id)))
            .map(|p| p.id)
            .collect()
    }

    pub fn display_title(&self, id: u64) -> String {
        match self.page(id) {
            Some(p) if !p.title.is_empty() => p.title.clone(),
            _ => "Untitled".into(),
        }
    }

    /// Opens (creating when needed) today's entry inside the "Journal" page.
    pub fn open_today(&mut self) {
        let root = match self.pages.iter().find(|p| p.title == "Journal" && p.parent.is_none() && !p.trashed && !p.row) {
            Some(p) => p.id,
            None => {
                let r = self.add_page(None, PageKind::Doc);
                let intro = self.blk(BlockKind::Callout, "Your daily journal. Press Ctrl+J any time to open today's entry.", 0);
                if let Some(p) = self.page_mut(r) {
                    p.title = "Journal".into();
                    p.icon = "📖".into();
                    p.blocks.insert(0, intro);
                }
                r
            }
        };
        let today = dates::long_today();
        if let Some(e) = self.pages.iter().find(|p| p.parent == Some(root) && p.title == today && !p.trashed) {
            self.current = Some(e.id);
            return;
        }
        let e = self.add_page(Some(root), PageKind::Doc);
        self.fill_template_doc(e, 5);
        if let Some(p) = self.page_mut(e) {
            p.title = today;
            p.icon = "📖".into();
        }
    }

    // ───────── migration of the old kanban pages ─────────

    pub fn migrate(&mut self) {
        let boards: Vec<u64> = self.pages.iter().filter(|p| p.kind == PageKind::Board).map(|p| p.id).collect();
        for id in boards {
            let Some(p) = self.page(id).cloned() else { continue };
            let names: Vec<String> = p.columns.iter().map(|c| c.title.clone()).collect();
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            if let Some(pg) = self.page_mut(id) {
                pg.kind = PageKind::Database;
                pg.db = Some(Database::default());
                pg.blocks.clear();
                pg.full_width = true;
            }
            let status = self.add_prop(id, "Status", PropKind::Select, &name_refs);
            self.add_view(id, ViewKind::Board);
            self.add_view(id, ViewKind::Table);
            for c in p.columns.iter() {
                let Some(oid) = self.opt_id(id, status, &c.title) else { continue };
                if let Some(o) = self.db_mut(id).and_then(|d| d.props[0].opts.iter_mut().find(|o| o.id == oid)) {
                    o.color = c.color;
                }
                for card in c.cards.iter() {
                    self.titled_row(id, &card.text, vec![(status, Val::Sel(oid))]);
                }
            }
            if let Some(d) = self.db_mut(id) {
                d.active = 0;
            }
        }
    }

    // ───────── templates ─────────

    pub fn fill_template_doc(&mut self, id: u64, tpl: usize) {
        let mut v: Vec<Block> = vec![];
        let mut add = |ws: &mut Workspace, k: BlockKind, t: &str, i: u8| v.push(ws.blk(k, t, i));
        use BlockKind::*;
        match tpl {
            0 => {
                add(self, Callout, "Tip: type / for blocks, Tab to nest, and **bold** with Ctrl+B.", 0);
                add(self, H2, "Attendees", 0);
                add(self, Bullet, "", 0);
                add(self, H2, "Agenda", 0);
                add(self, Numbered, "Topic one", 0);
                add(self, Numbered, "Topic two", 0);
                add(self, H2, "Notes", 0);
                add(self, Text, "", 0);
                add(self, H2, "Action items", 0);
                add(self, Todo, "", 0);
            }
            1 => {
                for d in ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"] {
                    add(self, H2, d, 0);
                    add(self, Todo, "", 0);
                    add(self, Todo, "", 0);
                }
                add(self, H2, "Weekend", 0);
                add(self, Bullet, "", 0);
            }
            5 => {
                add(self, H2, "How am I feeling?", 0);
                add(self, Text, "", 0);
                add(self, H2, "Three things I'm grateful for", 0);
                add(self, Numbered, "", 0);
                add(self, H2, "What would make today great?", 0);
                add(self, Todo, "", 0);
                add(self, H2, "Highlights", 0);
                add(self, Text, "", 0);
            }
            6 => {
                add(self, Callout, "Welcome to the team wiki. Keep everything the team needs in one place.", 0);
                add(self, Toc, "", 0);
                add(self, H1, "Getting started", 0);
                add(self, Bullet, "Who we are and what we do", 0);
                add(self, Bullet, "How to request access", 0);
                add(self, H1, "Processes", 0);
                add(self, Toggle, "How do we ship a release?", 0);
                add(self, Text, "Write the steps here — this text is nested inside the toggle.", 1);
                add(self, Toggle, "How do we handle incidents?", 0);
                add(self, Text, "Describe the on-call process.", 1);
                add(self, H1, "FAQ", 0);
                add(self, Text, "", 0);
            }
            7 => {
                add(self, Callout, "One paragraph summary of what we're building and why.", 0);
                add(self, H1, "Problem", 0);
                add(self, Text, "", 0);
                add(self, H1, "Goals", 0);
                add(self, Bullet, "", 0);
                add(self, H1, "Requirements", 0);
                add(self, Todo, "Must have", 0);
                add(self, Todo, "Should have", 0);
                add(self, Todo, "Nice to have", 0);
                add(self, H1, "Launch plan", 0);
                add(self, Numbered, "", 0);
                add(self, H1, "Open questions", 0);
                add(self, Text, "", 0);
            }
            _ => add(self, Text, "", 0),
        }
        if let Some(p) = self.page_mut(id) {
            p.blocks = v;
        }
    }

    fn day_val(offset: i64) -> Val {
        let (y, m, d) = dates::today();
        let (a, b, c) = dates::civil_from_days(dates::days_from_civil(y, m, d) + offset);
        Val::Text(dates::iso(a, b, c))
    }

    pub fn add_template(&mut self, tpl: usize, parent: Option<u64>) -> u64 {
        let (name, icon, _) = TEMPLATES[tpl.min(TEMPLATES.len() - 1)];
        match tpl {
            2 => {
                let db = self.add_database(parent, ViewKind::Table);
                self.clear_rows(db);
                let status = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let date = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                let prio = self.add_prop(db, "Priority", PropKind::Select, &["Low", "Medium", "High"]);
                let owner = self.add_prop(db, "Owner", PropKind::Text, &[]);
                if let Some(dp) = self.db_mut(db).and_then(|d| d.props.iter_mut().find(|x| x.id == date)) {
                    dp.name = "Due".into();
                }
                self.add_view(db, ViewKind::Board);
                self.add_view(db, ViewKind::Calendar);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                let items = [
                    ("Define project scope", "Not started", "High", 3),
                    ("Design mockups", "In progress", "Medium", 5),
                    ("Build prototype", "In progress", "High", 9),
                    ("User testing", "Not started", "Medium", 14),
                    ("Kickoff meeting", "Done", "Low", -2),
                ];
                for (t, s, p, n) in items {
                    let sv = Val::Sel(self.opt_id(db, status, s).unwrap_or(0));
                    let pv = Val::Sel(self.opt_id(db, prio, p).unwrap_or(0));
                    self.titled_row(db, t, vec![(status, sv), (prio, pv), (date, Self::day_val(n)), (owner, Val::Text("Me".into()))]);
                }
                self.finish_template(db, name, icon);
                db
            }
            3 => {
                let db = self.add_database(parent, ViewKind::Table);
                self.clear_rows(db);
                let done = self.add_prop(db, "Done", PropKind::Checkbox, &[]);
                self.add_view(db, ViewKind::List);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                    d.views[0].sorts.push(Sort { prop: done, asc: true });
                }
                for (t, c, n) in [("Buy groceries", false, 1), ("Reply to emails", true, 0), ("Plan the week", false, 2)] {
                    let date = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                    self.titled_row(db, t, vec![(done, Val::Check(c)), (date, Self::day_val(n))]);
                }
                self.finish_template(db, name, icon);
                db
            }
            4 => {
                let db = self.add_database(parent, ViewKind::Gallery);
                self.clear_rows(db);
                let status = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let author = self.add_prop(db, "Author", PropKind::Text, &[]);
                let rating = self.add_prop(db, "Rating", PropKind::Number, &[]);
                self.add_view(db, ViewKind::Table);
                self.add_view(db, ViewKind::Board);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                let items = [
                    ("The Rust Programming Language", "Klabnik & Nichols", "Done", 5.0),
                    ("Designing Data-Intensive Applications", "Martin Kleppmann", "In progress", 0.0),
                    ("The Pragmatic Programmer", "Hunt & Thomas", "Not started", 0.0),
                ];
                for (t, a, s, r) in items {
                    let sid = self.opt_id(db, status, s).unwrap_or(0);
                    let mut vals = vec![(author, Val::Text(a.into())), (status, Val::Sel(sid))];
                    if r > 0.0 {
                        vals.push((rating, Val::Num(r)));
                    }
                    self.titled_row(db, t, vals);
                }
                self.finish_template(db, name, icon);
                db
            }
            _ => {
                let id = self.add_page(parent, PageKind::Doc);
                self.fill_template_doc(id, tpl);
                self.finish_template(id, name, icon);
                id
            }
        }
    }

    fn finish_template(&mut self, id: u64, name: &str, icon: &str) {
        if let Some(p) = self.page_mut(id) {
            p.title = name.into();
            p.icon = icon.into();
        }
        self.current = Some(id);
    }

    // ───────── first-run content ─────────

    pub fn seed() -> Self {
        let mut ws = Workspace { name: "My Workspace".into(), pages: vec![], next_id: 0, current: None, dark: false, sidebar_open: true, accent: 0 };
        use BlockKind::*;

        let start = ws.add_page(None, PageKind::Doc);
        let mut bl: Vec<Block> = vec![];
        let mut add = |ws: &mut Workspace, k: BlockKind, t: &str, i: u8| bl.push(ws.blk(k, t, i));
        add(&mut ws, Callout, "Welcome to Lumen — a free, offline-first Notion alternative written 100% in Rust with egui. No web engine, no JavaScript UI. It runs natively on Windows and in your browser through WebAssembly.", 0);
        add(&mut ws, Toc, "", 0);
        add(&mut ws, H2, "Write with blocks", 0);
        add(&mut ws, Text, "Press Enter for a new block, Backspace at the start to merge or reset it, and type / to open the command menu. Select text and press Ctrl+B for **bold**, Ctrl+I for *italic*, Ctrl+E for `code`, Ctrl+Shift+S for ~~strike~~.", 0);
        add(&mut ws, Bullet, "Markdown shortcuts: '# ', '## ', '- ', '1. ', '[] ', '> ' (toggle), '\" ' (quote) or '---' at the start of a line", 0);
        add(&mut ws, Bullet, "Tab / Shift+Tab nests and un-nests blocks", 1);
        add(&mut ws, Bullet, "Drag the grip handle to reorder — children move with their parent", 1);
        add(&mut ws, Bullet, "Click the grip for colours, turn-into, duplicate and delete", 0);
        add(&mut ws, Toggle, "Toggles hide nested content (click the arrow)", 0);
        add(&mut ws, Text, "Anything indented under a toggle collapses with it.", 1);
        add(&mut ws, H2, "Databases", 0);
        add(&mut ws, Text, "Every database has typed properties and as many views as you like — table, board, list, gallery and calendar — with filters and sorts. Open the Roadmap database to try it.", 0);
        add(&mut ws, H2, "Get things done", 0);
        add(&mut ws, Todo, "Create your first page from the sidebar", 0);
        add(&mut ws, Todo, "Open the Roadmap database and drag a card between columns", 0);
        add(&mut ws, Todo, "Press Ctrl+J to start today's journal entry", 0);
        add(&mut ws, Todo, "Pick a template from the sidebar (Templates)", 0);
        add(&mut ws, Todo, "Lock your workspace with a password (workspace menu)", 0);
        add(&mut ws, Divider, "", 0);
        add(&mut ws, Quote, "Simplicity is the ultimate sophistication.", 0);
        add(&mut ws, Code, "fn main() {\n    println!(\"Hello from Lumen\");\n}", 0);
        add(&mut ws, Text, "", 0);
        if let Some(p) = ws.page_mut(start) {
            p.title = "Getting Started".into();
            p.icon = "🚀".into();
            p.cover = Some(1);
            p.blocks = bl;
            p.favorite = true;
            p.expanded = true;
        }

        let notes = ws.add_page(Some(start), PageKind::Doc);
        ws.fill_template_doc(notes, 0);
        if let Some(p) = ws.page_mut(notes) {
            p.title = "Meeting Notes".into();
            p.icon = "📝".into();
        }

        // Roadmap database
        let road = ws.add_database(None, ViewKind::Board);
        let status = ws.db(road).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
        let date = ws.db(road).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
        let tags = ws.db(road).and_then(|d| d.first_of(PropKind::MultiSelect)).unwrap_or(0);
        let prio = ws.add_prop(road, "Priority", PropKind::Select, &["Low", "Medium", "High"]);
        ws.add_view(road, ViewKind::Table);
        ws.add_view(road, ViewKind::Calendar);
        if let Some(d) = ws.db_mut(road) {
            d.active = 0;
        }
        let items = [
            ("Design onboarding flow", "Not started", "Medium", 6),
            ("Write launch blog post", "Not started", "Low", 11),
            ("Build command palette", "In progress", "High", 2),
            ("Password lock screen", "In progress", "High", 4),
            ("Block editor", "Done", "High", -6),
            ("WebAssembly build", "Done", "Medium", -3),
            ("Databases with 5 views", "Done", "High", -1),
        ];
        for (i, (t, s, p, n)) in items.iter().enumerate() {
            let sid = ws.opt_id(road, status, s).unwrap_or(0);
            let pid = ws.opt_id(road, prio, p).unwrap_or(0);
            let mut vals = vec![(status, Val::Sel(sid)), (prio, Val::Sel(pid)), (date, Self::day_val(*n as i64))];
            if i % 3 == 0 {
                if let Some(o) = ws.opt_id(road, tags, "Idea") {
                    vals.push((tags, Val::Multi(vec![o])));
                }
            }
            ws.titled_row(road, t, vals);
        }
        if let Some(p) = ws.page_mut(road) {
            p.title = "Roadmap".into();
            p.icon = "🎯".into();
            p.cover = Some(4);
        }

        let reading = ws.add_template(4, None);
        if let Some(p) = ws.page_mut(reading) {
            p.title = "Reading".into();
        }

        ws.current = Some(start);
        ws
    }
}

// ───────────────────────────── query helpers ─────────────────────────────

pub fn row_val(row: &Page, prop: u64) -> Option<Val> {
    if prop == 0 {
        Some(Val::Text(row.title.clone()))
    } else {
        row.props.get(&prop).cloned()
    }
}

fn names_of(d: &Database, prop: u64, v: &Val) -> Vec<String> {
    match v {
        Val::Sel(o) => d.opt(prop, *o).map(|o| vec![o.name.to_lowercase()]).unwrap_or_default(),
        Val::Multi(m) => m.iter().filter_map(|o| d.opt(prop, *o)).map(|o| o.name.to_lowercase()).collect(),
        _ => vec![],
    }
}

pub fn row_matches(row: &Page, d: &Database, f: &Filter) -> bool {
    let kind = d.kind_of(f.prop);
    let val = row_val(row, f.prop);
    let empty = match &val {
        None => true,
        Some(Val::Text(s)) => s.trim().is_empty(),
        Some(Val::Multi(m)) => m.is_empty(),
        _ => false,
    };
    let needle = f.value.trim().to_lowercase();
    match f.op {
        FilterOp::IsEmpty => empty,
        FilterOp::NotEmpty => !empty,
        FilterOp::Checked => matches!(val, Some(Val::Check(true))),
        FilterOp::Unchecked => !matches!(val, Some(Val::Check(true))),
        op => {
            if needle.is_empty() {
                return true; // incomplete filter: ignore
            }
            let Some(v) = val else { return op == FilterOp::IsNot };
            match kind {
                PropKind::Number => {
                    let (Val::Num(a), Ok(b)) = (&v, needle.parse::<f64>()) else { return false };
                    match op {
                        FilterOp::Is => (*a - b).abs() < 1e-9,
                        FilterOp::IsNot => (*a - b).abs() >= 1e-9,
                        FilterOp::Gt => *a > b,
                        FilterOp::Lt => *a < b,
                        _ => true,
                    }
                }
                PropKind::Select | PropKind::MultiSelect => {
                    let names = names_of(d, f.prop, &v);
                    match op {
                        FilterOp::Is => names.iter().any(|n| *n == needle),
                        FilterOp::IsNot => !names.iter().any(|n| *n == needle),
                        _ => names.iter().any(|n| n.contains(&needle)),
                    }
                }
                _ => {
                    let s = d.val_text(f.prop, &v).to_lowercase();
                    match op {
                        FilterOp::Is => s == needle,
                        FilterOp::IsNot => s != needle,
                        FilterOp::Gt => s > needle,
                        FilterOp::Lt => s < needle,
                        _ => s.contains(&needle),
                    }
                }
            }
        }
    }
}

pub fn cmp_vals(d: &Database, prop: u64, a: &Page, b: &Page) -> Ordering {
    let key = |p: &Page| -> (u8, f64, String) {
        match row_val(p, prop) {
            None => (1, 0.0, String::new()),
            Some(Val::Num(n)) => (0, n, String::new()),
            Some(Val::Check(c)) => (0, if c { 1.0 } else { 0.0 }, String::new()),
            Some(Val::Text(s)) if s.is_empty() => (1, 0.0, String::new()),
            Some(v) => (0, 0.0, d.val_text(prop, &v).to_lowercase()),
        }
    };
    let (ka, kb) = (key(a), key(b));
    ka.0.cmp(&kb.0).then(ka.1.partial_cmp(&kb.1).unwrap_or(Ordering::Equal)).then(ka.2.cmp(&kb.2))
}
