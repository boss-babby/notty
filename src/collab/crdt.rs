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
use crate::model::{Block, Page, PdfAnnot, Workspace};
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
}

impl Key {
    /// Owning container (pages own blocks / nodes / edges, documents own annotations).
    fn group(&self) -> Option<(u8, u64)> {
        match *self {
            Key::Page(_) => None,
            Key::Block(p, _) | Key::Node(p, _) | Key::Edge(p, _) => Some((0, p)),
            Key::Annot(d, _) => Some((1, d)),
        }
    }
    fn rank(&self) -> u8 {
        match self {
            Key::Page(_) => 0,
            Key::Annot(..) => 2,
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
    dead: bool,
}

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
}

fn hash_of(val: &Option<String>, pos: f64) -> u64 {
    let mut h = match val {
        Some(s) => crate::media::fnv(s.as_bytes()),
        None => 0x9e37_79b9,
    };
    h ^= pos.to_bits().rotate_left(17);
    h.wrapping_mul(0x100000001b3)
}

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
        Replica { clock: Clock::new(replica), scope, roots, cells: HashMap::new(), groups: HashMap::new(), in_scope: HashSet::new(), docs: HashSet::new(), seen_updated: HashMap::new(), rr: 0, quiet: false, last_cost_ms: 0.0 }
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

    fn page_allowed(&self, pid: u64, parent: Option<u64>) -> bool {
        if self.scope.kind == ScopeKind::Workspace || self.in_scope.contains(&pid) {
            return true;
        }
        self.scope.kind == ScopeKind::Tree && parent.map_or(false, |p| self.in_scope.contains(&p))
    }

    // ───────────── local changes → operations ─────────────

    fn set_cell(&mut self, key: Key, val: Option<String>, pos: f64, now: u64, out: &mut Vec<Op>) {
        let h = hash_of(&val, pos);
        let dead = val.is_none();
        if let Some(c) = self.cells.get(&key) {
            if c.hash == h && c.dead == dead {
                return;
            }
        }
        let hlc = if self.quiet { Hlc::default() } else { self.clock.tick(now) };
        self.cells.insert(key, Cell { hlc, pos, hash: h, dead });
        if let Some(g) = key.group() {
            self.groups.entry(g).or_default().insert(key);
        }
        if !self.quiet {
            out.push(Op { key, e: Entry { hlc, pos, val } });
        }
    }

    fn pos_of(&self, key: Key) -> Option<f64> {
        self.cells.get(&key).filter(|c| !c.dead).map(|c| c.pos)
    }

    /// Takes a baseline of an existing workspace without producing operations (every entity gets the oldest possible clock).
    pub fn adopt(&mut self, ws: &mut Workspace, now: u64) {
        self.quiet = true;
        let mut sink = vec![];
        self.tick_inner(ws, None, now, f64::MAX, true, &mut sink);
        self.quiet = false;
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
        let ids = self.scope_pages(ws);
        let id_set: HashSet<u64> = ids.iter().copied().collect();
        // id space of this device (no collisions with other peers' new pages / blocks / cards)
        let base = crate::collab::id_base();
        if base != 0 && !(base..base + (1u64 << 40)).contains(&ws.next_id) {
            ws.next_id = base;
        }
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
        let (ppos, _) = assign_positions(&existing);
        // which pages to look at: the open one, pages whose `updated` changed, then a rotating slice
        let mut order: Vec<u64> = vec![];
        if let Some(c) = current.filter(|c| self.in_scope.contains(c)) {
            order.push(c);
        }
        for id in &ids {
            let upd = ws.page(*id).map_or(0.0, |p| p.updated);
            if self.seen_updated.get(id).map_or(true, |u| *u != upd) && !order.contains(id) {
                order.push(*id);
            }
        }
        if !ids.is_empty() {
            for k in 0..ids.len() {
                let id = ids[(self.rr + k) % ids.len()];
                if !order.contains(&id) {
                    order.push(id);
                }
            }
        }
        let mut scanned_rr = 0usize;
        for (i, pid) in order.iter().enumerate() {
            if !all && i > 0 && crate::collab::mono_ms() - t0 > budget_ms {
                break;
            }
            let pos = ids.iter().position(|x| x == pid).map(|ix| ppos[ix]).unwrap_or(0.0);
            self.scan_page(ws, *pid, pos, now, out);
            if i >= 1 {
                scanned_rr += 1;
            }
        }
        if !ids.is_empty() {
            self.rr = (self.rr + scanned_rr.max(1)) % ids.len();
        }
        self.scan_annots(ws, now, out);
        self.last_cost_ms = crate::collab::mono_ms() - t0;
    }

    fn scan_page(&mut self, ws: &mut Workspace, pid: u64, ppos: f64, now: u64, out: &mut Vec<Op>) {
        let is_root = self.roots.contains(&pid);
        let Some(meta) = meta_string(ws, pid, is_root) else { return };
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
        let mut docs: HashSet<u64> = if self.scope.kind == ScopeKind::Workspace { ws.pdf_annots.keys().copied().collect() } else { self.docs.iter().copied().filter(|d| ws.pdf_annots.contains_key(d)).collect() };
        // documents that had annotations and lost the whole list
        docs.extend(self.groups.keys().filter(|g| g.0 == 1).map(|g| g.1));
        for d in docs {
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

    // ───────────── remote operations → workspace ─────────────

    /// Merges one remote operation. Returns `true` when it changed this replica (the caller then forwards it to the other peers).
    pub fn apply(&mut self, ws: &mut Workspace, op: &Op, now: u64, applied: &mut Applied) -> bool {
        self.clock.observe(op.e.hlc, now);
        let new_hash = hash_of(&op.e.val, op.e.pos);
        if let Some(c) = self.cells.get(&op.key) {
            if (op.e.hlc, new_hash) <= (c.hlc, c.hash) {
                return false;
            }
        }
        let ok = match (&op.key, &op.e.val) {
            (Key::Page(pid), None) => {
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
        self.cells.insert(op.key, Cell { hlc: op.e.hlc, pos: op.e.pos, hash, dead });
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
        let Ok(mut np) = serde_json::from_str::<Page>(json) else { return false };
        if !self.page_allowed(pid, np.parent) {
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
            out.push(Op { key, e: Entry { hlc: c.hlc, pos: c.pos, val } });
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
}
