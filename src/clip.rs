//! In-app rich clipboard.
//!
//! The operating-system clipboard only carries text, so a copied block would come back as a Markdown "slab".
//! Whenever we copy blocks (or a formatted fragment of a block) we additionally remember the *exact* data here, together with the
//! text we put on the system clipboard. On paste, if the system clipboard text still matches, the exact blocks / formatting are
//! restored instead of the text.

use std::cell::RefCell;

use crate::model::Block;
use crate::rich::Rich;

#[derive(Clone)]
pub struct BlockClip {
    /// Text that was written to the system clipboard (Markdown); used to recognise our own clip.
    pub text: String,
    /// Blocks with indents normalised so that the shallowest one is 0.
    pub blocks: Vec<Block>,
}

thread_local! {
    static BLOCKS: RefCell<Option<BlockClip>> = const { RefCell::new(None) };
    static RICH: RefCell<Option<Rich>> = const { RefCell::new(None) };
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_owned()
}

pub fn set_blocks(text: String, mut blocks: Vec<Block>) {
    let min = blocks.iter().map(|b| b.indent).min().unwrap_or(0);
    for b in &mut blocks {
        b.indent -= min;
    }
    BLOCKS.with(|c| *c.borrow_mut() = Some(BlockClip { text: norm(&text), blocks }));
    RICH.with(|c| *c.borrow_mut() = None);
}

/// The exact blocks behind a pasted text, if it is what we copied last.
pub fn blocks_for(pasted: &str) -> Option<Vec<Block>> {
    let p = norm(pasted);
    BLOCKS.with(|c| c.borrow().as_ref().filter(|b| b.text == p && !p.is_empty()).map(|b| b.blocks.clone()))
}

pub fn set_rich(fragment: Rich) {
    RICH.with(|c| *c.borrow_mut() = Some(fragment));
    BLOCKS.with(|c| *c.borrow_mut() = None);
}

/// The formatted fragment behind a pasted text, if it is what we copied last.
pub fn rich_for(pasted: &str) -> Option<Rich> {
    RICH.with(|c| c.borrow().as_ref().filter(|r| r.plain == pasted.replace("\r\n", "\n") && !pasted.is_empty()).cloned())
}

/// What a pasted text turns into.
pub enum Pasted {
    /// Real blocks (headings, lists, to-dos, quotes, code, images, tables, equations, callouts …).
    Blocks(Vec<Block>),
    /// Inline Markdown (`**bold**`, `` `code` ``, `[link](url)` …) inside one paragraph: inserted at the caret with its formatting.
    Inline(Rich),
    /// Nothing to interpret: the text editor inserts the text as it is.
    Plain,
}

/// Decides what to do with text pasted from elsewhere. Markdown is recognised by its *syntax* (a block prefix, a table, a fence, balanced
/// inline markers, HTML inline tags, `:emoji:` shortcodes); plain text without any of it stays plain (multi-line plain text → one block per line).
pub fn classify(text: &str) -> Pasted {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = t.trim_end_matches('\n');
    if trimmed.is_empty() {
        return Pasted::Plain;
    }
    if !trimmed.contains('\n') && crate::media::looks_like_url(trimmed.trim()) {
        return Pasted::Plain; // a lone link keeps its "turn into bookmark" behaviour
    }
    if let Some(blocks) = markdown_blocks(&t) {
        if blocks.len() == 1 && blocks[0].kind == crate::model::BlockKind::Text && !blocks[0].text.contains('\n') {
            return Pasted::Inline(Rich::parse(&blocks[0].text));
        }
        return Pasted::Blocks(blocks);
    }
    if trimmed.contains('\n') {
        let blocks: Vec<Block> = trimmed.split('\n').map(|l| Block::new(0, crate::model::BlockKind::Text, &Rich::plain(l).markup(), 0)).collect();
        return Pasted::Blocks(blocks);
    }
    Pasted::Plain
}

/// Parses pasted Markdown into blocks (ids are assigned on insertion). `None` when the text has no Markdown syntax at all.
pub fn markdown_blocks(md: &str) -> Option<Vec<Block>> {
    // spreadsheet cells (tab separated) become a table block
    if let Some(t) = crate::mdtable::from_tsv(md) {
        let mut b = Block::new(0, crate::model::BlockKind::Table, "", 0);
        b.kind = crate::model::BlockKind::Table;
        b.text = crate::mdtable::encode(&t.rows);
        b.url = t.align;
        b.checked = true;
        return Some(vec![b]);
    }
    let (rest, tables) = crate::mdtable::extract(md);
    let (mut out, structured) = parse_md(&rest, !tables.is_empty());
    if !structured || out.is_empty() {
        return None;
    }
    crate::mdtable::restore(&mut out, &tables);
    Some(out)
}

// ───────────────────────────── inline Markdown / HTML → Lumen markup ─────────────────────────────

const SHORTCODES: &[(&str, &str)] = &[
    ("smile", "😄"), ("grin", "😁"), ("joy", "😂"), ("wink", "😉"), ("heart_eyes", "😍"), ("thinking", "🤔"), ("sob", "😭"), ("cry", "😢"), ("sunglasses", "😎"),
    ("rocket", "🚀"), ("tada", "🎉"), ("fire", "🔥"), ("sparkles", "✨"), ("star", "⭐"), ("zap", "⚡"), ("boom", "💥"), ("bulb", "💡"), ("bug", "🐛"),
    ("white_check_mark", "✅"), ("heavy_check_mark", "✔️"), ("x", "❌"), ("warning", "⚠️"), ("no_entry", "⛔"), ("question", "❓"), ("exclamation", "❗"),
    ("memo", "📝"), ("books", "📚"), ("book", "📖"), ("pushpin", "📌"), ("paperclip", "📎"), ("link", "🔗"), ("lock", "🔒"), ("key", "🔑"), ("mag", "🔍"),
    ("calendar", "📅"), ("clipboard", "📋"), ("chart_with_upwards_trend", "📈"), ("bar_chart", "📊"), ("package", "📦"), ("hammer", "🔨"), ("wrench", "🔧"),
    ("gear", "⚙️"), ("construction", "🚧"), ("computer", "💻"), ("iphone", "📱"), ("email", "📧"), ("bell", "🔔"), ("bookmark", "🔖"), ("trophy", "🏆"),
    ("dart", "🎯"), ("hourglass", "⌛"), ("clock", "🕐"), ("bangbang", "‼️"), ("100", "💯"), ("+1", "👍"), ("thumbsup", "👍"), ("-1", "👎"), ("thumbsdown", "👎"),
    ("clap", "👏"), ("pray", "🙏"), ("muscle", "💪"), ("eyes", "👀"), ("point_right", "👉"), ("point_left", "👈"), ("wave", "👋"), ("heart", "❤️"),
    ("blue_heart", "💙"), ("green_heart", "💚"), ("globe_with_meridians", "🌐"), ("earth_africa", "🌍"), ("sunny", "☀️"), ("cloud", "☁️"), ("snowflake", "❄️"),
    ("coffee", "☕"), ("pizza", "🍕"), ("beer", "🍺"), ("gift", "🎁"), ("art", "🎨"), ("microscope", "🔬"), ("moneybag", "💰"), ("dollar", "💵"),
];

fn shortcode(name: &str) -> Option<&'static str> {
    SHORTCODES.iter().find(|(n, _)| *n == name).map(|(_, e)| *e)
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

fn is_word(c: Option<char>) -> bool {
    c.map_or(false, |c| c.is_alphanumeric())
}

/// `__x__` → `**x**`, `_x_` → `*x*` (CommonMark meaning; in Lumen's own markup `__` is underline). Word-internal underscores
/// (`snake_case`) are left alone.
fn emphasis(seg: &str) -> String {
    let ch: Vec<char> = seg.chars().collect();
    let mut out = String::with_capacity(seg.len());
    let mut i = 0;
    while i < ch.len() {
        if ch[i] == '\\' && i + 1 < ch.len() {
            out.push(ch[i]);
            out.push(ch[i + 1]);
            i += 2;
            continue;
        }
        if ch[i] == '_' {
            let run = ch[i..].iter().take_while(|c| **c == '_').count();
            let before = if i > 0 { Some(ch[i - 1]) } else { None };
            let after = ch.get(i + run).copied();
            if (run == 1 || run == 2) && !is_word(before) && after.map_or(false, |c| !c.is_whitespace() && c != '_') {
                // find the closing run of the same length
                let mut j = i + run;
                let mut close = None;
                while j < ch.len() {
                    if ch[j] == '_' {
                        let r2 = ch[j..].iter().take_while(|c| **c == '_').count();
                        let b2 = ch[j - 1];
                        let a2 = ch.get(j + r2).copied();
                        if r2 == run && !b2.is_whitespace() && !is_word(a2) {
                            close = Some(j);
                            break;
                        }
                        j += r2;
                    } else {
                        j += 1;
                    }
                }
                if let Some(j) = close {
                    let mark = if run == 2 { "**" } else { "*" };
                    out.push_str(mark);
                    out.extend(ch[i + run..j].iter());
                    out.push_str(mark);
                    i = j + run;
                    continue;
                }
            }
            out.extend(ch[i..i + run].iter());
            i += run;
            continue;
        }
        out.push(ch[i]);
        i += 1;
    }
    out
}

/// HTML inline tags, `:shortcodes:`, entities and CommonMark emphasis outside code spans → Lumen's markup.
pub fn inline_md(s: &str) -> String {
    // split into code / non-code segments on backticks
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find('`') {
            None => {
                out.push_str(&inline_plain(rest));
                break;
            }
            Some(a) => {
                let run = rest[a..].chars().take_while(|c| *c == '`').count();
                let fence = "`".repeat(run);
                let after = &rest[a + run..];
                match after.find(&fence) {
                    Some(b) => {
                        out.push_str(&inline_plain(&rest[..a]));
                        out.push_str(&rest[a..a + run + b + run]);
                        rest = &rest[a + run + b + run..];
                    }
                    None => {
                        out.push_str(&inline_plain(rest));
                        break;
                    }
                }
            }
        }
    }
    out
}

fn inline_plain(seg: &str) -> String {
    // CommonMark emphasis first (the `__` that HTML <u> produces below is Lumen's underline and must stay), then HTML, shortcodes, entities
    let seg = emphasis(seg);
    let mut t = String::with_capacity(seg.len());
    let ch: Vec<char> = seg.chars().collect();
    let mut i = 0;
    let mut hrefs: Vec<String> = vec![];
    while i < ch.len() {
        if ch[i] == '<' && i + 1 < ch.len() && (ch[i + 1].is_ascii_alphabetic() || ch[i + 1] == '/') {
            if let Some(len) = ch[i..].iter().take(300).position(|c| *c == '>') {
                let tag: String = ch[i + 1..i + len].iter().collect();
                let low = tag.trim().to_lowercase();
                let name = low.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_owned();
                let closing = low.starts_with('/');
                // autolink <https://…>
                if tag.starts_with("http://") || tag.starts_with("https://") || tag.starts_with("mailto:") {
                    t.push_str(&format!("[{tag}]({tag})"));
                    i += len + 1;
                    continue;
                }
                let mark = match name.as_str() {
                    "b" | "strong" => Some("**"),
                    "i" | "em" => Some("*"),
                    "u" | "ins" => Some("__"),
                    "s" | "del" | "strike" => Some("~~"),
                    "mark" => Some("=="),
                    "code" | "kbd" | "tt" => Some("`"),
                    _ => None,
                };
                if let Some(m) = mark {
                    t.push_str(m);
                } else if name == "br" {
                    t.push('\n');
                } else if name == "a" {
                    if closing {
                        let h = hrefs.pop().unwrap_or_default();
                        t.push_str(&format!("]({h})"));
                    } else {
                        let href = tag.find("href=").map(|p| {
                            let v = &tag[p + 5..];
                            let q = v.chars().next().unwrap_or('"');
                            if q == '"' || q == '\'' {
                                v[1..].split(q).next().unwrap_or("").to_owned()
                            } else {
                                v.split_whitespace().next().unwrap_or("").to_owned()
                            }
                        });
                        match href {
                            Some(h) if !h.is_empty() => {
                                hrefs.push(h);
                                t.push('[');
                            }
                            _ => hrefs.push(String::new()),
                        }
                    }
                } else if matches!(name.as_str(), "span" | "p" | "div" | "font" | "sup" | "sub" | "small" | "center" | "img" | "abbr" | "u:p") {
                    // dropped, the content stays
                } else {
                    // not a tag we know: keep the text as it is
                    t.extend(ch[i..=i + len].iter());
                }
                i += len + 1;
                continue;
            }
        }
        t.push(ch[i]);
        i += 1;
    }
    // 2. :shortcodes:
    let mut u = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(a) = rest.find(':') {
        let tail = &rest[a + 1..];
        let name_len = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')).count();
        if name_len > 0 && tail[name_len..].starts_with(':') {
            if let Some(e) = shortcode(&tail[..name_len]) {
                u.push_str(&rest[..a]);
                u.push_str(e);
                rest = &tail[name_len + 1..];
                continue;
            }
        }
        u.push_str(&rest[..=a]);
        rest = tail;
    }
    u.push_str(rest);
    decode_entities(&u)
}

// ───────────────────────────── block Markdown ─────────────────────────────

fn indent_unit(lines: &[&str]) -> usize {
    let mut unit = 0usize;
    for l in lines {
        let t = l.trim_start();
        let lead = l.len() - t.len();
        let list = t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") || t.chars().take_while(|c| c.is_ascii_digit()).count() > 0 && (t.contains(". ") || t.contains(") "));
        if lead > 0 && list && !l.starts_with('\t') {
            unit = if unit == 0 { lead } else { unit.min(lead) };
        }
    }
    if unit == 0 { 2 } else { unit.clamp(2, 4) }
}

fn lead_width(line: &str, unit: usize) -> usize {
    let mut w = 0;
    for c in line.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += unit,
            _ => break,
        }
    }
    w
}

fn is_rule(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

/// `![alt](url "title")` as a whole line.
fn image_line(t: &str) -> Option<(String, String)> {
    let r = t.strip_prefix("![")?;
    let close = r.find("](")?;
    let alt = &r[..close];
    let rest = r[close + 2..].strip_suffix(')')?;
    let url = rest.split_whitespace().next()?.trim_matches(|c| c == '<' || c == '>');
    if url.is_empty() || alt.contains('[') {
        return None;
    }
    Some((alt.to_owned(), url.to_owned()))
}

fn list_marker(t: &str) -> Option<(crate::model::BlockKind, bool, &str)> {
    use crate::model::BlockKind as K;
    for p in ["- [ ] ", "* [ ] ", "+ [ ] "] {
        if let Some(r) = t.strip_prefix(p) {
            return Some((K::Todo, false, r));
        }
    }
    for p in ["- [x] ", "- [X] ", "* [x] ", "* [X] ", "+ [x] ", "+ [X] "] {
        if let Some(r) = t.strip_prefix(p) {
            return Some((K::Todo, true, r));
        }
    }
    for p in ["- ", "* ", "+ ", "• ", "◦ ", "▪ ", "‣ "] {
        if let Some(r) = t.strip_prefix(p) {
            return Some((K::Bullet, false, r));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < 10 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
        return Some((K::Numbered, false, &t[digits + 2..]));
    }
    None
}

/// Returns the blocks and whether the text contained any Markdown at all.
fn parse_md(md: &str, force: bool) -> (Vec<Block>, bool) {
    use crate::model::BlockKind as K;
    let text = md.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().collect();
    let unit = indent_unit(&lines);
    let mut out: Vec<Block> = vec![];
    let mut structured = force;
    let mut i = 0;
    let mut nest: u8 = 0; // <details> nesting
    let mut para: Option<(String, bool)> = None; // text, previous line ended with a hard break
    let mut last_list: Option<usize> = None; // index of the list item that lazy continuation lines belong to
    let mut prev_blank = true;

    fn push_text(out: &mut Vec<Block>, kind: K, text: &str, checked: bool, indent: u8) {
        let conv = inline_md(text.trim());
        let mut b = Block::new(0, kind, &conv, indent);
        b.checked = checked;
        out.push(b);
    }
    macro_rules! flush {
        () => {
            if let Some((t, _)) = para.take() {
                push_text(&mut out, K::Text, &t, false, nest);
            }
        };
    }

    // front matter (Obsidian / Jekyll)
    if lines.first().map(|l| l.trim()) == Some("---") {
        if let Some(end) = lines.iter().skip(1).position(|l| matches!(l.trim(), "---" | "...")) {
            if end > 0 && lines[1..=end].iter().any(|l| l.contains(':')) {
                out.push(Block::new(0, K::Code, lines[1..=end].join("\n").trim_end(), 0));
                structured = true;
                i = end + 2;
            }
        }
    }
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let t = line.trim_start();
        let lead = lead_width(line, unit);
        let ind = ((lead / unit).min(6) as u8 + nest).min(6);
        // fenced code
        if t.starts_with("```") || t.starts_with("~~~") {
            flush!();
            let fence = &t[..3];
            let mut code = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                let l = lines[i];
                // remove the fence's own indentation
                let strip = l.len() - l.trim_start().len();
                code.push_str(&l[strip.min(lead)..]);
                code.push('\n');
                i += 1;
            }
            i += 1;
            out.push(Block::new(0, K::Code, code.trim_end_matches('\n'), ind));
            structured = true;
            last_list = None;
            prev_blank = false;
            continue;
        }
        // $$ equations
        if t.starts_with("$$") {
            flush!();
            let inner = t.trim_start_matches("$$");
            let mut tex = String::new();
            if let Some(one) = inner.strip_suffix("$$") {
                tex = one.trim().to_owned();
                i += 1;
            } else {
                if !inner.trim().is_empty() {
                    tex.push_str(inner.trim());
                    tex.push('\n');
                }
                i += 1;
                while i < lines.len() && !lines[i].trim().ends_with("$$") {
                    tex.push_str(lines[i]);
                    tex.push('\n');
                    i += 1;
                }
                if i < lines.len() {
                    let last = lines[i].trim().trim_end_matches("$$");
                    tex.push_str(last);
                }
                i += 1;
            }
            out.push(Block::new(0, K::Equation, tex.trim(), nest));
            structured = true;
            last_list = None;
            continue;
        }
        if t.is_empty() {
            flush!();
            prev_blank = true;
            last_list = None;
            i += 1;
            continue;
        }
        // table placeholders produced by `mdtable::extract`
        if crate::mdtable::is_placeholder(t) {
            flush!();
            let mut b = Block::new(0, K::Text, t, nest);
            b.indent = nest;
            out.push(b);
            structured = true;
            prev_blank = false;
            i += 1;
            continue;
        }
        // HTML details / structure lines
        let low = t.to_lowercase();
        if low.starts_with("<details") {
            flush!();
            structured = true;
            i += 1;
            continue;
        }
        if low.starts_with("<summary") {
            flush!();
            let mut inner = t.to_owned();
            for tag in ["<summary>", "</summary>", "<SUMMARY>", "</SUMMARY>"] {
                inner = inner.replace(tag, "");
            }
            push_text(&mut out, K::Toggle, &inner, false, nest);
            nest = (nest + 1).min(5);
            structured = true;
            i += 1;
            continue;
        }
        if low.starts_with("</details") {
            flush!();
            nest = nest.saturating_sub(1);
            i += 1;
            continue;
        }
        if matches!(low.as_str(), "<hr>" | "<hr/>" | "<hr />") {
            flush!();
            out.push(Block::new(0, K::Divider, "", nest));
            structured = true;
            i += 1;
            continue;
        }
        if matches!(low.as_str(), "<div>" | "</div>" | "<p>" | "</p>" | "<center>" | "</center>" | "<br>" | "<br/>" | "<br />" | "<!-- -->") || low.starts_with("<!--") && low.ends_with("-->") {
            i += 1;
            continue;
        }
        // headings
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && (t[hashes..].starts_with(' ') || t.len() == hashes) && t.len() > hashes {
            flush!();
            let body = t[hashes..].trim().trim_end_matches('#').trim_end();
            push_text(&mut out, match hashes { 1 => K::H1, 2 => K::H2, _ => K::H3 }, body, false, nest);
            structured = true;
            last_list = None;
            prev_blank = false;
            i += 1;
            continue;
        }
        // setext headings
        if para.is_none() && list_marker(t).is_none() && !t.starts_with('>') && i + 1 < lines.len() {
            let nx = lines[i + 1].trim();
            if nx.len() >= 2 && (nx.chars().all(|c| c == '=') || nx.chars().all(|c| c == '-')) && !is_rule(t) {
                push_text(&mut out, if nx.starts_with('=') { K::H1 } else { K::H2 }, t, false, nest);
                structured = true;
                i += 2;
                prev_blank = false;
                continue;
            }
        }
        if is_rule(t) {
            flush!();
            out.push(Block::new(0, K::Divider, "", nest));
            structured = true;
            last_list = None;
            prev_blank = false;
            i += 1;
            continue;
        }
        // images
        if let Some((alt, url)) = image_line(t) {
            flush!();
            let mut b = Block::new(0, K::Image, &alt, nest);
            b.url = url;
            out.push(b);
            structured = true;
            last_list = None;
            prev_blank = false;
            i += 1;
            continue;
        }
        // quotes and callouts
        if t.starts_with('>') {
            flush!();
            let mut qlines: Vec<String> = vec![];
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let l = lines[i].trim_start()[1..].strip_prefix(' ').unwrap_or(&lines[i].trim_start()[1..]);
                qlines.push(l.trim_end().to_owned());
                i += 1;
            }
            let callout = qlines.first().map_or(false, |l| l.trim_start().starts_with("[!") && l.contains(']'));
            let mut groups: Vec<String> = vec![];
            let mut cur = String::new();
            let first = if callout { 1 } else { 0 };
            let mut icon = String::new();
            if callout {
                let l = qlines[0].trim();
                let kind = l[2..].split(']').next().unwrap_or("").to_lowercase();
                icon = match kind.as_str() {
                    "note" | "info" => "ℹ️",
                    "tip" | "hint" | "success" => "💡",
                    "important" => "❗",
                    "warning" | "caution" | "attention" => "⚠️",
                    "danger" | "error" | "failure" | "bug" => "🚫",
                    "question" | "faq" => "❓",
                    "quote" | "cite" => "💬",
                    "example" => "📋",
                    _ => "📌",
                }
                .to_owned();
                let after = l.split(']').nth(1).unwrap_or("").trim();
                if !after.is_empty() {
                    cur.push_str(after);
                }
            }
            for l in qlines.iter().skip(first) {
                if l.trim().is_empty() {
                    if !cur.is_empty() {
                        groups.push(std::mem::take(&mut cur));
                    }
                } else {
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(l.trim());
                }
            }
            if !cur.is_empty() {
                groups.push(cur);
            }
            if callout {
                let body = groups.join(" ");
                let b = Block::new(0, K::Callout, &inline_md(&body), nest);
                let _ = &icon;
                out.push(b);
            } else {
                for g in groups {
                    push_text(&mut out, K::Quote, &g, false, nest);
                }
            }
            structured = true;
            last_list = None;
            prev_blank = false;
            continue;
        }
        // lists
        if let Some((kind, checked, body)) = list_marker(t) {
            flush!();
            push_text(&mut out, kind, body, checked, ind);
            last_list = Some(out.len() - 1);
            structured = true;
            prev_blank = false;
            i += 1;
            continue;
        }
        // lazy continuation of a list item
        if let (Some(li), false) = (last_list, prev_blank) {
            if lead > 0 || para.is_none() {
                let add = inline_md(t);
                let b = &mut out[li];
                b.text.push(if raw.ends_with("  ") || raw.ends_with('\\') { '\n' } else { ' ' });
                b.text.push_str(add.trim());
                i += 1;
                continue;
            }
        }
        // paragraph (soft line breaks join, two trailing spaces / backslash keep the break)
        let hard = raw.ends_with("  ") || raw.ends_with('\\');
        let body = t.trim_end_matches('\\').trim_end();
        match &mut para {
            Some((p, prev_hard)) => {
                p.push(if *prev_hard { '\n' } else { ' ' });
                p.push_str(body);
                *prev_hard = hard;
            }
            None => para = Some((body.to_owned(), hard)),
        }
        last_list = None;
        prev_blank = false;
        i += 1;
    }
    if let Some((t, _)) = para.take() {
        push_text(&mut out, K::Text, &t, false, nest);
    }
    // inline syntax alone also counts as Markdown: bold, italic, code, links, shortcodes, HTML tags (joining wrapped lines does not)
    if !structured {
        structured = lines.iter().any(|l| {
            let t = l.trim();
            let c = inline_md(t);
            c != t || crate::editor::parse_spans(&c).iter().any(|s| s.marker)
        });
    }
    (out, structured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BlockKind as K;

    fn kinds(md: &str) -> Vec<(K, String, u8)> {
        markdown_blocks(md).unwrap().into_iter().map(|b| (b.kind, b.text, b.indent)).collect()
    }

    #[test]
    fn headings_lists_todos_quotes_and_rules() {
        let k = kinds("# Title\n\nIntro with **bold**.\n\n- one\n  - nested\n- [x] done\n- [ ] open\n\n1. first\n2) second\n\n> quoted\n\n---\n");
        assert_eq!(k[0], (K::H1, "Title".into(), 0));
        assert_eq!(k[1], (K::Text, "Intro with **bold**.".into(), 0));
        assert_eq!(k[2], (K::Bullet, "one".into(), 0));
        assert_eq!(k[3], (K::Bullet, "nested".into(), 1));
        assert_eq!((k[4].0, k[4].1.as_str()), (K::Todo, "done"));
        assert_eq!(k[5].0, K::Todo);
        assert_eq!((k[6].0, k[7].0), (K::Numbered, K::Numbered));
        assert_eq!(k[8], (K::Quote, "quoted".into(), 0));
        assert_eq!(k[9].0, K::Divider);
    }

    #[test]
    fn four_space_and_tab_indents_nest_one_level_each() {
        let k = kinds("- a\n    - b\n        - c\n- d");
        assert_eq!(k.iter().map(|x| x.2).collect::<Vec<_>>(), vec![0, 1, 2, 0]);
        let k = kinds("- a\n\t- b\n\t\t- c");
        assert_eq!(k.iter().map(|x| x.2).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn wrapped_paragraphs_join_and_hard_breaks_stay() {
        let k = kinds("# T\n\nthis is one\nwrapped paragraph\n\nline one  \nline two\n");
        assert_eq!(k[1].1, "this is one wrapped paragraph");
        assert_eq!(k[2].1, "line one\nline two");
        // list items keep their continuation lines
        let k = kinds("- first item\n  continues here\n- second");
        assert_eq!(k[0].1, "first item continues here");
        assert_eq!(k.len(), 2);
    }

    #[test]
    fn code_math_images_callouts_details_and_tables() {
        let md = "```rust\nfn main() {}\n```\n\n$$\nE = mc^2\n$$\n\n![diagram](https://x.test/a.png)\n\n> [!WARNING]\n> careful\n> now\n\n<details>\n<summary>More</summary>\n\nhidden text\n\n</details>\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n";
        let b = markdown_blocks(md).unwrap();
        assert_eq!((b[0].kind, b[0].text.as_str()), (K::Code, "fn main() {}"));
        assert_eq!((b[1].kind, b[1].text.as_str()), (K::Equation, "E = mc^2"));
        assert_eq!((b[2].kind, b[2].url.as_str(), b[2].text.as_str()), (K::Image, "https://x.test/a.png", "diagram"));
        assert_eq!((b[3].kind, b[3].text.as_str()), (K::Callout, "careful now"));
        assert_eq!((b[4].kind, b[4].text.as_str(), b[4].indent), (K::Toggle, "More", 0));
        assert_eq!((b[5].kind, b[5].text.as_str(), b[5].indent), (K::Text, "hidden text", 1));
        assert_eq!(b[6].kind, K::Table);
        assert_eq!(b.len(), 7);
    }

    #[test]
    fn front_matter_becomes_a_code_block() {
        let b = markdown_blocks("---\ntitle: Hello\ntags: [a]\n---\n\n# Body").unwrap();
        assert_eq!((b[0].kind, b[0].text.as_str()), (K::Code, "title: Hello\ntags: [a]"));
        assert_eq!(b[1].kind, K::H1);
    }

    #[test]
    fn inline_markdown_html_and_shortcodes_convert() {
        assert_eq!(inline_md("__bold__ and _it_ and snake_case_name"), "**bold** and *it* and snake_case_name");
        assert_eq!(inline_md("`__keep__` _x_"), "`__keep__` *x*");
        assert_eq!(inline_md("<b>b</b> <i>i</i> <u>u</u> <s>s</s> <mark>m</mark>"), "**b** *i* __u__ ~~s~~ ==m==");
        assert_eq!(inline_md("<a href=\"https://x.test\">site</a>"), "[site](https://x.test)");
        assert_eq!(inline_md("ship it :rocket: &amp; :tada:"), "ship it 🚀 & 🎉");
        assert_eq!(inline_md("a < b and c > d"), "a < b and c > d");
        assert_eq!(inline_md("<https://x.test>"), "[https://x.test](https://x.test)");
    }

    #[test]
    fn classify_decides_between_blocks_inline_and_plain() {
        assert!(matches!(classify("## Heading"), Pasted::Blocks(b) if b[0].kind == K::H2));
        assert!(matches!(classify("- [ ] task"), Pasted::Blocks(b) if b[0].kind == K::Todo));
        assert!(matches!(classify("some **bold** words"), Pasted::Inline(r) if r.plain == "some bold words"));
        assert!(matches!(classify("just words, 2 * 3 = 6"), Pasted::Plain));
        assert!(matches!(classify("https://example.com/page"), Pasted::Plain));
        assert!(matches!(classify("line one\nline two"), Pasted::Blocks(b) if b.len() == 2 && b[0].kind == K::Text));
        assert!(matches!(classify("snake_case_word and file_name.rs"), Pasted::Plain));
        match classify("a\\*b literal") {
            Pasted::Inline(_) | Pasted::Plain => {}
            Pasted::Blocks(_) => panic!("escaped marker is not a block"),
        }
    }
}
