//! Media library: images and files that live inside the workspace (covers, icons, image blocks, attachments).
//!
//! * Items are content-addressed (FNV-1a hash of the bytes) so the same picture is only stored once.
//! * Native: every item is a file in `%APPDATA%\Lumen\media` (or `~/.local/share/Lumen/media`).
//! * Web: items are kept in the browser's local storage as base64 under `lumen_media_<id>`.
//! * Pictures are decoded, down-scaled (max 1600 px) and re-encoded so a workspace stays small.
//!
//! Sources are plain strings everywhere in the data model: `https://…` for online images, `media:<id>` for local ones.

use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
const MAX_TOTAL: usize = 2_600_000;
#[cfg(not(target_arch = "wasm32"))]
const MAX_TOTAL: usize = 400_000_000;

#[cfg(target_arch = "wasm32")]
pub const MAX_FILE: usize = 1_500_000;
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_FILE: usize = 60_000_000;

#[allow(dead_code)]
const INDEX_KEY: &str = "lumen_media_idx";

#[derive(Clone)]
pub struct MediaItem {
    pub name: String,
    pub mime: String,
    pub data: Arc<[u8]>,
}

#[derive(Serialize, Deserialize)]
struct IndexEntry {
    id: u64,
    name: String,
    mime: String,
}

#[derive(Default)]
pub struct MediaStore {
    pub items: BTreeMap<u64, MediaItem>,
    pending: Vec<u64>,
    removed: Vec<u64>,
    index_dirty: bool,
}

pub fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h | 1
}

pub fn uri_for(id: u64) -> String {
    format!("bytes://lumen-media-{id}")
}

/// Turns a stored source (`media:<id>` or a URL) into an egui image URI.
pub fn src_uri(src: &str) -> String {
    match src.strip_prefix("media:").and_then(|s| s.parse::<u64>().ok()) {
        Some(id) => uri_for(id),
        None => src.to_owned(),
    }
}

pub fn media_id(src: &str) -> Option<u64> {
    src.strip_prefix("media:").and_then(|s| s.parse().ok())
}

#[cfg(not(target_arch = "wasm32"))]
fn data_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local").join("share")))?;
    let dir = base.join("Lumen").join("media");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

impl MediaStore {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let mut s = MediaStore::default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = storage;
            if let Some(dir) = data_dir() {
                if let Some(idx) = std::fs::read_to_string(dir.join("index.json")).ok().and_then(|t| serde_json::from_str::<Vec<IndexEntry>>(&t).ok()) {
                    for e in idx {
                        if let Ok(bytes) = std::fs::read(dir.join(format!("{:016x}.bin", e.id))) {
                            s.items.insert(e.id, MediaItem { name: e.name, mime: e.mime, data: bytes.into() });
                        }
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(st) = storage {
            if let Some(idx) = st.get_string(INDEX_KEY).and_then(|t| serde_json::from_str::<Vec<IndexEntry>>(&t).ok()) {
                for e in idx {
                    if let Some(b) = st.get_string(&format!("lumen_media_{}", e.id)).and_then(|t| b64_decode(&t)) {
                        s.items.insert(e.id, MediaItem { name: e.name, mime: e.mime, data: b.into() });
                    }
                }
            }
        }
        s
    }

    /// Makes every stored item available to egui's image loaders.
    pub fn register(&self, ctx: &egui::Context) {
        for (id, it) in &self.items {
            ctx.include_bytes(uri_for(*id), egui::load::Bytes::from(it.data.clone()));
        }
    }

    pub fn total(&self) -> usize {
        self.items.values().map(|i| i.data.len()).sum()
    }

    pub fn get(&self, id: u64) -> Option<&MediaItem> {
        self.items.get(&id)
    }

    /// Adds a file; returns its id. Pictures are optimised first (see [`prepare_image`]).
    pub fn add(&mut self, ctx: &egui::Context, name: &str, bytes: Vec<u8>, as_image: bool) -> Result<u64, String> {
        let (data, mime) = if as_image {
            prepare_image(&bytes, 1600).ok_or_else(|| "That file is not a supported image (PNG, JPEG, GIF, WebP)".to_owned())?
        } else {
            (bytes, "application/octet-stream".to_owned())
        };
        if data.len() > MAX_FILE {
            return Err(format!("File is too large ({} — limit {})", fmt_size(data.len()), fmt_size(MAX_FILE)));
        }
        let id = fnv(&data);
        if self.items.contains_key(&id) {
            return Ok(id);
        }
        if self.total() + data.len() > MAX_TOTAL {
            return Err("Local media storage is full — paste an image link instead".to_owned());
        }
        let data: Arc<[u8]> = data.into();
        ctx.include_bytes(uri_for(id), egui::load::Bytes::from(data.clone()));
        self.items.insert(id, MediaItem { name: name.to_owned(), mime, data });
        self.pending.push(id);
        self.index_dirty = true;
        Ok(id)
    }

    /// Drops items nobody references any more (run once at start-up).
    pub fn gc(&mut self, used: &HashSet<u64>) {
        let dead: Vec<u64> = self.items.keys().copied().filter(|k| !used.contains(k)).collect();
        for k in dead {
            self.items.remove(&k);
            self.removed.push(k);
            self.index_dirty = true;
        }
    }

    pub fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.index_dirty && self.pending.is_empty() && self.removed.is_empty() {
            return;
        }
        let idx: Vec<IndexEntry> = self.items.iter().map(|(id, it)| IndexEntry { id: *id, name: it.name.clone(), mime: it.mime.clone() }).collect();
        let idx_json = serde_json::to_string(&idx).unwrap_or_default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = storage;
            if let Some(dir) = data_dir() {
                for id in self.pending.drain(..) {
                    if let Some(it) = self.items.get(&id) {
                        let _ = std::fs::write(dir.join(format!("{id:016x}.bin")), &*it.data);
                    }
                }
                for id in self.removed.drain(..) {
                    let _ = std::fs::remove_file(dir.join(format!("{id:016x}.bin")));
                }
                let _ = std::fs::write(dir.join("index.json"), idx_json);
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            for id in self.pending.drain(..) {
                if let Some(it) = self.items.get(&id) {
                    storage.set_string(&format!("lumen_media_{id}"), b64_encode(&it.data));
                }
            }
            for id in self.removed.drain(..) {
                storage.set_string(&format!("lumen_media_{id}"), String::new());
            }
            storage.set_string(INDEX_KEY, idx_json);
        }
        self.index_dirty = false;
    }
}

// ───────────────────────────── images ─────────────────────────────

/// Decodes, down-scales to `max_dim` and re-encodes (JPEG, or PNG when transparent). Keeps the original when it is already small.
pub fn prepare_image(bytes: &[u8], max_dim: u32) -> Option<(Vec<u8>, String)> {
    let fmt = image::guess_format(bytes).ok()?;
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    let small = w.max(h) <= max_dim;
    let mime_of = |f: image::ImageFormat| match f {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => "image/jpeg",
    };
    if small && bytes.len() < 450_000 {
        return Some((bytes.to_vec(), mime_of(fmt).to_owned()));
    }
    let img = if small { img } else { img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle) };
    let mut out = Vec::new();
    let mime;
    if img.color().has_alpha() {
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
        mime = "image/png";
    } else {
        let rgb = img.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&rgb).ok()?;
        mime = "image/jpeg";
    }
    if small && out.len() >= bytes.len() {
        return Some((bytes.to_vec(), mime_of(fmt).to_owned()));
    }
    Some((out, mime.to_owned()))
}

pub fn is_image_name(name: &str) -> bool {
    let l = name.to_lowercase();
    let l = l.split(['?', '#']).next().unwrap_or("");
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"].iter().any(|e| l.ends_with(e))
}

pub fn fmt_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

// ───────────────────────────── base64 ─────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[allow(dead_code)]
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

#[allow(dead_code)]
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ───────────────────────────── links / embeds ─────────────────────────────

pub fn normalize_url(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.contains("://") || t.starts_with("data:") {
        t.to_owned()
    } else {
        format!("https://{t}")
    }
}

pub fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_whitespace) && t.len() > 10
}

pub fn host_of(url: &str) -> String {
    let t = url.split("://").nth(1).unwrap_or(url);
    let h = t.split(['/', '?', '#']).next().unwrap_or("");
    h.trim_start_matches("www.").to_owned()
}

pub fn favicon_url(host: &str) -> String {
    format!("https://www.google.com/s2/favicons?domain={host}&sz=64")
}

fn pct(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EmbedKind {
    Video,
    Audio,
    Map,
    Design,
    Web,
}

#[derive(Clone, Debug)]
pub struct Embed {
    pub kind: EmbedKind,
    pub service: &'static str,
    pub host: String,
    /// Still image shown before the live frame is loaded.
    pub thumb: Option<String>,
    /// URL to load inside an iframe (the web build).
    pub frame: String,
}

fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let q = url.split_once('?')?.1.split('#').next()?;
    q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn path_seg(url: &str, after: &str) -> Option<String> {
    let t = url.split_once(after)?.1;
    let id: String = t.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Recognises well-known services and derives their embeddable URL.
pub fn embed_info(url: &str) -> Embed {
    let host = host_of(url);
    let mut e = Embed { kind: EmbedKind::Web, service: "Website", host: host.clone(), thumb: None, frame: url.to_owned() };
    let yt = if host.ends_with("youtube.com") {
        query_param(url, "v").map(|s| s.to_owned()).or_else(|| path_seg(url, "/embed/")).or_else(|| path_seg(url, "/shorts/")).or_else(|| path_seg(url, "/live/"))
    } else if host == "youtu.be" {
        path_seg(url, "youtu.be/")
    } else {
        None
    };
    if let Some(id) = yt {
        e.kind = EmbedKind::Video;
        e.service = "YouTube";
        e.thumb = Some(format!("https://img.youtube.com/vi/{id}/hqdefault.jpg"));
        e.frame = format!("https://www.youtube.com/embed/{id}");
    } else if host.ends_with("vimeo.com") {
        if let Some(id) = path_seg(url, "vimeo.com/").filter(|s| s.chars().all(|c| c.is_ascii_digit())) {
            e.kind = EmbedKind::Video;
            e.service = "Vimeo";
            e.frame = format!("https://player.vimeo.com/video/{id}");
        }
    } else if host.ends_with("loom.com") {
        if let Some(id) = path_seg(url, "/share/").or_else(|| path_seg(url, "/embed/")) {
            e.kind = EmbedKind::Video;
            e.service = "Loom";
            e.frame = format!("https://www.loom.com/embed/{id}");
        }
    } else if host == "open.spotify.com" {
        if let Some(rest) = url.split_once("spotify.com/").map(|x| x.1) {
            let rest = rest.split(['?', '#']).next().unwrap_or("");
            e.kind = EmbedKind::Audio;
            e.service = "Spotify";
            e.frame = format!("https://open.spotify.com/embed/{rest}");
        }
    } else if host.ends_with("google.com") && url.contains("/maps") || host == "maps.app.goo.gl" {
        e.kind = EmbedKind::Map;
        e.service = "Google Maps";
        e.frame = format!("https://www.google.com/maps?q={}&output=embed", pct(url.split("/maps/place/").nth(1).and_then(|s| s.split('/').next()).unwrap_or(&host)));
    } else if host.ends_with("figma.com") {
        e.kind = EmbedKind::Design;
        e.service = "Figma";
        e.frame = format!("https://www.figma.com/embed?embed_host=lumen&url={}", pct(url));
    } else if host.ends_with("codepen.io") {
        e.service = "CodePen";
        e.frame = url.replace("/pen/", "/embed/");
    } else if host.ends_with("docs.google.com") || host.ends_with("drive.google.com") {
        e.service = "Google Docs";
        e.frame = if url.contains("/edit") { url.replace("/edit", "/preview") } else { url.to_owned() };
    }
    e
}

/// Twemoji (CC-BY 4.0) image for an emoji, so every emoji renders in colour on every platform.
pub fn twemoji_url(emoji: &str) -> String {
    let zwj = emoji.contains('\u{200d}');
    let cps: Vec<String> = emoji.chars().filter(|c| zwj || *c != '\u{fe0f}').map(|c| format!("{:x}", c as u32)).collect();
    format!("https://cdn.jsdelivr.net/gh/jdecked/twemoji@15.1.0/assets/72x72/{}.png", cps.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data);
        }
    }

    #[test]
    fn embed_detection() {
        let e = embed_info("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3");
        assert_eq!(e.service, "YouTube");
        assert_eq!(e.frame, "https://www.youtube.com/embed/dQw4w9WgXcQ");
        assert!(e.thumb.unwrap().contains("dQw4w9WgXcQ"));
        assert_eq!(embed_info("https://youtu.be/abc123").frame, "https://www.youtube.com/embed/abc123");
        assert_eq!(embed_info("https://vimeo.com/76979871").frame, "https://player.vimeo.com/video/76979871");
        assert_eq!(embed_info("https://example.com/a").kind, EmbedKind::Web);
        assert_eq!(host_of("https://www.example.com/a?b=1"), "example.com");
        assert_eq!(normalize_url("example.com"), "https://example.com");
    }

    #[test]
    fn twemoji_names() {
        assert!(twemoji_url("😀").ends_with("/1f600.png"));
        assert!(twemoji_url("❤\u{fe0f}").ends_with("/2764.png"));
        assert!(twemoji_url("👩\u{200d}💻").ends_with("/1f469-200d-1f4bb.png"));
    }

    #[test]
    fn image_pipeline_shrinks_big_pictures() {
        let mut img = image::RgbImage::new(2400, 1200);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8]);
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let (out, mime) = prepare_image(&png, 1600).unwrap();
        assert_eq!(mime, "image/jpeg");
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!(back.width(), 1600);
        assert!(prepare_image(b"not an image", 1600).is_none());
    }
}
