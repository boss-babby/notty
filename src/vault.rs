//! Portability: the whole workspace as a plain folder of Markdown files, and the way back in.
//!
//! * **Export** — `export_zip` writes a ZIP with one `Title.md` per page (YAML front-matter: title, icon, id, timestamps, database
//!   properties), the page hierarchy as folders, databases as `Title.md` + `Title.csv`, and every picture / attached file under
//!   `assets/` (links are relative, so the folder opens as-is in Obsidian, VS Code or any Markdown tool). No lock-in.
//! * **Import** — `import_zip` reads a *Notion* "Markdown & CSV" export or an *Obsidian* vault (folders → nested pages, front-matter
//!   titles, `![[image.png]]` embeds, `[[wiki links]]`, CSV → databases, pictures → media) without any temporary files.
//! * **Storage** — `storage_report` / `prune_history` back the storage manager dialog.
//!
//! The ZIP reader / writer is ~120 lines on top of `miniz_oxide` (already a dependency), so no extra crate reaches the wasm build.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;

use crate::io;
use crate::model::{BlockKind, Page, PageKind, Workspace};

// ───────────────────────────── ZIP ─────────────────────────────

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(data: &[u8]) -> u32 {
    !data
        .iter()
        .fold(!0u32, |c, b| CRC_TABLE[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8))
}

struct CdEntry {
    name: String,
    crc: u32,
    csize: u32,
    size: u32,
    method: u16,
    offset: u32,
}

/// Minimal streaming ZIP writer (deflate or stored, UTF-8 names, no ZIP64: entries and archive stay below 4 GiB).
#[derive(Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<CdEntry>,
}

fn already_compressed(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" | "zip" | "mp4" | "mp3" | "woff2"
    )
}

impl ZipWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> bool {
        if data.len() >= u32::MAX as usize / 2 || self.out.len() >= u32::MAX as usize / 2 {
            return false;
        }
        let packed = if already_compressed(name) || data.len() < 64 {
            None
        } else {
            Some(compress_to_vec(data, 6)).filter(|c| c.len() < data.len())
        };
        let (method, body): (u16, &[u8]) = match &packed {
            Some(c) => (8, c),
            None => (0, data),
        };
        let crc = crc32(data);
        let offset = self.out.len() as u32;
        let o = &mut self.out;
        o.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        o.extend_from_slice(&20u16.to_le_bytes());
        o.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        o.extend_from_slice(&method.to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes()); // time
        o.extend_from_slice(&0x0021u16.to_le_bytes()); // date 1980-01-01
        o.extend_from_slice(&crc.to_le_bytes());
        o.extend_from_slice(&(body.len() as u32).to_le_bytes());
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o.extend_from_slice(&(name.len() as u16).to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(name.as_bytes());
        o.extend_from_slice(body);
        self.entries.push(CdEntry {
            name: name.to_owned(),
            crc,
            csize: body.len() as u32,
            size: data.len() as u32,
            method,
            offset,
        });
        true
    }

    pub fn finish(mut self) -> Vec<u8> {
        let start = self.out.len() as u32;
        for e in &self.entries {
            let o = &mut self.out;
            o.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            o.extend_from_slice(&20u16.to_le_bytes());
            o.extend_from_slice(&20u16.to_le_bytes());
            o.extend_from_slice(&0x0800u16.to_le_bytes());
            o.extend_from_slice(&e.method.to_le_bytes());
            o.extend_from_slice(&0u16.to_le_bytes());
            o.extend_from_slice(&0x0021u16.to_le_bytes());
            o.extend_from_slice(&e.crc.to_le_bytes());
            o.extend_from_slice(&e.csize.to_le_bytes());
            o.extend_from_slice(&e.size.to_le_bytes());
            o.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            o.extend_from_slice(&[0u8; 8]); // extra len, comment len, disk number, internal attrs
            o.extend_from_slice(&[0u8; 4]); // external attrs
            o.extend_from_slice(&e.offset.to_le_bytes());
            o.extend_from_slice(e.name.as_bytes());
        }
        let size = self.out.len() as u32 - start;
        let n = self.entries.len() as u16;
        let o = &mut self.out;
        o.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        o.extend_from_slice(&[0u8; 4]);
        o.extend_from_slice(&n.to_le_bytes());
        o.extend_from_slice(&n.to_le_bytes());
        o.extend_from_slice(&size.to_le_bytes());
        o.extend_from_slice(&start.to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        self.out
    }
}

#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub name: String,
    method: u16,
    csize: usize,
    size: usize,
    offset: usize,
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Lists the entries of a ZIP archive (central directory). Directories are included (name ends with `/`).
pub fn read_zip(b: &[u8]) -> Result<Vec<ZipEntry>, String> {
    let bad = || "That file is not a valid ZIP archive".to_owned();
    let lo = b.len().saturating_sub(65_557);
    let eocd = (lo..b.len().saturating_sub(21))
        .rev()
        .find(|&i| b[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(bad)?;
    let n = u16_at(b, eocd + 10).ok_or_else(bad)?;
    let mut p = u32_at(b, eocd + 16).ok_or_else(bad)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        if b.get(p..p + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(bad());
        }
        let (method, csize, size) = (
            u16_at(b, p + 10).ok_or_else(bad)?,
            u32_at(b, p + 20).ok_or_else(bad)?,
            u32_at(b, p + 24).ok_or_else(bad)?,
        );
        let (nl, el, cl) = (
            u16_at(b, p + 28).ok_or_else(bad)?,
            u16_at(b, p + 30).ok_or_else(bad)?,
            u16_at(b, p + 32).ok_or_else(bad)?,
        );
        let offset = u32_at(b, p + 42).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(b.get(p + 46..p + 46 + nl).ok_or_else(bad)?).replace('\\', "/");
        v.push(ZipEntry {
            name,
            method: method as u16,
            csize,
            size,
            offset,
        });
        p += 46 + nl + el + cl;
    }
    Ok(v)
}

/// Unpacks one entry. `limit` guards against decompression bombs.
pub fn unpack(b: &[u8], e: &ZipEntry, limit: usize) -> Option<Vec<u8>> {
    if e.size > limit {
        return None;
    }
    let (nl, el) = (u16_at(b, e.offset + 26)?, u16_at(b, e.offset + 28)?);
    let start = e.offset + 30 + nl + el;
    let raw = b.get(start..start + e.csize)?;
    match e.method {
        0 => Some(raw.to_vec()),
        8 => decompress_to_vec_with_limit(raw, limit.min(e.size.max(1) + 1024)).ok(),
        _ => None,
    }
}

// ───────────────────────────── export ─────────────────────────────

#[derive(Default)]
pub struct Exported {
    pub zip: Vec<u8>,
    pub pages: usize,
    pub databases: usize,
    pub assets: usize,
    /// Whiteboards and flows have no Markdown form (they stay in the JSON backup).
    pub skipped: usize,
}

/// Howard Hinnant's civil-from-days: epoch milliseconds → `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso(ms: f64) -> String {
    let secs = (ms / 1000.0).max(0.0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn yaml(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' | '\t' => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn front_matter(ws: &Workspace, p: &Page) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("title: {}\n", yaml(&ws.display_title(p.id))));
    if !p.icon.is_empty() {
        s.push_str(&format!("icon: {}\n", yaml(&p.icon)));
    }
    s.push_str(&format!("lumen_id: {}\n", p.id));
    if let Some(par) = p.parent.and_then(|x| ws.page(x)) {
        s.push_str(&format!("parent: {}\n", yaml(&ws.display_title(par.id))));
    }
    if p.updated > 0.0 {
        s.push_str(&format!("updated: {}\n", iso(p.updated)));
    }
    if p.favorite {
        s.push_str("favorite: true\n");
    }
    if p.row {
        if let Some(d) = p.parent.and_then(|x| ws.db(x)) {
            let props: Vec<String> = d
                .props
                .iter()
                .filter_map(|pr| p.props.get(&pr.id).map(|v| (pr, d.val_text(pr.id, v))))
                .filter(|(_, t)| !t.is_empty())
                .map(|(pr, t)| format!("  {}: {}\n", yaml(&pr.name), yaml(&t)))
                .collect();
            if !props.is_empty() {
                s.push_str("properties:\n");
                props.iter().for_each(|l| s.push_str(l));
            }
        }
    }
    s.push_str("---\n\n");
    s
}

fn unique(used: &mut HashSet<String>, path: String) -> String {
    let (stem, ext) = match path.rsplit_once('.') {
        Some((a, b)) if !b.contains('/') => (a.to_owned(), format!(".{b}")),
        _ => (path.clone(), String::new()),
    };
    let mut cand = path;
    let mut n = 2;
    while !used.insert(cand.to_lowercase()) {
        cand = format!("{stem} ({n}){ext}");
        n += 1;
    }
    cand
}

struct Exp<'a> {
    ws: &'a Workspace,
    zip: ZipWriter,
    used: HashSet<String>,
    media: &'a dyn Fn(u64) -> Option<(String, Arc<[u8]>)>,
    assets: RefCell<BTreeMap<u64, String>>,
    out: Exported,
}

impl Exp<'_> {
    fn asset_path(&self, id: u64) -> Option<String> {
        if let Some(p) = self.assets.borrow().get(&id) {
            return Some(p.clone());
        }
        let (name, _) = (self.media)(id)?;
        let (stem, ext) = name
            .rsplit_once('.')
            .map(|(a, b)| (a.to_owned(), format!(".{}", io::file_safe(b))))
            .unwrap_or((name.clone(), String::new()));
        let path = format!("assets/{id:x}-{}{ext}", io::file_safe(&stem).replace(' ', "-"));
        self.assets.borrow_mut().insert(id, path.clone());
        Some(path)
    }

    fn walk(&mut self, parent: Option<u64>, dir: &str, depth: usize) {
        let kids: Vec<&Page> = self.ws.pages.iter().filter(|p| p.parent == parent && !p.trashed).collect();
        for p in kids {
            if matches!(p.kind, PageKind::Canvas | PageKind::Flow) {
                self.out.skipped += 1;
                continue;
            }
            let t = self.ws.display_title(p.id);
            let base = match io::file_safe(&t) {
                s if s.trim().is_empty() => "Untitled".to_owned(),
                s => s,
            };
            let file = unique(&mut self.used, format!("{dir}{base}.md"));
            let stem = file.trim_end_matches(".md").rsplit('/').next().unwrap_or(&base).to_owned();
            let prefix = "../".repeat(depth);
            let md = {
                let up = prefix.clone();
                io::page_markdown_with(self.ws, p.id, &|m| self.asset_path(m).map(|a| format!("{up}{a}")))
            };
            let body = format!("{}{md}", front_matter(self.ws, p));
            self.zip.add(&file, body.as_bytes());
            self.out.pages += 1;
            if p.db.is_some() {
                self.out.databases += 1;
                let csv = io::db_csv(self.ws, p.id);
                self.zip.add(&format!("{}.csv", file.trim_end_matches(".md")), csv.as_bytes());
            }
            if self.ws.pages.iter().any(|c| c.parent == Some(p.id) && !c.trashed) {
                self.used.insert(format!("{dir}{stem}/").to_lowercase());
                let sub = format!("{dir}{stem}/");
                self.walk(Some(p.id), &sub, depth + 1);
            }
        }
    }
}

/// Packs the whole workspace into a Markdown vault ZIP. `media` returns `(file name, bytes)` for a local media id.
pub fn export_zip(ws: &Workspace, media: &dyn Fn(u64) -> Option<(String, Arc<[u8]>)>) -> Exported {
    let mut e = Exp {
        ws,
        zip: ZipWriter::new(),
        used: HashSet::new(),
        media,
        assets: RefCell::new(BTreeMap::new()),
        out: Exported::default(),
    };
    e.walk(None, "", 0);
    let assets: Vec<(u64, String)> = e.assets.borrow().iter().map(|(k, v)| (*k, v.clone())).collect();
    for (id, path) in assets {
        if let Some((_, bytes)) = media(id) {
            if e.zip.add(&path, &bytes) {
                e.out.assets += 1;
            }
        }
    }
    let readme = format!("# {}\n\nExported from Lumen on {}.\n\n* `*.md` — pages (YAML front-matter + Markdown), folders mirror the page tree\n* `*.csv` — databases (rows are also exported as pages)\n* `assets/` — pictures and attached files\n\nOpen this folder in Obsidian, or import the ZIP again in Lumen (Import → ZIP vault).\n", ws.name, crate::dates::long_today());
    e.zip.add("_README.md", readme.as_bytes());
    let mut out = e.out;
    out.zip = e.zip.finish();
    out
}

// ───────────────────────────── import ─────────────────────────────

#[derive(Default, Debug)]
pub struct Imported {
    pub pages: usize,
    pub databases: usize,
    pub assets: usize,
    pub skipped: usize,
    pub first: Option<u64>,
}

/// Removes the 32-hex id Notion appends to every file and folder name (`Roadmap 1a2b…`).
pub fn strip_notion_id(name: &str) -> &str {
    match name.len().checked_sub(33).and_then(|i| name.get(i..)) {
        Some(t) if t.starts_with(' ') && t[1..].bytes().all(|b| b.is_ascii_hexdigit()) => name[..name.len() - 33].trim_end(),
        _ => name,
    }
}

/// Splits `---` front-matter off. Returns the `title:` value (when present) and the body.
pub fn split_front_matter(text: &str) -> (Option<String>, &str) {
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---\n").or_else(|| t.strip_prefix("---\r\n")) else {
        return (None, t);
    };
    let Some(end) = rest.find("\n---") else { return (None, t) };
    let head = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.split_once('\n').map_or("", |x| x.1);
    let title = head
        .lines()
        .find_map(|l| l.strip_prefix("title:"))
        .map(|v| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .map(|x| x.replace("\\\"", "\"").replace("\\\\", "\\"))
                .unwrap_or_else(|| v.trim_matches('\'').to_owned());
            v
        })
        .filter(|v| !v.is_empty());
    (title, body)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                o.push(v);
                i += 3;
                continue;
            }
        }
        o.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&o).into_owned()
}

fn is_image(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
}

/// Obsidian `![[pic.png]]` → `![](pic.png)`.
fn embeds_to_links(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(i) = rest.find("![[") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 3..];
        match tail.find("]]") {
            Some(j) if is_image(tail[..j].split('|').next().unwrap_or("")) => {
                out.push_str(&format!("![]({})", tail[..j].split('|').next().unwrap_or("").replace(' ', "%20")));
                rest = &tail[j + 2..];
            }
            _ => {
                out.push_str("![[");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if rel.starts_with('/') {
        vec![]
    } else {
        dir.split('/').filter(|x| !x.is_empty()).collect()
    };
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |x| x.0)
}

fn ensure_dir(ws: &mut Workspace, map: &mut HashMap<String, u64>, dir: &str, count: &mut usize) -> Option<u64> {
    if dir.is_empty() {
        return None;
    }
    if let Some(id) = map.get(dir) {
        return Some(*id);
    }
    let parent = ensure_dir(ws, map, dir_of(dir), count);
    let id = ws.add_page(parent, PageKind::Doc);
    if let Some(p) = ws.page_mut(id) {
        p.title = strip_notion_id(dir.rsplit('/').next().unwrap_or(dir)).to_owned();
        p.icon = "📁".into();
    }
    *count += 1;
    map.insert(dir.to_owned(), id);
    Some(id)
}

/// Imports a Notion / Obsidian ZIP. `add_media(name, bytes)` stores a picture and returns its media id.
pub fn import_zip(ws: &mut Workspace, bytes: &[u8], add_media: &mut dyn FnMut(&str, Vec<u8>) -> Option<u64>) -> Result<Imported, String> {
    const LIMIT: usize = 64 << 20;
    let mut entries: Vec<ZipEntry> = read_zip(bytes)?
        .into_iter()
        .filter(|e| {
            !e.name.ends_with('/')
                && !e.name.starts_with("__MACOSX")
                && !e.name.contains("/.")
                && !e.name.starts_with('.')
                && !e.name.split('/').any(|c| c == ".." || c == ".obsidian" || c == ".trash")
        })
        .collect();
    // a single wrapping folder ("Export-1234/…") is not a page of its own
    let top: BTreeSet<&str> = entries.iter().filter_map(|e| e.name.split_once('/').map(|x| x.0)).collect();
    let wrapped = top.len() == 1 && entries.iter().all(|e| e.name.contains('/'));
    let strip = |n: &str| -> String {
        if wrapped {
            n.split_once('/').map_or(n, |x| x.1).to_owned()
        } else {
            n.to_owned()
        }
    };
    for e in &mut entries {
        e.name = strip(&e.name);
    }
    let mut assets: HashMap<String, usize> = HashMap::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if is_image(&e.name) {
            assets.insert(e.name.to_lowercase(), i);
            by_name.entry(e.name.rsplit('/').next().unwrap_or("").to_lowercase()).or_insert(i);
        }
    }
    let mut docs: Vec<usize> = (0..entries.len())
        .filter(|i| {
            matches!(
                entries[*i].name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(),
                Some("md" | "markdown")
            )
        })
        .collect();
    docs.sort_by_key(|i| (entries[*i].name.matches('/').count(), entries[*i].name.to_lowercase()));
    let mut rep = Imported::default();
    let mut map: HashMap<String, u64> = HashMap::new();
    let mut folders = 0usize;
    let mut media_cache: HashMap<usize, Option<u64>> = HashMap::new();

    for i in docs {
        let e = entries[i].clone();
        let Some(raw) = unpack(bytes, &e, LIMIT) else {
            rep.skipped += 1;
            continue;
        };
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (fm_title, body) = split_front_matter(&text);
        let dir = dir_of(&e.name).to_owned();
        let file = e.name.rsplit('/').next().unwrap_or(&e.name);
        let stem_raw = file.rsplit_once('.').map_or(file, |x| x.0).to_owned();
        let title = fm_title.unwrap_or_else(|| strip_notion_id(&stem_raw).to_owned());
        // Notion repeats the title as the first heading
        let body = match body.trim_start().split_once('\n') {
            Some((first, rest)) if first.trim_start_matches('#').trim().eq_ignore_ascii_case(&title) && first.starts_with('#') => rest,
            _ => body,
        };
        let body = embeds_to_links(body);
        let parent = ensure_dir(ws, &mut map, &dir, &mut folders);
        let key = if dir.is_empty() {
            stem_raw.clone()
        } else {
            format!("{dir}/{stem_raw}")
        };
        // a folder page that was created for a child before its own file was read is filled instead of duplicated
        let id = match map.get(&key).copied() {
            Some(existing) => {
                let tmp = io::import_markdown(ws, &title, &body, parent);
                let blocks = ws.page(tmp).map(|p| p.blocks.clone()).unwrap_or_default();
                ws.pages.retain(|p| p.id != tmp);
                if let Some(p) = ws.page_mut(existing) {
                    p.blocks = blocks;
                    p.title = title.clone();
                }
                existing
            }
            None => io::import_markdown(ws, &title, &body, parent),
        };
        map.insert(key, id);
        rep.pages += 1;
        rep.first.get_or_insert(id);
        // local pictures → media
        let n = ws.page(id).map_or(0, |p| p.blocks.len());
        for bi in 0..n {
            let url = match ws.page(id).and_then(|p| p.blocks.get(bi)) {
                Some(b) if b.kind == BlockKind::Image && b.media == 0 && !b.url.starts_with("http") => b.url.clone(),
                _ => continue,
            };
            let rel = pct_decode(&url);
            let hit = assets
                .get(&resolve(&dir, &rel).to_lowercase())
                .or_else(|| by_name.get(&rel.rsplit('/').next().unwrap_or("").to_lowercase()))
                .copied();
            let Some(ai) = hit else { continue };
            let mid = *media_cache.entry(ai).or_insert_with(|| {
                let data = unpack(bytes, &entries[ai], LIMIT)?;
                let name = entries[ai].name.rsplit('/').next().unwrap_or("image").to_owned();
                add_media(&name, data)
            });
            if let (Some(mid), Some(b)) = (mid, ws.page_mut(id).and_then(|p| p.blocks.get_mut(bi))) {
                b.media = mid;
                b.url.clear();
            }
        }
    }
    rep.assets = media_cache.values().filter(|m| m.is_some()).count();
    rep.pages += folders;

    for e in entries
        .iter()
        .filter(|e| e.name.to_ascii_lowercase().ends_with(".csv") && !e.name.to_ascii_lowercase().ends_with("_all.csv"))
    {
        let Some(raw) = unpack(bytes, e, LIMIT) else { continue };
        let text = String::from_utf8_lossy(&raw).trim_start_matches('\u{feff}').to_owned();
        let file = e.name.rsplit('/').next().unwrap_or(&e.name);
        let name = strip_notion_id(file.rsplit_once('.').map_or(file, |x| x.0)).to_owned();
        let parent = ensure_dir(ws, &mut map, dir_of(&e.name), &mut folders);
        match io::import_csv(ws, &name, &text, parent) {
            Some(id) => {
                rep.databases += 1;
                rep.first.get_or_insert(id);
            }
            None => rep.skipped += 1,
        }
    }
    if rep.pages + rep.databases == 0 {
        return Err("The ZIP contains no Markdown (.md) or CSV files".to_owned());
    }
    Ok(rep)
}

// ───────────────────────────── storage manager ─────────────────────────────

#[derive(Default, Clone)]
pub struct Storage {
    pub pages: usize,
    pub trashed: usize,
    pub blocks: usize,
    /// Size of the workspace as saved (JSON).
    pub workspace_bytes: usize,
    pub history_versions: usize,
    pub history_bytes: usize,
    pub media_items: usize,
    pub media_bytes: usize,
    pub annotations: usize,
    /// The five biggest attachments: (name, bytes).
    pub largest: Vec<(String, usize)>,
}

pub fn storage_report(ws: &Workspace, media: impl Iterator<Item = (String, usize)>) -> Storage {
    let mut s = Storage {
        pages: ws.pages.len(),
        ..Default::default()
    };
    s.trashed = ws.pages.iter().filter(|p| p.trashed).count();
    s.blocks = ws.pages.iter().map(|p| p.blocks.len()).sum();
    s.workspace_bytes = serde_json::to_vec(ws).map_or(0, |v| v.len());
    for p in &ws.pages {
        s.history_versions += p.history.len();
        if !p.history.is_empty() {
            s.history_bytes += serde_json::to_vec(&p.history).map_or(0, |v| v.len());
        }
    }
    s.annotations = ws.pdf_annots.values().map(Vec::len).sum();
    let mut all: Vec<(String, usize)> = media.collect();
    s.media_items = all.len();
    s.media_bytes = all.iter().map(|m| m.1).sum();
    all.sort_by(|a, b| b.1.cmp(&a.1));
    all.truncate(5);
    s.largest = all;
    s
}

/// Drops automatic snapshots beyond the newest `keep_auto` of every page (named versions are always kept). Returns how many went.
pub fn prune_history(ws: &mut Workspace, keep_auto: usize) -> usize {
    let mut removed = 0;
    for p in &mut ws.pages {
        let mut auto_seen = 0;
        let before = p.history.len();
        // history is oldest first: walk from the newest
        let mut keep: Vec<bool> = vec![true; before];
        for (i, v) in p.history.iter().enumerate().rev() {
            if !v.manual {
                auto_seen += 1;
                if auto_seen > keep_auto {
                    keep[i] = false;
                }
            }
        }
        let mut it = keep.into_iter();
        p.history.retain(|_| it.next().unwrap_or(true));
        removed += before - p.history.len();
    }
    removed
}

pub fn fmt_bytes(n: usize) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_round_trip_and_crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let mut z = ZipWriter::new();
        let big = "Lumen ".repeat(500);
        z.add("a/b.md", big.as_bytes());
        z.add("small.txt", b"hi");
        z.add("pic.png", &[1, 2, 3, 4]);
        let bytes = z.finish();
        let list = read_zip(&bytes).expect("zip");
        assert_eq!(list.len(), 3);
        assert_eq!(unpack(&bytes, &list[0], 1 << 20).as_deref(), Some(big.as_bytes()));
        assert_eq!(unpack(&bytes, &list[1], 1 << 20).as_deref(), Some(&b"hi"[..]));
        assert!(unpack(&bytes, &list[0], 10).is_none(), "size limit");
        assert!(read_zip(b"not a zip").is_err());
    }

    #[test]
    fn notion_names_front_matter_and_dates() {
        assert_eq!(strip_notion_id("Roadmap 0123456789abcdef0123456789abcdef"), "Roadmap");
        assert_eq!(strip_notion_id("Plain name"), "Plain name");
        let (t, body) = split_front_matter("---\ntitle: \"My \\\"page\\\"\"\ntags: x\n---\n\nHello");
        assert_eq!(t.as_deref(), Some("My \"page\""));
        assert!(body.trim() == "Hello");
        assert_eq!(split_front_matter("no front matter").0, None);
        assert_eq!(iso(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000_000.0), "2023-11-14T22:13:20Z");
        assert_eq!(
            embeds_to_links("a ![[my pic.png|200]] b ![[Note]]"),
            "a ![](my%20pic.png) b ![[Note]]"
        );
        assert_eq!(resolve("a/b", "../c.png"), "a/c.png");
    }

    #[test]
    fn vault_export_then_import_keeps_pages_and_hierarchy() {
        let ws = Workspace::seed();
        let live = ws
            .pages
            .iter()
            .filter(|p| !p.trashed && !matches!(p.kind, PageKind::Canvas | PageKind::Flow))
            .count();
        let out = export_zip(&ws, &|_| None);
        assert_eq!(out.pages, live);
        let names: Vec<String> = read_zip(&out.zip).expect("zip").into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "_README.md"));
        assert!(names.iter().filter(|n| n.ends_with(".md")).count() > live);
        let mut back = Workspace::seed();
        let before = back.pages.len();
        let rep = import_zip(&mut back, &out.zip, &mut |_, _| None).expect("import");
        assert!(rep.pages >= 1 && back.pages.len() > before);
    }

    #[test]
    fn pruning_keeps_named_versions_and_the_newest_snapshots() {
        let mut ws = Workspace::seed();
        let id = ws.pages.iter().find(|p| !p.trashed).map(|p| p.id).expect("page");
        let p = ws.page_mut(id).expect("page");
        p.history.clear();
        for i in 0..10 {
            p.history.push(crate::model::Version {
                ts: i as f64,
                title: "t".into(),
                blocks: vec![],
                manual: i == 0,
                note: String::new(),
            });
        }
        let removed = prune_history(&mut ws, 3);
        let p = ws.page(id).expect("page");
        assert_eq!(removed, 6);
        assert_eq!(p.history.len(), 4);
        assert!(p.history[0].manual && p.history.last().map(|v| v.ts) == Some(9.0));
    }
}
