//! Workspace data model: pages, blocks, databases. Everything is serde-serializable.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::auth::now_ms;
use crate::dates;
use crate::lucide as l;

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
    /// Picture: uploaded from disk (`media`) or linked (`url`).
    Image,
    /// Website / video / map / design file shown as a rich embed (live iframe on the web build).
    Embed,
    /// Link card with favicon and address.
    Bookmark,
    /// Attached file stored in the workspace.
    File,
    /// Layout container: its children are `Column` blocks laid out side by side.
    Columns,
    /// One column of a `Columns` container (width in `start`, permille of the row; 0 = equal share). Content = its children.
    Column,
    /// A linked view of a database: `link` = the source database, its own layout / filters / sorts live in a hidden `View`.
    DbView,
    /// A dashboard widget placed inside the page (`media` = id of the widget on the hidden store board, see `inlinew.rs`).
    Widget,
    /// One-click automation button: `text` = label, `acts` = what it does (see `automation.rs`).
    Button,
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
        !matches!(self, BlockKind::Divider | BlockKind::Toc | BlockKind::PageLink | BlockKind::Image | BlockKind::Embed | BlockKind::Bookmark | BlockKind::File | BlockKind::Columns | BlockKind::Column | BlockKind::DbView | BlockKind::Widget | BlockKind::Button)
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
            BlockKind::Image => "Image",
            BlockKind::Embed => "Embed",
            BlockKind::Bookmark => "Web bookmark",
            BlockKind::File => "File",
            BlockKind::Columns => "Columns",
            BlockKind::Column => "Column",
            BlockKind::DbView => "Linked database view",
            BlockKind::Widget => "Dashboard widget",
            BlockKind::Button => "Button",
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
            BlockKind::Image => "Upload a picture or paste an image link.",
            BlockKind::Embed => "Embed a website, video, map or design.",
            BlockKind::Bookmark => "Save a link as a visual card.",
            BlockKind::File => "Attach a file to this page.",
            BlockKind::Columns => "Lay content out side by side.",
            BlockKind::Column => "A column.",
            BlockKind::DbView => "Show a filtered view of any database right here.",
            BlockKind::Widget => "Place a dashboard widget (chart, number, tasks …) in the page.",
            BlockKind::Button => "One click runs actions: set properties, create tasks, move rows, notify.",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            BlockKind::Text => l::TYPE,
            BlockKind::H1 => l::HEADING_1,
            BlockKind::H2 => l::HEADING_2,
            BlockKind::H3 => l::HEADING_3,
            BlockKind::Todo => l::SQUARE_CHECK,
            BlockKind::Bullet => l::LIST,
            BlockKind::Numbered => l::LIST_ORDERED,
            BlockKind::Toggle => l::TOGGLE_RIGHT,
            BlockKind::Quote => l::TEXT_QUOTE,
            BlockKind::Callout => l::LIGHTBULB,
            BlockKind::Code => l::CODE,
            BlockKind::Divider => l::MINUS,
            BlockKind::Toc => l::TABLE_OF_CONTENTS,
            BlockKind::PageLink => l::LINK_2,
            BlockKind::Image => l::IMAGE,
            BlockKind::Embed => l::GLOBE,
            BlockKind::Bookmark => l::BOOKMARK,
            BlockKind::File => l::PAPERCLIP,
            BlockKind::Columns | BlockKind::Column => l::COLUMNS_2,
            BlockKind::DbView => l::TABLE_2,
            BlockKind::Widget => l::CHART_PIE,
            BlockKind::Button => l::MOUSE_POINTER_2,
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
            BlockKind::Image => "image picture photo upload png jpg gif",
            BlockKind::Embed => "embed website iframe video youtube vimeo map figma spotify",
            BlockKind::Bookmark => "bookmark link url web card",
            BlockKind::File => "file attachment upload pdf document",
            BlockKind::Columns | BlockKind::Column => "columns layout side by side split grid",
            BlockKind::DbView => "linked database view filter embed table inline",
            BlockKind::Widget => "widget chart kpi number gauge stats dashboard",
            BlockKind::Button => "button automation action click run rule trigger macro",
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
    /// Online source (image / embed / bookmark URL).
    #[serde(default)]
    pub url: String,
    /// Local media id (0 = none).
    #[serde(default)]
    pub media: u64,
    /// Image width in percent of the column (0 = 100).
    #[serde(default)]
    pub size: u8,
    /// Video blocks: start offset in seconds.
    #[serde(default)]
    pub start: u32,
    /// Video blocks: 1 = autoplay, 2 = muted, 4 = loop. (`size` holds the aspect ratio: 0 = 16:9, 1 = 4:3, 2 = 21:9, 3 = 1:1, 4 = 9:16.)
    #[serde(default)]
    pub vopts: u8,
    /// Button blocks: the automation actions one click runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acts: Vec<crate::automation::Action>,
}

impl Block {
    pub fn new(id: u64, kind: BlockKind, text: &str, indent: u8) -> Block {
        Block { id, kind, text: text.into(), checked: false, indent, collapsed: false, color: 0, bg: 0, link: None, url: String::new(), media: 0, size: 0, start: 0, vopts: 0, acts: vec![] }
    }

    /// Source string of a media block: `media:<id>` for local files, otherwise the URL.
    pub fn src(&self) -> String {
        if self.media != 0 {
            format!("media:{}", self.media)
        } else {
            self.url.clone()
        }
    }
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
    /// Computed from other properties with a Notion-style expression.
    Formula,
    /// Links to rows of another (or the same) database.
    Relation,
    /// Aggregates a property of the related rows.
    Rollup,
}

impl PropKind {
    pub const ALL: [PropKind; 10] = [
        PropKind::Text,
        PropKind::Number,
        PropKind::Select,
        PropKind::MultiSelect,
        PropKind::Checkbox,
        PropKind::Date,
        PropKind::Url,
        PropKind::Formula,
        PropKind::Relation,
        PropKind::Rollup,
    ];

    /// Formula and rollup values are computed (read-only).
    pub fn computed(self) -> bool {
        matches!(self, PropKind::Formula | PropKind::Rollup)
    }

    pub fn label(self) -> &'static str {
        match self {
            PropKind::Text => "Text",
            PropKind::Number => "Number",
            PropKind::Select => "Select",
            PropKind::MultiSelect => "Multi-select",
            PropKind::Checkbox => "Checkbox",
            PropKind::Date => "Date",
            PropKind::Url => "URL",
            PropKind::Formula => "Formula",
            PropKind::Relation => "Relation",
            PropKind::Rollup => "Rollup",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            PropKind::Text => l::ALIGN_LEFT,
            PropKind::Number => l::HASH,
            PropKind::Select => l::CIRCLE_DOT,
            PropKind::MultiSelect => l::LIST,
            PropKind::Checkbox => l::SQUARE_CHECK,
            PropKind::Date => l::CALENDAR_DAYS,
            PropKind::Url => l::LINK_2,
            PropKind::Formula => l::SIGMA,
            PropKind::Relation => l::ARROW_UP_RIGHT,
            PropKind::Rollup => l::LAYERS,
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
    /// Settings of formula / relation / rollup properties.
    #[serde(default)]
    pub cfg: PropCfg,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum RollupFn {
    #[default]
    Count,
    CountValues,
    Unique,
    Sum,
    Average,
    Min,
    Max,
    Median,
    Range,
    PercentChecked,
    PercentEmpty,
    ShowOriginal,
    Earliest,
    Latest,
}

impl RollupFn {
    pub const ALL: [RollupFn; 14] = [
        RollupFn::Count,
        RollupFn::CountValues,
        RollupFn::Unique,
        RollupFn::Sum,
        RollupFn::Average,
        RollupFn::Min,
        RollupFn::Max,
        RollupFn::Median,
        RollupFn::Range,
        RollupFn::PercentChecked,
        RollupFn::PercentEmpty,
        RollupFn::ShowOriginal,
        RollupFn::Earliest,
        RollupFn::Latest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RollupFn::Count => "Count all",
            RollupFn::CountValues => "Count values",
            RollupFn::Unique => "Count unique values",
            RollupFn::Sum => "Sum",
            RollupFn::Average => "Average",
            RollupFn::Min => "Min",
            RollupFn::Max => "Max",
            RollupFn::Median => "Median",
            RollupFn::Range => "Range (max − min)",
            RollupFn::PercentChecked => "Percent checked",
            RollupFn::PercentEmpty => "Percent empty",
            RollupFn::ShowOriginal => "Show original",
            RollupFn::Earliest => "Earliest date",
            RollupFn::Latest => "Latest date",
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Debug)]
pub struct PropCfg {
    /// Formula expression, e.g. `prop("Price") * prop("Qty")`.
    #[serde(default)]
    pub formula: String,
    /// Relation: id of the related database page.
    #[serde(default)]
    pub relation: u64,
    /// Rollup: the relation property (of this database) to follow.
    #[serde(default)]
    pub rollup_rel: u64,
    /// Rollup: the property of the related database to aggregate (0 = its title).
    #[serde(default)]
    pub rollup_prop: u64,
    #[serde(default)]
    pub rollup_fn: RollupFn,
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
    /// Gantt-style timeline with draggable date-range bars and dependency arrows.
    Timeline,
}

impl ViewKind {
    pub const ALL: [ViewKind; 6] = [ViewKind::Table, ViewKind::Board, ViewKind::List, ViewKind::Gallery, ViewKind::Calendar, ViewKind::Timeline];

    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Table => "Table",
            ViewKind::Board => "Board",
            ViewKind::List => "List",
            ViewKind::Gallery => "Gallery",
            ViewKind::Calendar => "Calendar",
            ViewKind::Timeline => "Timeline",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            ViewKind::Table => l::TABLE,
            ViewKind::Board => l::KANBAN,
            ViewKind::List => l::LAYOUT_LIST,
            ViewKind::Gallery => l::LAYOUT_GRID,
            ViewKind::Calendar => l::CALENDAR_DAYS,
            ViewKind::Timeline => l::CHART_GANTT,
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
    /// Date filters relative to today (no value needed).
    Today,
    ThisWeek,
    ThisMonth,
    Next7,
    Past7,
    Overdue,
}

impl FilterOp {
    /// The window (inclusive day numbers) of a relative date filter.
    pub fn window(self, today: i64) -> Option<(i64, i64)> {
        use crate::dashcfg::Range;
        match self {
            FilterOp::Today => Range::Today.bounds(today),
            FilterOp::ThisWeek => Range::ThisWeek.bounds(today),
            FilterOp::ThisMonth => Range::ThisMonth.bounds(today),
            FilterOp::Next7 => Range::Next7.bounds(today),
            FilterOp::Past7 => Range::Last7.bounds(today),
            FilterOp::Overdue => Some((i64::MIN / 2, today - 1)),
            _ => None,
        }
    }

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
            (FilterOp::Today, _) => "is today",
            (FilterOp::ThisWeek, _) => "is this week",
            (FilterOp::ThisMonth, _) => "is this month",
            (FilterOp::Next7, _) => "is in the next 7 days",
            (FilterOp::Past7, _) => "is in the past 7 days",
            (FilterOp::Overdue, _) => "is overdue",
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
            PropKind::Date => &[Today, ThisWeek, ThisMonth, Next7, Past7, Overdue, Is, Gt, Lt, IsEmpty, NotEmpty],
            PropKind::Formula | PropKind::Rollup => &[Contains, Is, Gt, Lt, IsEmpty, NotEmpty],
            PropKind::Relation => &[IsEmpty, NotEmpty],
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
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
    /// Timeline: self-relation used for dependency arrows (0 = auto: first self-relation).
    #[serde(default)]
    pub dep: u64,
    /// Timeline: pixels per day (0 = default).
    #[serde(default)]
    pub zoom: u8,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub sorts: Vec<Sort>,
    /// Table: custom column widths (property id → points; 0 = the name column).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub widths: BTreeMap<u64, f32>,
    /// 0 = a normal view (shown as a tab of the database). Otherwise the id of the page block (`BlockKind::DbView`) that owns
    /// this view: it only appears inside that block, with its own filters and sorts ("linked database view").
    #[serde(default)]
    pub linked: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Database {
    pub props: Vec<Prop>,
    pub views: Vec<View>,
    #[serde(default)]
    pub active: usize,
}

impl Database {
    /// Indexes of the views that are tabs of the database page (linked views are hidden).
    pub fn tabs(&self) -> Vec<usize> {
        (0..self.views.len()).filter(|i| self.views[*i].linked == 0).collect()
    }

    /// The active view index, never pointing at a linked (hidden) view.
    pub fn active_tab(&self) -> usize {
        let a = self.active.min(self.views.len().saturating_sub(1));
        if self.views.get(a).map_or(true, |v| v.linked == 0) {
            a
        } else {
            self.tabs().first().copied().unwrap_or(0)
        }
    }

    /// Index of the view owned by the linked block `block`.
    pub fn linked_index(&self, block: u64) -> Option<usize> {
        self.views.iter().position(|v| v.linked == block)
    }

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

    /// Self-relation (a relation pointing at this very database) used for task dependencies.
    pub fn dep_prop(&self, v: &View, my_id: u64) -> Option<u64> {
        self.prop(v.dep)
            .filter(|p| p.kind == PropKind::Relation && p.cfg.relation == my_id)
            .map(|p| p.id)
            .or_else(|| self.props.iter().find(|p| p.kind == PropKind::Relation && p.cfg.relation == my_id).map(|p| p.id))
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
    /// Infinite whiteboard of cards, pages, images and connectors (see `canvas.rs`).
    Canvas,
    /// Node-based data-flow / automation graph (see `flow.rs`).
    Flow,
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

pub const ZOOM_MIN: f32 = 0.7;
pub const ZOOM_MAX: f32 = 1.8;

fn default_zoom() -> f32 {
    1.0
}

/// Clamps a content size factor; NaN / 0 / negative values fall back to 100 %.
pub fn clamp_zoom(z: f32) -> f32 {
    if z.is_finite() && z > 0.0 {
        z.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: u64,
    pub parent: Option<u64>,
    pub title: String,
    pub icon: String,
    pub cover: Option<usize>,
    /// Cover picture (`https://…` or `media:<id>`); wins over the gradient `cover`.
    #[serde(default)]
    pub cover_img: String,
    /// Vertical focus of the cover picture (0 = top, 1 = bottom).
    #[serde(default = "half")]
    pub cover_pos: f32,
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
    /// Content size of the page body (1.0 = 100 %), set from the ⋯ page menu. Only the page content scales; menus never do.
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    /// Versioned snapshots (oldest first), see `history.rs`.
    #[serde(default)]
    pub history: Vec<Version>,
    /// Repeat rules of this row's date properties (date property id → rule), see `automation.rs`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repeats: BTreeMap<u64, crate::automation::Repeat>,
    /// The whiteboard of a [`PageKind::Canvas`] page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<crate::canvas::Canvas>,
    /// The node graph of a [`PageKind::Flow`] page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<crate::flow::Flow>,
}

/// A saved snapshot of a page's content.
#[derive(Clone, Serialize, Deserialize)]
pub struct Version {
    pub ts: f64,
    pub title: String,
    pub blocks: Vec<Block>,
    /// True for versions the user saved by hand (kept when pruning).
    #[serde(default)]
    pub manual: bool,
    #[serde(default)]
    pub note: String,
}

fn half() -> f32 {
    0.5
}

impl Page {
    /// A cheap stand-in with the same identity but no blocks / history / database (see `editor::show_page`).
    pub fn shell(&self) -> Page {
        Page {
            id: self.id,
            parent: self.parent,
            title: self.title.clone(),
            icon: self.icon.clone(),
            cover: self.cover,
            cover_img: self.cover_img.clone(),
            cover_pos: self.cover_pos,
            kind: self.kind,
            blocks: vec![],
            columns: vec![],
            favorite: self.favorite,
            trashed: self.trashed,
            expanded: self.expanded,
            updated: self.updated,
            row: self.row,
            props: self.props.clone(),
            db: None,
            small_text: self.small_text,
            full_width: self.full_width,
            zoom: self.zoom,
            history: vec![],
            repeats: self.repeats.clone(),
            canvas: None,
            flow: None,
        }
    }

    /// Content size factor, always inside [`ZOOM_MIN`, `ZOOM_MAX`] (a damaged / hand-edited backup can never blow the layout up).
    pub fn content_zoom(&self) -> f32 {
        clamp_zoom(self.zoom)
    }

    pub fn has_cover(&self) -> bool {
        self.cover.is_some() || !self.cover_img.is_empty()
    }
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
    #[serde(default)]
    pub icons_v2: bool,
    /// PDF annotations (highlights, underlines, notes, drawings) per document key — see `pdf.rs`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pdf_annots: BTreeMap<u64, Vec<PdfAnnot>>,
    /// Customisable dashboard: boards of widgets (see `dashcfg.rs`).
    #[serde(default)]
    pub dash: crate::dashcfg::Dash,
    /// Automations: rules, reminders, notification inbox and run log (see `automation.rs`).
    #[serde(default)]
    pub auto: crate::automation::AutoState,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AnnotKind {
    Highlight,
    Underline,
    Strike,
    Note,
    Ink,
}

/// One annotation on a PDF page. All coordinates are fractions (0‥1) of the page width / height, so they survive any zoom level.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct PdfAnnot {
    pub id: u64,
    pub page: u32,
    pub kind: AnnotKind,
    /// Highlight / underline / strike: `[x0, y0, x1, y1]`; note: the anchor point (`x0, y0`).
    pub rect: [f32; 4],
    /// Freehand strokes: points of the stroke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pts: Vec<[f32; 2]>,
    pub color: u8,
    /// Comment attached to the annotation.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub created: f64,
    /// Text marks made on real text: one `[x0, y0, x1, y1]` rectangle (page fractions) per selected line. Empty for free marks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quads: Vec<[f32; 4]>,
    /// The text under a text mark (shown in the notes panel and exported with the Markdown notes).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
}

pub const ICONS: [&str; 36] = [
    "📄", "📝", "✅", "📚", "💡", "🎯", "🚀", "⭐", "🔥", "🌱", "🎨", "📅", "🏠", "💼", "📎", "❤", "☀", "☕", "⚡", "✈", "🔒", "🔔",
    "🎵", "🌍", "📊", "📁", "🎬", "🛠", "📌", "💬", "🏆", "🎓", "💰", "🔍", "📖", "📷",
];

pub const TEMPLATES: [(&str, &str, &str); 13] = [
    ("Meeting notes", "📝", "Agenda, notes and action items."),
    ("Weekly planner", "📅", "Plan every day of the week."),
    ("Project tracker", "🎯", "Database with table, board and calendar views."),
    ("Task list", "✅", "Tasks with due dates and tags."),
    ("Reading list", "📚", "Books with status, author and rating."),
    ("Journal entry", "📖", "Daily reflection prompts."),
    ("Team wiki", "🏠", "Docs home with contents and FAQ."),
    ("Product spec", "📋", "Problem, goals, requirements, launch plan."),
    ("Sales CRM", "💼", "Contacts + deals pipeline: stages, value, win probability, weighted forecast, relations."),
    ("Support tickets", "🎧", "Ticket board with priority, customer, assignee and due-date calendar."),
    ("Recruiting pipeline", "👥", "Candidates through screening, interview and offer, with rating, role and source."),
    ("Content calendar", "📣", "Ideas to published posts by channel, with board, table and calendar views."),
    ("OKRs & goals", "🏆", "Objectives with quarter, owner, status and progress — charted on the dashboard."),
];

fn blank_page(id: u64, parent: Option<u64>, kind: PageKind) -> Page {
    Page {
        id,
        parent,
        title: String::new(),
        icon: String::new(),
        cover: None,
        cover_img: String::new(),
        cover_pos: 0.5,
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
        zoom: 1.0,
        history: vec![],
        repeats: BTreeMap::new(),
        canvas: None,
        flow: None,
    }
}

impl Workspace {
    /// Converts legacy emoji page icons to Lucide glyphs (cheap no-op when nothing needs migrating).
    pub fn migrate_icons(&mut self) {
        // One-time conversion of the legacy emoji icons. Afterwards the user's own emoji choices are left alone.
        if self.icons_v2 {
            return;
        }
        self.icons_v2 = true;
        for p in self.pages.iter_mut() {
            if p.icon.chars().next().map_or(false, |c| (c as u32) >= 0x2000 && !(0xE000..=0xF8FF).contains(&(c as u32))) {
                let m = l::migrate(&p.icon);
                if m != p.icon {
                    p.icon = m;
                }
            }
        }
    }

    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn blk(&mut self, kind: BlockKind, text: &str, indent: u8) -> Block {
        { let id = self.id(); Block::new(id, kind, text, indent) }
    }

    /// Position of page `id` in `pages`. A thread-local id → index map makes this O(1) instead of a scan (every row loop of a
    /// database / dashboard used to be O(rows²)). The map is *validated* on every hit (`pages[i].id == id`) and rebuilt on a miss, so it
    /// is always correct no matter how `pages` was mutated.
    fn index_of(&self, id: u64) -> Option<usize> {
        thread_local! {
            static IDX: std::cell::RefCell<std::collections::HashMap<u64, usize>> = std::cell::RefCell::new(std::collections::HashMap::new());
        }
        if self.pages.len() < 24 {
            return self.pages.iter().position(|p| p.id == id);
        }
        IDX.with(|c| {
            let mut c = c.borrow_mut();
            if let Some(&i) = c.get(&id) {
                if self.pages.get(i).map_or(false, |p| p.id == id) {
                    return Some(i);
                }
            }
            c.clear();
            c.reserve(self.pages.len());
            for (i, p) in self.pages.iter().enumerate().rev() {
                c.insert(p.id, i);
            }
            c.get(&id).copied()
        })
    }

    pub fn page(&self, id: u64) -> Option<&Page> {
        self.index_of(id).map(|i| &self.pages[i])
    }

    pub fn page_mut(&mut self, id: u64) -> Option<&mut Page> {
        match self.index_of(id) {
            Some(i) => self.pages.get_mut(i),
            None => None,
        }
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
        if kind == PageKind::Canvas {
            return self.add_canvas(parent);
        }
        if kind == PageKind::Flow {
            return self.add_flow(parent);
        }
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

    /// New empty canvas page.
    pub fn add_canvas(&mut self, parent: Option<u64>) -> u64 {
        let id = self.id();
        let mut page = blank_page(id, parent, PageKind::Canvas);
        page.icon = crate::lucide::NETWORK.into();
        page.canvas = Some(crate::canvas::Canvas::new());
        self.push_page(page, parent);
        id
    }

    /// New empty flow (node graph) page.
    pub fn add_flow(&mut self, parent: Option<u64>) -> u64 {
        let id = self.id();
        let mut page = blank_page(id, parent, PageKind::Flow);
        page.icon = crate::lucide::ZAP.into();
        page.flow = Some(crate::flow::Flow::default());
        self.push_page(page, parent);
        id
    }

    /// Repairs flows loaded from disk (dangling wires, loops, flow pages without a graph).
    pub fn fix_flows(&mut self) {
        for p in self.pages.iter_mut().filter(|p| p.kind == PageKind::Flow) {
            match p.flow.as_mut() {
                Some(f) => f.repair(),
                None => p.flow = Some(crate::flow::Flow::default()),
            }
        }
    }

    /// Repairs canvases loaded from disk (damaged geometry, dangling connectors, canvas pages without a board).
    pub fn fix_canvases(&mut self) {
        for p in self.pages.iter_mut().filter(|p| p.kind == PageKind::Canvas) {
            match p.canvas.as_mut() {
                Some(c) => {
                    if c.nodes.iter().any(|n| !n.x.is_finite() || !n.y.is_finite() || !n.w.is_finite() || n.w < 1.0 || n.color >= crate::canvas::COLOR_COUNT) || c.edges.iter().any(|e| e.from == e.to) {
                        c.repair();
                    }
                }
                None => p.canvas = Some(crate::canvas::Canvas::new()),
            }
        }
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
            d.props.push(Prop { id: pid, name: name.into(), kind, opts, cfg: PropCfg::default() });
        }
        pid
    }

    pub fn add_view(&mut self, db: u64, kind: ViewKind) -> u64 {
        let vid = self.id();
        if let Some(d) = self.db_mut(db) {
            let same = d.views.iter().filter(|v| v.kind == kind).count();
            let name = if same == 0 { format!("{} view", kind.label()) } else { format!("{} view {}", kind.label(), same + 1) };
            d.views.push(View { id: vid, name, kind, group: 0, date: 0, dep: 0, zoom: 0, filters: vec![], sorts: vec![], widths: BTreeMap::new(), linked: 0 });
            d.active = d.views.len() - 1;
        }
        vid
    }

    /// Creates (or returns) the view that belongs to the linked-database block `block`; the database's own tabs are untouched.
    pub fn ensure_linked_view(&mut self, db: u64, block: u64, kind: ViewKind) -> Option<usize> {
        if let Some(i) = self.db(db)?.linked_index(block) {
            return Some(i);
        }
        let vid = self.id();
        let d = self.db_mut(db)?;
        d.views.push(View { id: vid, name: format!("{} view", kind.label()), kind, group: 0, date: 0, dep: 0, zoom: 0, filters: vec![], sorts: vec![], widths: BTreeMap::new(), linked: block });
        Some(d.views.len() - 1)
    }

    /// Drops the hidden views of linked blocks that no longer exist on any page.
    pub fn prune_linked_views(&mut self) {
        let live: std::collections::HashSet<u64> = self.pages.iter().filter(|p| !p.trashed).flat_map(|p| p.blocks.iter()).filter(|b| b.kind == BlockKind::DbView).map(|b| b.id).collect();
        for p in self.pages.iter_mut() {
            if let Some(d) = p.db.as_mut() {
                if d.views.iter().any(|v| v.linked != 0 && !live.contains(&v.linked)) {
                    d.views.retain(|v| v.linked == 0 || live.contains(&v.linked));
                    d.active = d.active_tab();
                }
            }
        }
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
        use std::hash::{Hash, Hasher};
        thread_local! {
            static QC: std::cell::RefCell<std::collections::HashMap<(u64, usize), (u64, Vec<u64>)>> = std::cell::RefCell::new(std::collections::HashMap::new());
        }
        let Some(d) = self.db(db) else { return vec![] };
        let Some(view) = d.views.get(vi) else { return self.query_uncached(db, vi) };
        // Formulas / rollups read other rows: never cache what depends on them.
        let derived = |p: u64| matches!(d.kind_of(p), PropKind::Formula | PropKind::Rollup | PropKind::Relation);
        if view.filters.iter().any(|f| derived(f.prop)) || view.sorts.iter().any(|s| derived(s.prop)) {
            return self.query_uncached(db, vi);
        }
        // fingerprint of everything the result depends on: the view, the property definitions, today's date and every row's title + values
        let mut h = std::collections::hash_map::DefaultHasher::new();
        crate::dashdata::today().hash(&mut h);
        format!("{:?}", view.filters).hash(&mut h);
        for s in &view.sorts {
            (s.prop, s.asc).hash(&mut h);
        }
        serde_json::to_string(&d.props).unwrap_or_default().hash(&mut h);
        for p in self.pages.iter().filter(|p| p.row && p.parent == Some(db) && !p.trashed) {
            p.id.hash(&mut h);
            p.title.hash(&mut h);
            for (k, v) in &p.props {
                k.hash(&mut h);
                match v {
                    Val::Text(s) => (0u8, s).hash(&mut h),
                    Val::Num(n) => (1u8, n.to_bits()).hash(&mut h),
                    Val::Sel(o) => (2u8, o).hash(&mut h),
                    Val::Multi(m) => (3u8, m).hash(&mut h),
                    Val::Check(b) => (4u8, b).hash(&mut h),
                }
            }
        }
        let fp = h.finish();
        if let Some(v) = QC.with(|c| c.borrow().get(&(db, vi)).filter(|(f, _)| *f == fp).map(|(_, v)| v.clone())) {
            return v;
        }
        let rows = self.query_uncached(db, vi);
        QC.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 64 {
                c.clear();
            }
            c.insert((db, vi), (fp, rows.clone()));
        });
        rows
    }

    fn query_uncached(&self, db: u64, vi: usize) -> Vec<u64> {
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
        copy.history.clear();
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
            8 => {
                // A CRM hub: a Contacts database and a Deals pipeline that points at it through a relation.
                let hub = self.add_page(parent, PageKind::Doc);
                let contacts = self.add_database(Some(hub), ViewKind::Table);
                self.clear_rows(contacts);
                let stage = self.db(contacts).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let last = self.db(contacts).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(contacts, stage, "Stage", &["Lead", "Contacted", "Qualified", "Customer", "Churned"]);
                self.rename_prop(contacts, last, "Last contact");
                let company = self.add_prop(contacts, "Company", PropKind::Text, &[]);
                let email = self.add_prop(contacts, "Email", PropKind::Text, &[]);
                let phone = self.add_prop(contacts, "Phone", PropKind::Text, &[]);
                let owner = self.add_prop(contacts, "Owner", PropKind::Text, &[]);
                self.add_prop(contacts, "Profile", PropKind::Url, &[]);
                self.add_view(contacts, ViewKind::Board);
                if let Some(d) = self.db_mut(contacts) {
                    d.active = 0;
                }
                let mut ids = vec![];
                for (n, c, e, ph, st, ago) in [
                    ("Ada Lovelace", "Analytical Engines", "ada@analytical.io", "+44 20 7946 0001", "Customer", -3),
                    ("Grace Hopper", "Navy Labs", "grace@navylabs.com", "+1 202 555 0112", "Qualified", -1),
                    ("Linus T.", "Kernel Co", "linus@kernel.co", "+358 9 555 0100", "Contacted", -9),
                    ("Margaret H.", "Apollo Software", "mh@apollo.dev", "+1 617 555 0145", "Lead", -14),
                    ("Alan Turing", "Bletchley Ltd", "alan@bletchley.uk", "+44 1908 555 010", "Customer", -6),
                ] {
                    let sid = self.opt_id(contacts, stage, st).unwrap_or(0);
                    ids.push(self.titled_row(contacts, n, vec![(stage, Val::Sel(sid)), (last, Self::day_val(ago)), (company, Val::Text(c.into())), (email, Val::Text(e.into())), (phone, Val::Text(ph.into())), (owner, Val::Text("Me".into()))]));
                }
                let deals = self.add_database(Some(hub), ViewKind::Board);
                self.clear_rows(deals);
                let dstage = self.db(deals).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let close = self.db(deals).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(deals, dstage, "Stage", &["Prospecting", "Proposal", "Negotiation", "Won", "Lost"]);
                self.rename_prop(deals, close, "Close date");
                let value = self.add_prop(deals, "Value", PropKind::Number, &[]);
                let prob = self.add_prop(deals, "Probability", PropKind::Number, &[]);
                let weighted = self.add_prop(deals, "Weighted", PropKind::Formula, &[]);
                let contact = self.add_prop(deals, "Contact", PropKind::Relation, &[]);
                self.add_prop(deals, "Owner", PropKind::Text, &[]);
                if let Some(d) = self.db_mut(deals) {
                    for p in d.props.iter_mut() {
                        if p.id == weighted {
                            p.cfg.formula = "prop(\"Value\") * prop(\"Probability\") / 100".into();
                        } else if p.id == contact {
                            p.cfg.relation = contacts;
                        }
                    }
                }
                self.add_view(deals, ViewKind::Table);
                self.add_view(deals, ViewKind::Calendar);
                if let Some(d) = self.db_mut(deals) {
                    d.active = 0;
                }
                for (n, st, v, p, c, days) in [
                    ("Analytical Engines — annual plan", "Won", 48000.0, 100.0, 0, -4),
                    ("Navy Labs — pilot", "Negotiation", 25000.0, 70.0, 1, 9),
                    ("Kernel Co — support contract", "Proposal", 12000.0, 40.0, 2, 21),
                    ("Apollo Software — onboarding", "Prospecting", 8000.0, 20.0, 3, 35),
                    ("Bletchley — expansion", "Negotiation", 31000.0, 60.0, 4, 14),
                    ("Legacy migration", "Lost", 9000.0, 0.0, 2, -11),
                ] {
                    let sid = self.opt_id(deals, dstage, st).unwrap_or(0);
                    self.titled_row(deals, n, vec![(dstage, Val::Sel(sid)), (close, Self::day_val(days)), (value, Val::Num(v)), (prob, Val::Num(p)), (contact, Val::Multi(vec![ids[c]]))]);
                }
                let (b1, b2, b3) = (self.blk(BlockKind::Callout, "Your sales CRM. Move deals across the pipeline board; the Weighted column forecasts revenue (value × probability).", 0), self.blk(BlockKind::Text, "Contacts live in the Contacts database. Each deal links to its contact through the Contact relation.", 0), self.blk(BlockKind::Todo, "Add your first real contact and deal", 0));
                if let Some(pg) = self.page_mut(hub) {
                    pg.blocks = vec![b1, b2, b3];
                    pg.expanded = true;
                }
                self.finish_template(hub, name, icon);
                hub
            }
            9 => {
                let db = self.add_database(parent, ViewKind::Board);
                self.clear_rows(db);
                let st = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let due = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(db, st, "Status", &["New", "Open", "Waiting", "Resolved"]);
                self.rename_prop(db, due, "Due");
                let prio = self.add_prop(db, "Priority", PropKind::Select, &["Low", "Normal", "High", "Urgent"]);
                let cust = self.add_prop(db, "Customer", PropKind::Text, &[]);
                let who = self.add_prop(db, "Assignee", PropKind::Text, &[]);
                self.add_view(db, ViewKind::Table);
                self.add_view(db, ViewKind::Calendar);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                for (t, s, p, c, n) in [
                    ("Cannot export report to PDF", "Open", "High", "Navy Labs", 1),
                    ("SSO login loop", "New", "Urgent", "Kernel Co", 0),
                    ("Invoice address change", "Waiting", "Low", "Bletchley Ltd", 6),
                    ("Dark mode request", "New", "Normal", "Apollo Software", 12),
                    ("Password reset email delayed", "Resolved", "Normal", "Analytical Engines", -2),
                ] {
                    let (sid, pid) = (self.opt_id(db, st, s).unwrap_or(0), self.opt_id(db, prio, p).unwrap_or(0));
                    self.titled_row(db, t, vec![(st, Val::Sel(sid)), (prio, Val::Sel(pid)), (cust, Val::Text(c.into())), (who, Val::Text("Me".into())), (due, Self::day_val(n))]);
                }
                self.finish_template(db, name, icon);
                db
            }
            10 => {
                let db = self.add_database(parent, ViewKind::Board);
                self.clear_rows(db);
                let st = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let when = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(db, st, "Stage", &["Applied", "Screening", "Interview", "Offer", "Hired", "Rejected"]);
                self.rename_prop(db, when, "Next step");
                let role = self.add_prop(db, "Role", PropKind::Text, &[]);
                let src = self.add_prop(db, "Source", PropKind::Select, &["Referral", "LinkedIn", "Website", "Agency"]);
                let rate = self.add_prop(db, "Rating", PropKind::Number, &[]);
                let mail = self.add_prop(db, "Email", PropKind::Text, &[]);
                self.add_view(db, ViewKind::Table);
                self.add_view(db, ViewKind::Calendar);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                for (n, r, s, so, rt, days) in [
                    ("Priya Nair", "Backend engineer", "Interview", "Referral", 4.0, 3),
                    ("Tomás Silva", "Product designer", "Screening", "LinkedIn", 3.0, 1),
                    ("Mei Chen", "Backend engineer", "Offer", "Website", 5.0, 6),
                    ("Sam Okafor", "Data analyst", "Applied", "Agency", 0.0, 8),
                    ("Lena Fischer", "Product designer", "Hired", "Referral", 5.0, -5),
                ] {
                    let (sid, soid) = (self.opt_id(db, st, s).unwrap_or(0), self.opt_id(db, src, so).unwrap_or(0));
                    self.titled_row(db, n, vec![(st, Val::Sel(sid)), (src, Val::Sel(soid)), (role, Val::Text(r.into())), (rate, Val::Num(rt)), (when, Self::day_val(days)), (mail, Val::Text(String::new()))]);
                }
                self.finish_template(db, name, icon);
                db
            }
            11 => {
                let db = self.add_database(parent, ViewKind::Board);
                self.clear_rows(db);
                let st = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let when = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(db, st, "Status", &["Idea", "Drafting", "Review", "Scheduled", "Published"]);
                self.rename_prop(db, when, "Publish date");
                let ch = self.add_prop(db, "Channel", PropKind::Select, &["Blog", "Newsletter", "YouTube", "Social"]);
                let who = self.add_prop(db, "Author", PropKind::Text, &[]);
                self.add_prop(db, "Link", PropKind::Url, &[]);
                self.add_view(db, ViewKind::Calendar);
                self.add_view(db, ViewKind::Table);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                for (t, s, c, days) in [
                    ("Launch announcement", "Scheduled", "Blog", 2),
                    ("Customer story: Navy Labs", "Drafting", "Newsletter", 9),
                    ("Product walkthrough video", "Idea", "YouTube", 20),
                    ("Weekly tips thread", "Review", "Social", 4),
                    ("Release notes", "Published", "Blog", -3),
                ] {
                    let (sid, cid) = (self.opt_id(db, st, s).unwrap_or(0), self.opt_id(db, ch, c).unwrap_or(0));
                    self.titled_row(db, t, vec![(st, Val::Sel(sid)), (ch, Val::Sel(cid)), (who, Val::Text("Me".into())), (when, Self::day_val(days))]);
                }
                self.finish_template(db, name, icon);
                db
            }
            12 => {
                let db = self.add_database(parent, ViewKind::Table);
                self.clear_rows(db);
                let st = self.db(db).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
                let due = self.db(db).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
                self.retag(db, st, "Status", &["On track", "At risk", "Off track", "Done"]);
                self.rename_prop(db, due, "Due");
                let q = self.add_prop(db, "Quarter", PropKind::Select, &["Q1", "Q2", "Q3", "Q4"]);
                let prog = self.add_prop(db, "Progress %", PropKind::Number, &[]);
                let own = self.add_prop(db, "Owner", PropKind::Text, &[]);
                self.add_view(db, ViewKind::Board);
                if let Some(d) = self.db_mut(db) {
                    d.active = 0;
                }
                for (t, s, qq, p, days) in [
                    ("Reach 100 paying customers", "On track", "Q2", 64.0, 60),
                    ("Cut support response time to < 2h", "At risk", "Q2", 35.0, 45),
                    ("Ship mobile app", "Off track", "Q3", 12.0, 120),
                    ("Publish 12 case studies", "Done", "Q1", 100.0, -10),
                ] {
                    let (sid, qid) = (self.opt_id(db, st, s).unwrap_or(0), self.opt_id(db, q, qq).unwrap_or(0));
                    self.titled_row(db, t, vec![(st, Val::Sel(sid)), (q, Val::Sel(qid)), (prog, Val::Num(p)), (own, Val::Text("Me".into())), (due, Self::day_val(days))]);
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

    /// Renames a property and replaces its select options.
    fn retag(&mut self, db: u64, prop: u64, name: &str, opts: &[&str]) {
        let ids: Vec<u64> = opts.iter().map(|_| self.id()).collect();
        if let Some(p) = self.db_mut(db).and_then(|d| d.props.iter_mut().find(|p| p.id == prop)) {
            p.name = name.into();
            p.opts = opts.iter().zip(ids).enumerate().map(|(i, (n, id))| Opt { id, name: (*n).into(), color: i % 9 }).collect();
        }
    }

    fn rename_prop(&mut self, db: u64, prop: u64, name: &str) {
        if let Some(p) = self.db_mut(db).and_then(|d| d.props.iter_mut().find(|p| p.id == prop)) {
            p.name = name.into();
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
        let mut ws = Workspace { name: "My Workspace".into(), pages: vec![], next_id: 0, current: None, dark: false, sidebar_open: true, accent: 0, icons_v2: false, pdf_annots: BTreeMap::new(), dash: Default::default(), auto: Default::default() };
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

        ws.seed_projects();
        // a ready-made canvas that links to two of the pages above
        let cid = ws.add_canvas(None);
        let link_a = ws.pages.iter().find(|p| p.title == "Roadmap").map(|p| p.id);
        if let Some(p) = ws.page_mut(cid) {
            p.title = "Launch board".into();
            p.canvas = Some(crate::canvas::Canvas::sample(link_a, Some(start)));
        }
        ws.current = Some(start);
        ws
    }

    /// Demo databases showing date-range timelines with dependencies, formulas, relations and rollups.
    fn seed_projects(&mut self) {
        let plan = self.add_database(None, ViewKind::Timeline);
        self.clear_rows(plan);
        let status = self.db(plan).and_then(|d| d.first_of(PropKind::Select)).unwrap_or(0);
        let date = self.db(plan).and_then(|d| d.first_of(PropKind::Date)).unwrap_or(0);
        let done = self.add_prop(plan, "Done", PropKind::Checkbox, &[]);
        let dep = self.add_prop(plan, "Blocked by", PropKind::Relation, &[]);
        let days = self.add_prop(plan, "Days", PropKind::Formula, &[]);
        if let Some(d) = self.db_mut(plan) {
            for p in d.props.iter_mut() {
                if p.id == date {
                    p.name = "Schedule".into();
                } else if p.id == dep {
                    p.cfg.relation = plan;
                } else if p.id == days {
                    p.cfg.formula = "dateBetween(end(prop(\"Schedule\")), start(prop(\"Schedule\")), \"days\") + 1".into();
                }
            }
        }
        self.add_view(plan, ViewKind::Table);
        if let Some(d) = self.db_mut(plan) {
            d.active = 0;
        }
        let items = [
            ("Research & discovery", "Done", -8, -3, true),
            ("Design system", "In progress", -2, 4, false),
            ("Build prototype", "In progress", 5, 12, false),
            ("User testing", "Not started", 10, 15, false),
            ("Launch", "Not started", 17, 18, false),
        ];
        let mut ids = vec![];
        for (t, s, a, b, d) in items {
            let sid = self.opt_id(plan, status, s).unwrap_or(0);
            let (da, db) = (dates::today_day() + a, dates::today_day() + b);
            ids.push(self.titled_row(plan, t, vec![(status, Val::Sel(sid)), (date, Val::Text(dates::span_value(da, db))), (done, Val::Check(d))]));
        }
        for (i, pred) in [(1usize, 0usize), (2, 1), (3, 2), (4, 3)] {
            if let Some(r) = self.page_mut(ids[i]) {
                r.props.insert(dep, Val::Multi(vec![ids[pred]]));
            }
        }
        if let Some(p) = self.page_mut(plan) {
            p.title = "Project plan".into();
            p.icon = "📅".into();
        }

        let ms = self.add_database(None, ViewKind::Table);
        self.clear_rows(ms);
        let tasks = self.add_prop(ms, "Tasks", PropKind::Relation, &[]);
        let count = self.add_prop(ms, "Task count", PropKind::Rollup, &[]);
        let prog = self.add_prop(ms, "Progress %", PropKind::Rollup, &[]);
        let ends = self.add_prop(ms, "Ends", PropKind::Rollup, &[]);
        if let Some(d) = self.db_mut(ms) {
            for p in d.props.iter_mut() {
                if p.id == tasks {
                    p.cfg.relation = plan;
                } else if p.id == count {
                    p.cfg = PropCfg { rollup_rel: tasks, rollup_prop: 0, rollup_fn: RollupFn::Count, ..Default::default() };
                } else if p.id == prog {
                    p.cfg = PropCfg { rollup_rel: tasks, rollup_prop: done, rollup_fn: RollupFn::PercentChecked, ..Default::default() };
                } else if p.id == ends {
                    p.cfg = PropCfg { rollup_rel: tasks, rollup_prop: date, rollup_fn: RollupFn::Latest, ..Default::default() };
                }
            }
            d.props.retain(|p| p.name != "Date" && p.name != "Tags");
        }
        for (t, set) in [("Alpha", vec![0usize, 1]), ("Beta", vec![2, 3]), ("Release", vec![4])] {
            let r = self.titled_row(ms, t, vec![(tasks, Val::Multi(set.iter().map(|i| ids[*i]).collect()))]);
            let _ = r;
        }
        if let Some(p) = self.page_mut(ms) {
            p.title = "Milestones".into();
            p.icon = "🏆".into();
        }
        crate::formula::recompute(self);
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
    let val = row_val(row, f.prop);
    let kind = match (d.kind_of(f.prop), &val) {
        (PropKind::Formula | PropKind::Rollup, Some(Val::Num(_))) => PropKind::Number,
        (PropKind::Formula | PropKind::Rollup, _) => PropKind::Text,
        (k, _) => k,
    };
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
        FilterOp::Today | FilterOp::ThisWeek | FilterOp::ThisMonth | FilterOp::Next7 | FilterOp::Past7 | FilterOp::Overdue => {
            let (Some(Val::Text(s)), Some((lo, hi))) = (&val, f.op.window(dates::today_day())) else { return false };
            // a record matches when its date (or date range) touches the window
            dates::span(s).map_or(false, |(a, b)| a <= hi && b >= lo)
        }
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
