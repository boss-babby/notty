//! A real text CRDT (RGA — replicated growable array) for the characters of one block.
//!
//! * Every character has a unique id `(counter, replica)`; an insertion names the character it was typed *after* (`origin`). Two people who
//!   type at the same place get a deterministic order (bigger id first) on every device, and a deletion is a tombstone, so any delivery
//!   order of the same set of operations gives the same text (convergence), duplicates are ignored (idempotence), and an operation whose
//!   origin has not arrived yet waits in `pending` (causal delivery).
//! * [`Doc::set_text`] is the **editor adapter**: the editor owns a plain `String` (egui's `TextEdit`), the adapter diffs the previous and the
//!   new string (common prefix / suffix, then a bounded character LCS for several separate edits) and returns the minimal operations.
//! * [`Doc::anchor`] / [`Doc::index_of`] give carets that survive remote edits (a caret is an id, not an offset).
//! * [`merge3`] merges two edits of the same base text through the CRDT: both sides derive the same replica ids from the content, so the
//!   result is identical whichever device computes it. `textmerge::merge3` uses it for every conflict.
//!
//! Cost: ~24 bytes per character while the document is alive, linear scans (blocks are paragraphs, not books); `compact` drops tombstones
//! that every replica has seen (the caller decides when that is true, e.g. after a session ends).
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// `(counter, replica)`, ordered by counter first (Lamport order) then replica.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug, Default,
)]
pub struct Id {
    pub c: u32,
    pub r: u32,
}

/// The virtual start of every document.
pub const ROOT: Id = Id { c: 0, r: 0 };

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(tag = "k")]
pub enum TextOp {
    /// `s` typed after `origin`; its characters have the ids `(id.c + i, id.r)`, each one after the previous.
    Ins { id: Id, origin: Id, s: String },
    /// The `n` characters `(id.c + i, id.r)` were deleted.
    Del { id: Id, n: u32 },
}

#[derive(Clone, Debug)]
struct Item {
    id: Id,
    ch: char,
    origin: Id,
    dead: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Doc {
    rep: u32,
    clock: u32,
    items: Vec<Item>,
    pending: Vec<TextOp>,
    /// Visible characters (cache).
    live: usize,
}

impl Doc {
    pub fn new(rep: u32) -> Doc {
        Doc {
            rep: rep.max(1),
            ..Doc::default()
        }
    }

    /// Every device that builds a document from the same base text gets identical items (replica 0), which is what makes a three-way merge
    /// and a late joiner agree on the common ancestor.
    pub fn from_text(rep: u32, base: &str) -> Doc {
        let mut d = Doc::new(rep);
        let mut prev = ROOT;
        for (i, ch) in base.chars().enumerate() {
            let id = Id {
                c: i as u32 + 1,
                r: 0,
            };
            d.items.push(Item {
                id,
                ch,
                origin: prev,
                dead: false,
            });
            prev = id;
        }
        d.live = d.items.len();
        d.clock = d.items.len() as u32;
        d
    }

    pub fn replica(&self) -> u32 {
        self.rep
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn text(&self) -> String {
        self.items
            .iter()
            .filter(|i| !i.dead)
            .map(|i| i.ch)
            .collect()
    }

    /// Operations waiting for their origin.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn find(&self, id: Id) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    /// The id of the character before visible index `at` (`ROOT` at 0).
    fn origin_for(&self, at: usize) -> Id {
        if at == 0 {
            return ROOT;
        }
        let mut seen = 0;
        for it in &self.items {
            if !it.dead {
                seen += 1;
                if seen == at {
                    return it.id;
                }
            }
        }
        self.items.last().map_or(ROOT, |i| i.id)
    }

    // ───────────── local edits ─────────────

    pub fn insert(&mut self, at: usize, s: &str) -> Option<TextOp> {
        let n = s.chars().count();
        if n == 0 {
            return None;
        }
        let at = at.min(self.live);
        let origin = self.origin_for(at);
        let id = Id {
            c: self.clock + 1,
            r: self.rep,
        };
        self.clock += n as u32;
        let op = TextOp::Ins {
            id,
            origin,
            s: s.to_owned(),
        };
        self.integrate_ins(id, origin, s);
        Some(op)
    }

    /// Deletes `n` visible characters from `at`; one operation per run of consecutive ids.
    pub fn delete(&mut self, at: usize, n: usize) -> Vec<TextOp> {
        let mut ops: Vec<TextOp> = vec![];
        let mut seen = 0;
        let mut left = n;
        for it in self.items.iter_mut() {
            if left == 0 {
                break;
            }
            if it.dead {
                continue;
            }
            if seen >= at {
                it.dead = true;
                self.live -= 1;
                left -= 1;
                match ops.last_mut() {
                    Some(TextOp::Del { id, n }) if id.r == it.id.r && id.c + *n == it.id.c => {
                        *n += 1
                    }
                    _ => ops.push(TextOp::Del { id: it.id, n: 1 }),
                }
            }
            seen += 1;
        }
        ops
    }

    /// Editor adapter: makes the document equal to `new` and returns the operations that did it (empty = no change).
    pub fn set_text(&mut self, new: &str) -> Vec<TextOp> {
        let old: Vec<char> = self
            .items
            .iter()
            .filter(|i| !i.dead)
            .map(|i| i.ch)
            .collect();
        let new: Vec<char> = new.chars().collect();
        let edits = diff(&old, &new);
        // apply from the back so earlier indexes stay valid
        let mut ops = vec![];
        for (at, del, ins) in edits.into_iter().rev() {
            ops.extend(self.delete(at, del));
            if !ins.is_empty() {
                let s: String = ins.iter().collect();
                ops.extend(self.insert(at, &s));
            }
        }
        ops
    }

    // ───────────── remote operations ─────────────

    /// Applies an operation from any replica. Returns `false` when it had to wait for its origin.
    pub fn apply(&mut self, op: &TextOp) -> bool {
        let ok = self.try_apply(op);
        if !ok && self.pending.len() < 4096 {
            self.pending.push(op.clone());
        }
        if ok && !self.pending.is_empty() {
            self.drain_pending();
        }
        ok
    }

    fn drain_pending(&mut self) {
        loop {
            let before = self.pending.len();
            let list = std::mem::take(&mut self.pending);
            for op in list {
                if !self.try_apply(&op) {
                    self.pending.push(op);
                }
            }
            if self.pending.len() == before || self.pending.is_empty() {
                break;
            }
        }
    }

    fn try_apply(&mut self, op: &TextOp) -> bool {
        match op {
            TextOp::Ins { id, origin, s } => {
                let n = s.chars().count() as u32;
                if n == 0 || (id.r == 0 && id.c == 0) {
                    return true;
                }
                if *origin != ROOT && self.find(*origin).is_none() {
                    return false;
                }
                self.clock = self.clock.max(id.c.saturating_add(n - 1));
                self.integrate_ins(*id, *origin, s);
                true
            }
            TextOp::Del { id, n } => {
                let n = (*n).min(1 << 20);
                if n == 0 {
                    return true;
                }
                // every character must be known, otherwise wait
                let last = Id {
                    c: id.c.saturating_add(n - 1),
                    r: id.r,
                };
                if self.find(last).is_none() {
                    return false;
                }
                for k in 0..self.items.len() {
                    let it = &mut self.items[k];
                    if it.id.r == id.r
                        && it.id.c >= id.c
                        && it.id.c < id.c.saturating_add(n)
                        && !it.dead
                    {
                        it.dead = true;
                        self.live -= 1;
                    }
                }
                true
            }
        }
    }

    /// RGA integration of a run: each character after the previous one; characters that already exist (duplicate delivery) are skipped.
    fn integrate_ins(&mut self, id: Id, origin: Id, s: &str) {
        let mut origin = origin;
        for (i, ch) in s.chars().enumerate() {
            let cid = Id {
                c: id.c + i as u32,
                r: id.r,
            };
            if self.find(cid).is_some() {
                origin = cid;
                continue;
            }
            let mut pos = if origin == ROOT {
                0
            } else {
                self.find(origin).map_or(self.items.len(), |p| p + 1)
            };
            // skip concurrent siblings (and their subtrees) with a bigger id
            while pos < self.items.len() && self.items[pos].id > cid {
                pos += 1;
            }
            self.items.insert(
                pos,
                Item {
                    id: cid,
                    ch,
                    origin,
                    dead: false,
                },
            );
            self.live += 1;
            origin = cid;
        }
    }

    // ───────────── carets ─────────────

    /// A caret position that survives edits: the id of the character before it.
    pub fn anchor(&self, index: usize) -> Id {
        self.origin_for(index.min(self.live))
    }

    /// Visible index of an anchor (characters before the caret); deleted anchors resolve to the nearest visible predecessor.
    pub fn index_of(&self, anchor: Id) -> usize {
        if anchor == ROOT {
            return 0;
        }
        let Some(k) = self.find(anchor) else {
            return self.live;
        };
        self.items[..=k].iter().filter(|i| !i.dead).count()
    }

    /// Drops tombstones (only when every replica has seen every deletion). Returns how many were removed.
    pub fn compact(&mut self) -> usize {
        let referenced: std::collections::HashSet<Id> =
            self.items.iter().map(|i| i.origin).collect();
        let before = self.items.len();
        self.items.retain(|i| !i.dead || referenced.contains(&i.id));
        before - self.items.len()
    }

    pub fn slot_count(&self) -> usize {
        self.items.len()
    }
}

/// Edits turning `old` into `new`: `(index in old, characters removed, characters inserted)` in increasing order.
fn diff(old: &[char], new: &[char]) -> Vec<(usize, usize, Vec<char>)> {
    let mut p = 0;
    let max = old.len().min(new.len());
    while p < max && old[p] == new[p] {
        p += 1;
    }
    let mut s = 0;
    while s < max - p && old[old.len() - 1 - s] == new[new.len() - 1 - s] {
        s += 1;
    }
    let a = &old[p..old.len() - s];
    let b = &new[p..new.len() - s];
    if a.is_empty() && b.is_empty() {
        return vec![];
    }
    if a.is_empty() || b.is_empty() || a.len().saturating_mul(b.len()) > 1_500_000 {
        return vec![(p, a.len(), b.to_vec())];
    }
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut t = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if a[i] == b[j] {
                t[(i + 1) * w + j + 1].saturating_add(1)
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out: Vec<(usize, usize, Vec<char>)> = vec![];
    let mut cur: Option<(usize, usize, Vec<char>)> = None;
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            i += 1;
            j += 1;
        } else if j < m && (i == n || t[i * w + j + 1] >= t[(i + 1) * w + j]) {
            cur.get_or_insert((p + i, 0, vec![])).2.push(b[j]);
            j += 1;
        } else {
            cur.get_or_insert((p + i, 0, vec![])).1 += 1;
            i += 1;
        }
    }
    if let Some(c) = cur.take() {
        out.push(c);
    }
    out
}

fn fnv32(s: &str) -> u32 {
    let mut h = 0x811C_9DC5u32;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h | 1
}

/// Three-way merge through the CRDT: `a` and `b` are two edits of `base`. The result does not depend on the argument order.
pub fn merge3(base: &str, a: &str, b: &str) -> String {
    if a == b {
        return a.to_owned();
    }
    // the replica id of each side is derived from the content it produced; sort so that even a hash collision resolves the same way everywhere
    let (x, y) = if (fnv32(a), a) <= (fnv32(b), b) {
        (a, b)
    } else {
        (b, a)
    };
    let (rx, ry) = (fnv32(x), fnv32(y));
    let ry = if rx == ry { ry.wrapping_add(2) } else { ry };
    let mut dx = Doc::from_text(rx, base);
    let mut dy = Doc::from_text(ry, base);
    let ox = dx.set_text(x);
    let oy = dy.set_text(y);
    for op in &oy {
        dx.apply(op);
    }
    for op in &ox {
        dy.apply(op);
    }
    debug_assert_eq!(dx.text(), dy.text());
    dx.text()
}

/// The documents of the open blocks of a session (block id → document); the editor adapter lives here.
#[derive(Default)]
pub struct Hub {
    rep: u32,
    docs: HashMap<u64, Doc>,
}

impl Hub {
    pub fn new(rep: u32) -> Hub {
        Hub {
            rep,
            docs: HashMap::new(),
        }
    }

    /// The block text changed locally (what the editor produced): operations to broadcast.
    pub fn local(&mut self, block: u64, text: &str) -> Vec<TextOp> {
        let rep = self.rep;
        self.docs
            .entry(block)
            .or_insert_with(|| Doc::new(rep))
            .set_text(text)
    }

    /// Operations from a peer: the new text when something changed.
    pub fn remote(&mut self, block: u64, ops: &[TextOp]) -> Option<String> {
        let rep = self.rep;
        let d = self.docs.entry(block).or_insert_with(|| Doc::new(rep));
        let before = d.text();
        for o in ops {
            d.apply(o);
        }
        let after = d.text();
        (after != before).then_some(after)
    }

    pub fn text(&self, block: u64) -> Option<String> {
        self.docs.get(&block).map(|d| d.text())
    }

    pub fn doc(&mut self, block: u64) -> &mut Doc {
        let rep = self.rep;
        self.docs.entry(block).or_insert_with(|| Doc::new(rep))
    }

    pub fn forget(&mut self, block: u64) {
        self.docs.remove(&block);
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(a: &mut Doc, b: &mut Doc, oa: &[TextOp], ob: &[TextOp]) {
        for o in ob {
            a.apply(o);
        }
        for o in oa {
            b.apply(o);
        }
    }

    #[test]
    fn typing_at_the_same_place_converges() {
        let mut a = Doc::from_text(11, "hello");
        let mut b = Doc::from_text(22, "hello");
        let oa = a.set_text("helloAAA");
        let ob = b.set_text("helloBBB");
        sync(&mut a, &mut b, &oa, &ob);
        assert_eq!(a.text(), b.text());
        // runs are not interleaved
        assert!(
            a.text() == "helloAAABBB" || a.text() == "helloBBBAAA",
            "{}",
            a.text()
        );
    }

    #[test]
    fn delivery_order_and_duplicates_do_not_matter() {
        let mut a = Doc::from_text(1, "the cat sat");
        let mut b = Doc::from_text(2, "the cat sat");
        let mut c = Doc::from_text(3, "the cat sat");
        let oa = a.set_text("the big cat sat");
        let ob = b.set_text("the cat sat down");
        let oc = c.set_text("a cat sat");
        let all: Vec<&TextOp> = oa.iter().chain(&ob).chain(&oc).collect();
        let mut x = Doc::from_text(9, "the cat sat");
        let mut y = Doc::from_text(8, "the cat sat");
        for o in &all {
            x.apply(o);
        }
        for o in all.iter().rev() {
            y.apply(o);
            y.apply(o);
        }
        assert_eq!(x.text(), y.text());
        assert_eq!(y.pending(), 0);
        assert!(x.text().contains("big") && x.text().contains("down"));
    }

    #[test]
    fn a_remote_insert_into_text_that_was_deleted_survives_the_delete() {
        let mut a = Doc::from_text(1, "abcdef");
        let mut b = Doc::from_text(2, "abcdef");
        let oa = a.set_text("af"); // deletes bcde
        let ob = b.set_text("abcXdef");
        sync(&mut a, &mut b, &oa, &ob);
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "aXf");
    }

    #[test]
    fn operations_wait_for_their_origin() {
        let mut a = Doc::new(1);
        let o1 = a.insert(0, "ab").unwrap();
        let o2 = a.insert(2, "cd").unwrap();
        let mut b = Doc::new(2);
        assert!(!b.apply(&o2));
        assert_eq!(b.pending(), 1);
        assert!(b.apply(&o1));
        assert_eq!(b.pending(), 0);
        assert_eq!(b.text(), "abcd");
    }

    #[test]
    fn the_adapter_produces_minimal_operations_and_several_hunks() {
        let mut d = Doc::from_text(5, "one two three four");
        let ops = d.set_text("one 2 three 4");
        assert_eq!(d.text(), "one 2 three 4");
        assert!(ops.len() <= 4, "{ops:?}");
        assert!(d.set_text("one 2 three 4").is_empty());
        let mut e = Doc::from_text(6, "ünï — 日本");
        e.set_text("ünï — 日本語!");
        assert_eq!(e.text(), "ünï — 日本語!");
    }

    #[test]
    fn carets_follow_remote_edits() {
        let mut a = Doc::from_text(1, "hello world");
        let caret = a.anchor(5); // after "hello"
        let mut b = Doc::from_text(2, "hello world");
        let ob = b.set_text("oh, hello world");
        for o in &ob {
            a.apply(o);
        }
        assert_eq!(a.index_of(caret), 5 + 4);
        let od = a.set_text("oh,  world");
        assert!(!od.is_empty());
        assert_eq!(a.index_of(caret), 4);
    }

    #[test]
    fn merge3_is_symmetric_and_keeps_both_edits() {
        let cases = [
            ("hello world", "Hello world", "hello world!"),
            ("abcdef", "abXef", "abYef"),
            ("", "a", "b"),
            ("one", "one two", "zero one"),
            ("a b c", "a c", "a b c!"),
        ];
        for (base, x, y) in cases {
            let m = merge3(base, x, y);
            assert_eq!(m, merge3(base, y, x), "{base:?} {x:?} {y:?}");
        }
        assert_eq!(
            merge3("hello world", "Hello world", "hello world!"),
            "Hello world!"
        );
        assert_eq!(merge3("a b c", "a c", "a b c!"), "a c!");
    }

    #[test]
    fn random_sessions_converge() {
        let mut seed = 0x1234_5678u64;
        let mut rnd = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        for _round in 0..30 {
            let mut docs: Vec<Doc> = (1..=3).map(|r| Doc::from_text(r, "abcdefghij")).collect();
            let mut logs: Vec<Vec<TextOp>> = vec![vec![], vec![], vec![]];
            for (k, d) in docs.iter_mut().enumerate() {
                for _ in 0..6 {
                    if rnd(3) == 0 && !d.is_empty() {
                        let at = rnd(d.len());
                        logs[k].extend(d.delete(at, 1 + rnd(2)));
                    } else {
                        let at = rnd(d.len() + 1);
                        let s: String = (0..1 + rnd(3))
                            .map(|_| (b'A' + rnd(26) as u8) as char)
                            .collect();
                        logs[k].extend(d.insert(at, &s));
                    }
                }
            }
            let mut texts = vec![];
            for k in 0..3 {
                let mut d = docs[k].clone();
                let mut others: Vec<&TextOp> = (0..3)
                    .filter(|j| *j != k)
                    .flat_map(|j| logs[j].iter())
                    .collect();
                if k == 1 {
                    others.reverse();
                }
                for o in others {
                    d.apply(o);
                }
                assert_eq!(d.pending(), 0);
                texts.push(d.text());
            }
            assert!(texts.windows(2).all(|w| w[0] == w[1]), "{texts:?}");
        }
    }

    #[test]
    fn the_hub_round_trips_between_two_devices() {
        let mut ha = Hub::new(1);
        let mut hb = Hub::new(2);
        let o1 = ha.local(7, "Hello");
        assert_eq!(hb.remote(7, &o1).as_deref(), Some("Hello"));
        let o2 = hb.local(7, "Hello there");
        assert_eq!(ha.remote(7, &o2).as_deref(), Some("Hello there"));
        assert!(ha.remote(7, &o2).is_none());
        let mut d = Doc::from_text(3, "x");
        d.set_text("");
        assert!(d.compact() <= 1);
    }
}
