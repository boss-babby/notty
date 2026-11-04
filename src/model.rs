//! Workspace data model: pages, blocks and boards. Everything is serde-serializable.

use serde::{Deserialize, Serialize};

use crate::auth::now_ms;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum BlockKind {
    Text,
    H1,
    H2,
    H3,
    Todo,
    Bullet,
    Numbered,
    Quote,
    Callout,
    Code,
    Divider,
}

impl BlockKind {
    pub const ALL: [BlockKind; 11] = [
        BlockKind::Text,
        BlockKind::H1,
        BlockKind::H2,
        BlockKind::H3,
        BlockKind::Todo,
        BlockKind::Bullet,
        BlockKind::Numbered,
        BlockKind::Quote,
        BlockKind::Callout,
        BlockKind::Code,
        BlockKind::Divider,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlockKind::Text => "Text",
            BlockKind::H1 => "Heading 1",
            BlockKind::H2 => "Heading 2",
            BlockKind::H3 => "Heading 3",
            BlockKind::Todo => "To-do list",
            BlockKind::Bullet => "Bulleted list",
            BlockKind::Numbered => "Numbered list",
            BlockKind::Quote => "Quote",
            BlockKind::Callout => "Callout",
            BlockKind::Code => "Code",
            BlockKind::Divider => "Divider",
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
            BlockKind::Quote => "Capture a quote.",
            BlockKind::Callout => "Make writing stand out.",
            BlockKind::Code => "Capture a code snippet.",
            BlockKind::Divider => "Visually divide blocks.",
        }
    }

    /// Short glyph shown in menus.
    pub fn glyph(self) -> &'static str {
        match self {
            BlockKind::Text => "T",
            BlockKind::H1 => "H1",
            BlockKind::H2 => "H2",
            BlockKind::H3 => "H3",
            BlockKind::Todo => "☑",
            BlockKind::Bullet => "•",
            BlockKind::Numbered => "1.",
            BlockKind::Quote => "❝",
            BlockKind::Callout => "💡",
            BlockKind::Code => "</>",
            BlockKind::Divider => "—",
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
            BlockKind::Quote => "quote blockquote cite",
            BlockKind::Callout => "callout note info tip",
            BlockKind::Code => "code snippet pre",
            BlockKind::Divider => "divider line hr separator",
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    Doc,
    Board,
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
    pub columns: Vec<Column>,
    pub favorite: bool,
    pub trashed: bool,
    pub expanded: bool,
    pub updated: f64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub pages: Vec<Page>,
    pub next_id: u64,
    pub current: Option<u64>,
    pub dark: bool,
    pub sidebar_open: bool,
}

pub const ICONS: [&str; 24] = [
    "📄", "📝", "✅", "📚", "💡", "🎯", "🚀", "⭐", "🔥", "🌱", "🎨", "📅", "🏠", "💼", "🧠", "❤", "☀", "☕", "⚡", "✈", "🔒", "🔔", "🎵", "🌍",
];

impl Workspace {
    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn page(&self, id: u64) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn page_mut(&mut self, id: u64) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub fn children(&self, parent: Option<u64>) -> Vec<u64> {
        self.pages.iter().filter(|p| p.parent == parent && !p.trashed).map(|p| p.id).collect()
    }

    pub fn has_children(&self, id: u64) -> bool {
        self.pages.iter().any(|p| p.parent == Some(id) && !p.trashed)
    }

    pub fn add_page(&mut self, parent: Option<u64>, kind: PageKind) -> u64 {
        let id = self.id();
        let mut page = Page {
            id,
            parent,
            title: String::new(),
            icon: if kind == PageKind::Board { "🎯".into() } else { "📄".into() },
            cover: None,
            kind,
            blocks: vec![],
            columns: vec![],
            favorite: false,
            trashed: false,
            expanded: false,
            updated: now_ms(),
        };
        if kind == PageKind::Doc {
            let bid = self.id();
            page.blocks.push(Block { id: bid, kind: BlockKind::Text, text: String::new(), checked: false });
        } else {
            for (i, t) in ["Not started", "In progress", "Done"].iter().enumerate() {
                let cid = self.id();
                page.columns.push(Column { id: cid, title: (*t).into(), color: [0, 3, 2][i], cards: vec![] });
            }
        }
        if let Some(p) = parent {
            if let Some(pp) = self.page_mut(p) {
                pp.expanded = true;
            }
        }
        self.pages.push(page);
        self.current = Some(id);
        id
    }

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

    pub fn set_trashed(&mut self, id: u64, trashed: bool) {
        let mut ids = self.descendants(id);
        ids.push(id);
        for p in self.pages.iter_mut().filter(|p| ids.contains(&p.id)) {
            p.trashed = trashed;
        }
        if trashed {
            if let Some(cur) = self.current {
                if ids.contains(&cur) {
                    self.current = self.pages.iter().find(|p| !p.trashed).map(|p| p.id);
                }
            }
        } else if let Some(parent) = self.page(id).and_then(|p| p.parent) {
            // make sure restored page has a live parent
            if self.page(parent).map(|p| p.trashed).unwrap_or(true) {
                if let Some(p) = self.page_mut(id) {
                    p.parent = None;
                }
            }
        }
    }

    pub fn delete_forever(&mut self, id: u64) {
        let mut ids = self.descendants(id);
        ids.push(id);
        self.pages.retain(|p| !ids.contains(&p.id));
        if let Some(cur) = self.current {
            if ids.contains(&cur) {
                self.current = self.pages.iter().find(|p| !p.trashed).map(|p| p.id);
            }
        }
    }

    pub fn duplicate(&mut self, id: u64) -> Option<u64> {
        let src = self.page(id)?.clone();
        let nid = self.id();
        let mut copy = src;
        copy.id = nid;
        copy.title = if copy.title.is_empty() { "Untitled copy".into() } else { format!("{} (copy)", copy.title) };
        copy.favorite = false;
        copy.updated = now_ms();
        let mut ids = vec![];
        for _ in 0..copy.blocks.len() {
            ids.push(self.id());
        }
        for (b, nid) in copy.blocks.iter_mut().zip(ids) {
            b.id = nid;
        }
        let mut cols = vec![];
        for c in copy.columns.iter() {
            let cid = self.id();
            let cards: Vec<u64> = c.cards.iter().map(|_| self.id()).collect();
            cols.push((cid, cards));
        }
        for (c, (cid, cards)) in copy.columns.iter_mut().zip(cols) {
            c.id = cid;
            for (card, nid) in c.cards.iter_mut().zip(cards) {
                card.id = nid;
            }
        }
        self.pages.push(copy);
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

    pub fn seed() -> Self {
        let mut ws = Workspace {
            name: "My Workspace".into(),
            pages: vec![],
            next_id: 0,
            current: None,
            dark: false,
            sidebar_open: true,
        };

        let start = ws.add_page(None, PageKind::Doc);
        let b = |ws: &mut Workspace, kind: BlockKind, text: &str, checked: bool| Block {
            id: ws.id(),
            kind,
            text: text.into(),
            checked,
        };
        let blocks = vec![
            b(&mut ws, BlockKind::Callout, "Welcome to Lumen — a Notion-style workspace written 100% in Rust with egui. No web engine, no JavaScript UI. It runs natively and in your browser through WebAssembly.", false),
            b(&mut ws, BlockKind::H2, "Write with blocks", false),
            b(&mut ws, BlockKind::Text, "Press Enter to create a new block, Backspace at the start to merge or reset it, and type / to open the command menu.", false),
            b(&mut ws, BlockKind::Bullet, "Markdown shortcuts: type '# ', '## ', '- ', '1. ', '[] ', '> ' or '---' at the start of a line", false),
            b(&mut ws, BlockKind::Bullet, "Drag the grip handle to reorder blocks, or click it for more actions", false),
            b(&mut ws, BlockKind::Bullet, "Press Ctrl/Cmd + K to jump anywhere with the command palette", false),
            b(&mut ws, BlockKind::H2, "Get things done", false),
            b(&mut ws, BlockKind::Todo, "Create your first page from the sidebar", true),
            b(&mut ws, BlockKind::Todo, "Try the Roadmap board", false),
            b(&mut ws, BlockKind::Todo, "Lock your workspace with a password (workspace menu)", false),
            b(&mut ws, BlockKind::Divider, "", false),
            b(&mut ws, BlockKind::Quote, "Simplicity is the ultimate sophistication.", false),
            b(&mut ws, BlockKind::Code, "fn main() {\n    println!(\"Hello from Lumen 🦀\");\n}", false),
            b(&mut ws, BlockKind::Text, "", false),
        ];
        if let Some(p) = ws.page_mut(start) {
            p.title = "Getting Started".into();
            p.icon = "🚀".into();
            p.cover = Some(1);
            p.blocks = blocks;
            p.favorite = true;
            p.expanded = true;
        }

        let notes = ws.add_page(Some(start), PageKind::Doc);
        let nb = vec![
            b(&mut ws, BlockKind::H1, "Weekly sync", false),
            b(&mut ws, BlockKind::Numbered, "Review last week's goals", false),
            b(&mut ws, BlockKind::Numbered, "Plan the next release", false),
            b(&mut ws, BlockKind::Numbered, "Assign owners", false),
            b(&mut ws, BlockKind::Text, "", false),
        ];
        if let Some(p) = ws.page_mut(notes) {
            p.title = "Meeting Notes".into();
            p.icon = "📝".into();
            p.blocks = nb;
        }

        let road = ws.add_page(None, PageKind::Board);
        let cards = [
            (0, "Design onboarding flow"),
            (0, "Write launch blog post"),
            (1, "Build command palette"),
            (1, "Password lock screen"),
            (2, "Block editor"),
            (2, "WebAssembly build"),
        ];
        let mut ids = vec![];
        for _ in cards.iter() {
            ids.push(ws.id());
        }
        if let Some(p) = ws.page_mut(road) {
            p.title = "Roadmap".into();
            p.icon = "🎯".into();
            p.cover = Some(4);
            for ((col, text), id) in cards.iter().zip(ids) {
                p.columns[*col].cards.push(Card { id, text: (*text).into() });
            }
        }

        let journal = ws.add_page(None, PageKind::Doc);
        let jb = vec![
            b(&mut ws, BlockKind::H1, "Reading list", false),
            b(&mut ws, BlockKind::Todo, "The Rust Programming Language", true),
            b(&mut ws, BlockKind::Todo, "Programming Rust", false),
            b(&mut ws, BlockKind::Todo, "Designing Data-Intensive Applications", false),
        ];
        if let Some(p) = ws.page_mut(journal) {
            p.title = "Reading".into();
            p.icon = "📚".into();
            p.blocks = jb;
        }

        ws.current = Some(start);
        ws
    }
}
