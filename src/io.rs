//! Import / export: Markdown pages, CSV databases, word statistics.

use crate::dates;
use crate::model::*;

pub fn word_count(page: &Page) -> usize {
    page.title.split_whitespace().count() + page.blocks.iter().map(|b| b.text.split_whitespace().count()).sum::<usize>()
}

pub fn char_count(page: &Page) -> usize {
    page.blocks.iter().map(|b| b.text.chars().count()).sum()
}

fn table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Page → Markdown (databases become a table).
pub fn page_markdown(ws: &Workspace, id: u64) -> String {
    let Some(p) = ws.page(id) else { return String::new() };
    let mut out = String::new();
    if !p.title.is_empty() {
        out.push_str(&format!("# {}\n\n", p.title));
    }
    if let Some(d) = &p.db {
        let rows = ws.query(id, d.active);
        out.push_str("| Name |");
        for pr in &d.props {
            out.push_str(&format!(" {} |", table_cell(&pr.name)));
        }
        out.push_str("\n|---|");
        for _ in &d.props {
            out.push_str("---|");
        }
        out.push('\n');
        for r in rows.iter().filter_map(|r| ws.page(*r)) {
            out.push_str(&format!("| {} |", table_cell(&r.title)));
            for pr in &d.props {
                let t = r.props.get(&pr.id).map(|v| d.val_text(pr.id, v)).unwrap_or_default();
                out.push_str(&format!(" {} |", table_cell(&t)));
            }
            out.push('\n');
        }
        return out;
    }
    let mut counters = [0usize; 8];
    // blocks inside a column layout are exported as plain sequential content (two levels shallower)
    let mut cols: Option<u8> = None;
    for b in &p.blocks {
        let mut shift = 0usize;
        if let Some(k) = cols {
            if b.indent > k {
                shift = 2;
            } else {
                cols = None;
            }
        }
        if b.kind == BlockKind::Columns {
            cols = Some(b.indent);
        }
        let ind = (b.indent as usize).saturating_sub(shift).min(7);
        for c in counters.iter_mut().skip(ind + 1) {
            *c = 0;
        }
        if b.kind == BlockKind::Numbered {
            counters[ind] += 1;
        } else {
            counters[ind] = 0;
        }
        let pad = "  ".repeat(ind);
        let cont = |t: &str, pre: &str| -> String { t.replace('\n', &format!("\n{pad}{}", " ".repeat(pre.chars().count()))) };
        let line = match b.kind {
            BlockKind::Text => format!("{pad}{}", cont(&b.text, "")),
            BlockKind::H1 => format!("{pad}# {}", b.text),
            BlockKind::H2 => format!("{pad}## {}", b.text),
            BlockKind::H3 => format!("{pad}### {}", b.text),
            BlockKind::Todo => format!("{pad}- [{}] {}", if b.checked { "x" } else { " " }, b.text),
            BlockKind::Bullet => format!("{pad}- {}", cont(&b.text, "- ")),
            BlockKind::Numbered => format!("{pad}{}. {}", counters[ind], b.text),
            BlockKind::Toggle => format!("{pad}- ▸ {}", b.text),
            BlockKind::Quote => format!("{pad}> {}", cont(&b.text, "> ").replace('\n', "\n> ")),
            BlockKind::Callout => format!("{pad}> 💡 {}", b.text),
            BlockKind::Code => format!("{pad}```\n{}\n{pad}```", b.text),
            BlockKind::Divider => format!("{pad}---"),
            BlockKind::Toc => String::new(),
            BlockKind::Image => format!("{pad}![{}]({})", b.text, if b.media != 0 { "(embedded image)".to_owned() } else { b.url.clone() }),
            BlockKind::Embed | BlockKind::Bookmark => format!("{pad}[{}]({})", if b.text.is_empty() { b.url.as_str() } else { b.text.as_str() }, b.url),
            BlockKind::File => format!("{pad}📎 {}", if b.url.is_empty() { b.text.clone() } else { format!("[{}]({})", b.text, b.url) }),
            BlockKind::PageLink => format!("{pad}[{}]", b.link.map(|l| ws.display_title(l)).unwrap_or_else(|| "Missing page".into())),
            BlockKind::Columns | BlockKind::Column => String::new(),
            BlockKind::Widget => format!("{pad}[Dashboard widget]"),
            BlockKind::Button => format!("{pad}[Button: {}]", if b.text.is_empty() { "Button" } else { &b.text }),
            BlockKind::DbView => format!("{pad}[Database view: {}]", b.link.map(|l| ws.display_title(l)).unwrap_or_else(|| "none".into())),
        };
        let tight = matches!(b.kind, BlockKind::Bullet | BlockKind::Numbered | BlockKind::Todo | BlockKind::Toggle);
        if !line.is_empty() {
            out.push_str(&line);
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
    }
    out.trim_end().to_owned() + "\n"
}

pub fn file_safe(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let s = s.trim().to_owned();
    if s.is_empty() {
        "Untitled".into()
    } else {
        s
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

pub fn db_csv(ws: &Workspace, id: u64) -> String {
    let Some(d) = ws.db(id) else { return String::new() };
    let mut out = String::from("Name");
    for p in &d.props {
        out.push(',');
        out.push_str(&csv_escape(&p.name));
    }
    out.push('\n');
    for r in ws.query(id, d.active).iter().filter_map(|r| ws.page(*r)) {
        out.push_str(&csv_escape(&r.title));
        for p in &d.props {
            out.push(',');
            out.push_str(&csv_escape(&r.props.get(&p.id).map(|v| d.val_text(p.id, v)).unwrap_or_default()));
        }
        out.push('\n');
    }
    out
}

// ───────────────────────────── import ─────────────────────────────

pub fn import_markdown(ws: &mut Workspace, name: &str, md: &str, parent: Option<u64>) -> u64 {
    let id = ws.add_page(parent, PageKind::Doc);
    let mut blocks: Vec<Block> = vec![];
    let mut title = String::new();
    let mut in_code = false;
    let mut code = String::new();
    let num_re = |s: &str| -> Option<usize> {
        let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && s[digits..].starts_with(". ") {
            Some(digits + 2)
        } else {
            None
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            if in_code {
                blocks.push(ws.blk(BlockKind::Code, code.trim_end_matches('\n'), 0));
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let lead = line.len() - line.trim_start().len();
        let ind = ((line[..lead].chars().map(|c| if c == '\t' { 2 } else { 1 }).sum::<usize>()) / 2).min(6) as u8;
        let t = line.trim_start();
        let (kind, text, checked, indent) = if let Some(r) = t.strip_prefix("### ").or_else(|| t.strip_prefix("#### ")) {
            (BlockKind::H3, r, false, 0)
        } else if let Some(r) = t.strip_prefix("## ") {
            (BlockKind::H2, r, false, 0)
        } else if let Some(r) = t.strip_prefix("# ") {
            if title.is_empty() && blocks.is_empty() {
                title = r.trim().to_owned();
                continue;
            }
            (BlockKind::H1, r, false, 0)
        } else if t == "---" || t == "***" || t == "___" {
            (BlockKind::Divider, "", false, 0)
        } else if let Some(r) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("* [ ] ")) {
            (BlockKind::Todo, r, false, ind)
        } else if let Some(r) = t.strip_prefix("- [x] ").or_else(|| t.strip_prefix("- [X] ")).or_else(|| t.strip_prefix("* [x] ")) {
            (BlockKind::Todo, r, true, ind)
        } else if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
            (BlockKind::Bullet, r, false, ind)
        } else if let Some(n) = num_re(t) {
            (BlockKind::Numbered, &t[n..], false, ind)
        } else if let Some(r) = t.strip_prefix("> ") {
            (BlockKind::Quote, r, false, 0)
        } else {
            (BlockKind::Text, t, false, 0)
        };
        let mut b = ws.blk(kind, text, indent);
        b.checked = checked;
        blocks.push(b);
    }
    if in_code && !code.is_empty() {
        blocks.push(ws.blk(BlockKind::Code, code.trim_end_matches('\n'), 0));
    }
    if blocks.is_empty() {
        blocks.push(ws.blk(BlockKind::Text, "", 0));
    }
    if let Some(p) = ws.page_mut(id) {
        p.title = if title.is_empty() { name.to_owned() } else { title };
        p.blocks = blocks;
        p.icon = "📄".into();
    }
    id
}

pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let first = text.lines().next().unwrap_or("");
    let delim = if first.matches('\t').count() > first.matches(',').count() { '\t' } else { ',' };
    let mut rows = vec![];
    let mut row: Vec<String> = vec![];
    let mut cell = String::new();
    let mut q = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if q {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    q = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' {
            q = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut cell));
            if row.iter().any(|c| !c.is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

pub fn import_csv(ws: &mut Workspace, name: &str, text: &str, parent: Option<u64>) -> Option<u64> {
    let rows = parse_csv(text);
    if rows.len() < 2 || rows[0].len() < 2 {
        return None;
    }
    let id = ws.add_database(parent, ViewKind::Table);
    ws.pages.retain(|p| !(p.row && p.parent == Some(id)));
    if let Some(d) = ws.db_mut(id) {
        d.props.clear();
    }
    let header = &rows[0];
    let body = &rows[1..];
    let mut cols: Vec<(u64, PropKind)> = vec![];
    for (ci, h) in header.iter().enumerate().skip(1) {
        let vals: Vec<&str> = body.iter().map(|r| r.get(ci).map(|s| s.trim()).unwrap_or("")).filter(|s| !s.is_empty()).collect();
        let mut distinct: Vec<&str> = vals.clone();
        distinct.sort();
        distinct.dedup();
        let kind = if vals.is_empty() {
            PropKind::Text
        } else if vals.iter().all(|v| v.parse::<f64>().is_ok()) {
            PropKind::Number
        } else if vals.iter().all(|v| matches!(v.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "x" | "✓")) {
            PropKind::Checkbox
        } else if vals.iter().all(|v| dates::parse(v).is_some()) {
            PropKind::Date
        } else if vals.iter().all(|v| v.starts_with("http://") || v.starts_with("https://")) {
            PropKind::Url
        } else if distinct.len() <= 8 && vals.len() >= distinct.len() * 2 {
            PropKind::Select
        } else {
            PropKind::Text
        };
        let opts: Vec<&str> = if kind == PropKind::Select { distinct } else { vec![] };
        let pid = ws.add_prop(id, if h.trim().is_empty() { "Column" } else { h.trim() }, kind, &opts);
        cols.push((pid, kind));
    }
    for r in body {
        let row = ws.add_row(id, None);
        let mut vals: Vec<(u64, Val)> = vec![];
        for (i, (pid, kind)) in cols.iter().enumerate() {
            let s = r.get(i + 1).map(|s| s.trim()).unwrap_or("");
            if s.is_empty() {
                continue;
            }
            let v = match kind {
                PropKind::Number => s.parse::<f64>().ok().map(Val::Num),
                PropKind::Checkbox => Some(Val::Check(matches!(s.to_lowercase().as_str(), "true" | "yes" | "x" | "✓"))),
                PropKind::Select => ws.opt_id(id, *pid, s).map(Val::Sel),
                _ => Some(Val::Text(s.to_owned())),
            };
            if let Some(v) = v {
                vals.push((*pid, v));
            }
        }
        if let Some(p) = ws.page_mut(row) {
            p.title = r.first().cloned().unwrap_or_default();
            for (k, v) in vals {
                p.props.insert(k, v);
            }
        }
    }
    if let Some(p) = ws.page_mut(id) {
        p.title = name.to_owned();
        p.icon = "📊".into();
    }
    Some(id)
}
