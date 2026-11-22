//! Large tables (`Workspace.bigs`, rows in `lumen-store`) inside a session — protocol 7.
//!
//! The rows are not entities of the CRDT (a million 48-byte cells would cost 48 MB and a million digest entries), so they travel by their own,
//! much cheaper anti-entropy:
//!
//! * **Buckets.** Row ids are cut into ranges of [`BUCKET`] ids. Per bucket a device keeps `(rows, sum of mix(id, updated, content))` — a
//!   commutative sum, so one edited row updates its bucket without touching the others. A table of 1 M rows is ≈ 2 000 buckets ≈ 40 KB.
//!   Tombstones (deleted rows) are part of the sum, so a deletion is distinguishable from "never had it".
//! * **`BigDigest`** (per table, 1 500 buckets per frame) is sent after the entity digest; the receiver compares it with its own index and
//!   asks for the buckets that differ (**`BigGet`**). Both sides do this, so the exchange repairs in both directions.
//! * **`BigChunk`** carries the rows of a bucket (`lumen_store::encode_chunk`, base64) plus the tombstones. Rows merge last-writer-wins by
//!   `updated`, ties broken by a content hash, a tombstone beats a row with the same or an older stamp, a newer row beats an older tombstone.
//! * **Live edits** are recorded by `bigtable::note_write / note_delete`, and go out as small `BigChunk`s (`b = LIVE`) to everybody; the host
//!   relays what it accepted to the other devices (never back to the sender).
//! * **Gates** are those of `Ops`: viewers' and suggesters' rows are dropped by the host; a host only serves and accepts tables that are in
//!   the scope of the share; a guest can never make the host create a store. A joiner creates a store for a table whose (empty) database
//!   page it already has.
//! * **Pacing.** Buckets are produced lazily, two per connection per frame while that connection's backlog is short, so a million-row table
//!   neither floods the socket nor the memory; the first full index of a big table is built on a worker thread (native).
use super::{random_u64, Collab, Mode, Msg, Role};
use crate::bigtable as bt;
use crate::model::{BigMeta, PageKind, Workspace};
use lumen_store::{decode_chunk, encode_chunk, Row, Store, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::sync::Mutex;

/// Row ids per digest bucket.
pub const BUCKET: u64 = 512;
/// `BigChunk::b` of a live edit (not an answer to `BigGet`).
pub const LIVE: u64 = u64::MAX;
const MAX_B64: usize = 6 << 20;
const MAX_ROWS: usize = 2_000;
const MAX_DEAD: usize = 4_000;
const DIGEST_FRAME: usize = 1_500;
const MAX_DIGEST: usize = 400_000;
const MAX_SERVE: usize = 300_000;

pub fn bucket_of(id: u64) -> u64 {
    id / BUCKET
}

pub fn range_of(b: u64) -> (u64, u64) {
    (b.saturating_mul(BUCKET), b.saturating_add(1).saturating_mul(BUCKET))
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
}

/// Hash of everything but the id and the stamp: breaks the tie of two edits in the same millisecond identically on every device.
pub fn content_hash(r: &Row) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    fnv(&mut h, r.title.as_bytes());
    for (p, v) in &r.props {
        fnv(&mut h, &p.to_le_bytes());
        match v {
            Value::Text(s) => {
                fnv(&mut h, &[0]);
                fnv(&mut h, s.as_bytes());
            }
            Value::Num(n) => {
                fnv(&mut h, &[1]);
                fnv(&mut h, &n.to_bits().to_le_bytes());
            }
            Value::Sel(x) => {
                fnv(&mut h, &[2]);
                fnv(&mut h, &x.to_le_bytes());
            }
            Value::Multi(xs) => {
                fnv(&mut h, &[3]);
                for x in xs {
                    fnv(&mut h, &x.to_le_bytes());
                }
            }
            Value::Check(b) => fnv(&mut h, &[4, *b as u8]),
        }
    }
    h
}

fn row_sig(r: &Row) -> u64 {
    mix(r.id ^ mix(r.updated) ^ mix(content_hash(r).rotate_left(17)))
}

fn dead_sig(id: u64, ms: u64) -> u64 {
    mix(id ^ mix(ms | (1 << 63)) ^ 0x5151)
}

/// `(live rows, signature)` per non-empty bucket.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Index {
    pub buckets: BTreeMap<u64, (u32, u64)>,
}

impl Index {
    fn add_row(&mut self, r: &Row) {
        let e = self.buckets.entry(bucket_of(r.id)).or_default();
        e.0 += 1;
        e.1 = e.1.wrapping_add(row_sig(r));
    }

    fn add_dead(&mut self, id: u64, ms: u64) {
        let e = self.buckets.entry(bucket_of(id)).or_default();
        e.1 = e.1.wrapping_add(dead_sig(id, ms));
    }

    pub fn rows(&self) -> u64 {
        self.buckets.values().map(|b| b.0 as u64).sum()
    }

    pub fn items(&self) -> Vec<(u64, u32, u64)> {
        self.buckets.iter().map(|(b, (n, h))| (*b, *n, *h)).collect()
    }
}

/// Full scan (one pass over the chunks, the store is not locked while decoding) — the first digest of a table.
pub fn build_all(st: &Mutex<Store>, dir: &str) -> io::Result<Index> {
    let mut idx = Index::default();
    lumen_store::shared_scan(st, |bytes| {
        for r in decode_chunk(bytes)? {
            idx.add_row(&r);
        }
        Ok(())
    })?;
    for (id, ms) in bt::grave_in(dir, 0, u64::MAX) {
        idx.add_dead(id, ms);
    }
    Ok(idx)
}

/// Re-reads one bucket (after an edit).
pub fn recompute(st: &Mutex<Store>, dir: &str, idx: &mut Index, b: u64) -> io::Result<()> {
    let (lo, hi) = range_of(b);
    let rows = st.lock().unwrap_or_else(|e| e.into_inner()).rows_in(lo, hi)?;
    let mut one = Index::default();
    for r in &rows {
        one.add_row(r);
    }
    for (id, ms) in bt::grave_in(dir, lo, hi) {
        one.add_dead(id, ms);
    }
    match one.buckets.remove(&b) {
        Some(v) if v != (0, 0) => {
            idx.buckets.insert(b, v);
        }
        _ => {
            idx.buckets.remove(&b);
        }
    }
    Ok(())
}

/// Buckets whose signature differs from ours (or that we do not have).
pub fn wanted(local: &Index, remote: &[(u64, u32, u64)]) -> Vec<u64> {
    remote.iter().filter(|(b, n, h)| local.buckets.get(b).map_or(true, |l| *l != (*n, *h))).map(|r| r.0).collect()
}

pub fn pack(rows: &[Row]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    crate::media::b64_encode(&encode_chunk(rows).0)
}

pub fn unpack(s: &str) -> Option<Vec<Row>> {
    if s.is_empty() {
        return Some(vec![]);
    }
    if s.len() > MAX_B64 {
        return None;
    }
    decode_chunk(&crate::media::b64_decode(s)?).ok()
}

/// What a bucket holds right now: rows (id order) and tombstones.
pub fn bucket_content(st: &Mutex<Store>, dir: &str, b: u64) -> io::Result<(Vec<Row>, Vec<(u64, u64)>)> {
    let (lo, hi) = range_of(b);
    let rows = st.lock().unwrap_or_else(|e| e.into_inner()).rows_in(lo, hi)?;
    Ok((rows, bt::grave_in(dir, lo, hi)))
}

/// Rows and tombstones that were accepted by a merge.
#[derive(Default, Debug)]
pub struct Accepted {
    pub rows: Vec<Row>,
    pub dead: Vec<(u64, u64)>,
}

/// Last-writer-wins merge of remote rows / tombstones into the local store. Returns what changed locally.
pub fn merge(st: &mut Store, dir: &str, mut rows: Vec<Row>, dead: &[(u64, u64)]) -> io::Result<Accepted> {
    let mut acc = Accepted::default();
    rows.sort_by_key(|r| r.id);
    rows.dedup_by_key(|r| r.id);
    for r in rows {
        let local = st.get(r.id)?;
        let take = match (&local, bt::grave_get(dir, r.id)) {
            (Some(l), _) => (r.updated, content_hash(&r)) > (l.updated, content_hash(l)),
            (None, Some(ms)) => r.updated > ms,
            (None, None) => true,
        };
        if take {
            st.upsert(r.clone())?;
            bt::grave_drop(dir, r.id);
            acc.rows.push(r);
        }
    }
    for &(id, ms) in dead {
        let local = st.get(id)?;
        let take = match &local {
            Some(l) => ms >= l.updated,
            None => bt::grave_get(dir, id).map_or(true, |g| ms > g),
        };
        if take {
            if local.is_some() {
                st.delete(id)?;
            }
            bt::grave_put(dir, id, ms);
            acc.dead.push((id, ms));
        }
    }
    Ok(acc)
}

/// Per-table session state.
#[derive(Default)]
pub struct Tbl {
    pub dir: String,
    pub idx: Option<Index>,
    /// Buckets edited since the index was built.
    pub stale: BTreeSet<u64>,
    #[cfg(not(target_arch = "wasm32"))]
    pub building: Option<std::sync::mpsc::Receiver<io::Result<Index>>>,
}

type Waiting = (u64, u64, Vec<(u64, u32, u64)>, f64);

#[derive(Default)]
pub struct BigState {
    pub tables: HashMap<u64, Tbl>,
    /// Digest frames received so far, per (connection, table).
    pub incoming: HashMap<(u64, u64), Vec<(u64, u32, u64)>>,
    /// Complete digests of tables whose database page has not arrived yet: (conn, table, items, since).
    pub waiting: Vec<Waiting>,
    /// Digests we still owe: (conn, table).
    pub owed: VecDeque<(u64, u64)>,
    /// Buckets a peer asked for, per connection: (table, bucket).
    pub serve: HashMap<u64, VecDeque<(u64, u64)>>,
    /// Buckets we asked for and have not received.
    pub pending: u64,
    /// Rows received / sent in this session.
    pub rows_in: u64,
    pub rows_out: u64,
    /// The workspace gained a store (the app has to save it).
    pub changed: bool,
    pub had_pending: bool,
}

impl Collab {
    /// Directory and store of a table this device may talk about; `create` lets a joiner make a store for a database page it has.
    fn big_open(&mut self, ws: &mut Workspace, table: u64, create: bool) -> Option<(String, bt::Shared)> {
        if let Some(m) = ws.bigs.get(&table).cloned() {
            if self.mode == Mode::Hosting && !self.rep.as_ref().map_or(false, |r| r.page_in_scope(ws, table)) {
                return None;
            }
            let st = bt::open_store(&m, true)?;
            return Some((m.dir, st));
        }
        if create && self.mode == Mode::Joined && ws.page(table).map_or(false, |p| p.kind == PageKind::Database) {
            let meta = BigMeta { dir: format!("collab-{:x}-{table}", random_u64()), volatile: cfg!(target_arch = "wasm32") };
            let st = bt::open_store(&meta, true)?;
            ws.bigs.insert(table, meta.clone());
            self.big.changed = true;
            return Some((meta.dir, st));
        }
        None
    }

    /// Queues the digests of every shared big table for `conn` (called when the entity digest of that connection is complete).
    pub(super) fn big_announce(&mut self, ws: &Workspace, conn: u64) {
        let ids: Vec<u64> = ws.bigs.keys().copied().filter(|t| self.mode != Mode::Hosting || self.rep.as_ref().map_or(false, |r| r.page_in_scope(ws, *t))).collect();
        for t in ids {
            if !self.big.owed.contains(&(conn, t)) {
                self.big.owed.push_back((conn, t));
            }
        }
    }

    fn big_ready(&mut self, table: u64) -> bool {
        let ctx = self.ctx.clone();
        let Some(t) = self.big.tables.get_mut(&table) else {
            return false;
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(rx) = &t.building {
            match rx.try_recv() {
                Ok(Ok(idx)) => {
                    t.idx = Some(idx);
                    t.building = None;
                }
                Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    t.building = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            }
        }
        let dir = t.dir.clone();
        let Some(st) = bt::open_store(&BigMeta { dir: dir.clone(), volatile: false }, true) else {
            return false;
        };
        if t.idx.is_none() {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let big = st.lock().map(|mut s| s.len().unwrap_or(0) > 30_000).unwrap_or(false);
                if big {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let (st2, dir2) = (st.clone(), dir.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(build_all(&st2, &dir2));
                        ctx.request_repaint();
                    });
                    t.building = Some(rx);
                    return false;
                }
            }
            match build_all(&st, &dir) {
                Ok(i) => t.idx = Some(i),
                Err(_) => return false,
            }
        }
        // fold the edits made since
        if !t.stale.is_empty() {
            if let Some(idx) = t.idx.as_mut() {
                for b in std::mem::take(&mut t.stale).into_iter().take(4000) {
                    let _ = recompute(&st, &dir, idx, b);
                }
            }
        }
        true
    }

    fn big_table(&mut self, table: u64, dir: &str) {
        let t = self.big.tables.entry(table).or_default();
        if t.dir != dir {
            *t = Tbl { dir: dir.to_owned(), ..Default::default() };
        }
    }

    fn big_send(&mut self, conn: u64, m: Msg) {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.active {
                c.backlog.push_back(m);
            }
        }
    }

    pub(super) fn big_on_digest(&mut self, ws: &mut Workspace, conn: u64, table: u64, items: Vec<(u64, u32, u64)>, last: bool) {
        if self.big.incoming.len() > 64 && !self.big.incoming.contains_key(&(conn, table)) {
            return;
        }
        let acc = self.big.incoming.entry((conn, table)).or_default();
        if acc.len() + items.len() > MAX_DIGEST {
            self.big.incoming.remove(&(conn, table));
            self.drop_conn(conn, "Digest too large");
            return;
        }
        acc.extend(items);
        if !last {
            return;
        }
        let items = self.big.incoming.remove(&(conn, table)).unwrap_or_default();
        self.big_compare(ws, conn, table, items, mono_secs());
    }

    fn big_compare(&mut self, ws: &mut Workspace, conn: u64, table: u64, items: Vec<(u64, u32, u64)>, since: f64) {
        let Some((dir, _)) = self.big_open(ws, table, true) else {
            // the page may still be on its way (entities are sent before the rows)
            if self.mode == Mode::Joined && self.big.waiting.len() < 32 && ws.page(table).is_none() {
                self.big.waiting.push((conn, table, items, since));
            }
            return;
        };
        self.big_table(table, &dir);
        if !self.big_ready(table) {
            self.big.waiting.push((conn, table, items, since));
            return;
        }
        let want = self.big.tables.get(&table).and_then(|t| t.idx.as_ref()).map(|i| wanted(i, &items)).unwrap_or_default();
        self.big.pending += want.len() as u64;
        if !want.is_empty() {
            self.big.had_pending = true;
        }
        for part in want.chunks(256) {
            self.big_send(conn, Msg::BigGet { table, buckets: part.to_vec() });
        }
    }

    pub(super) fn big_on_get(&mut self, ws: &mut Workspace, conn: u64, table: u64, buckets: Vec<u64>) {
        if buckets.len() > 256 || self.big_open(ws, table, false).is_none() {
            return;
        }
        let q = self.big.serve.entry(conn).or_default();
        for b in buckets {
            if q.len() < MAX_SERVE {
                q.push_back((table, b));
            }
        }
    }

    pub(super) fn big_on_chunk(&mut self, ws: &mut Workspace, conn: u64, table: u64, b: u64, rows: String, dead: Vec<(u64, u64)>) {
        // the same gates as `Ops`
        let role = self.conns.get(&conn).map(|c| c.role);
        if self.mode == Mode::Hosting && role != Some(Role::Editor) {
            return;
        }
        if dead.len() > MAX_DEAD {
            return;
        }
        let Some(rows) = unpack(&rows) else { return };
        if rows.len() > MAX_ROWS {
            return;
        }
        let Some((dir, st)) = self.big_open(ws, table, false) else {
            return;
        };
        self.big_table(table, &dir);
        if b != LIVE {
            self.big.pending = self.big.pending.saturating_sub(1);
        }
        self.big.rows_in += rows.len() as u64;
        let acc = match merge(&mut st.lock().unwrap_or_else(|e| e.into_inner()), &dir, rows, &dead) {
            Ok(a) => a,
            Err(_) => return,
        };
        if acc.rows.is_empty() && acc.dead.is_empty() {
            return;
        }
        if let Some(t) = self.big.tables.get_mut(&table) {
            for r in &acc.rows {
                t.stale.insert(bucket_of(r.id));
            }
            for (id, _) in &acc.dead {
                t.stale.insert(bucket_of(*id));
            }
        }
        bt::bump_version(&dir);
        if self.mode == Mode::Hosting {
            // the star: relay what changed here to everybody else
            let ids: Vec<u64> = self.conns.iter().filter(|(k, c)| **k != conn && c.active).map(|(k, _)| *k).collect();
            for part in acc.rows.chunks(500) {
                for id in &ids {
                    self.big_send(*id, Msg::BigChunk { table, b: LIVE, rows: pack(part), dead: vec![] });
                }
            }
            for part in acc.dead.chunks(2000) {
                for id in &ids {
                    self.big_send(*id, Msg::BigChunk { table, b: LIVE, rows: String::new(), dead: part.to_vec() });
                }
            }
        }
    }

    /// Per frame: owed digests, answers to `BigGet`, local edits → live chunks, retries of digests that waited for their page.
    pub(super) fn big_tick(&mut self, ws: &mut Workspace, now: f64) -> bool {
        let live = matches!(self.mode, Mode::Hosting | Mode::Joined);
        if live != self.big_tracking {
            self.big_tracking = live;
            bt::track(live);
        }
        if !live {
            if self.big.tables.len() + self.big.owed.len() + self.big.serve.len() > 0 {
                self.big = BigState::default();
            }
            return false;
        }
        // 1. retries
        if !self.big.waiting.is_empty() {
            let list = std::mem::take(&mut self.big.waiting);
            for (conn, table, items, since) in list {
                if mono_secs() - since > 90.0 || !self.conns.contains_key(&conn) {
                    continue;
                }
                self.big_compare(ws, conn, table, items, since);
            }
        }
        let _ = now;
        // 2. local edits
        let mine: Vec<&str> = ws.bigs.values().map(|m| m.dir.as_str()).collect();
        let dirty = bt::take_dirty(&mine);
        if !dirty.is_empty() {
            let may_write = self.mode == Mode::Hosting || (self.role != Role::Viewer && self.role != Role::Suggester);
            for (dir, ids) in dirty {
                let Some(table) = ws.bigs.iter().find(|(_, m)| m.dir == dir).map(|(t, _)| *t) else {
                    continue;
                };
                self.big_table(table, &dir);
                let Some(st) = bt::open_store(&BigMeta { dir: dir.clone(), volatile: false }, true) else {
                    continue;
                };
                if ids.contains(&bt::OVERFLOW) {
                    if let Some(t) = self.big.tables.get_mut(&table) {
                        t.idx = None;
                    }
                    let conns: Vec<u64> = self.conns.iter().filter(|(_, c)| c.active).map(|(k, _)| *k).collect();
                    for c in conns {
                        if !self.big.owed.contains(&(c, table)) {
                            self.big.owed.push_back((c, table));
                        }
                    }
                    continue;
                }
                let mut ids = ids;
                ids.sort_unstable();
                ids.dedup();
                let (mut rows, mut dead) = (vec![], vec![]);
                {
                    let mut g = st.lock().unwrap_or_else(|e| e.into_inner());
                    for id in &ids {
                        match g.get(*id) {
                            Ok(Some(r)) => rows.push(r),
                            _ => {
                                if let Some(ms) = bt::grave_get(&dir, *id) {
                                    dead.push((*id, ms));
                                }
                            }
                        }
                    }
                }
                if let Some(t) = self.big.tables.get_mut(&table) {
                    t.stale.extend(ids.iter().map(|i| bucket_of(*i)));
                }
                if !may_write {
                    continue;
                }
                self.big.rows_out += rows.len() as u64;
                let conns: Vec<u64> = self.conns.iter().filter(|(_, c)| c.active).map(|(k, _)| *k).collect();
                for part in rows.chunks(500) {
                    for c in &conns {
                        self.big_send(*c, Msg::BigChunk { table, b: LIVE, rows: pack(part), dead: vec![] });
                    }
                }
                for part in dead.chunks(2000) {
                    for c in &conns {
                        self.big_send(*c, Msg::BigChunk { table, b: LIVE, rows: String::new(), dead: part.to_vec() });
                    }
                }
            }
        }
        // 3. digests we owe
        let mut keep = VecDeque::new();
        while let Some((conn, table)) = self.big.owed.pop_front() {
            if !self.conns.get(&conn).map_or(false, |c| c.active) {
                continue;
            }
            let Some((dir, _)) = self.big_open(ws, table, false) else {
                continue;
            };
            self.big_table(table, &dir);
            if !self.big_ready(table) {
                keep.push_back((conn, table));
                continue;
            }
            let items = self.big.tables.get(&table).and_then(|t| t.idx.as_ref()).map(|i| i.items()).unwrap_or_default();
            let n = items.len().div_ceil(DIGEST_FRAME).max(1);
            for i in 0..n {
                let part = items.iter().skip(i * DIGEST_FRAME).take(DIGEST_FRAME).copied().collect();
                self.big_send(conn, Msg::BigDigest { table, items: part, last: i + 1 == n });
            }
        }
        self.big.owed = keep;
        // 4. answers to BigGet, lazily
        let conns: Vec<u64> = self.big.serve.keys().copied().collect();
        for conn in conns {
            for _ in 0..2 {
                let short = self.conns.get(&conn).map_or(false, |c| c.active && c.backlog.len() < 4);
                if !short {
                    break;
                }
                let Some((table, b)) = self.big.serve.get_mut(&conn).and_then(|q| q.pop_front()) else {
                    break;
                };
                let Some((dir, st)) = self.big_open(ws, table, false) else {
                    continue;
                };
                if let Ok((rows, dead)) = bucket_content(&st, &dir, b) {
                    self.big.rows_out += rows.len() as u64;
                    self.big_send(conn, Msg::BigChunk { table, b, rows: pack(&rows), dead });
                }
            }
        }
        let alive: HashSet<u64> = self.conns.keys().copied().collect();
        self.big.serve.retain(|c, q| !q.is_empty() && alive.contains(c));
        if self.big.had_pending && self.big.pending == 0 && self.big.owed.is_empty() && self.big.waiting.is_empty() {
            self.big.had_pending = false;
            let n = self.big.rows_in;
            self.toast(format!("Big tables are in sync ({n} rows received)"));
        }
        std::mem::take(&mut self.big.changed)
    }

    /// Short line for the status area: what the table sync is doing.
    pub fn big_status(&self) -> Option<String> {
        if self.big.pending > 0 {
            Some(format!("Syncing big tables: {} blocks of rows to go", self.big.pending))
        } else {
            None
        }
    }
}

use std::collections::HashSet;

fn mono_secs() -> f64 {
    super::mono_ms() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_store::MemSource;

    fn row(id: u64, upd: u64, t: &str) -> Row {
        Row { id, title: t.into(), updated: upd, props: vec![(1, Value::Num(id as f64))] }
    }

    fn store() -> Store {
        Store::open(Box::new(MemSource::default()), 8 << 20).unwrap()
    }

    #[test]
    fn newer_rows_win_and_ties_are_deterministic() {
        let mut a = store();
        a.upsert(row(1, 100, "old")).unwrap();
        let acc = merge(&mut a, "t-a1", vec![row(1, 200, "new")], &[]).unwrap();
        assert_eq!(acc.rows.len(), 1);
        assert_eq!(a.get(1).unwrap().unwrap().title, "new");
        let acc = merge(&mut a, "t-a1", vec![row(1, 100, "old")], &[]).unwrap();
        assert!(acc.rows.is_empty(), "an older copy must not overwrite");
        // same millisecond, different content: both sides pick the same winner
        let (mut x, mut y) = (store(), store());
        x.upsert(row(7, 5, "x")).unwrap();
        y.upsert(row(7, 5, "y")).unwrap();
        merge(&mut x, "t-x", vec![row(7, 5, "y")], &[]).unwrap();
        merge(&mut y, "t-y", vec![row(7, 5, "x")], &[]).unwrap();
        assert_eq!(x.get(7).unwrap().unwrap().title, y.get(7).unwrap().unwrap().title);
    }

    #[test]
    fn tombstones_beat_older_rows_and_lose_to_newer_ones() {
        let mut a = store();
        a.upsert(row(3, 100, "keep?")).unwrap();
        let acc = merge(&mut a, "t-tomb", vec![], &[(3, 150)]).unwrap();
        assert_eq!(acc.dead.len(), 1);
        assert!(a.get(3).unwrap().is_none());
        assert_eq!(bt::grave_get("t-tomb", 3), Some(150));
        // an older copy arriving later does not resurrect the row …
        assert!(merge(&mut a, "t-tomb", vec![row(3, 120, "stale")], &[]).unwrap().rows.is_empty());
        assert!(a.get(3).unwrap().is_none());
        // … a newer edit (re-created on another device) does
        assert_eq!(merge(&mut a, "t-tomb", vec![row(3, 300, "back")], &[]).unwrap().rows.len(), 1);
        assert_eq!(a.get(3).unwrap().unwrap().title, "back");
        assert_eq!(bt::grave_get("t-tomb", 3), None);
    }

    #[test]
    fn two_stores_converge_through_digest_and_chunks() {
        let (mut a, mut b) = (store(), store());
        for i in 1..=3000u64 {
            a.upsert(row(i, 1000 + i, &format!("a{i}"))).unwrap();
        }
        // b has most of it, some older, one extra of its own, and a deletion a does not know about
        for i in 1..=2500u64 {
            b.upsert(row(i, if i % 100 == 0 { 10 } else { 1000 + i }, &format!("a{i}"))).unwrap();
        }
        b.upsert(row(9_000_000, 5000, "only-b")).unwrap();
        bt::grave_put("t-conv-b", 2999, 1_000_000);
        let (am, bm) = (Mutex::new(a), Mutex::new(b));
        for _ in 0..2 {
            let (ia, ib) = (build_all(&am, "t-conv-a").unwrap(), build_all(&bm, "t-conv-b").unwrap());
            for (from, from_dir, to, to_dir, ifrom, ito) in [(&am, "t-conv-a", &bm, "t-conv-b", &ia, &ib), (&bm, "t-conv-b", &am, "t-conv-a", &ib, &ia)] {
                for bkt in wanted(ito, &ifrom.items()) {
                    let (rows, dead) = bucket_content(from, from_dir, bkt).unwrap();
                    let rows = unpack(&pack(&rows)).unwrap();
                    merge(&mut to.lock().unwrap(), to_dir, rows, &dead).unwrap();
                }
            }
        }
        let (ia, ib) = (build_all(&am, "t-conv-a").unwrap(), build_all(&bm, "t-conv-b").unwrap());
        assert_eq!(ia, ib, "digests must be identical after one exchange");
        assert!(am.lock().unwrap().get(2999).unwrap().is_none(), "the deletion travelled");
        assert!(am.lock().unwrap().get(9_000_000).unwrap().is_some());
        assert_eq!(bm.lock().unwrap().get(100).unwrap().unwrap().updated, 1100, "older rows were refreshed");
        assert!(wanted(&ia, &ib.items()).is_empty());
    }

    #[test]
    fn an_edited_bucket_is_recomputed_exactly() {
        let mut s = store();
        for i in 1..=2000u64 {
            s.upsert(row(i, 10 + i, "x")).unwrap();
        }
        let m = Mutex::new(s);
        let mut idx = build_all(&m, "t-inc").unwrap();
        m.lock().unwrap().upsert(row(700, 99_999, "edited")).unwrap();
        m.lock().unwrap().delete(1500).unwrap();
        bt::grave_put("t-inc", 1500, 42);
        recompute(&m, "t-inc", &mut idx, bucket_of(700)).unwrap();
        recompute(&m, "t-inc", &mut idx, bucket_of(1500)).unwrap();
        assert_eq!(idx, build_all(&m, "t-inc").unwrap());
    }

    #[test]
    fn hostile_chunks_are_refused_not_trusted() {
        assert!(unpack("!!!not base64!!!").is_none());
        assert!(unpack(&crate::media::b64_encode(b"garbage that is not a chunk")).is_none());
        assert!(unpack(&"A".repeat(MAX_B64 + 1)).is_none());
        assert_eq!(unpack("").unwrap().len(), 0);
        assert_eq!(build_all(&Mutex::new(store()), "t-empty").unwrap(), Index::default());
    }

    /// 1 M rows: the first digest, an incremental update and a bucket transfer stay cheap. `cargo test --release --lib soak -- --ignored`
    #[test]
    #[ignore]
    fn soak_one_million_rows() {
        let mut s = store();
        s.bulk_append((1..=1_000_000u64).map(|i| row(i, 1_700_000_000_000 + i, "r"))).unwrap();
        let m = Mutex::new(s);
        let t = std::time::Instant::now();
        let mut idx = build_all(&m, "t-soak").unwrap();
        let full = t.elapsed();
        assert_eq!(idx.rows(), 1_000_000);
        assert_eq!(idx.buckets.len(), (1_000_000 / BUCKET as usize) + 1);
        let t = std::time::Instant::now();
        m.lock().unwrap().upsert(row(500_000, 1_800_000_000_000, "edit")).unwrap();
        recompute(&m, "t-soak", &mut idx, bucket_of(500_000)).unwrap();
        let inc = t.elapsed();
        let t = std::time::Instant::now();
        let (rows, dead) = bucket_content(&m, "t-soak", 100).unwrap();
        let wire = pack(&rows);
        let chunk = t.elapsed();
        eprintln!("1M rows: full digest {full:?}, one bucket {inc:?}, chunk of {} rows = {} bytes in {chunk:?}, index {} buckets", rows.len(), wire.len(), idx.buckets.len());
        assert!(dead.is_empty() && rows.len() == BUCKET as usize);
        assert!(inc.as_millis() < 250 && chunk.as_millis() < 250);
    }
}
