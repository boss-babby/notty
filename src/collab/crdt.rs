//! Replicated workspace state: a hybrid-logical-clock **LWW-element map** with fractional ordering.
//!
//! Why this shape (see COLLAB.md):
//!  * Every page, block, canvas node / edge and PDF annotation is an *entity* with a stable id → concurrent edits of **different**
//!    entities always merge; the same entity resolves last-writer-wins by `(HLC, content hash)`, which is deterministic on every peer.
//!  * Order (block order, z-order of canvas cards, sidebar order) is a per-entity fractional position (`pos`), so inserting / moving
//!    one block is *one* small operation and two concurrent inserts never overwrite each other.
//!  * The replica keeps only `(hlc, pos, hash)` per entity (~48 bytes), never a second copy of the content: memory stays flat on
//!    large workspaces and values are serialised from the workspace only when they must be sent.
//!  * Local changes are found by hashing — only the open page every tick, the rest round-robin inside a time budget.
//!
//! The text *inside* one block is still last-writer-wins; the next step is a `yrs` (Y.Text) register per block — the entity model
//! was chosen so that this is a drop-in replacement of the value, not a redesign.
use crate::canvas::{Edge, Node};
use crate::model::{Block, Comment, Page, PdfAnnot, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug, Default)]
pub struct Hlc {
    pub t: u64,
    pub r: u32,
}

pub struct Clock {
    last: u64,
    pub replica: u32,
}

impl Clock {
    pub fn new(replica: u32) -> Self {
        Clock { last: 0, replica }
    }
    pub fn tick(&mut self, wall_ms: u64) -> Hlc {
        self.last = (self.last + 1).max(wall_ms << 16);
        Hlc { t: self.last, r: self.replica }
    }
    /// Never lets a peer with a wrong clock push ours more than a day ahead.
    pub fn observe(&mut self, h: Hlc, wall_ms: u64) {
        if h.t <= (wall_ms + 86_400_000) << 16 {
            self.last = self.last.max(h.t);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
pub enum Key {
    Page(u64),
    Block(u64, u64),
    Node(u64, u64),
    Edge(u64, u64),
    Annot(u64, u64),
    /// (page, comment id)
    Comment(u64, u64),
}

impl Key {
    /// Owning container (pages own blocks / nodes / edges, documents own annotations).
    fn group(&self) -> Option<(u8, u64)> {
        match *self {
            Key::Page(_) => None,
            Key::Block(p, _) | Key::Node(p, _) | Key::Edge(p, _) => Some((0, p)),
            Key::Annot(d, _) => Some((1, d)),
            Key::Comment(p, _) => Some((2, p)),
        }
    }
    fn rank(&self) -> u8 {
        match self {
            Key::Page(_) => 0,
            Key::Annot(..) | Key::Comment(..) => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Entry {
    pub hlc: Hlc,
    pub pos: f64,
    /// JSON of the entity; `None` = deleted (tombstone).
    pub val: Option<String>,
    /// Content hash of the value this one was derived from (0 = unknown). Lets a receiver tell a concurrent edit from a plain update
    /// and merge the two (see `textmerge.rs`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub base: u64,
}

fn is_zero(x: &u64) -> bool {
    *x == 0
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Op {
    pub key: Key,
    pub e: Entry,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    hlc: Hlc,
    pos: f64,
    hash: u64,
    /// Hash of the content alone (without the position) — what `Entry::base` refers to.
    chash: u64,
    dead: bool,
}

fn chash_of(val: &Option<String>) -> u64 {
    val.as_ref().map_or(0, |s| crate::media::fnv(s.as_bytes()))
}

/// Previous versions of a block kept as merge bases (open page + blocks that received remote edits only).
const HIST_KEYS: usize = 4096;
const HIST_DEPTH: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ScopeKind {
    Workspace,
    /// One page, without its sub-pages.
    Page,
    /// A page including every nested page (and database rows).
    Tree,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub root: u64,
}

impl Scope {
    pub const ALL: Scope = Scope { kind: ScopeKind::Workspace, root: 0 };
}

#[derive(Default)]
pub struct Applied {
    pub pages: HashSet<u64>,
    pub reorder_pages: bool,
    pub n: usize,
    /// Record what changed (before / after text) for the review window.
    pub track: bool,
    pub log: Vec<super::changes::Change>,
    /// Remote changes to things that are not itemised (canvas cards, edges, annotations).
    pub other: usize,
}

/// Largest accepted value of one entity (a block, a page header, a canvas card …).
const MAX_VALUE: usize = 4 << 20;
/// Entities one replica will hold (a hostile editor cannot grow the host without bound).
const MAX_ENTITIES: usize = 3_000_000;

/// Would giving `pid` the parent `parent` make the page tree loop?
fn would_cycle(ws: &Workspace, pid: u64, parent: Option<u64>) -> bool {
    let mut cur = parent;
    let mut steps = 0usize;
    while let Some(c) = cur {
        if c == pid {
            return true;
        }
        steps += 1;
        if steps > ws.pages.len() + 1 {
            return true;
        }
        cur = ws.page(c).and_then(|p| p.parent);
    }
    false
}

fn hash_of(val: &Option<String>, pos: f64) -> u64 {
    let mut h = match val {
        Some(s) => crate::media::fnv(s.as_bytes()),
        None => 0x9e37_79b9,
    };
    h ^= pos.to_bits().rotate_left(17);
    h.wrapping_mul(0x100000001b3)
}

/// Upper bound of persisted stamps (a pathological workspace must not make the persisted string grow without limit).
const MAX_STAMPS: usize = 400_000;

pub struct Replica {
    pub clock: Clock,
    pub scope: Scope,
    roots: HashSet<u64>,
    cells: HashMap<Key, Cell>,
    groups: HashMap<(u8, u64), HashSet<Key>>,
    /// Pages known to be in scope (host: kept in sync with the workspace each tick).
    in_scope: HashSet<u64>,
    docs: HashSet<u64>,
    seen_updated: HashMap<u64, f64>,
    rr: usize,
    quiet: bool,
    pub last_cost_ms: f64,
    /// Recent block values by content hash: the bases of three-way merges.
    hist: HashMap<Key, std::collections::VecDeque<(u64, String)>>,
    /// Page the user is looking at (its blocks are remembered as merge bases).
    cur_page: Option<u64>,
    /// Ping-pong limiter of the merge: (window start ms, merges in the window) per entity.
    merges: HashMap<Key, (u64, u32)>,
    /// Number of concurrent edits that were merged instead of overwritten (statistics / tests).
    pub merged: u64,
    /// Signature of the ordered page list of the last full tick: while it is unchanged the O(pages) bookkeeping (disappeared pages,
    /// sidebar positions) is skipped — this is what keeps a 100 000-page workspace from stalling every scan.
    ids_sig: Option<u64>,
    ppos: Vec<f64>,
    pidx: HashMap<u64, usize>,
    /// Fingerprints of annotation / comment lists: an unchanged list is not serialised again.
    fps: HashMap<(u8, u64), u64>,
    /// Wall-clock stamp (ms) given to entities adopted without operations: the `updated` time of the page being scanned. A host that
    /// restarted its session and a joiner that re-opened its copy therefore compare *real* edit times instead of all-zero clocks.
    stamp: u64,
    /// Upper bound of adopt stamps (the wall clock at adoption): a page "updated" in the future must not outrank every later edit.
    cap: u64,
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(5)
}

/// Order-sensitive fingerprint of a list of annotations (no allocation).
fn annots_fp(list: &[crate::model::PdfAnnot]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325 ^ list.len() as u64;
    for a in list {
        h = mix(h, a.id);
        h = mix(h, ((a.page as u64) << 8) | a.kind as u64);
        for r in a.rect {
            h = mix(h, r.to_bits() as u64);
        }
        h = mix(h, a.color as u64);
        h = mix(h, a.created.to_bits());
        h = mix(h, crate::media::fnv(a.text.as_bytes()));
        h = mix(h, crate::media::fnv(a.quote.as_bytes()));
        h = mix(h, ((a.pts.len() as u64) << 32) | a.quads.len() as u64);
        for q in &a.pts {
            h = mix(h, ((q[0].to_bits() as u64) << 32) | q[1].to_bits() as u64);
        }
        for q in &a.quads {
            h = mix(h, q.iter().fold(0u64, |x, v| x ^ (v.to_bits() as u64).rotate_left(7)));
        }
    }
    h | 1
}

fn comments_fp(list: &[crate::model::Comment]) -> u64 {
    let mut h = 0x84222325cbf29ce4 ^ list.len() as u64;
    for c in list {
        h = mix(h, c.id);
        h = mix(h, c.block);
        h = mix(h, c.reply_to);
        h = mix(h, c.ts as u64);
        h = mix(h, c.resolved as u64);
        h = mix(h, crate::media::fnv(c.text.as_bytes()));
        h = mix(h, crate::media::fnv(c.author.as_bytes()));
    }
    h | 1
}

/// What the review window shows for an entity: (kind of thing, page title, text).
/// The whole block as JSON (kept in the change log so a change can be reverted without losing formatting); bounded.
fn block_json(ws: &Workspace, pid: u64, bid: u64) -> Option<String> {
    let b = ws.page(pid)?.blocks.iter().find(|b| b.id == bid)?;
    serde_json::to_string(b).ok().filter(|s| s.len() <= 16 * 1024)
}

/// What an incoming operation would change, without applying it: (what, page, block, title, before text, after text).
pub fn describe(ws: &Workspace, op: &Op) -> Option<(&'static str, u64, u64, String, String, String)> {
    let (page, block) = match op.key {
        Key::Page(p) => (p, 0),
        Key::Block(p, b) | Key::Node(p, b) => (p, b),
        _ => return None,
    };
    let before = snapshot(ws, &op.key);
    let title = ws.page(page).map(|p| p.title.clone()).unwrap_or_default();
    let after = match (&op.key, op.e.val.as_deref()) {
        (Key::Block(..), Some(v)) => serde_json::from_str::<Block>(v).ok().map(|b| {
            let plain = crate::rich::strip(&b.text);
            if plain.is_empty() {
                format!("[{:?}]", b.kind)
            } else if b.kind == crate::model::BlockKind::Todo {
                format!("[{}] {plain}", if b.checked { "x" } else { " " })
            } else {
                plain
            }
        }),
        (Key::Page(..), Some(_)) => Some("(page title / settings)".to_owned()),
        (Key::Node(..), Some(v)) => serde_json::from_str::<crate::canvas::Node>(v).ok().map(|n| node_line(&n)),
        _ => None,
    };
    let what = match op.key {
        Key::Page(_) => "Page",
        Key::Node(..) => "Card",
        _ => "Block",
    };
    Some((what, page, block, title, before.map(|b| b.2).unwrap_or_default(), after.unwrap_or_default()))
}

/// One readable line for a canvas card: kind, text and geometry (so a move or a resize shows up as a changed line in the review).
fn node_line(n: &crate::canvas::Node) -> String {
    let text: String = crate::rich::strip(&n.text).chars().take(80).collect();
    let text = text.replace('\n', " ");
    let mut s = format!("{:?}", n.kind);
    if !text.is_empty() {
        s.push_str(&format!(" “{text}”"));
    } else if !n.url.is_empty() {
        s.push_str(&format!(" {}", n.url.chars().take(48).collect::<String>()));
    }
    s.push_str(&format!(" · at {:.0}, {:.0} · {:.0}×{:.0}", n.x, n.y, n.w, n.h));
    if n.color != 0 {
        s.push_str(&format!(" · colour {}", n.color));
    }
    s
}

fn node_json(ws: &Workspace, pid: u64, nid: u64) -> Option<String> {
    let n = ws.page(pid)?.canvas.as_ref()?.nodes.iter().find(|n| n.id == nid)?;
    serde_json::to_string(n).ok().filter(|s| s.len() <= 16 * 1024)
}

fn snapshot(ws: &Workspace, key: &Key) -> Option<(&'static str, String, String)> {
    match *key {
        Key::Page(pid) => ws.page(pid).map(|p| ("Page", p.title.clone(), p.title.clone())),
        Key::Block(pid, bid) => {
            let p = ws.page(pid)?;
            let b = p.blocks.iter().find(|b| b.id == bid)?;
            let plain = crate::rich::strip(&b.text);
            let txt = if plain.is_empty() {
                format!("[{:?}]", b.kind)
            } else if b.kind == crate::model::BlockKind::Todo {
                format!("[{}] {plain}", if b.checked { "x" } else { " " })
            } else {
                plain
            };
            Some(("Block", p.title.clone(), txt))
        }
        Key::Node(pid, nid) => {
            let p = ws.page(pid)?;
            let n = p.canvas.as_ref()?.nodes.iter().find(|n| n.id == nid)?;
            Some(("Card", p.title.clone(), node_line(n)))
        }
        Key::Comment(pid, cid) => {
            let c = ws.comments.get(&pid)?.iter().find(|c| c.id == cid)?;
            Some((
                "Comment",
                ws.page(pid).map(|p| p.title.clone()).unwrap_or_default(),
                format!("{}: {}", c.author, c.text),
            ))
        }
        _ => None,
    }
}

/// Positions for an ordered list: items that already have a position in increasing order keep it (longest increasing subsequence),
/// everything else gets a fresh fractional position between its neighbours. Returns `(positions, renormalised)`.
pub fn assign_positions(existing: &[Option<f64>]) -> (Vec<f64>, bool) {
    let n = existing.len();
    let mut tails: Vec<usize> = vec![];
    let mut prev: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let Some(v) = existing[i] else { continue };
        let (mut lo, mut hi) = (0, tails.len());
        while lo < hi {
            let m = (lo + hi) / 2;
            if existing[tails[m]].unwrap_or(0.0) < v {
                lo = m + 1;
            } else {
                hi = m;
            }
        }
        if lo > 0 {
            prev[i] = Some(tails[lo - 1]);
        }
        if lo == tails.len() {
            tails.push(i);
        } else {
            tails[lo] = i;
        }
    }
    let mut keep = vec![false; n];
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        keep[i] = true;
        cur = prev[i];
    }
    let mut out = vec![0.0; n];
    let mut last: Option<f64> = None;
    let mut i = 0;
    while i < n {
        if keep[i] {
            out[i] = existing[i].unwrap_or(0.0);
            last = Some(out[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && !keep[j] {
            j += 1;
        }
        let next = if j < n { existing[j] } else { None };
        let m = (j - i) as f64;
        for k in i..j {
            let t = (k - i + 1) as f64;
            out[k] = match (last, next) {
                (Some(a), Some(b)) => {
                    let step = (b - a) / (m + 1.0);
                    if step < 1e-9 {
                        return ((0..n).map(|x| x as f64 + 1.0).collect(), true);
                    }
                    a + step * t
                }
                (Some(a), None) => a + t,
                (None, Some(b)) => b - (m + 1.0 - t),
                (None, None) => t,
            };
        }
        i = j;
    }
    (out, false)
}

fn meta_string(ws: &mut Workspace, pid: u64, is_root: bool) -> Option<String> {
    let p = ws.page_mut(pid)?;
    let blocks = std::mem::take(&mut p.blocks);
    let hist = std::mem::take(&mut p.history);
    let (fav, exp, parent) = (p.favorite, p.expanded, p.parent);
    p.favorite = false;
    p.expanded = false;
    if is_root {
        p.parent = None;
    }
    let stash = p.canvas.as_mut().map(|c| {
        let s = (std::mem::take(&mut c.nodes), std::mem::take(&mut c.edges), c.view, c.next_id);
        c.view = Default::default();
        c.next_id = 0;
        s
    });
    let s = serde_json::to_string(&*p).ok();
    p.blocks = blocks;
    p.history = hist;
    p.favorite = fav;
    p.expanded = exp;
    p.parent = parent;
    if let (Some(c), Some((n, e, v, id))) = (p.canvas.as_mut(), stash) {
        c.nodes = n;
        c.edges = e;
        c.view = v;
        c.next_id = id;
    }
    s
}

fn doc_keys_of_block(b: &Block, docs: &mut HashSet<u64>) {
    if b.media != 0 {
        docs.insert(b.media);
    }
    if !b.url.is_empty() {
        docs.insert(crate::pdf::key_of(&b.url));
    }
}

impl Replica {
    pub fn new(replica: u32, scope: Scope) -> Self {
        let roots = if scope.kind == ScopeKind::Workspace { HashSet::new() } else { HashSet::from([scope.root]) };
        Replica {
            clock: Clock::new(replica),
            scope,
            roots,
            cells: HashMap::new(),
            groups: HashMap::new(),
            in_scope: HashSet::new(),
            docs: HashSet::new(),
            seen_updated: HashMap::new(),
            rr: 0,
            quiet: false,
            last_cost_ms: 0.0,
            hist: HashMap::new(),
            cur_page: None,
            merges: HashMap::new(),
            merged: 0,
            ids_sig: None,
            ppos: vec![],
            pidx: HashMap::new(),
            fps: HashMap::new(),
            stamp: 0,
            cap: u64::MAX,
        }
    }

    /// Joiner side: the pages the host calls roots keep this device's own `parent` (none) instead of the host's.
    pub fn set_roots(&mut self, roots: impl IntoIterator<Item = u64>) {
        self.roots = roots.into_iter().collect();
    }

    pub fn roots(&self) -> Vec<u64> {
        self.roots.iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    // ───────────── scope ─────────────

    fn scope_pages(&self, ws: &Workspace) -> Vec<u64> {
        match self.scope.kind {
            ScopeKind::Workspace => ws.pages.iter().map(|p| p.id).collect(),
            ScopeKind::Page => ws.page(self.scope.root).map(|p| vec![p.id]).unwrap_or_default(),
            ScopeKind::Tree => {
                let mut out = vec![];
                let mut set: HashSet<u64> = HashSet::new();
                if ws.page(self.scope.root).is_some() {
                    set.insert(self.scope.root);
                    out.push(self.scope.root);
                }
                loop {
                    let before = out.len();
                    for p in &ws.pages {
                        if !set.contains(&p.id) && p.parent.map_or(false, |x| set.contains(&x)) {
                            set.insert(p.id);
                            out.push(p.id);
                        }
                    }
                    if out.len() == before {
                        break;
                    }
                }
                out
            }
        }
    }

    /// Is this page part of what the session shares (a workspace share shares everything)?
    fn in_scope_page(&self, ws: &Workspace, pid: u64) -> bool {
        self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(&pid) || (ws.page(pid).is_none() && self.cells.contains_key(&Key::Page(pid)))
    }

    /// Whether a page (and therefore the rows of a big table on it) belongs to what this session shares.
    pub fn page_in_scope(&self, ws: &Workspace, pid: u64) -> bool {
        self.in_scope_page(ws, pid)
    }

    fn page_allowed(&self, pid: u64, parent: Option<u64>) -> bool {
        if self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(&pid) {
            return true;
        }
        self.scope.kind == ScopeKind::Tree && parent.map_or(false, |p| self.in_scope.contains(&p))
    }

    // ───────────── local changes → operations ─────────────

    fn remember(&mut self, key: Key, val: &str, ch: u64) {
        if !matches!(key, Key::Block(..)) {
            return;
        }
        if self.hist.len() > HIST_KEYS {
            self.hist.clear();
        }
        let q = self.hist.entry(key).or_default();
        if q.front().map_or(true, |(h, _)| *h != ch) {
            q.push_front((ch, val.to_owned()));
            q.truncate(HIST_DEPTH);
        }
    }

    fn set_cell(&mut self, key: Key, val: Option<String>, pos: f64, now: u64, out: &mut Vec<Op>) {
        let ch = chash_of(&val);
        let h = hash_of(&val, pos);
        let dead = val.is_none();
        // blocks of the open page are remembered, so the first local edit already has its base on record
        if let (Key::Block(p, _), Some(v)) = (key, val.as_ref()) {
            if !self.quiet && (self.cur_page == Some(p) || self.hist.contains_key(&key)) {
                self.remember(key, v, ch);
            }
        }
        let mut base = 0;
        if let Some(c) = self.cells.get(&key) {
            if c.hash == h && c.dead == dead {
                return;
            }
            if !c.dead {
                base = c.chash;
            }
        }
        let hlc = if self.quiet { Hlc { t: self.stamp << 16, r: 0 } } else { self.clock.tick(now) };
        self.cells.insert(key, Cell { hlc, pos, hash: h, chash: ch, dead });
        if let Some(g) = key.group() {
            self.groups.entry(g).or_default().insert(key);
        }
        if !self.quiet {
            out.push(Op { key, e: Entry { hlc, pos, val, base } });
        }
    }

    /// Forgets tombstones older than `cutoff_ms` (wall clock). Safe because a device that was away for longer than the grace period is
    /// re-synchronised as a fresh join (see `Collab::try_resume`), so it can never bring a deleted entity back.
    pub fn gc(&mut self, cutoff_ms: u64) -> usize {
        let dead: Vec<Key> = self.cells.iter().filter(|(_, c)| c.dead && (c.hlc.t >> 16) < cutoff_ms && c.hlc.t != 0).map(|(k, _)| *k).collect();
        for k in &dead {
            self.cells.remove(k);
            if let Some(g) = k.group() {
                if let Some(set) = self.groups.get_mut(&g) {
                    set.remove(k);
                }
            }
            self.hist.remove(k);
        }
        dead.len()
    }

    /// Number of tombstones (statistics).
    pub fn tombstones(&self) -> usize {
        self.cells.values().filter(|c| c.dead).count()
    }

    /// Concurrent edit of one block: merges the remote value into the local one when the base of the remote edit is known.
    /// `Some(changed)` = handled here, `None` = use the normal last-writer-wins path.
    fn merge_block(&mut self, ws: &mut Workspace, pid: u64, bid: u64, op: &Op, val: &str, new_hash: u64, now: u64, applied: &mut Applied) -> Option<bool> {
        let c = *self.cells.get(&op.key)?;
        if c.dead || op.e.base == 0 || !self.page_allowed(pid, None) {
            return None;
        }
        let rch = crate::media::fnv(val.as_bytes());
        if rch == c.chash || op.e.base == c.chash {
            return None; // same content, or the remote edit was made on what we hold: plain update
        }
        let local = serde_json::to_string(ws.page(pid)?.blocks.iter().find(|b| b.id == bid)?).ok()?;
        let base = self.hist.get(&op.key)?.iter().find(|(h, _)| *h == op.e.base)?.1.clone();
        // ping-pong guard: at most 4 merges of one block per 3 s, then last-writer-wins takes over
        let e = self.merges.entry(op.key).or_insert((now, 0));
        if now.saturating_sub(e.0) > 3000 {
            *e = (now, 0);
        }
        if e.1 >= 4 {
            return None;
        }
        e.1 += 1;
        let remote_wins = (op.e.hlc, new_hash) > (c.hlc, c.hash);
        let merged = super::textmerge::merge_json(&base, &local, val, remote_wins)?;
        let nb: Block = serde_json::from_str(&merged).ok()?;
        let merged = serde_json::to_string(&nb).ok()?;
        self.remember(op.key, &local, chash_of(&Some(local.clone())));
        self.remember(op.key, val, rch);
        let page = ws.page_mut(pid)?;
        let slot = page.blocks.iter_mut().find(|b| b.id == bid)?;
        *slot = nb;
        applied.pages.insert(pid);
        applied.n += 1;
        self.merged += 1;
        if remote_wins {
            // the cell now stands for the remote value; the local scan sees the merged text differs and emits one newer operation
            self.cells.insert(op.key, Cell { hlc: op.e.hlc, pos: op.e.pos, hash: new_hash, chash: rch, dead: false });
        }
        let _ = merged;
        Some(true)
    }

    fn pos_of(&self, key: Key) -> Option<f64> {
        self.cells.get(&key).filter(|c| !c.dead).map(|c| c.pos)
    }

    /// Takes a baseline of an existing workspace without producing operations (every entity gets the oldest possible clock).
    pub fn adopt(&mut self, ws: &mut Workspace, now: u64) {
        self.quiet = true;
        self.cap = now;
        let mut sink = vec![];
        self.tick_inner(ws, None, now, f64::MAX, true, &mut sink);
        self.quiet = false;
    }

    /// Compact text snapshot of the clock of every live entity that has a real stamp (one line each: `kind a b t r content-hash`, hex).
    /// Persisted next to the workspace by the session, so a restart does not fall back to page granularity ([`Self::import_stamps`]).
    pub fn export_stamps(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(self.cells.len().min(MAX_STAMPS) * 40);
        for (k, c) in self.cells.iter().filter(|(_, c)| !c.dead && c.hlc.t != 0).take(MAX_STAMPS) {
            let (tag, a, b) = match *k {
                Key::Page(a) => ('p', a, 0),
                Key::Block(a, b) => ('b', a, b),
                Key::Node(a, b) => ('n', a, b),
                Key::Edge(a, b) => ('e', a, b),
                Key::Annot(a, b) => ('a', a, b),
                Key::Comment(a, b) => ('c', a, b),
            };
            let _ = writeln!(out, "{tag} {a:x} {b:x} {:x} {:x} {:x}", c.hlc.t, c.hlc.r, c.chash);
        }
        out
    }

    /// Gives entities back the exact clock they had when [`Self::export_stamps`] ran — but only those whose *content* is unchanged
    /// since (an entity edited while the app was closed keeps the stamp of the page edit time it got from [`Self::adopt`]).
    /// A stamp is never allowed to lie in the future of the adoption time. Returns how many stamps were restored.
    pub fn import_stamps(&mut self, text: &str) -> usize {
        let max_t = (self.cap << 16) | 0xffff;
        let mut n = 0;
        for line in text.lines().take(MAX_STAMPS) {
            let mut it = line.split(' ');
            let (Some(tag), Some(a), Some(b), Some(t), Some(r), Some(h)) = (it.next(), it.next(), it.next(), it.next(), it.next(), it.next()) else {
                continue;
            };
            let hx = |x: &str| u64::from_str_radix(x, 16).ok();
            let (Some(a), Some(b), Some(t), Some(r), Some(h)) = (hx(a), hx(b), hx(t), hx(r), hx(h)) else {
                continue;
            };
            let key = match tag {
                "p" => Key::Page(a),
                "b" => Key::Block(a, b),
                "n" => Key::Node(a, b),
                "e" => Key::Edge(a, b),
                "a" => Key::Annot(a, b),
                "c" => Key::Comment(a, b),
                _ => continue,
            };
            if let Some(c) = self.cells.get_mut(&key) {
                if !c.dead && c.chash == h && t <= max_t {
                    c.hlc = Hlc { t, r: r as u32 };
                    n += 1;
                }
            }
        }
        n
    }

    /// One incremental scan: returns the operations for everything that changed since the last one. `budget_ms` bounds the work spent
    /// on pages other than the open one.
    pub fn tick(&mut self, ws: &mut Workspace, current: Option<u64>, now: u64, budget_ms: f64) -> Vec<Op> {
        let mut out = vec![];
        self.tick_inner(ws, current, now, budget_ms, false, &mut out);
        out
    }

    fn tick_inner(&mut self, ws: &mut Workspace, current: Option<u64>, now: u64, budget_ms: f64, all: bool, out: &mut Vec<Op>) {
        let t0 = crate::collab::mono_ms();
        self.cur_page = current;
        let ids = self.scope_pages(ws);
        // id space of this device (no collisions with other peers' new pages / blocks / cards)
        let base = crate::collab::id_base();
        if base != 0 && !(base..base + (1u64 << 40)).contains(&ws.next_id) {
            ws.next_id = base;
        }
        let sig = ids.iter().fold(ids.len() as u64 ^ 0xcbf2_9ce4_8422_2325, |h, id| (h ^ id).wrapping_mul(0x0000_0100_0000_01b3));
        if self.ids_sig != Some(sig) {
            let id_set: HashSet<u64> = ids.iter().copied().collect();
            // pages that disappeared
            let gone: Vec<u64> = self
                .cells
                .iter()
                .filter_map(|(k, c)| match k {
                    Key::Page(p) if !c.dead && !id_set.contains(p) => Some(*p),
                    _ => None,
                })
                .collect();
            for pid in gone {
                if self.scope.kind != ScopeKind::Workspace && !self.in_scope.contains(&pid) {
                    continue;
                }
                self.set_cell(Key::Page(pid), None, 0.0, now, out);
                if let Some(g) = self.groups.get(&(0, pid)).cloned() {
                    for k in g {
                        self.set_cell(k, None, 0.0, now, out);
                    }
                }
                self.seen_updated.remove(&pid);
            }
            self.in_scope = id_set;
            // positions of pages (sidebar order)
            let existing: Vec<Option<f64>> = ids.iter().map(|id| self.pos_of(Key::Page(*id))).collect();
            self.ppos = assign_positions(&existing).0;
            self.pidx = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            self.ids_sig = Some(sig);
        }
        // which pages to look at: the open one, pages whose `updated` changed, then a rotating slice
        let mut order: Vec<u64> = vec![];
        let mut queued: HashSet<u64> = HashSet::new();
        if let Some(c) = current.filter(|c| self.in_scope.contains(c)) {
            order.push(c);
            queued.insert(c);
        }
        for id in &ids {
            let upd = ws.page(*id).map_or(0.0, |p| p.updated);
            if self.seen_updated.get(id).map_or(true, |u| *u != upd) && queued.insert(*id) {
                order.push(*id);
            }
        }
        // a bounded rotating slice: the budget never allows more than a few hundred pages per tick anyway
        for k in 0..ids.len().min(512) {
            let id = ids[(self.rr + k) % ids.len()];
            if queued.insert(id) {
                order.push(id);
            }
        }
        let mut scanned_rr = 0usize;
        for (i, pid) in order.iter().enumerate() {
            if !all && i > 0 && crate::collab::mono_ms() - t0 > budget_ms {
                break;
            }
            let pos = self.pidx.get(pid).and_then(|ix| self.ppos.get(*ix)).copied().unwrap_or(0.0);
            self.scan_page(ws, *pid, pos, now, out);
            if i >= 1 {
                scanned_rr += 1;
            }
        }
        if !ids.is_empty() {
            self.rr = (self.rr + scanned_rr.max(1)) % ids.len();
        }
        self.stamp = 0;
        self.scan_annots(ws, now, out);
        self.scan_comments(ws, now, out);
        self.last_cost_ms = crate::collab::mono_ms() - t0;
    }

    /// Cheap scan that runs every frame: annotation and comment lists only (fingerprint-guarded, no serialisation when unchanged),
    /// so comments and PDF marks reach the other devices at network speed instead of waiting for the adaptive full scan.
    pub fn tick_hot(&mut self, ws: &Workspace, now: u64) -> Vec<Op> {
        let mut out = vec![];
        self.scan_annots(ws, now, &mut out);
        self.scan_comments(ws, now, &mut out);
        out
    }

    /// Per-frame scan of the one block being typed in: its edit leaves in the same frame (no waiting for the adaptive full scan), so the
    /// other side's text and the caret that travels in `Presence` stay in step. The block must already be known (new blocks are found
    /// by the full scan, which also assigns their order).
    pub fn tick_block(&mut self, ws: &Workspace, pid: u64, bid: u64, now: u64) -> Vec<Op> {
        let mut out = vec![];
        let key = Key::Block(pid, bid);
        let Some(pos) = self.cells.get(&key).filter(|c| !c.dead).map(|c| c.pos) else {
            return out;
        };
        if !self.in_scope.contains(&pid) {
            return out;
        }
        let Some(b) = ws.page(pid).and_then(|p| p.blocks.iter().find(|b| b.id == bid)) else {
            return out;
        };
        if let Ok(s) = serde_json::to_string(b) {
            self.cur_page = Some(pid);
            self.set_cell(key, Some(s), pos, now, &mut out);
        }
        out
    }

    /// The host refused a suggestion: assert the value it holds (or a deletion) with a clock newer than the suggestion, so the
    /// suggesting device's copy goes back to what the host has.
    pub fn reassert(&mut self, ws: &mut Workspace, key: Key, min: Hlc, now: u64) -> Option<Op> {
        self.clock.observe(min, now);
        let hlc = self.clock.tick(now);
        let live = self.cells.get(&key).filter(|c| !c.dead).copied();
        match live {
            Some(c) => {
                let val = self.materialize(ws, key, &mut HashMap::new(), &mut HashMap::new())?;
                self.cells.insert(key, Cell { hlc, ..c });
                Some(Op { key, e: Entry { hlc, pos: c.pos, val: Some(val), base: 0 } })
            }
            None => {
                self.cells.insert(key, Cell { hlc, pos: 0.0, hash: hash_of(&None, 0.0), chash: chash_of(&None), dead: true });
                Some(Op { key, e: Entry { hlc, pos: 0.0, val: None, base: 0 } })
            }
        }
    }

    fn scan_page(&mut self, ws: &mut Workspace, pid: u64, ppos: f64, now: u64, out: &mut Vec<Op>) {
        if self.quiet {
            self.stamp = ws.page(pid).map_or(0, |p| if p.updated.is_finite() && p.updated > 0.0 { (p.updated as u64).min(self.cap) } else { 0 });
        }
        let is_root = self.roots.contains(&pid);
        let Some(meta) = meta_string(ws, pid, is_root) else {
            return;
        };
        self.set_cell(Key::Page(pid), Some(meta), ppos, now, out);
        let base = crate::collab::id_base();
        let Some(p) = ws.page_mut(pid) else { return };
        self.seen_updated.insert(pid, p.updated);
        let mut live: HashSet<Key> = HashSet::new();
        // blocks
        {
            let existing: Vec<Option<f64>> = p.blocks.iter().map(|b| self.cells.get(&Key::Block(pid, b.id)).filter(|c| !c.dead).map(|c| c.pos)).collect();
            let (pos, _) = assign_positions(&existing);
            for (i, b) in p.blocks.iter().enumerate() {
                doc_keys_of_block(b, &mut self.docs);
                let key = Key::Block(pid, b.id);
                live.insert(key);
                if let Ok(s) = serde_json::to_string(b) {
                    self.set_cell(key, Some(s), pos[i], now, out);
                }
            }
        }
        // canvas
        if let Some(c) = p.canvas.as_mut() {
            if base != 0 && !(base..base + (1u64 << 40)).contains(&c.next_id) {
                c.next_id = base;
            }
            let existing: Vec<Option<f64>> = c.nodes.iter().map(|n| self.cells.get(&Key::Node(pid, n.id)).filter(|c| !c.dead).map(|c| c.pos)).collect();
            let (pos, _) = assign_positions(&existing);
            for (i, n) in c.nodes.iter().enumerate() {
                if !n.url.is_empty() {
                    match crate::media::media_id(&n.url) {
                        Some(id) => self.docs.insert(id),
                        None => self.docs.insert(crate::pdf::key_of(&n.url)),
                    };
                }
                let key = Key::Node(pid, n.id);
                live.insert(key);
                if let Ok(s) = serde_json::to_string(n) {
                    self.set_cell(key, Some(s), pos[i], now, out);
                }
            }
            for e in &c.edges {
                let key = Key::Edge(pid, e.id);
                live.insert(key);
                if let Ok(s) = serde_json::to_string(e) {
                    self.set_cell(key, Some(s), 0.0, now, out);
                }
            }
        }
        // deletions inside the page
        if let Some(g) = self.groups.get(&(0, pid)).cloned() {
            for k in g {
                if !live.contains(&k) && self.cells.get(&k).map_or(false, |c| !c.dead) {
                    self.set_cell(k, None, 0.0, now, out);
                }
            }
        }
    }

    fn scan_annots(&mut self, ws: &Workspace, now: u64, out: &mut Vec<Op>) {
        let mut docs: HashSet<u64> =
            if self.scope.kind == ScopeKind::Workspace { ws.pdf_annots.keys().copied().collect() } else { self.docs.iter().copied().filter(|d| ws.pdf_annots.contains_key(d)).collect() };
        // documents that had annotations and lost the whole list
        docs.extend(self.groups.keys().filter(|g| g.0 == 1).map(|g| g.1));
        for d in docs {
            let fp = ws.pdf_annots.get(&d).map_or(0, |l| annots_fp(l));
            if self.fps.get(&(1, d)) == Some(&fp) {
                continue;
            }
            self.fps.insert((1, d), fp);
            let mut live: HashSet<Key> = HashSet::new();
            if let Some(list) = ws.pdf_annots.get(&d) {
                for a in list {
                    let key = Key::Annot(d, a.id);
                    live.insert(key);
                    if let Ok(s) = serde_json::to_string(a) {
                        self.set_cell(key, Some(s), 0.0, now, out);
                    }
                }
            }
            if let Some(g) = self.groups.get(&(1, d)).cloned() {
                for k in g {
                    if !live.contains(&k) && self.cells.get(&k).map_or(false, |c| !c.dead) {
                        self.set_cell(k, None, 0.0, now, out);
                    }
                }
            }
        }
    }

    fn scan_comments(&mut self, ws: &Workspace, now: u64, out: &mut Vec<Op>) {
        let mut pages: HashSet<u64> = ws.comments.keys().copied().filter(|p| self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(p)).collect();
        pages.extend(self.groups.keys().filter(|g| g.0 == 2).map(|g| g.1));
        for p in pages {
            let fp = ws.comments.get(&p).map_or(0, |l| comments_fp(l));
            if self.fps.get(&(2, p)) == Some(&fp) {
                continue;
            }
            self.fps.insert((2, p), fp);
            let mut live: HashSet<Key> = HashSet::new();
            if let Some(list) = ws.comments.get(&p) {
                for c in list {
                    let key = Key::Comment(p, c.id);
                    live.insert(key);
                    if let Ok(s) = serde_json::to_string(c) {
                        self.set_cell(key, Some(s), 0.0, now, out);
                    }
                }
            }
            if let Some(g) = self.groups.get(&(2, p)).cloned() {
                for k in g {
                    if !live.contains(&k) && self.cells.get(&k).map_or(false, |c| !c.dead) {
                        self.set_cell(k, None, 0.0, now, out);
                    }
                }
            }
        }
    }

    // ───────────── remote operations → workspace ─────────────

    /// Merges one remote operation. Returns `true` when it changed this replica (the caller then forwards it to the other peers).
    pub fn apply(&mut self, ws: &mut Workspace, op: &Op, now: u64, applied: &mut Applied) -> bool {
        if !applied.track {
            return self.apply_inner(ws, op, now, applied);
        }
        let before = snapshot(ws, &op.key);
        let raw = match op.key {
            Key::Block(p, b) => block_json(ws, p, b),
            Key::Node(p, n) => node_json(ws, p, n),
            Key::Page(p) if ws.page(p).is_some() => {
                let root = self.roots.contains(&p);
                meta_string(ws, p, root).filter(|s| s.len() <= 16 * 1024)
            }
            _ => None,
        };
        let ok = self.apply_inner(ws, op, now, applied);
        if ok {
            let after = snapshot(ws, &op.key);
            let (page, block) = match op.key {
                Key::Page(p) => (p, 0),
                Key::Block(p, b) | Key::Comment(p, b) | Key::Node(p, b) => (p, b),
                _ => (0, 0),
            };
            use super::changes::{Change, Kind};
            let ch = match (before, after) {
                (None, Some((what, title, a))) => Some((Kind::Added, what, title, String::new(), a)),
                (Some((what, title, b)), None) => Some((Kind::Removed, what, title, b, String::new())),
                (Some((what, title, b)), Some((_, t2, a))) if a != b => Some((Kind::Edited, what, if t2.is_empty() { title } else { t2 }, b, a)),
                (Some(_), Some(_)) => None, // moved / re-ordered: not a content change
                (None, None) => {
                    applied.other += 1;
                    None
                }
            };
            if let Some((kind, what, title, before, after)) = ch {
                applied.log.push(Change { kind, what, page, block, title, who: op.e.hlc.r, at: now, before, after, raw });
            }
        }
        ok
    }

    fn apply_inner(&mut self, ws: &mut Workspace, op: &Op, now: u64, applied: &mut Applied) -> bool {
        // a value this large is an attack (or a bug), not a block: never let one entity swallow the memory of the host
        if op.e.val.as_ref().map_or(false, |v| v.len() > MAX_VALUE) || !op.e.pos.is_finite() || self.cells.len() > MAX_ENTITIES {
            return false;
        }
        self.clock.observe(op.e.hlc, now);
        let new_hash = hash_of(&op.e.val, op.e.pos);
        if let (Key::Block(pid, bid), Some(val)) = (&op.key, &op.e.val) {
            if let Some(done) = self.merge_block(ws, *pid, *bid, op, val, new_hash, now, applied) {
                return done;
            }
        }
        if let Some(c) = self.cells.get(&op.key) {
            if (op.e.hlc, new_hash) <= (c.hlc, c.hash) {
                return false;
            }
        }
        if let (Key::Block(pid, bid), Some(_)) = (&op.key, &op.e.val) {
            // the value we are about to overwrite may be the base of a concurrent edit that arrives next
            if !self.hist.contains_key(&op.key) {
                let old = ws.page(*pid).and_then(|p| p.blocks.iter().find(|b| b.id == *bid)).and_then(|b| serde_json::to_string(b).ok());
                if let Some(old) = old {
                    let h = chash_of(&Some(old.clone()));
                    self.remember(op.key, &old, h);
                }
            }
        }
        let ok = match (&op.key, &op.e.val) {
            (Key::Page(pid), None) => {
                // deleting a page is an operation on that page: it must be inside the shared scope (an editor of ONE shared page
                // could otherwise delete any page of the host by guessing its small sequential id)
                if !self.in_scope_page(ws, *pid) {
                    return false;
                }
                ws.pages.retain(|p| p.id != *pid);
                self.in_scope.remove(pid);
                applied.reorder_pages = true;
                true
            }
            (Key::Page(pid), Some(json)) => self.apply_page(ws, *pid, json, applied),
            (Key::Block(pid, bid), val) => match ws.page_mut(*pid) {
                Some(p) if self.page_allowed(*pid, None) => match val {
                    None => {
                        p.blocks.retain(|b| b.id != *bid);
                        true
                    }
                    Some(j) => match serde_json::from_str::<Block>(j) {
                        Ok(b) => {
                            match p.blocks.iter_mut().find(|x| x.id == *bid) {
                                Some(x) => *x = b,
                                None => p.blocks.push(b),
                            }
                            true
                        }
                        Err(_) => false,
                    },
                },
                _ => false,
            },
            (Key::Node(pid, nid), val) => match ws.page_mut(*pid).and_then(|p| p.canvas.as_mut()) {
                Some(c) if self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(pid) => {
                    c.rev += 1;
                    match val {
                        None => {
                            c.nodes.retain(|n| n.id != *nid);
                            true
                        }
                        Some(j) => match serde_json::from_str::<Node>(j) {
                            Ok(n) => {
                                match c.nodes.iter_mut().find(|x| x.id == *nid) {
                                    Some(x) => *x = n,
                                    None => c.nodes.push(n),
                                }
                                true
                            }
                            Err(_) => false,
                        },
                    }
                }
                _ => false,
            },
            (Key::Edge(pid, eid), val) => match ws.page_mut(*pid).and_then(|p| p.canvas.as_mut()) {
                Some(c) if self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(pid) => {
                    c.rev += 1;
                    match val {
                        None => {
                            c.edges.retain(|e| e.id != *eid);
                            true
                        }
                        Some(j) => match serde_json::from_str::<Edge>(j) {
                            Ok(e) => {
                                match c.edges.iter_mut().find(|x| x.id == *eid) {
                                    Some(x) => *x = e,
                                    None => c.edges.push(e),
                                }
                                true
                            }
                            Err(_) => false,
                        },
                    }
                }
                _ => false,
            },
            (Key::Comment(pid, cid), val) => {
                if !(ws.page(*pid).is_some() && self.in_scope_page(ws, *pid)) {
                    return false;
                }
                let list = ws.comments.entry(*pid).or_default();
                match val {
                    None => {
                        list.retain(|c| c.id != *cid);
                        true
                    }
                    Some(j) => match serde_json::from_str::<Comment>(j) {
                        Ok(c) if c.id == *cid && list.len() < 5000 || list.iter().any(|x| x.id == *cid) => {
                            match list.iter_mut().find(|x| x.id == *cid) {
                                Some(x) => *x = c,
                                None => list.push(c),
                            }
                            true
                        }
                        _ => false,
                    },
                }
            }
            (Key::Annot(doc, _), _) if self.scope.kind != ScopeKind::Workspace && !self.docs.contains(doc) => false,
            (Key::Annot(doc, aid), val) => {
                let list = ws.pdf_annots.entry(*doc).or_default();
                match val {
                    None => {
                        list.retain(|a| a.id != *aid);
                        true
                    }
                    Some(j) => match serde_json::from_str::<PdfAnnot>(j) {
                        Ok(a) => {
                            match list.iter_mut().find(|x| x.id == *aid) {
                                Some(x) => *x = a,
                                None => list.push(a),
                            }
                            true
                        }
                        Err(_) => false,
                    },
                }
            }
        };
        if !ok {
            return false;
        }
        // record what we now hold (hash of OUR serialisation of a page, so the next local scan sees "no change" and does not echo)
        let hash = match (&op.key, &op.e.val) {
            (Key::Page(pid), Some(_)) => {
                let m = meta_string(ws, *pid, self.roots.contains(pid));
                hash_of(&m, op.e.pos)
            }
            _ => new_hash,
        };
        let dead = op.e.val.is_none();
        let ch = chash_of(&op.e.val);
        if let (Key::Block(..), Some(v)) = (&op.key, &op.e.val) {
            // remote edits keep their predecessor around: that is the base of a later concurrent edit
            if self.hist.contains_key(&op.key) || self.cur_page.map_or(false, |p| matches!(op.key, Key::Block(q, _) if q == p)) {
                self.remember(op.key, v, ch);
            }
        }
        self.cells.insert(op.key, Cell { hlc: op.e.hlc, pos: op.e.pos, hash, chash: ch, dead });
        if let Some(g) = op.key.group() {
            self.groups.entry(g).or_default().insert(op.key);
            if let Key::Block(p, _) | Key::Node(p, _) = op.key {
                applied.pages.insert(p);
            }
        }
        if let Key::Annot(doc, _) = op.key {
            self.docs.insert(doc);
        }
        applied.n += 1;
        true
    }

    fn apply_page(&mut self, ws: &mut Workspace, pid: u64, json: &str, applied: &mut Applied) -> bool {
        let Ok(mut np) = serde_json::from_str::<Page>(json) else {
            return false;
        };
        if !self.page_allowed(pid, np.parent) {
            return false;
        }
        // page take-over: a page that already exists on this device but is not part of the shared scope cannot be pulled into it by
        // claiming a shared parent (that would let a guest read / overwrite any page of the host)
        if self.scope.kind != ScopeKind::Workspace && !self.in_scope.contains(&pid) && ws.page(pid).is_some() {
            return false;
        }
        // a page can never be its own ancestor: a cycle would hang the sidebar / breadcrumb of every device
        if np.parent == Some(pid) || would_cycle(ws, pid, np.parent) {
            return false;
        }
        np.id = pid;
        let is_root = self.roots.contains(&pid);
        if let Some(old) = ws.page_mut(pid) {
            np.blocks = std::mem::take(&mut old.blocks);
            np.history = std::mem::take(&mut old.history);
            np.favorite = old.favorite;
            np.expanded = old.expanded;
            if is_root {
                np.parent = old.parent;
            }
            let remote_canvas = np.canvas.take();
            np.canvas = old.canvas.take();
            if let (Some(oc), Some(rc)) = (np.canvas.as_mut(), remote_canvas.as_ref()) {
                oc.grid = rc.grid;
                oc.snap = rc.snap;
                oc.minimap = rc.minimap;
                oc.rev += 1;
            } else if np.canvas.is_none() {
                np.canvas = remote_canvas;
            }
            *old = np;
        } else {
            if is_root {
                np.parent = None;
            }
            np.blocks.clear();
            np.expanded = false;
            np.favorite = false;
            if let Some(c) = np.canvas.as_mut() {
                c.nodes.clear();
                c.edges.clear();
            }
            ws.pages.push(np);
            applied.reorder_pages = true;
        }
        self.in_scope.insert(pid);
        applied.pages.insert(pid);
        true
    }

    /// After a batch of operations: restores the order of everything that has a fractional position.
    pub fn finish(&mut self, ws: &mut Workspace, applied: &Applied) {
        for pid in &applied.pages {
            let Some(p) = ws.page_mut(*pid) else { continue };
            let cells = &self.cells;
            let pos = |k: Key| cells.get(&k).map_or(f64::MAX, |c| c.pos);
            p.blocks.sort_by(|a, b| pos(Key::Block(*pid, a.id)).total_cmp(&pos(Key::Block(*pid, b.id))).then(a.id.cmp(&b.id)));
            if let Some(c) = p.canvas.as_mut() {
                c.nodes.sort_by(|a, b| pos(Key::Node(*pid, a.id)).total_cmp(&pos(Key::Node(*pid, b.id))).then(a.id.cmp(&b.id)));
                c.rev += 1;
            }
        }
        if applied.reorder_pages {
            let synced: Vec<usize> = ws.pages.iter().enumerate().filter(|(_, p)| self.cells.contains_key(&Key::Page(p.id))).map(|(i, _)| i).collect();
            let mut pages: Vec<Page> = synced.iter().map(|i| ws.pages[*i].clone()).collect();
            pages.sort_by(|a, b| {
                let pa = self.cells.get(&Key::Page(a.id)).map_or(f64::MAX, |c| c.pos);
                let pb = self.cells.get(&Key::Page(b.id)).map_or(f64::MAX, |c| c.pos);
                pa.total_cmp(&pb).then(a.id.cmp(&b.id))
            });
            for (slot, p) in synced.into_iter().zip(pages) {
                ws.pages[slot] = p;
            }
        }
    }

    // ───────────── anti-entropy (initial sync / reconnect) ─────────────

    pub fn digest(&self) -> Vec<(Key, Hlc, u64)> {
        self.cells.iter().map(|(k, c)| (*k, c.hlc, c.hash)).collect()
    }

    /// Operations the remote side is missing or has older (it sent us its digest). Pages first, so containers exist before content.
    pub fn diff(&self, ws: &mut Workspace, remote: &HashMap<Key, (Hlc, u64)>) -> Vec<Op> {
        let mut want: Vec<(Key, Cell)> = self.cells.iter().filter(|(k, c)| remote.get(*k).map_or(true, |r| (c.hlc, c.hash) > *r)).map(|(k, c)| (*k, *c)).collect();
        want.sort_by(|a, b| a.0.rank().cmp(&b.0.rank()).then(a.1.pos.total_cmp(&b.1.pos)).then(a.0.cmp(&b.0)));
        let mut out = Vec::with_capacity(want.len());
        let mut block_ix: HashMap<u64, HashMap<u64, usize>> = HashMap::new();
        let mut node_ix: HashMap<u64, HashMap<u64, usize>> = HashMap::new();
        for (key, c) in want {
            let val = if c.dead { None } else { self.materialize(ws, key, &mut block_ix, &mut node_ix) };
            if val.is_none() && !c.dead {
                continue;
            }
            out.push(Op { key, e: Entry { hlc: c.hlc, pos: c.pos, val, base: 0 } });
        }
        out
    }

    fn materialize(&self, ws: &mut Workspace, key: Key, bix: &mut HashMap<u64, HashMap<u64, usize>>, nix: &mut HashMap<u64, HashMap<u64, usize>>) -> Option<String> {
        match key {
            Key::Page(pid) => meta_string(ws, pid, self.roots.contains(&pid)),
            Key::Block(pid, id) => {
                let p = ws.page(pid)?;
                let ix = bix.entry(pid).or_insert_with(|| p.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect());
                serde_json::to_string(p.blocks.get(*ix.get(&id)?)?).ok()
            }
            Key::Node(pid, id) => {
                let c = ws.page(pid)?.canvas.as_ref()?;
                let ix = nix.entry(pid).or_insert_with(|| c.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect());
                serde_json::to_string(c.nodes.get(*ix.get(&id)?)?).ok()
            }
            Key::Edge(pid, id) => serde_json::to_string(ws.page(pid)?.canvas.as_ref()?.edges.iter().find(|e| e.id == id)?).ok(),
            Key::Annot(doc, id) => serde_json::to_string(ws.pdf_annots.get(&doc)?.iter().find(|a| a.id == id)?).ok(),
            Key::Comment(pid, id) => serde_json::to_string(ws.comments.get(&pid)?.iter().find(|a| a.id == id)?).ok(),
        }
    }

    /// Media ids referenced by a page (fetched on demand by the session when this device does not hold them yet).
    pub fn referenced_media(&self, ws: &Workspace, pid: u64, out: &mut HashSet<u64>) {
        if let Some(p) = ws.page(pid) {
            for b in &p.blocks {
                if b.media != 0 {
                    out.insert(b.media);
                }
                if let Some(id) = crate::media::media_id(&b.url) {
                    out.insert(id);
                }
            }
            if let Some(c) = &p.canvas {
                for n in &c.nodes {
                    if let Some(id) = crate::media::media_id(&n.url) {
                        out.insert(id);
                    }
                }
            }
            if let Some(id) = crate::media::media_id(&p.cover_img) {
                out.insert(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> (Workspace, Workspace, Replica, Replica) {
        let mut a = Workspace::seed();
        let mut b = Workspace::blank("joiner", false, 0);
        b.pages.clear();
        b.current = None;
        let mut ra = Replica::new(1, Scope::ALL);
        let mut rb = Replica::new(2, Scope::ALL);
        ra.adopt(&mut a, 1000);
        let ops = ra.diff(&mut a, &HashMap::new());
        let mut ap = Applied::default();
        for op in &ops {
            rb.apply(&mut b, op, 2000, &mut ap);
        }
        rb.finish(&mut b, &ap);
        (a, b, ra, rb)
    }

    #[test]
    fn positions_keep_the_longest_increasing_run() {
        let (p, r) = assign_positions(&[Some(1.0), Some(2.0), Some(3.0)]);
        assert_eq!(p, vec![1.0, 2.0, 3.0]);
        assert!(!r);
        let (p, _) = assign_positions(&[Some(3.0), Some(1.0), Some(2.0)]);
        assert_eq!(&p[1..], &[1.0, 2.0]);
        assert!(p[0] < 1.0);
        let (p, _) = assign_positions(&[Some(1.0), None, Some(2.0)]);
        assert!(p[1] > 1.0 && p[1] < 2.0);
    }

    #[test]
    fn a_fresh_peer_receives_the_whole_workspace_and_does_not_echo_it_back() {
        let (a, mut b, _ra, mut rb) = two();
        assert_eq!(a.pages.len(), b.pages.len());
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        assert_eq!(blocks(&a), blocks(&b));
        let cur = b.current;
        let echo = rb.tick(&mut b, cur, 3000, f64::MAX);
        assert!(echo.is_empty(), "{} echoed ops", echo.len());
    }

    #[test]
    fn concurrent_edits_of_different_blocks_both_survive() {
        let (mut a, mut b, mut ra, mut rb) = two();
        let pid = a.pages.iter().find(|p| p.blocks.len() > 2).map(|p| p.id).unwrap();
        a.page_mut(pid).unwrap().blocks[0].text = "from A".into();
        b.page_mut(pid).unwrap().blocks[1].text = "from B".into();
        let oa = ra.tick(&mut a, Some(pid), 5000, f64::MAX);
        let ob = rb.tick(&mut b, Some(pid), 5001, f64::MAX);
        assert!(!oa.is_empty() && !ob.is_empty());
        let (mut x, mut y) = (Applied::default(), Applied::default());
        for op in &ob {
            ra.apply(&mut a, op, 6000, &mut x);
        }
        for op in &oa {
            rb.apply(&mut b, op, 6000, &mut y);
        }
        ra.finish(&mut a, &x);
        rb.finish(&mut b, &y);
        for w in [&a, &b] {
            let p = w.page(pid).unwrap();
            assert_eq!(p.blocks[0].text, "from A");
            assert_eq!(p.blocks[1].text, "from B");
        }
    }

    fn page_op(ws: &mut Workspace, pid: u64, parent: Option<u64>, hlc: u64) -> Op {
        let mut p = ws.page(pid).cloned().unwrap_or_else(|| ws.pages[0].clone());
        p.id = pid;
        p.parent = parent;
        p.blocks.clear();
        Op { key: Key::Page(pid), e: Entry { hlc: Hlc { t: hlc, r: 9 }, pos: 1.0, val: serde_json::to_string(&p).ok(), base: 0 } }
    }

    #[test]
    fn a_guest_cannot_take_over_delete_or_loop_pages_outside_its_scope() {
        let mut a = Workspace::seed();
        let ids: Vec<u64> = a.pages.iter().map(|p| p.id).collect();
        assert!(ids.len() >= 3);
        let tops: Vec<u64> = a.pages.iter().filter(|p| p.parent.is_none()).map(|p| p.id).collect();
        assert!(tops.len() >= 2, "seed has two top-level pages");
        let (root, other) = (tops[0], tops[1]);
        let mut ra = Replica::new(1, Scope { kind: ScopeKind::Tree, root });
        ra.adopt(&mut a, 1000);
        let mut ap = Applied::default();
        // 1. delete a page outside the shared tree
        let del = Op { key: Key::Page(other), e: Entry { hlc: Hlc { t: 5_000_000_000_000, r: 9 }, pos: 1.0, val: None, base: 0 } };
        assert!(!ra.apply(&mut a, &del, 6000, &mut ap), "delete outside scope must be refused");
        assert!(a.page(other).is_some());
        // 2. pull an existing outside page into the tree by claiming the shared root as its parent
        let hijack = page_op(&mut a, other, Some(root), 5_000_000_000_000);
        assert!(!ra.apply(&mut a, &hijack, 6000, &mut ap), "take-over of an existing page must be refused");
        // 3. a page that is its own parent
        let selfp = page_op(&mut a, root, Some(root), 5_000_000_000_001);
        assert!(!ra.apply(&mut a, &selfp, 6000, &mut ap), "self parent must be refused");
        // 4. oversize value
        let mut big = page_op(&mut a, root, None, 5_000_000_000_002);
        big.e.val = Some("x".repeat(MAX_VALUE + 1));
        assert!(!ra.apply(&mut a, &big, 6000, &mut ap), "oversize value must be refused");
    }

    /// Delivers operations to a replica.
    fn deliver(ops: &[Op], r: &mut Replica, w: &mut Workspace, now: u64) {
        let mut ap = Applied::default();
        for op in ops {
            r.apply(w, op, now, &mut ap);
        }
        r.finish(w, &ap);
    }

    #[test]
    fn two_people_typing_in_the_same_paragraph_keep_both_edits() {
        let (mut a, mut b, mut ra, mut rb) = two();
        let pid = a.pages.iter().find(|p| p.blocks.len() > 2).map(|p| p.id).unwrap();
        let original = a.page(pid).unwrap().blocks[0].text.clone();
        // both look at the page (their first tick records the base of the coming edits)
        ra.tick(&mut a, Some(pid), 3000, f64::MAX);
        rb.tick(&mut b, Some(pid), 3000, f64::MAX);
        a.page_mut(pid).unwrap().blocks[0].text = format!("{original} (A)");
        b.page_mut(pid).unwrap().blocks[0].text = format!("(B) {original}");
        let oa = ra.tick(&mut a, Some(pid), 4000, f64::MAX);
        let ob = rb.tick(&mut b, Some(pid), 4000, f64::MAX);
        assert!(!oa.is_empty() && !ob.is_empty());
        deliver(&ob, &mut ra, &mut a, 5000);
        deliver(&oa, &mut rb, &mut b, 5000);
        // the merge results are re-published by the next scan and must converge
        for round in 0..4u64 {
            let na = ra.tick(&mut a, Some(pid), 6000 + round, f64::MAX);
            let nb = rb.tick(&mut b, Some(pid), 6000 + round, f64::MAX);
            deliver(&nb, &mut ra, &mut a, 7000 + round);
            deliver(&na, &mut rb, &mut b, 7000 + round);
        }
        let want = format!("(B) {original} (A)");
        assert_eq!(a.page(pid).unwrap().blocks[0].text, want);
        assert_eq!(b.page(pid).unwrap().blocks[0].text, want);
        assert!(ra.merged + rb.merged >= 1);
    }

    #[test]
    fn comments_sync_resolve_and_delete() {
        let (mut a, mut b, mut ra, mut rb) = two();
        let pid = a.pages[0].id;
        a.comments.entry(pid).or_default().push(Comment { id: 77, text: "hi @Bob".into(), author: "Ann".into(), author_id: 1, ts: 1.0, ..Default::default() });
        let ops = ra.tick(&mut a, Some(pid), 4000, f64::MAX);
        assert!(ops.iter().any(|o| matches!(o.key, Key::Comment(..))));
        deliver(&ops, &mut rb, &mut b, 5000);
        assert_eq!(b.comments[&pid][0].text, "hi @Bob");
        b.comments.get_mut(&pid).unwrap()[0].resolved = true;
        let ops = rb.tick(&mut b, Some(pid), 6000, f64::MAX);
        deliver(&ops, &mut ra, &mut a, 7000);
        assert!(a.comments[&pid][0].resolved);
        a.comments.get_mut(&pid).unwrap().clear();
        let ops = ra.tick(&mut a, Some(pid), 8000, f64::MAX);
        deliver(&ops, &mut rb, &mut b, 9000);
        assert!(b.comments[&pid].is_empty());
    }

    #[test]
    fn old_tombstones_are_collected_and_young_ones_stay() {
        let (mut a, _b, mut ra, _rb) = two();
        let pid = a.pages.iter().find(|p| p.blocks.len() > 2).map(|p| p.id).unwrap();
        a.page_mut(pid).unwrap().blocks.remove(0);
        ra.tick(&mut a, Some(pid), 10_000, f64::MAX);
        assert_eq!(ra.tombstones(), 1);
        assert_eq!(ra.gc(5_000), 0, "younger than the cut-off");
        assert_eq!(ra.gc(20_000), 1);
        assert_eq!(ra.tombstones(), 0);
    }

    #[test]
    fn parent_cycles_are_detected() {
        let mut a = Workspace::seed();
        let (x, y) = (a.pages[0].id, a.pages[1].id);
        a.page_mut(y).unwrap().parent = Some(x);
        assert!(would_cycle(&a, x, Some(y)));
        assert!(would_cycle(&a, x, Some(x)));
        assert!(!would_cycle(&a, y, Some(x)));
        assert!(!would_cycle(&a, x, None));
    }

    #[test]
    fn restart_stamps_let_the_newer_page_win_and_keep_unchanged_ones_quiet() {
        let mut a = Workspace::seed();
        let pid = a.pages.iter().find(|p| !p.blocks.is_empty()).map(|p| p.id).unwrap();
        a.page_mut(pid).unwrap().updated = 1_000_000.0;
        let mut b: Workspace = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        b.page_mut(pid).unwrap().blocks[0].text = "edited while away".into();
        b.page_mut(pid).unwrap().updated = 1_005_000.0;
        let (mut ra, mut rb) = (Replica::new(1, Scope::ALL), Replica::new(2, Scope::ALL));
        ra.adopt(&mut a, 2_000_000);
        rb.adopt(&mut b, 2_000_000);
        let dig = |r: &Replica| r.digest().into_iter().map(|(k, h, x)| (k, (h, x))).collect::<HashMap<_, _>>();
        let from_a = ra.diff(&mut a, &dig(&rb));
        let from_b = rb.diff(&mut b, &dig(&ra));
        assert!(
            from_a.iter().all(|o| o.key != Key::Block(pid, a.page(pid).unwrap().blocks[0].id)),
            "the older copy must not overwrite the newer edit"
        );
        assert!(
            from_b.iter().any(|o| o.key == Key::Block(pid, b.page(pid).unwrap().blocks[0].id)),
            "the newer edit must travel"
        );
        let other = a.pages.iter().find(|p| p.id != pid && !p.blocks.is_empty()).map(|p| p.id);
        if let Some(o) = other {
            assert!(from_a.iter().chain(from_b.iter()).all(|op| !matches!(op.key, Key::Block(p, _) if p == o)));
        }
    }

    #[test]
    fn the_typed_block_is_scanned_on_its_own_and_a_refusal_reasserts_the_hosts_text() {
        let (mut a, mut b, mut ra, mut rb) = two();
        let pid = a.pages.iter().find(|p| p.blocks.len() > 1).map(|p| p.id).unwrap();
        let bid = a.page(pid).unwrap().blocks[0].id;
        let original = a.page(pid).unwrap().blocks[0].text.clone();
        b.page_mut(pid).unwrap().blocks[0].text = "suggested".into();
        let ops = rb.tick_block(&b, pid, bid, 3000);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].key, Key::Block(pid, bid));
        let re = ra.reassert(&mut a, Key::Block(pid, bid), ops[0].e.hlc, 3001).expect("reassert");
        let mut ap = Applied::default();
        assert!(rb.apply(&mut b, &re, 3002, &mut ap));
        assert_eq!(b.page(pid).unwrap().blocks[0].text, original);
    }

    #[test]
    fn stamps_survive_a_restart_per_block_and_edits_made_while_closed_fall_back_to_the_page() {
        let mut a = Workspace::seed();
        let pid = a.pages.iter().find(|p| p.blocks.len() > 2).map(|p| p.id).unwrap();
        a.page_mut(pid).unwrap().updated = 900_000.0;
        let mut ra = Replica::new(1, Scope::ALL);
        ra.adopt(&mut a, 1_000_000);
        a.page_mut(pid).unwrap().blocks[0].text = "edited later".into();
        a.page_mut(pid).unwrap().updated = 1_200_000.0;
        let _ = ra.tick(&mut a, Some(pid), 1_200_000, f64::MAX);
        let saved = ra.export_stamps();
        assert!(saved.lines().count() > 0);
        // restart: a fresh replica adopts the same workspace — page granularity …
        a.page_mut(pid).unwrap().blocks[2].text = "edited while closed".into();
        let mut rb = Replica::new(1, Scope::ALL);
        rb.adopt(&mut a, 2_000_000);
        let stamp = |r: &Replica, bid: u64| r.digest().into_iter().find(|(k, _, _)| *k == Key::Block(pid, bid)).map(|(_, h, _)| h.t >> 16).unwrap();
        let ids: Vec<u64> = a.page(pid).unwrap().blocks.iter().map(|b| b.id).collect();
        assert_eq!(stamp(&rb, ids[1]), 1_200_000);
        // … until the stamps are imported: the untouched block gets its real (older) clock back, the edited ones keep the page stamp
        let restored = rb.import_stamps(&saved);
        assert!(restored > 0);
        assert_eq!(stamp(&rb, ids[1]), 900_000);
        assert_eq!(stamp(&rb, ids[0]), 1_200_000);
        assert_eq!(stamp(&rb, ids[2]), 1_200_000, "content changed while closed: no stale stamp");
        assert_eq!(rb.import_stamps("garbage\nb zz 1 2 3 4"), 0);
    }
}
