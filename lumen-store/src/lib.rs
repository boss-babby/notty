//! # lumen-store
//!
//! A small, dependency-free storage engine for database rows.
//!
//! * **Chunks** — rows are kept in id order in immutable *chunks* of ~4 096 rows. A chunk is column-oriented (ids delta-varint, titles,
//!   one column per property, text dictionary-coded) and carries a checksum. Chunks are only ever appended (copy-on-write).
//! * **Index** — the only thing read at start-up: per chunk offset, length, row count, id range and a *zone map* per numeric property
//!   (min, max, sum, count). Opening a table with a million rows reads ~100 KB, not the data.
//! * **Zone maps** — unfiltered aggregates are answered from the index alone (no chunk is read); filtered ones skip every chunk whose range
//!   cannot match.
//! * **WAL** — edits are appended to a log (checksummed records, torn tail tolerated) and held in a tiny overlay; `checkpoint` rewrites only the
//!   chunks that changed and swaps the index atomically.
//! * **Cache** — decoded chunks live in an LRU with a hard *byte budget* (`set_budget`) so RAM use is a setting, not a consequence of data size.
//!   Column scans (`scan_num`, `scan_sel`) never populate it: they decode one column of one chunk at a time.
//! * **Sources** — all I/O goes through [`Source`]; [`FileSource`] (native / Android), [`MemSource`] (tests, fallback) and — later — an
//!   OPFS / IndexedDB source for the browser or an object-store source for the cloud (chunks are immutable → perfect for HTTP range reads,
//!   CDN caching and content addressing; the WAL is the sync log).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;

pub const CHUNK_ROWS: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Num(f64),
    Sel(u64),
    Multi(Vec<u64>),
    Check(bool),
}

impl Value {
    fn kind(&self) -> u8 {
        match self {
            Value::Text(_) => 0,
            Value::Num(_) => 1,
            Value::Sel(_) => 2,
            Value::Multi(_) => 3,
            Value::Check(_) => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: u64,
    pub title: String,
    /// milliseconds since the epoch
    pub updated: u64,
    /// sorted by property id
    pub props: Vec<(u32, Value)>,
}

impl Row {
    pub fn get(&self, prop: u32) -> Option<&Value> {
        self.props.binary_search_by_key(&prop, |p| p.0).ok().map(|i| &self.props[i].1)
    }
    fn heap_estimate(&self) -> usize {
        64 + self.title.len()
            + self
                .props
                .iter()
                .map(|(_, v)| {
                    40 + match v {
                        Value::Text(s) => s.len(),
                        Value::Multi(m) => m.len() * 8,
                        _ => 0,
                    }
                })
                .sum::<usize>()
    }
}

// ───────────────────────────── varint / checksum ─────────────────────────────

fn put_var(b: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        b.push((v as u8) | 0x80);
        v >>= 7;
    }
    b.push(v as u8);
}

fn get_var(b: &[u8], p: &mut usize) -> Option<u64> {
    let (mut v, mut s) = (0u64, 0u32);
    loop {
        let x = *b.get(*p)?;
        *p += 1;
        v |= ((x & 0x7f) as u64) << s;
        if x < 0x80 {
            return Some(v);
        }
        s += 7;
        if s > 63 {
            return None;
        }
    }
}

/// Corruption check (not cryptographic): 8 bytes per step, ~8 GB/s — a byte-wise FNV was the bottleneck of every read.
fn fnv(b: &[u8]) -> u32 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15 ^ b.len() as u64;
    let mut it = b.chunks_exact(8);
    for w in &mut it {
        let x = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        h = (h ^ x).wrapping_mul(0xFF51_AFD7_ED55_8CCD).rotate_left(31);
    }
    for x in it.remainder() {
        h = (h ^ *x as u64).wrapping_mul(0x0100_0000_01B3);
    }
    h ^= h >> 32;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    (h >> 32) as u32 ^ h as u32
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

// ───────────────────────────── chunk codec ─────────────────────────────

/// Min / max / sum / count of a numeric property inside one chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    pub prop: u32,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub count: u32,
}

/// Encodes rows (ascending, unique ids) into one chunk and returns the bytes plus its zone maps.
pub fn encode_chunk(rows: &[Row]) -> (Vec<u8>, Vec<Zone>) {
    let n = rows.len();
    let (mut ids, mut upd, mut titles) = (Vec::new(), Vec::new(), Vec::new());
    let mut prev = 0u64;
    for (i, r) in rows.iter().enumerate() {
        put_var(&mut ids, if i == 0 { r.id } else { r.id - prev });
        prev = r.id;
        put_var(&mut upd, r.updated);
        put_var(&mut titles, r.title.len() as u64);
        titles.extend_from_slice(r.title.as_bytes());
    }
    let mut cols: BTreeMap<(u32, u8), Vec<(usize, &Value)>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        for (p, v) in &r.props {
            cols.entry((*p, v.kind())).or_default().push((i, v));
        }
    }
    let mut zones = vec![];
    let mut col_bytes: Vec<((u32, u8), Vec<u8>)> = vec![];
    for ((prop, kind), vals) in cols {
        let mut b = vec![0u8; n.div_ceil(8)];
        for (i, _) in &vals {
            b[i / 8] |= 1 << (i % 8);
        }
        match kind {
            0 => {
                let mut dict: Vec<&str> = vec![];
                let mut index: HashMap<&str, u64> = HashMap::new();
                let mut codes = vec![];
                for (_, v) in &vals {
                    if let Value::Text(s) = v {
                        let c = *index.entry(s.as_str()).or_insert_with(|| {
                            dict.push(s.as_str());
                            dict.len() as u64 - 1
                        });
                        put_var(&mut codes, c);
                    }
                }
                put_var(&mut b, dict.len() as u64);
                for s in dict {
                    put_var(&mut b, s.len() as u64);
                    b.extend_from_slice(s.as_bytes());
                }
                b.extend_from_slice(&codes);
            }
            1 => {
                let (mut mn, mut mx, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
                for (_, v) in &vals {
                    if let Value::Num(x) = v {
                        b.extend_from_slice(&x.to_le_bytes());
                        mn = mn.min(*x);
                        mx = mx.max(*x);
                        sum += *x;
                    }
                }
                zones.push(Zone { prop, min: mn, max: mx, sum, count: vals.len() as u32 });
            }
            2 => {
                for (_, v) in &vals {
                    if let Value::Sel(x) = v {
                        put_var(&mut b, *x);
                    }
                }
            }
            3 => {
                for (_, v) in &vals {
                    if let Value::Multi(m) = v {
                        put_var(&mut b, m.len() as u64);
                        for x in m {
                            put_var(&mut b, *x);
                        }
                    }
                }
            }
            _ => {
                for (_, v) in &vals {
                    if let Value::Check(x) = v {
                        b.push(*x as u8);
                    }
                }
            }
        }
        col_bytes.push(((prop, kind), b));
    }
    let mut head = vec![];
    put_var(&mut head, n as u64);
    for s in [&ids, &upd, &titles] {
        put_var(&mut head, s.len() as u64);
    }
    put_var(&mut head, col_bytes.len() as u64);
    for ((p, k), b) in &col_bytes {
        put_var(&mut head, *p as u64);
        head.push(*k);
        put_var(&mut head, b.len() as u64);
    }
    let mut out = Vec::with_capacity(head.len() + ids.len() + upd.len() + titles.len() + col_bytes.iter().map(|c| c.1.len()).sum::<usize>() + 8);
    put_var(&mut out, head.len() as u64);
    out.extend_from_slice(&head);
    out.extend_from_slice(&ids);
    out.extend_from_slice(&upd);
    out.extend_from_slice(&titles);
    for (_, b) in &col_bytes {
        out.extend_from_slice(b);
    }
    let ck = fnv(&out);
    out.extend_from_slice(&ck.to_le_bytes());
    (out, zones)
}

struct View<'a> {
    n: usize,
    ids: &'a [u8],
    upd: &'a [u8],
    titles: &'a [u8],
    cols: Vec<(u32, u8, &'a [u8])>,
}

fn view(bytes: &[u8]) -> io::Result<View<'_>> {
    view_ex(bytes, true)
}

/// `check = false` for bytes that were verified when they entered the cache.
fn view_ex(bytes: &[u8], check: bool) -> io::Result<View<'_>> {
    if bytes.len() < 5 {
        return Err(bad("short chunk"));
    }
    let (body, ck) = bytes.split_at(bytes.len() - 4);
    if check && fnv(body).to_le_bytes() != ck {
        return Err(bad("chunk checksum mismatch"));
    }
    let mut p = 0;
    let hl = get_var(body, &mut p).ok_or_else(|| bad("head"))? as usize;
    let head = body.get(p..p + hl).ok_or_else(|| bad("head len"))?;
    let mut off = p + hl;
    let mut q = 0;
    let n = get_var(head, &mut q).ok_or_else(|| bad("n"))? as usize;
    let mut sec = [0usize; 3];
    for s in sec.iter_mut() {
        *s = get_var(head, &mut q).ok_or_else(|| bad("sec"))? as usize;
    }
    let nc = get_var(head, &mut q).ok_or_else(|| bad("ncols"))? as usize;
    let mut take = |len: usize, off: &mut usize| -> io::Result<&[u8]> {
        let s = body.get(*off..*off + len).ok_or_else(|| bad("section"))?;
        *off += len;
        Ok(s)
    };
    let ids = take(sec[0], &mut off)?;
    let upd = take(sec[1], &mut off)?;
    let titles = take(sec[2], &mut off)?;
    let mut cols = vec![];
    for _ in 0..nc {
        let prop = get_var(head, &mut q).ok_or_else(|| bad("col"))? as u32;
        let kind = *head.get(q).ok_or_else(|| bad("kind"))?;
        q += 1;
        let len = get_var(head, &mut q).ok_or_else(|| bad("collen"))? as usize;
        cols.push((prop, kind, take(len, &mut off)?));
    }
    Ok(View { n, ids, upd, titles, cols })
}

/// Checks the trailing checksum of a chunk.
fn verify(bytes: &[u8]) -> io::Result<()> {
    view(bytes).map(|_| ())
}

fn decode_ids(v: &View) -> io::Result<Vec<u64>> {
    let (mut p, mut prev) = (0, 0u64);
    let mut out = Vec::with_capacity(v.n);
    for i in 0..v.n {
        let d = get_var(v.ids, &mut p).ok_or_else(|| bad("ids"))?;
        prev = if i == 0 { d } else { prev + d };
        out.push(prev);
    }
    Ok(out)
}

fn present(bm: &[u8], i: usize) -> bool {
    bm[i / 8] & (1 << (i % 8)) != 0
}

/// Decodes a whole chunk.
pub fn decode_chunk(bytes: &[u8]) -> io::Result<Vec<Row>> {
    let v = view(bytes)?;
    let ids = decode_ids(&v)?;
    let (mut pu, mut pt) = (0, 0);
    let mut rows = Vec::with_capacity(v.n);
    for id in ids {
        let updated = get_var(v.upd, &mut pu).ok_or_else(|| bad("upd"))?;
        let l = get_var(v.titles, &mut pt).ok_or_else(|| bad("tl"))? as usize;
        let t = v.titles.get(pt..pt + l).ok_or_else(|| bad("title"))?;
        pt += l;
        rows.push(Row { id, title: String::from_utf8_lossy(t).into_owned(), updated, props: vec![] });
    }
    for (prop, kind, b) in &v.cols {
        let bm_len = v.n.div_ceil(8);
        let bm = b.get(..bm_len).ok_or_else(|| bad("bitmap"))?;
        let mut p = bm_len;
        let mut dict: Vec<String> = vec![];
        if *kind == 0 {
            let c = get_var(b, &mut p).ok_or_else(|| bad("dict"))? as usize;
            for _ in 0..c {
                let l = get_var(b, &mut p).ok_or_else(|| bad("dl"))? as usize;
                dict.push(String::from_utf8_lossy(b.get(p..p + l).ok_or_else(|| bad("ds"))?).into_owned());
                p += l;
            }
        }
        for (i, row) in rows.iter_mut().enumerate() {
            if !present(bm, i) {
                continue;
            }
            let val = match kind {
                0 => Value::Text(dict.get(get_var(b, &mut p).ok_or_else(|| bad("code"))? as usize).cloned().unwrap_or_default()),
                1 => {
                    let x = b.get(p..p + 8).ok_or_else(|| bad("num"))?;
                    p += 8;
                    Value::Num(f64::from_le_bytes(x.try_into().map_err(|_| bad("num"))?))
                }
                2 => Value::Sel(get_var(b, &mut p).ok_or_else(|| bad("sel"))?),
                3 => {
                    let c = get_var(b, &mut p).ok_or_else(|| bad("ml"))? as usize;
                    let mut m = Vec::with_capacity(c);
                    for _ in 0..c {
                        m.push(get_var(b, &mut p).ok_or_else(|| bad("mv"))?);
                    }
                    Value::Multi(m)
                }
                _ => {
                    let x = *b.get(p).ok_or_else(|| bad("chk"))?;
                    p += 1;
                    Value::Check(x != 0)
                }
            };
            row.props.push((*prop, val));
        }
    }
    for r in rows.iter_mut() {
        r.props.sort_by_key(|p| p.0);
    }
    Ok(rows)
}

fn rank(bm: &[u8], i: usize) -> usize {
    let full = i / 8;
    let mut r: usize = bm[..full].iter().map(|b| b.count_ones() as usize).sum();
    if i % 8 != 0 {
        r += (bm[full] & ((1u8 << (i % 8)) - 1)).count_ones() as usize;
    }
    r
}

/// Decodes exactly one row of a chunk (no other row is materialised): ~50 µs instead of ~2 ms for the whole chunk.
pub fn decode_row(bytes: &[u8], id: u64) -> io::Result<Option<Row>> {
    let v = view_ex(bytes, false)?;
    let (mut p, mut prev, mut idx) = (0usize, 0u64, None);
    for i in 0..v.n {
        let d = get_var(v.ids, &mut p).ok_or_else(|| bad("ids"))?;
        prev = if i == 0 { d } else { prev + d };
        if prev == id {
            idx = Some(i);
            break;
        }
        if prev > id {
            return Ok(None);
        }
    }
    let Some(i) = idx else { return Ok(None) };
    let (mut pu, mut pt) = (0, 0);
    let mut updated = 0;
    for _ in 0..=i {
        updated = get_var(v.upd, &mut pu).ok_or_else(|| bad("upd"))?;
    }
    let mut title = String::new();
    for k in 0..=i {
        let l = get_var(v.titles, &mut pt).ok_or_else(|| bad("tl"))? as usize;
        if k == i {
            title = String::from_utf8_lossy(v.titles.get(pt..pt + l).ok_or_else(|| bad("title"))?).into_owned();
        }
        pt += l;
    }
    let mut props = vec![];
    for (prop, kind, b) in &v.cols {
        let bm_len = v.n.div_ceil(8);
        let bm = b.get(..bm_len).ok_or_else(|| bad("bitmap"))?;
        if !present(bm, i) {
            continue;
        }
        let r = rank(bm, i);
        let mut p = bm_len;
        let val = match kind {
            1 => {
                let o = bm_len + r * 8;
                Value::Num(f64::from_le_bytes(b.get(o..o + 8).ok_or_else(|| bad("num"))?.try_into().map_err(|_| bad("num"))?))
            }
            0 => {
                let c = get_var(b, &mut p).ok_or_else(|| bad("dict"))? as usize;
                let mut dict = Vec::with_capacity(c);
                for _ in 0..c {
                    let l = get_var(b, &mut p).ok_or_else(|| bad("dl"))? as usize;
                    dict.push(b.get(p..p + l).ok_or_else(|| bad("ds"))?);
                    p += l;
                }
                for _ in 0..r {
                    get_var(b, &mut p).ok_or_else(|| bad("code"))?;
                }
                let code = get_var(b, &mut p).ok_or_else(|| bad("code"))? as usize;
                Value::Text(dict.get(code).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default())
            }
            2 => {
                for _ in 0..r {
                    get_var(b, &mut p).ok_or_else(|| bad("sel"))?;
                }
                Value::Sel(get_var(b, &mut p).ok_or_else(|| bad("sel"))?)
            }
            3 => {
                for _ in 0..r {
                    let c = get_var(b, &mut p).ok_or_else(|| bad("ml"))?;
                    for _ in 0..c {
                        get_var(b, &mut p).ok_or_else(|| bad("mv"))?;
                    }
                }
                let c = get_var(b, &mut p).ok_or_else(|| bad("ml"))? as usize;
                let mut m = Vec::with_capacity(c);
                for _ in 0..c {
                    m.push(get_var(b, &mut p).ok_or_else(|| bad("mv"))?);
                }
                Value::Multi(m)
            }
            _ => Value::Check(*b.get(bm_len + r).ok_or_else(|| bad("chk"))? != 0),
        };
        props.push((*prop, val));
    }
    props.sort_by_key(|p| p.0);
    Ok(Some(Row { id, title, updated, props }))
}

/// Values of a numeric column without ids: values are densely packed behind the presence bitmap, so this is a straight pass over bytes.
fn decode_num_values(bytes: &[u8], prop: u32, mut f: impl FnMut(f64)) -> io::Result<()> {
    let v = view(bytes)?;
    let Some((_, _, b)) = v.cols.iter().find(|c| c.0 == prop && c.1 == 1) else {
        return Ok(());
    };
    for x in b[v.n.div_ceil(8)..].chunks_exact(8) {
        f(f64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]));
    }
    Ok(())
}

fn decode_sel_values(bytes: &[u8], prop: u32, mut f: impl FnMut(u64)) -> io::Result<()> {
    let v = view(bytes)?;
    let Some((_, _, b)) = v.cols.iter().find(|c| c.0 == prop && c.1 == 2) else {
        return Ok(());
    };
    let mut p = v.n.div_ceil(8);
    while p < b.len() {
        f(get_var(b, &mut p).ok_or_else(|| bad("sel"))?);
    }
    Ok(())
}

/// Decodes only the ids and one numeric column of a chunk (streaming scans never build rows).
fn decode_num_col(bytes: &[u8], prop: u32, mut f: impl FnMut(u64, f64)) -> io::Result<()> {
    let v = view(bytes)?;
    let Some((_, _, b)) = v.cols.iter().find(|c| c.0 == prop && c.1 == 1) else {
        return Ok(());
    };
    let ids = decode_ids(&v)?;
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    for (i, id) in ids.iter().enumerate() {
        if present(&b[..bm_len], i) {
            let x = b.get(p..p + 8).ok_or_else(|| bad("num"))?;
            p += 8;
            f(*id, f64::from_le_bytes(x.try_into().map_err(|_| bad("num"))?));
        }
    }
    Ok(())
}

fn decode_sel_col(bytes: &[u8], prop: u32, mut f: impl FnMut(u64, u64)) -> io::Result<()> {
    let v = view(bytes)?;
    let Some((_, _, b)) = v.cols.iter().find(|c| c.0 == prop && c.1 == 2) else {
        return Ok(());
    };
    let ids = decode_ids(&v)?;
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    for (i, id) in ids.iter().enumerate() {
        if present(&b[..bm_len], i) {
            f(*id, get_var(b, &mut p).ok_or_else(|| bad("sel"))?);
        }
    }
    Ok(())
}

// ───────────────────────────── sources ─────────────────────────────

/// Where bytes live. Implement this for OPFS / IndexedDB (web) or an object store (cloud).
pub trait Source: Send {
    fn data_len(&self) -> u64;
    fn read(&self, off: u64, len: usize) -> io::Result<Vec<u8>>;
    fn append(&mut self, bytes: &[u8]) -> io::Result<u64>;
    fn put_meta(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn get_meta(&self) -> io::Result<Option<Vec<u8>>>;
    fn wal_append(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn wal_read(&self) -> io::Result<Vec<u8>>;
    fn wal_clear(&mut self) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
    /// Rewrites the data file keeping only `live` ranges; returns their new offsets.
    fn compact(&mut self, live: &[(u64, u32)]) -> io::Result<Vec<u64>>;
}

#[derive(Default)]
pub struct MemSource {
    data: Vec<u8>,
    meta: Option<Vec<u8>>,
    wal: Vec<u8>,
}

impl Source for MemSource {
    fn data_len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read(&self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        self.data.get(off as usize..off as usize + len).map(|s| s.to_vec()).ok_or_else(|| bad("read past end"))
    }
    fn append(&mut self, b: &[u8]) -> io::Result<u64> {
        let o = self.data.len() as u64;
        self.data.extend_from_slice(b);
        Ok(o)
    }
    fn put_meta(&mut self, b: &[u8]) -> io::Result<()> {
        self.meta = Some(b.to_vec());
        Ok(())
    }
    fn get_meta(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.meta.clone())
    }
    fn wal_append(&mut self, b: &[u8]) -> io::Result<()> {
        self.wal.extend_from_slice(b);
        Ok(())
    }
    fn wal_read(&self) -> io::Result<Vec<u8>> {
        Ok(self.wal.clone())
    }
    fn wal_clear(&mut self) -> io::Result<()> {
        self.wal.clear();
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn compact(&mut self, live: &[(u64, u32)]) -> io::Result<Vec<u64>> {
        let mut nd = vec![];
        let mut offs = vec![];
        for (o, l) in live {
            offs.push(nd.len() as u64);
            nd.extend_from_slice(&self.data[*o as usize..*o as usize + *l as usize]);
        }
        self.data = nd;
        Ok(offs)
    }
}

/// Files in a directory: `data.lst` (append-only chunks), `meta.lst` (index, replaced atomically), `wal.lst`.
pub struct FileSource {
    dir: std::path::PathBuf,
    data: std::fs::File,
    wal: std::fs::File,
    len: u64,
}

#[cfg(unix)]
fn read_at(f: &std::fs::File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}
#[cfg(windows)]
fn read_at(f: &std::fs::File, buf: &mut [u8], mut off: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = std::os::windows::fs::FileExt::seek_read(f, &mut buf[done..], off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_at(_f: &std::fs::File, _buf: &mut [u8], _off: u64) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no file system on this platform: use MemSource or a browser Source"))
}

impl FileSource {
    pub fn open(dir: impl AsRef<std::path::Path>) -> io::Result<Self> {
        use std::fs::OpenOptions;
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let data = OpenOptions::new().read(true).append(true).create(true).open(dir.join("data.lst"))?;
        let wal = OpenOptions::new().read(true).append(true).create(true).open(dir.join("wal.lst"))?;
        let len = data.metadata()?.len();
        Ok(FileSource { dir, data, wal, len })
    }
}

impl Source for FileSource {
    fn data_len(&self) -> u64 {
        self.len
    }
    fn read(&self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut b = vec![0u8; len];
        read_at(&self.data, &mut b, off)?;
        Ok(b)
    }
    fn append(&mut self, b: &[u8]) -> io::Result<u64> {
        use std::io::Write;
        let o = self.len;
        self.data.write_all(b)?;
        self.len += b.len() as u64;
        Ok(o)
    }
    fn put_meta(&mut self, b: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let tmp = self.dir.join("meta.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(b)?;
        f.sync_all()?;
        std::fs::rename(tmp, self.dir.join("meta.lst"))
    }
    fn get_meta(&self) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.dir.join("meta.lst")) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn wal_append(&mut self, b: &[u8]) -> io::Result<()> {
        use std::io::Write;
        self.wal.write_all(b)
    }
    fn wal_read(&self) -> io::Result<Vec<u8>> {
        std::fs::read(self.dir.join("wal.lst"))
    }
    fn wal_clear(&mut self) -> io::Result<()> {
        self.wal.set_len(0)?;
        self.wal.sync_all()
    }
    fn sync(&mut self) -> io::Result<()> {
        self.data.sync_data()?;
        self.wal.sync_data()
    }
    fn compact(&mut self, live: &[(u64, u32)]) -> io::Result<Vec<u64>> {
        use std::io::Write;
        let tmp = self.dir.join("data.tmp");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut offs = vec![];
        let mut at = 0u64;
        for (o, l) in live {
            offs.push(at);
            out.write_all(&self.read(*o, *l as usize)?)?;
            at += *l as u64;
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, self.dir.join("data.lst"))?;
        self.data = std::fs::OpenOptions::new().read(true).append(true).open(self.dir.join("data.lst"))?;
        self.len = at;
        Ok(offs)
    }
}

// ───────────────────────────── the store ─────────────────────────────

#[derive(Clone, Debug)]
struct Meta {
    off: u64,
    len: u32,
    n: u32,
    min_id: u64,
    max_id: u64,
    zones: Vec<Zone>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub chunk_reads: u64,
    pub cache_hits: u64,
    pub bytes_read: u64,
    pub chunks_skipped: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NumAgg {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl NumAgg {
    fn new() -> Self {
        NumAgg { count: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
    fn add(&mut self, x: f64) {
        self.count += 1;
        self.sum += x;
        self.min = self.min.min(x);
        self.max = self.max.max(x);
    }
}

pub struct Store {
    src: Box<dyn Source>,
    chunks: Vec<Meta>,
    overlay: BTreeMap<u64, Option<Row>>,
    cache: HashMap<u64, (Arc<Vec<u8>>, usize, u64)>,
    cache_bytes: usize,
    budget: usize,
    tick: u64,
    gen: u64,
    pub stats: Stats,
}

const MAGIC: &[u8] = b"LSTORE1\n";
/// Overlay size that triggers an automatic checkpoint.
const AUTO_CHECKPOINT: usize = 8192;

impl Store {
    /// Opens a table: reads the index only, then replays the WAL into the overlay.
    pub fn open(src: Box<dyn Source>, budget_bytes: usize) -> io::Result<Store> {
        let mut s = Store { src, chunks: vec![], overlay: BTreeMap::new(), cache: HashMap::new(), cache_bytes: 0, budget: budget_bytes, tick: 0, gen: 0, stats: Stats::default() };
        if let Some(m) = s.src.get_meta()? {
            s.chunks = parse_meta(&m)?;
        }
        let wal = s.src.wal_read()?;
        let mut p = 0;
        while p + 4 <= wal.len() {
            let len = u32::from_le_bytes([wal[p], wal[p + 1], wal[p + 2], wal[p + 3]]) as usize;
            let Some(rec) = wal.get(p + 4..p + 4 + len + 4) else {
                break;
            }; // torn tail
            let (body, ck) = rec.split_at(len);
            if fnv(body).to_le_bytes() != ck {
                break;
            }
            match body.first() {
                Some(1) => {
                    if let Ok(mut r) = decode_chunk(&body[1..]) {
                        if let Some(r) = r.pop() {
                            s.overlay.insert(r.id, Some(r));
                        }
                    }
                }
                Some(2) => {
                    let mut q = 1;
                    if let Some(id) = get_var(body, &mut q) {
                        s.overlay.insert(id, None);
                    }
                }
                _ => break,
            }
            p += 4 + len + 4;
        }
        Ok(s)
    }

    pub fn open_dir(dir: impl AsRef<std::path::Path>, budget_bytes: usize) -> io::Result<Store> {
        Store::open(Box::new(FileSource::open(dir)?), budget_bytes)
    }

    /// Highest row id (overlay included); 0 when empty.
    pub fn max_id(&self) -> u64 {
        self.chunks.last().map_or(0, |c| c.max_id).max(self.overlay.keys().next_back().copied().unwrap_or(0))
    }
    /// Edits waiting for the next checkpoint.
    pub fn pending(&self) -> usize {
        self.overlay.len()
    }
    pub fn budget(&self) -> usize {
        self.budget
    }
    /// Chunk list for lock-light scans: `(generation, [(offset, length)])`. The overlay is folded in first.
    pub fn raw_refs(&mut self) -> io::Result<(u64, Vec<(u64, u32)>)> {
        self.checkpoint()?;
        Ok((self.gen, self.chunks.iter().map(|c| (c.off, c.len)).collect()))
    }
    /// Reads one chunk if the file layout is still the one `raw_refs` described.
    pub fn raw_read(&self, gen: u64, off: u64, len: u32) -> io::Result<Vec<u8>> {
        if gen != self.gen {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "store was compacted during the scan"));
        }
        self.src.read(off, len as usize)
    }

    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.evict();
    }
    pub fn cache_bytes(&self) -> usize {
        self.cache_bytes
    }
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
    pub fn disk_bytes(&self) -> u64 {
        self.src.data_len()
    }

    fn wal_record(&mut self, op: u8, payload: &[u8]) -> io::Result<()> {
        let mut rec = Vec::with_capacity(payload.len() + 9);
        rec.extend_from_slice(&((payload.len() + 1) as u32).to_le_bytes());
        rec.push(op);
        rec.extend_from_slice(payload);
        let ck = fnv(&rec[4..]);
        rec.extend_from_slice(&ck.to_le_bytes());
        self.src.wal_append(&rec)
    }

    /// Inserts or replaces a row (durable once this returns).
    pub fn upsert(&mut self, row: Row) -> io::Result<()> {
        let (bytes, _) = encode_chunk(std::slice::from_ref(&row));
        self.wal_record(1, &bytes)?;
        self.overlay.insert(row.id, Some(row));
        if self.overlay.len() >= AUTO_CHECKPOINT {
            self.checkpoint()?;
        }
        Ok(())
    }

    pub fn delete(&mut self, id: u64) -> io::Result<()> {
        let mut p = vec![];
        put_var(&mut p, id);
        self.wal_record(2, &p)?;
        self.overlay.insert(id, None);
        Ok(())
    }

    /// Appends a large, already ascending batch (ids above every existing id) straight into chunks — the import / first-load path.
    pub fn bulk_append(&mut self, rows: impl Iterator<Item = Row>) -> io::Result<()> {
        self.checkpoint()?;
        let mut last = self.chunks.last().map_or(0, |c| c.max_id);
        let mut buf: Vec<Row> = Vec::with_capacity(CHUNK_ROWS);
        let mut metas = self.chunks.clone();
        for r in rows {
            if r.id <= last && !(last == 0 && metas.is_empty()) {
                return Err(bad("bulk_append needs ascending ids above the existing ones"));
            }
            last = r.id;
            buf.push(r);
            if buf.len() == CHUNK_ROWS {
                metas.push(self.write_chunk(&buf)?);
                buf.clear();
            }
        }
        if !buf.is_empty() {
            metas.push(self.write_chunk(&buf)?);
        }
        self.install(metas)
    }

    fn write_chunk(&mut self, rows: &[Row]) -> io::Result<Meta> {
        let (bytes, zones) = encode_chunk(rows);
        let off = self.src.append(&bytes)?;
        Ok(Meta { off, len: bytes.len() as u32, n: rows.len() as u32, min_id: rows[0].id, max_id: rows[rows.len() - 1].id, zones })
    }

    fn install(&mut self, metas: Vec<Meta>) -> io::Result<()> {
        self.src.sync()?;
        self.src.put_meta(&write_meta(&metas))?;
        self.chunks = metas;
        Ok(())
    }

    fn locate(&self, id: u64) -> Option<usize> {
        let i = self.chunks.partition_point(|c| c.max_id < id);
        (i < self.chunks.len() && self.chunks[i].min_id <= id).then_some(i)
    }

    fn evict(&mut self) {
        while self.cache_bytes > self.budget && !self.cache.is_empty() {
            let oldest = self.cache.iter().min_by_key(|(_, v)| v.2).map(|(k, _)| *k);
            if let Some(k) = oldest {
                if let Some(v) = self.cache.remove(&k) {
                    self.cache_bytes -= v.1;
                }
            }
        }
    }

    /// Raw (compact, still encoded) bytes of chunk `i`, through the byte-budgeted LRU. The encoded form is ~5× smaller than decoded rows,
    /// so the same budget caches ~5× more data, and a point read decodes one row, not 4 096.
    fn chunk_bytes(&mut self, i: usize) -> io::Result<Arc<Vec<u8>>> {
        self.tick += 1;
        let m = self.chunks[i].clone();
        if let Some(e) = self.cache.get_mut(&m.off) {
            e.2 = self.tick;
            self.stats.cache_hits += 1;
            return Ok(e.0.clone());
        }
        let bytes = Arc::new(self.src.read(m.off, m.len as usize)?);
        verify(&bytes)?;
        self.stats.chunk_reads += 1;
        self.stats.bytes_read += bytes.len() as u64;
        self.cache.insert(m.off, (bytes.clone(), bytes.len(), self.tick));
        self.cache_bytes += bytes.len();
        self.evict();
        Ok(bytes)
    }

    /// Point read. Overlay first, then one cached chunk, then one decoded row.
    pub fn get(&mut self, id: u64) -> io::Result<Option<Row>> {
        if let Some(o) = self.overlay.get(&id) {
            return Ok(o.clone());
        }
        let Some(i) = self.locate(id) else {
            return Ok(None);
        };
        let bytes = self.chunk_bytes(i)?;
        decode_row(&bytes, id)
    }

    /// Applies the overlay: only the chunks that changed are rewritten (appended), the index is swapped atomically, the WAL is cleared.
    pub fn checkpoint(&mut self) -> io::Result<()> {
        if self.overlay.is_empty() {
            return Ok(());
        }
        let ops = std::mem::take(&mut self.overlay);
        let mut by_chunk: BTreeMap<usize, Vec<(u64, Option<Row>)>> = BTreeMap::new();
        for (id, op) in ops {
            let i = self.chunks.partition_point(|c| c.max_id < id).min(self.chunks.len().saturating_sub(1));
            by_chunk.entry(i).or_default().push((id, op));
        }
        let old = std::mem::take(&mut self.chunks);
        let mut metas: Vec<Meta> = Vec::with_capacity(old.len() + 2);
        let mut dead: Vec<u64> = vec![];
        let mut ops_iter = by_chunk.into_iter().peekable();
        let virtual_empty = old.is_empty();
        let total = if virtual_empty { 1 } else { old.len() };
        for i in 0..total {
            let dirty = ops_iter.peek().map_or(false, |(k, _)| *k == i);
            if !dirty {
                metas.push(old[i].clone());
                continue;
            }
            let (_, ops) = ops_iter.next().expect("peeked");
            let mut rows: BTreeMap<u64, Row> = BTreeMap::new();
            if !virtual_empty {
                let bytes = self.src.read(old[i].off, old[i].len as usize)?;
                for r in decode_chunk(&bytes)? {
                    rows.insert(r.id, r);
                }
                dead.push(old[i].off);
            }
            for (id, op) in ops {
                match op {
                    Some(r) => {
                        rows.insert(id, r);
                    }
                    None => {
                        rows.remove(&id);
                    }
                }
            }
            let all: Vec<Row> = rows.into_values().collect();
            for part in all.chunks(CHUNK_ROWS) {
                let m = self.write_chunk(part)?;
                metas.push(m);
            }
        }
        for d in dead {
            if let Some(e) = self.cache.remove(&d) {
                self.cache_bytes -= e.1;
            }
        }
        self.install(metas)?;
        self.src.wal_clear()?;
        // garbage: more than half of the file is dead chunks → rewrite
        let live: u64 = self.chunks.iter().map(|c| c.len as u64).sum();
        if self.src.data_len() > 8 << 20 && live * 2 < self.src.data_len() {
            self.compact()?;
        }
        Ok(())
    }

    pub fn compact(&mut self) -> io::Result<()> {
        self.checkpoint()?;
        let live: Vec<(u64, u32)> = self.chunks.iter().map(|c| (c.off, c.len)).collect();
        let offs = self.src.compact(&live)?;
        for (c, o) in self.chunks.iter_mut().zip(offs) {
            c.off = o;
        }
        self.gen += 1;
        self.cache.clear();
        self.cache_bytes = 0;
        let m = write_meta(&self.chunks);
        self.src.put_meta(&m)
    }

    pub fn len(&mut self) -> io::Result<u64> {
        self.checkpoint()?;
        Ok(self.chunks.iter().map(|c| c.n as u64).sum())
    }

    /// Every row with `lo <= id < hi`, in id order (one bucket of the collaboration digest). Reads only the chunks that overlap the range.
    pub fn rows_in(&mut self, lo: u64, hi: u64) -> io::Result<Vec<Row>> {
        self.checkpoint()?;
        let mut out = Vec::new();
        let first = self.chunks.partition_point(|c| c.max_id < lo);
        for i in first..self.chunks.len() {
            if self.chunks[i].min_id >= hi {
                break;
            }
            let rows = decode_chunk(&self.chunk_bytes(i)?)?;
            out.extend(rows.into_iter().filter(|r| r.id >= lo && r.id < hi));
        }
        Ok(out)
    }

    /// `count` rows starting at row number `start` (id order) — what a virtualised table asks for. Reads one or two chunks.
    pub fn window(&mut self, start: u64, count: usize) -> io::Result<Vec<Row>> {
        self.checkpoint()?;
        let mut out = Vec::with_capacity(count);
        let mut before = 0u64;
        for i in 0..self.chunks.len() {
            let n = self.chunks[i].n as u64;
            if before + n > start && out.len() < count {
                let rows = decode_chunk(&self.chunk_bytes(i)?)?;
                let from = start.saturating_sub(before) as usize;
                out.extend(rows.iter().skip(from).take(count - out.len()).cloned());
            }
            before += n;
            if out.len() >= count {
                break;
            }
        }
        Ok(out)
    }

    /// Count / sum / min / max of a numeric property, optionally restricted to `lo ≤ x ≤ hi`.
    /// Without a filter nothing is read from the data file; with one, chunks outside the range are skipped from their zone map alone.
    pub fn aggregate_num(&mut self, prop: u32, range: Option<(f64, f64)>) -> io::Result<NumAgg> {
        self.checkpoint()?;
        let mut agg = NumAgg::new();
        for i in 0..self.chunks.len() {
            let Some(z) = self.chunks[i].zones.iter().find(|z| z.prop == prop).copied() else {
                continue;
            };
            match range {
                None => merge_zone(&mut agg, &z),
                Some((lo, hi)) if z.max < lo || z.min > hi => self.stats.chunks_skipped += 1,
                Some((lo, hi)) if lo <= z.min && z.max <= hi => merge_zone(&mut agg, &z),
                Some((lo, hi)) => {
                    let m = self.chunks[i].clone();
                    let bytes = self.src.read(m.off, m.len as usize)?;
                    self.stats.chunk_reads += 1;
                    self.stats.bytes_read += bytes.len() as u64;
                    decode_num_values(&bytes, prop, |x| {
                        if x >= lo && x <= hi {
                            agg.add(x)
                        }
                    })?;
                }
            }
        }
        Ok(agg)
    }

    /// Streams `(id, value)` of a numeric column; memory = one chunk's column at a time, the cache is not touched.
    pub fn scan_num(&mut self, prop: u32, mut f: impl FnMut(u64, f64)) -> io::Result<()> {
        self.checkpoint()?;
        for i in 0..self.chunks.len() {
            if !self.chunks[i].zones.iter().any(|z| z.prop == prop) {
                continue;
            }
            let m = self.chunks[i].clone();
            let bytes = self.src.read(m.off, m.len as usize)?;
            self.stats.bytes_read += bytes.len() as u64;
            decode_num_col(&bytes, prop, &mut f)?;
        }
        Ok(())
    }

    /// Streams the option ids of a select column without row ids — the fastest base of a "group by" chart.
    pub fn scan_sel_values(&mut self, prop: u32, mut f: impl FnMut(u64)) -> io::Result<()> {
        self.checkpoint()?;
        for i in 0..self.chunks.len() {
            let m = self.chunks[i].clone();
            let bytes = self.src.read(m.off, m.len as usize)?;
            self.stats.bytes_read += bytes.len() as u64;
            decode_sel_values(&bytes, prop, &mut f)?;
        }
        Ok(())
    }

    /// Streams `(id, option id)` of a select column — the base of every "group by" chart.
    pub fn scan_sel(&mut self, prop: u32, mut f: impl FnMut(u64, u64)) -> io::Result<()> {
        self.checkpoint()?;
        for i in 0..self.chunks.len() {
            let m = self.chunks[i].clone();
            let bytes = self.src.read(m.off, m.len as usize)?;
            self.stats.bytes_read += bytes.len() as u64;
            decode_sel_col(&bytes, prop, &mut f)?;
        }
        Ok(())
    }
}

fn merge_zone(a: &mut NumAgg, z: &Zone) {
    a.count += z.count as u64;
    a.sum += z.sum;
    a.min = a.min.min(z.min);
    a.max = a.max.max(z.max);
}

fn write_meta(m: &[Meta]) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    put_var(&mut b, m.len() as u64);
    for c in m {
        put_var(&mut b, c.off);
        put_var(&mut b, c.len as u64);
        put_var(&mut b, c.n as u64);
        put_var(&mut b, c.min_id);
        put_var(&mut b, c.max_id);
        put_var(&mut b, c.zones.len() as u64);
        for z in &c.zones {
            put_var(&mut b, z.prop as u64);
            for f in [z.min, z.max, z.sum] {
                b.extend_from_slice(&f.to_le_bytes());
            }
            put_var(&mut b, z.count as u64);
        }
    }
    let ck = fnv(&b);
    b.extend_from_slice(&ck.to_le_bytes());
    b
}

fn parse_meta(b: &[u8]) -> io::Result<Vec<Meta>> {
    if b.len() < MAGIC.len() + 5 || &b[..MAGIC.len()] != MAGIC {
        return Err(bad("not a lumen-store index"));
    }
    let (body, ck) = b.split_at(b.len() - 4);
    if fnv(body).to_le_bytes() != ck {
        return Err(bad("index checksum mismatch"));
    }
    let mut p = MAGIC.len();
    let n = get_var(body, &mut p).ok_or_else(|| bad("n"))? as usize;
    let mut out = Vec::with_capacity(n);
    let f64_at = |p: &mut usize| -> io::Result<f64> {
        let x = body.get(*p..*p + 8).ok_or_else(|| bad("f64"))?;
        *p += 8;
        Ok(f64::from_le_bytes(x.try_into().map_err(|_| bad("f64"))?))
    };
    for _ in 0..n {
        let mut g = |p: &mut usize| get_var(body, p).ok_or_else(|| bad("meta"));
        let (off, len, cn, min_id, max_id, nz) = (g(&mut p)?, g(&mut p)? as u32, g(&mut p)? as u32, g(&mut p)?, g(&mut p)?, g(&mut p)? as usize);
        let mut zones = Vec::with_capacity(nz);
        for _ in 0..nz {
            let prop = get_var(body, &mut p).ok_or_else(|| bad("zp"))? as u32;
            let (min, max, sum) = (f64_at(&mut p)?, f64_at(&mut p)?, f64_at(&mut p)?);
            let count = get_var(body, &mut p).ok_or_else(|| bad("zc"))? as u32;
            zones.push(Zone { prop, min, max, sum, count });
        }
        out.push(Meta { off, len, n: cn, min_id, max_id, zones });
    }
    Ok(out)
}

// ───────────────────────────── aligned column decoders + shared scans ─────────────────────────────

fn col_bytes<'a>(v: &View<'a>, prop: u32, kind: u8) -> Option<&'a [u8]> {
    v.cols.iter().find(|c| c.0 == prop && c.1 == kind).map(|c| c.2)
}

/// Row ids of a chunk (verified bytes).
pub fn chunk_ids(bytes: &[u8]) -> io::Result<Vec<u64>> {
    decode_ids(&view_ex(bytes, false)?)
}

pub fn chunk_titles(bytes: &[u8]) -> io::Result<Vec<String>> {
    let v = view_ex(bytes, false)?;
    let mut p = 0;
    let mut out = Vec::with_capacity(v.n);
    for _ in 0..v.n {
        let l = get_var(v.titles, &mut p).ok_or_else(|| bad("tl"))? as usize;
        out.push(String::from_utf8_lossy(v.titles.get(p..p + l).ok_or_else(|| bad("title"))?).into_owned());
        p += l;
    }
    Ok(out)
}

/// One entry per row of the chunk (`None` = empty cell), so several columns can be combined row by row.
pub fn col_num(bytes: &[u8], prop: u32) -> io::Result<Vec<Option<f64>>> {
    let v = view_ex(bytes, false)?;
    let Some(b) = col_bytes(&v, prop, 1) else {
        return Ok(vec![None; v.n]);
    };
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    let mut out = Vec::with_capacity(v.n);
    for i in 0..v.n {
        if present(&b[..bm_len], i) {
            let x = b.get(p..p + 8).ok_or_else(|| bad("num"))?;
            p += 8;
            out.push(Some(f64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]])));
        } else {
            out.push(None);
        }
    }
    Ok(out)
}

pub fn col_sel(bytes: &[u8], prop: u32) -> io::Result<Vec<Option<u64>>> {
    let v = view_ex(bytes, false)?;
    let Some(b) = col_bytes(&v, prop, 2) else {
        return Ok(vec![None; v.n]);
    };
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    let mut out = Vec::with_capacity(v.n);
    for i in 0..v.n {
        out.push(if present(&b[..bm_len], i) { Some(get_var(b, &mut p).ok_or_else(|| bad("sel"))?) } else { None });
    }
    Ok(out)
}

pub fn col_check(bytes: &[u8], prop: u32) -> io::Result<Vec<Option<bool>>> {
    let v = view_ex(bytes, false)?;
    let Some(b) = col_bytes(&v, prop, 4) else {
        return Ok(vec![None; v.n]);
    };
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    let mut out = Vec::with_capacity(v.n);
    for i in 0..v.n {
        if present(&b[..bm_len], i) {
            out.push(Some(*b.get(p).ok_or_else(|| bad("chk"))? != 0));
            p += 1;
        } else {
            out.push(None);
        }
    }
    Ok(out)
}

/// Dictionary + one code per row — text is compared / bucketed per *distinct* value, not per row.
pub fn col_text(bytes: &[u8], prop: u32) -> io::Result<(Vec<String>, Vec<Option<u32>>)> {
    let v = view_ex(bytes, false)?;
    let Some(b) = col_bytes(&v, prop, 0) else {
        return Ok((vec![], vec![None; v.n]));
    };
    let bm_len = v.n.div_ceil(8);
    let mut p = bm_len;
    let c = get_var(b, &mut p).ok_or_else(|| bad("dict"))? as usize;
    let mut dict = Vec::with_capacity(c);
    for _ in 0..c {
        let l = get_var(b, &mut p).ok_or_else(|| bad("dl"))? as usize;
        dict.push(String::from_utf8_lossy(b.get(p..p + l).ok_or_else(|| bad("ds"))?).into_owned());
        p += l;
    }
    let mut out = Vec::with_capacity(v.n);
    for i in 0..v.n {
        out.push(if present(&b[..bm_len], i) { Some(get_var(b, &mut p).ok_or_else(|| bad("code"))? as u32) } else { None });
    }
    Ok((dict, out))
}

/// Visits every chunk's verified bytes **without holding the lock while decoding**: the mutex is taken only to list the chunks and,
/// per chunk, for the file read. The UI thread (scrolling, edits) and any number of workers can use one store at the same time.
/// A compaction in between restarts the scan (at most 3 times).
pub fn shared_scan(store: &std::sync::Mutex<Store>, mut f: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
    'retry: for _ in 0..3 {
        let (gen, refs) = store.lock().map_err(|_| bad("poisoned"))?.raw_refs()?;
        for (off, len) in refs {
            let bytes = match store.lock().map_err(|_| bad("poisoned"))?.raw_read(gen, off, len) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue 'retry,
                Err(e) => return Err(e),
            };
            verify(&bytes)?;
            f(&bytes)?;
        }
        return Ok(());
    }
    Err(io::Error::new(io::ErrorKind::Interrupted, "store kept changing during the scan"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(i: u64) -> Row {
        Row {
            id: i,
            title: format!("Task {i}"),
            updated: 1_700_000_000_000 + i,
            props: vec![(1, Value::Sel(i % 5)), (2, Value::Num((i % 100) as f64)), (3, Value::Text(format!("tag{}", i % 7))), (4, Value::Check(i % 2 == 0)), (5, Value::Multi(vec![i % 3, 9]))],
        }
    }

    fn mem(budget: usize) -> Store {
        Store::open(Box::new(MemSource::default()), budget).unwrap()
    }

    #[test]
    fn codec_roundtrip() {
        let rows: Vec<Row> = (10..500).map(row).collect();
        let (b, zones) = encode_chunk(&rows);
        assert_eq!(decode_chunk(&b).unwrap(), rows);
        let z = zones.iter().find(|z| z.prop == 2).unwrap();
        assert_eq!((z.min, z.max, z.count), (0.0, 99.0, 490));
        let mut bad_bytes = b.clone();
        bad_bytes[20] ^= 0xff;
        assert!(decode_chunk(&bad_bytes).is_err(), "checksum catches corruption");
    }

    #[test]
    fn aligned_columns_and_shared_scan() {
        let m = std::sync::Mutex::new(mem(1 << 20));
        m.lock().unwrap().bulk_append((1..=10_000).map(row)).unwrap();
        let (mut n, mut sum, mut sel_ok, mut titles) = (0u64, 0.0, true, 0usize);
        shared_scan(&m, |b| {
            let ids = chunk_ids(b)?;
            let nums = col_num(b, 2)?;
            let sels = col_sel(b, 1)?;
            let (dict, codes) = col_text(b, 3)?;
            titles += chunk_titles(b)?.len();
            for (i, id) in ids.iter().enumerate() {
                n += 1;
                sum += nums[i].unwrap();
                sel_ok &= sels[i] == Some(id % 5);
                assert_eq!(dict[codes[i].unwrap() as usize], format!("tag{}", id % 7));
            }
            assert_eq!(col_check(b, 4)?.iter().filter(|c| **c == Some(true)).count(), ids.iter().filter(|i| *i % 2 == 0).count());
            Ok(())
        })
        .unwrap();
        assert_eq!((n, titles, sel_ok), (10_000, 10_000, true));
        assert_eq!(sum, (1..=10_000u64).map(|i| (i % 100) as f64).sum::<f64>());
        assert_eq!(m.lock().unwrap().max_id(), 10_000);
    }

    #[test]
    fn single_row_decode_equals_full_decode() {
        let mut rows: Vec<Row> = (100..900).map(row).collect();
        rows[7].props.retain(|(p, _)| *p != 3); // sparse columns
        rows[8].props.clear();
        let (b, _) = encode_chunk(&rows);
        for r in &rows {
            assert_eq!(decode_row(&b, r.id).unwrap().as_ref(), Some(r), "row {}", r.id);
        }
        assert_eq!(decode_row(&b, 5).unwrap(), None);
        assert_eq!(decode_row(&b, 100_000).unwrap(), None);
    }

    #[test]
    fn sparse_and_unicode_rows() {
        let rows = vec![Row { id: 1, title: "😀 émoji".into(), updated: 1, props: vec![] }, Row { id: 5, title: String::new(), updated: 2, props: vec![(7, Value::Num(-1.5))] }];
        assert_eq!(decode_chunk(&encode_chunk(&rows).0).unwrap(), rows);
    }

    #[test]
    fn crud_checkpoint_and_wal_replay() {
        let mut s = mem(1 << 20);
        s.bulk_append((1..=10_000).map(row)).unwrap();
        assert_eq!(s.len().unwrap(), 10_000);
        assert_eq!(s.get(7_777).unwrap().unwrap(), row(7_777));
        let mut r = row(42);
        r.title = "renamed".into();
        s.upsert(r.clone()).unwrap();
        s.delete(43).unwrap();
        s.upsert(row(10_001)).unwrap();
        assert_eq!(s.get(42).unwrap().unwrap().title, "renamed");
        assert_eq!(s.get(43).unwrap(), None);
        assert_eq!(s.len().unwrap(), 10_000); // −1 +1
        assert_eq!(s.get(10_001).unwrap().unwrap(), row(10_001));
        assert_eq!(s.get(43).unwrap(), None);
    }

    #[test]
    fn crash_recovery_replays_the_log() {
        let dir = std::env::temp_dir().join(format!("lstore-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut s = Store::open_dir(&dir, 1 << 20).unwrap();
            s.bulk_append((1..=5_000).map(row)).unwrap();
            let mut r = row(10);
            r.title = "edited before the crash".into();
            s.upsert(r).unwrap();
            s.delete(11).unwrap();
            // no checkpoint: the process "dies" here
        }
        let mut s = Store::open_dir(&dir, 1 << 20).unwrap();
        assert_eq!(s.get(10).unwrap().unwrap().title, "edited before the crash");
        assert_eq!(s.get(11).unwrap(), None);
        assert_eq!(s.len().unwrap(), 4_999);
        // torn last record is ignored
        {
            let mut w = FileSource::open(&dir).unwrap();
            w.wal_append(&[9, 9, 9]).unwrap();
        }
        let mut s = Store::open_dir(&dir, 1 << 20).unwrap();
        assert_eq!(s.len().unwrap(), 4_999);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_reads_only_what_it_needs() {
        let mut s = mem(1 << 24);
        s.bulk_append((1..=50_000).map(row)).unwrap();
        let w = s.window(25_000, 60).unwrap();
        assert_eq!(w.len(), 60);
        assert_eq!(w[0].id, 25_001);
        assert!(s.stats.chunk_reads <= 2, "{:?}", s.stats);
        let tail = s.window(49_990, 100).unwrap();
        assert_eq!(tail.len(), 10);
    }

    #[test]
    fn zone_maps_answer_aggregates() {
        let mut s = mem(1 << 20);
        s.bulk_append((1..=30_000).map(row)).unwrap();
        let reads0 = s.stats.chunk_reads;
        let all = s.aggregate_num(2, None).unwrap();
        assert_eq!(s.stats.chunk_reads, reads0, "unfiltered aggregate reads no chunk");
        let brute: f64 = (1..=30_000u64).map(|i| (i % 100) as f64).sum();
        assert_eq!((all.count, all.sum, all.min, all.max), (30_000, brute, 0.0, 99.0));
        let part = s.aggregate_num(2, Some((10.0, 19.0))).unwrap();
        let brute_n = (1..=30_000u64).filter(|i| (10..20).contains(&(i % 100))).count() as u64;
        assert_eq!(part.count, brute_n);
        // an impossible range skips every chunk
        let none = s.aggregate_num(2, Some((1000.0, 2000.0))).unwrap();
        assert_eq!(none.count, 0);
        assert!(s.stats.chunks_skipped >= s.chunk_count() as u64);
    }

    #[test]
    fn group_by_scan_matches_brute_force() {
        let mut s = mem(1 << 20);
        s.bulk_append((1..=20_000).map(row)).unwrap();
        let mut counts: HashMap<u64, u64> = HashMap::new();
        s.scan_sel(1, |_, o| *counts.entry(o).or_default() += 1).unwrap();
        assert_eq!(counts.len(), 5);
        assert!(counts.values().all(|c| *c == 4_000));
        assert_eq!(s.cache_bytes(), 0, "scans bypass the cache");
    }

    #[test]
    fn cache_respects_the_byte_budget() {
        let mut s = mem(200_000);
        s.bulk_append((1..=100_000).map(row)).unwrap();
        for i in (1..=100_000).step_by(997) {
            assert_eq!(s.get(i).unwrap().unwrap().id, i);
            assert!(s.cache_bytes() <= 200_000 + 60_000, "cache {}", s.cache_bytes());
        }
        s.set_budget(0);
        assert_eq!(s.cache_bytes(), 0);
    }

    #[test]
    fn checkpoint_splits_big_chunks_and_compaction_keeps_data() {
        let mut s = mem(1 << 20);
        s.bulk_append((1..=4_096).map(row)).unwrap();
        for i in 0..9_000u64 {
            s.upsert(Row { id: 5_000 + i, ..row(5_000 + i) }).unwrap();
        }
        s.checkpoint().unwrap();
        assert!(s.chunk_count() >= 3);
        let before: Vec<Row> = s.window(0, 20_000).unwrap();
        s.compact().unwrap();
        assert_eq!(s.window(0, 20_000).unwrap(), before);
        assert_eq!(before.len(), 4_096 + 9_000);
    }
}
