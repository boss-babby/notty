//! Print-ready export: a self-contained HTML document (images inlined as data URIs, databases rendered as tables, timelines
//! as SVG Gantt charts) with print CSS (A4, page-break rules). It opens in the browser's print dialog — "Save as PDF" there
//! yields a PDF with selectable text.

use crate::dates;
use crate::media::{b64_encode, MediaStore};
use crate::model::*;
use crate::rich::{Rich, BOLD, CODE, HIGHLIGHT, ITALIC, STRIKE, UNDERLINE};
use crate::theme::{tag_color, COVERS};

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

fn css(c: egui::Color32) -> String {
    format!("rgb({},{},{})", c.r(), c.g(), c.b())
}

fn safe_url(u: &str) -> bool {
    let l = u.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:") || l.starts_with("data:image/")
}

fn inline(markup: &str) -> String {
    let r = Rich::parse(markup);
    let mut out = String::new();
    for (range, f) in r.runs() {
        let mut t = esc(&r.plain[range.clone()]).replace('\n', "<br>");
        let mut open = String::new();
        let mut close = String::new();
        for (flag, tag) in [(BOLD, "b"), (ITALIC, "i"), (STRIKE, "s"), (CODE, "code"), (UNDERLINE, "u"), (HIGHLIGHT, "mark")] {
            if f.flags & flag != 0 {
                open.push_str(&format!("<{tag}>"));
                close.insert_str(0, &format!("</{tag}>"));
            }
        }
        if f.link != 0 {
            if let Some(url) = r.links.get(f.link as usize - 1).filter(|u| safe_url(u)) {
                open.push_str(&format!("<a href=\"{}\">", esc(url)));
                close.insert_str(0, "</a>");
            }
        }
        t = format!("{open}{t}{close}");
        out.push_str(&t);
    }
    out
}

fn mime_of(name: &str, fallback: &str) -> String {
    if !fallback.is_empty() {
        return fallback.to_owned();
    }
    let l = name.to_lowercase();
    if l.ends_with(".png") {
        "image/png".into()
    } else if l.ends_with(".gif") {
        "image/gif".into()
    } else if l.ends_with(".webp") {
        "image/webp".into()
    } else {
        "image/jpeg".into()
    }
}

fn image_src(media: &MediaStore, src: &str) -> Option<String> {
    if let Some(id) = crate::media::media_id(src) {
        let it = media.get(id)?;
        let data = media.bytes(id)?;
        return Some(format!("data:{};base64,{}", mime_of(&it.name, &it.mime), b64_encode(&data)));
    }
    if safe_url(src) {
        Some(esc(src))
    } else {
        None
    }
}

fn val_html(ws: &Workspace, db: &Database, row: &Page, p: &Prop) -> String {
    let v = row.props.get(&p.id);
    match (p.kind, v) {
        (_, None) => String::new(),
        (PropKind::Select | PropKind::MultiSelect, Some(v)) => {
            let ids: Vec<u64> = match v {
                Val::Sel(o) => vec![*o],
                Val::Multi(m) => m.clone(),
                _ => vec![],
            };
            ids.iter().filter_map(|o| db.opt(p.id, *o)).map(|o| format!("<span class=\"chip\" style=\"background:{}\">{}</span>", css(tag_color(o.color).gamma_multiply(0.22)), esc(&o.name))).collect::<Vec<_>>().join(" ")
        }
        (PropKind::Checkbox | PropKind::Formula | PropKind::Rollup, Some(Val::Check(b))) => (if *b { "☑" } else { "☐" }).to_owned(),
        (PropKind::Relation, Some(Val::Multi(ids))) => ids.iter().filter_map(|i| ws.page(*i)).filter(|r| !r.trashed).map(|r| format!("<span class=\"chip\">{}</span>", esc(if r.title.is_empty() { "Untitled" } else { &r.title }))).collect::<Vec<_>>().join(" "),
        (PropKind::Url, Some(Val::Text(s))) => {
            let href = if s.starts_with("http") { s.clone() } else { format!("https://{s}") };
            format!("<a href=\"{}\">{}</a>", esc(&href), esc(s))
        }
        (PropKind::Date, Some(Val::Text(s))) => esc(&dates::pretty(s)),
        (_, Some(Val::Text(s))) if (s.len() == 10 || s.len() == 21) && dates::parse(s).is_some() => esc(&dates::pretty(s)),
        (_, Some(Val::Num(n))) => esc(&crate::formula::fmt_num(*n)),
        (_, Some(v)) => esc(&db.val_text(p.id, v)),
    }
}

/// Table (and, for timeline views, an SVG Gantt chart) of a database.
fn database_html(ws: &Workspace, id: u64, out: &mut String) {
    database_html_at(ws, id, out, None)
}

fn database_html_at(ws: &Workspace, id: u64, out: &mut String, at: Option<usize>) {
    let Some(db) = ws.db(id) else { return };
    let vi = at.filter(|i| *i < db.views.len()).unwrap_or_else(|| db.active_tab());
    let rows = ws.query(id, vi);
    let view = db.views.get(vi);
    if let (Some(v), true) = (view, view.map_or(false, |v| v.kind == ViewKind::Timeline)) {
        if let Some(svg) = gantt_svg(ws, id, db, v, &rows) {
            out.push_str("<div class=\"gantt\">");
            out.push_str(&svg);
            out.push_str("</div>");
        }
    }
    out.push_str("<table class=\"db\"><thead><tr><th>Name</th>");
    for p in &db.props {
        out.push_str(&format!("<th>{}</th>", esc(&p.name)));
    }
    out.push_str("</tr></thead><tbody>");
    for rid in rows {
        let Some(r) = ws.page(rid) else { continue };
        out.push_str(&format!("<tr><td><b>{}</b></td>", esc(if r.title.is_empty() { "Untitled" } else { &r.title })));
        for p in &db.props {
            out.push_str(&format!("<td>{}</td>", val_html(ws, db, r, p)));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
}

fn gantt_svg(ws: &Workspace, id: u64, db: &Database, view: &View, rows: &[u64]) -> Option<String> {
    let dp = db.date_prop(view)?;
    let dep = db.dep_prop(view, id);
    struct It {
        id: u64,
        title: String,
        s: i64,
        e: i64,
        col: egui::Color32,
    }
    let status = db.group_prop(view);
    let mut items: Vec<It> = vec![];
    for rid in rows {
        let r = ws.page(*rid)?;
        let Some(Val::Text(t)) = r.props.get(&dp) else { continue };
        let Some((s, e)) = dates::span(t) else { continue };
        let col = status
            .and_then(|g| match r.props.get(&g) {
                Some(Val::Sel(o)) => db.opt(g, *o).map(|o| tag_color(o.color)),
                _ => None,
            })
            .unwrap_or(egui::Color32::from_rgb(51, 126, 169));
        items.push(It { id: *rid, title: if r.title.is_empty() { "Untitled".into() } else { r.title.clone() }, s, e, col });
    }
    if items.is_empty() {
        return None;
    }
    let lo = items.iter().map(|i| i.s).min()? - 2;
    let hi = items.iter().map(|i| i.e).max()? + 3;
    let n = (hi - lo + 1) as f32;
    let (label_w, row_h, head) = (170.0_f32, 26.0_f32, 34.0_f32);
    let px = (640.0 / n).clamp(3.0, 36.0);
    let w = label_w + n * px;
    let h = head + items.len() as f32 * row_h + 6.0;
    let x_of = |d: i64| label_w + (d - lo) as f32 * px;
    let mut s = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"100%\" font-family=\"system-ui,sans-serif\" font-size=\"11\"><defs><marker id=\"ah\" markerWidth=\"7\" markerHeight=\"7\" refX=\"6\" refY=\"3.5\" orient=\"auto\"><path d=\"M0,0 L7,3.5 L0,7 z\" fill=\"#666\"/></marker></defs>");
    for d in lo..=hi {
        let (y, m, dd) = dates::civil_from_days(d);
        let x = x_of(d);
        if dd == 1 || d == lo {
            s.push_str(&format!("<line x1=\"{x}\" y1=\"0\" x2=\"{x}\" y2=\"{h}\" stroke=\"#ccc\"/><text x=\"{}\" y=\"13\" font-weight=\"600\">{} {}</text>", x + 3.0, &dates::MONTHS[m as usize - 1][..3], y));
        } else if dates::weekday(y, m, dd) == 0 {
            s.push_str(&format!("<line x1=\"{x}\" y1=\"22\" x2=\"{x}\" y2=\"{h}\" stroke=\"#eee\"/>"));
            if px * 7.0 > 26.0 {
                s.push_str(&format!("<text x=\"{}\" y=\"29\" fill=\"#888\">{dd}</text>", x + 2.0));
            }
        }
    }
    let t = dates::today_day();
    if (lo..=hi).contains(&t) {
        let x = x_of(t) + px / 2.0;
        s.push_str(&format!("<line x1=\"{x}\" y1=\"20\" x2=\"{x}\" y2=\"{h}\" stroke=\"#e03e3e\" stroke-width=\"1.2\"/>"));
    }
    for (i, it) in items.iter().enumerate() {
        let y = head + i as f32 * row_h;
        s.push_str(&format!("<line x1=\"0\" y1=\"{}\" x2=\"{w}\" y2=\"{}\" stroke=\"#eee\"/>", y + row_h, y + row_h));
        s.push_str(&format!("<text x=\"4\" y=\"{}\">{}</text>", y + row_h / 2.0 + 4.0, esc(&it.title.chars().take(26).collect::<String>())));
        let (x0, x1) = (x_of(it.s), x_of(it.e + 1));
        s.push_str(&format!("<rect x=\"{x0}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"5\" fill=\"{}\"/>", y + 4.0, (x1 - x0).max(4.0), row_h - 8.0, css(it.col)));
    }
    if let Some(dpid) = dep {
        for (i, it) in items.iter().enumerate() {
            let Some(Val::Multi(preds)) = ws.page(it.id).and_then(|r| r.props.get(&dpid)) else { continue };
            for p in preds {
                let Some(j) = items.iter().position(|x| x.id == *p) else { continue };
                let (ax, ay) = (x_of(items[j].e + 1), head + j as f32 * row_h + row_h / 2.0);
                let (bx, by) = (x_of(it.s), head + i as f32 * row_h + row_h / 2.0);
                let bad = it.s <= items[j].e;
                s.push_str(&format!(
                    "<polyline points=\"{ax},{ay} {},{ay} {},{by} {bx},{by}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1.4\" marker-end=\"url(#ah)\"/>",
                    ax + 6.0,
                    ax + 6.0,
                    if bad { "#e03e3e" } else { "#666" }
                ));
            }
        }
    }
    s.push_str("</svg>");
    Some(s)
}

const CSS: &str = r#"
@page { size: A4; margin: 16mm 15mm; }
* { box-sizing: border-box; }
html { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body { font: 11.5pt/1.55 -apple-system, "Segoe UI", Inter, system-ui, sans-serif; color: #1f1f1f; margin: 0; background: #f4f4f4; }
.page { max-width: 820px; margin: 0 auto; padding: 28px 44px 60px; background: #fff; min-height: 100vh; }
.bar { position: sticky; top: 0; z-index: 5; display: flex; gap: 10px; align-items: center; padding: 10px 16px; background: #1f1f1f; color: #eee; font: 500 13px system-ui; }
.bar button { background: #2f81f7; color: #fff; border: 0; border-radius: 6px; padding: 7px 14px; font: 600 13px system-ui; cursor: pointer; }
.bar span { opacity: .75; }
.cover { height: 150px; margin: -28px -44px 18px; background-size: cover; background-position: center; }
h1.title { font-size: 28pt; line-height: 1.15; margin: 0 0 14px; }
h1, h2, h3 { break-after: avoid; margin: 1.1em 0 .35em; line-height: 1.25; }
h1 { font-size: 20pt } h2 { font-size: 15.5pt } h3 { font-size: 12.5pt }
p, .blk { margin: .25em 0; }
.blk { display: flex; gap: .5em; break-inside: avoid; }
.mk { flex: none; min-width: 1.3em; text-align: right; color: #555; }
.todo.done .tx { color: #888; text-decoration: line-through; }
blockquote { margin: .6em 0; padding: .1em 1em; border-left: 3px solid #1f1f1f; font-size: 1.1em; break-inside: avoid; }
.callout { display: flex; gap: .6em; background: #f1f1ef; border-radius: 6px; padding: .7em .9em; margin: .5em 0; break-inside: avoid; }
pre { background: #f6f6f4; border-radius: 6px; padding: .8em 1em; font: 9.5pt/1.5 ui-monospace, Consolas, monospace; white-space: pre-wrap; break-inside: avoid; }
code { background: #efefec; padding: .05em .3em; border-radius: 4px; font: .9em ui-monospace, Consolas, monospace; }
mark { background: #fff1a8; } hr { border: 0; border-top: 1px solid #ddd; margin: 1em 0; }
figure { margin: .7em 0; break-inside: avoid; } figure img { max-width: 100%; height: auto; border-radius: 4px; display: block; }
figcaption { font-size: .85em; color: #666; margin-top: .25em; }
a { color: #1a5fb4; }
.card { border: 1px solid #ddd; border-radius: 6px; padding: .5em .8em; margin: .4em 0; break-inside: avoid; }
.card small { color: #666; display: block; word-break: break-all; }
table.db { border-collapse: collapse; width: 100%; margin: .6em 0 1em; font-size: 9.5pt; }
table.db th, table.db td { border: 1px solid #d8d8d8; padding: 4px 7px; text-align: left; vertical-align: top; }
table.db th { background: #f3f3f1; font-weight: 600; } table.db thead { display: table-header-group; } table.db tr { break-inside: avoid; }
.chip { display: inline-block; border-radius: 10px; padding: 0 7px; font-size: .88em; background: #eee; margin: 1px 2px 1px 0; }
.dbtitle { font-weight: 700; font-size: 13pt; margin: 1em 0 .2em; }
.gantt { margin: .6em 0; break-inside: avoid; border: 1px solid #e3e3e3; border-radius: 6px; padding: 6px; }
.toc a { display: block; text-decoration: none; color: #444; padding: 1px 0; }
.foot { margin-top: 40px; color: #999; font-size: 9pt; border-top: 1px solid #eee; padding-top: 8px; }
@media print { body { background: #fff; } .page { padding: 0; max-width: none; min-height: 0; } .bar { display: none; } .cover { margin: 0 0 14px; } }
"#;

fn kind_class(c: u8, bg: u8) -> String {
    let mut st = String::new();
    if c > 0 {
        st.push_str(&format!("color:{};", css(tag_color(c as usize - 1))));
    }
    if bg > 0 {
        st.push_str(&format!("background:{};border-radius:4px;padding:0 .25em;", css(tag_color(bg as usize - 1).gamma_multiply(0.22))));
    }
    st
}

/// Complete printable HTML document for page `id`.
pub fn page_html(ws: &Workspace, media: &MediaStore, id: u64, auto_print: bool) -> String {
    let Some(page) = ws.page(id) else { return String::new() };
    let title = if page.title.is_empty() { "Untitled".to_owned() } else { page.title.clone() };
    let mut o = String::new();
    o.push_str(&format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{CSS}</style></head><body>", esc(&title)));
    o.push_str("<div class=\"bar\"><button onclick=\"window.print()\">Print / Save as PDF</button><span>Tip: choose “Save as PDF” as the destination. Images and databases are included.</span></div><div class=\"page\">");
    // cover
    if let Some(src) = image_src(media, &page.cover_img).filter(|_| !page.cover_img.is_empty()) {
        o.push_str(&format!("<div class=\"cover\" style=\"background-image:url('{src}')\"></div>"));
    } else if let Some(c) = page.cover {
        let (a, b) = COVERS[c % COVERS.len()];
        o.push_str(&format!("<div class=\"cover\" style=\"background:linear-gradient(135deg,rgb({},{},{}),rgb({},{},{}))\"></div>", a.0, a.1, a.2, b.0, b.1, b.2));
    }
    let icon = if page.icon.starts_with("img:") || page.icon.chars().next().map_or(true, |c| (0xE000..=0xF8FF).contains(&(c as u32))) { String::new() } else { format!("{} ", esc(&page.icon)) };
    o.push_str(&format!("<h1 class=\"title\">{icon}{}</h1>", esc(&title)));

    if page.row {
        if let Some(dbp) = page.parent.and_then(|p| ws.db(p)) {
            o.push_str("<table class=\"db\"><tbody>");
            for p in &dbp.props {
                o.push_str(&format!("<tr><th style=\"width:28%\">{}</th><td>{}</td></tr>", esc(&p.name), val_html(ws, dbp, page, p)));
            }
            o.push_str("</tbody></table>");
        }
    }
    if page.kind == PageKind::Database {
        database_html(ws, id, &mut o);
    }

    let mut nums = [0u32; 8];
    // open <div>s of column layouts: (indent of the block, is a Columns container)
    let mut open: Vec<(u8, bool)> = vec![];
    for (bi, b) in page.blocks.iter().enumerate() {
        while let Some(&(k, _)) = open.last() {
            if b.indent <= k {
                o.push_str("</div>");
                open.pop();
            } else {
                break;
            }
        }
        let depth = open.iter().filter(|x| x.1).count();
        let ind = (b.indent as usize).saturating_sub(2 * depth).min(6);
        for n in nums.iter_mut().skip(ind + 1) {
            *n = 0;
        }
        if b.kind != BlockKind::Numbered {
            nums[ind] = 0;
        }
        let pad = format!("margin-left:{}px;", ind * 24);
        let st = kind_class(b.color, b.bg);
        let tx = inline(&b.text);
        match b.kind {
            BlockKind::Text => {
                if b.text.trim().is_empty() {
                    o.push_str(&format!("<p style=\"{pad}\">&nbsp;</p>"));
                } else {
                    o.push_str(&format!("<p style=\"{pad}{st}\">{tx}</p>"));
                }
            }
            BlockKind::H1 => o.push_str(&format!("<h1 style=\"{pad}{st}\">{tx}</h1>")),
            BlockKind::H2 => o.push_str(&format!("<h2 style=\"{pad}{st}\">{tx}</h2>")),
            BlockKind::H3 => o.push_str(&format!("<h3 style=\"{pad}{st}\">{tx}</h3>")),
            BlockKind::Todo => o.push_str(&format!("<div class=\"blk todo {}\" style=\"{pad}\"><span class=\"mk\">{}</span><span class=\"tx\" style=\"{st}\">{tx}</span></div>", if b.checked { "done" } else { "" }, if b.checked { "☑" } else { "☐" })),
            BlockKind::Bullet => o.push_str(&format!("<div class=\"blk\" style=\"{pad}\"><span class=\"mk\">•</span><span style=\"{st}\">{tx}</span></div>")),
            BlockKind::Toggle => o.push_str(&format!("<div class=\"blk\" style=\"{pad}\"><span class=\"mk\">▾</span><span style=\"{st}\"><b>{tx}</b></span></div>")),
            BlockKind::Numbered => {
                nums[ind] += 1;
                o.push_str(&format!("<div class=\"blk\" style=\"{pad}\"><span class=\"mk\">{}.</span><span style=\"{st}\">{tx}</span></div>", nums[ind]));
            }
            BlockKind::Quote => o.push_str(&format!("<blockquote style=\"{pad}{st}\">{tx}</blockquote>")),
            BlockKind::Callout => o.push_str(&format!("<div class=\"callout\" style=\"{pad}{st}\"><span>💡</span><span>{tx}</span></div>")),
            BlockKind::Code => o.push_str(&format!("<pre style=\"{pad}\">{}</pre>", esc(&b.text))),
            BlockKind::Divider => o.push_str("<hr>"),
            BlockKind::Toc => {
                o.push_str("<div class=\"toc\" style=\"margin:.6em 0\">");
                for h in page.blocks.iter().filter(|x| matches!(x.kind, BlockKind::H1 | BlockKind::H2 | BlockKind::H3)) {
                    let lvl = match h.kind {
                        BlockKind::H1 => 0,
                        BlockKind::H2 => 1,
                        _ => 2,
                    };
                    o.push_str(&format!("<a style=\"margin-left:{}px\">{}</a>", lvl * 18, esc(&crate::rich::strip(&h.text))));
                }
                o.push_str("</div>");
            }
            BlockKind::Image => match image_src(media, &b.src()) {
                Some(src) => {
                    let wpct = if b.size == 0 { 100 } else { b.size.min(100) };
                    o.push_str(&format!("<figure style=\"{pad}\"><img src=\"{src}\" style=\"width:{wpct}%\" alt=\"{}\">{}</figure>", esc(&crate::rich::strip(&b.text)), if b.text.is_empty() { String::new() } else { format!("<figcaption>{tx}</figcaption>") }));
                }
                None => o.push_str(&format!("<p style=\"{pad}\"><i>[image unavailable]</i></p>")),
            },
            BlockKind::Embed | BlockKind::Bookmark => {
                let u = b.url.trim();
                o.push_str(&format!("<div class=\"card\" style=\"{pad}\"><b>{}</b><small><a href=\"{}\">{}</a></small></div>", if b.text.is_empty() { esc(&crate::media::host_of(u)) } else { tx }, if safe_url(u) { esc(u) } else { String::new() }, esc(u)));
            }
            BlockKind::File => {
                let name = if b.text.is_empty() { "Attachment".to_owned() } else { crate::rich::strip(&b.text) };
                let pdf = crate::pdf::looks_like_pdf(&name, &b.url);
                o.push_str(&format!("<div class=\"card\" style=\"{pad}\">{} <b>{}</b>{}</div>", if pdf { "📄 PDF" } else { "📎" }, esc(&name), if b.url.is_empty() { String::new() } else { format!("<small><a href=\"{}\">{}</a></small>", if safe_url(&b.url) { esc(&b.url) } else { String::new() }, esc(&b.url)) }));
            }
            BlockKind::Columns => {
                o.push_str("<div style=\"display:flex;gap:28px;align-items:flex-start\">");
                open.push((b.indent, true));
            }
            BlockKind::Column => {
                let w = if b.start == 0 { 1000 } else { b.start };
                o.push_str(&format!("<div style=\"flex:{w} 1 0;min-width:0\">"));
                open.push((b.indent, false));
            }
            BlockKind::Widget => {
                o.push_str(&format!("<p style=\"{pad};color:#888\"><em>[Dashboard widget]</em></p>"));
            }
            BlockKind::Button => {
                o.push_str(&format!("<p style=\"{pad}\"><span style=\"display:inline-block;padding:6px 14px;border-radius:8px;background:#2383e2;color:#fff;font-weight:600\">{}</span></p>", esc(if b.text.is_empty() { "Button" } else { &b.text })));
            }
            BlockKind::DbView => {
                if let Some(t) = b.link.and_then(|l| ws.page(l)).filter(|t| !t.trashed && t.db.is_some()) {
                    let tt = if t.title.is_empty() { "Untitled" } else { &t.title };
                    o.push_str(&format!("<div class=\"dbtitle\" style=\"{pad}\">{}</div>", esc(tt)));
                    database_html_at(ws, t.id, &mut o, t.db.as_ref().and_then(|d| d.linked_index(b.id)));
                }
            }
            BlockKind::PageLink => {
                if let Some(t) = b.link.and_then(|l| ws.page(l)).filter(|t| !t.trashed) {
                    let tt = if t.title.is_empty() { "Untitled" } else { &t.title };
                    if t.db.is_some() {
                        o.push_str(&format!("<div class=\"dbtitle\" style=\"{pad}\">{}</div>", esc(tt)));
                        database_html(ws, t.id, &mut o);
                    } else {
                        o.push_str(&format!("<p style=\"{pad}\">↗ <b>{}</b></p>", esc(tt)));
                    }
                }
            }
        }
        let _ = bi;
    }
    for _ in open.drain(..) {
        o.push_str("</div>");
    }
    o.push_str(&format!("<div class=\"foot\">Exported from Lumen · {}</div></div>", esc(&dates::long_today())));
    if auto_print {
        o.push_str("<script>window.addEventListener('load',function(){setTimeout(function(){window.print()},500)})</script>");
    }
    o.push_str("</body></html>");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_text_images_and_databases() {
        let mut ws = Workspace::seed();
        let media = MediaStore::default();
        let cur = ws.current.unwrap();
        let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
        let link = ws.blk(BlockKind::PageLink, "", 0);
        let mut l = link;
        l.link = Some(road);
        let mut img = ws.blk(BlockKind::Image, "cap", 0);
        img.url = "https://example.com/a.png".into();
        let p = ws.page_mut(cur).unwrap();
        p.blocks.push(l);
        p.blocks.push(img);
        let h = page_html(&ws, &media, cur, true);
        assert!(h.contains("Getting Started") && h.contains("<table class=\"db\">") && h.contains("Design onboarding flow"));
        assert!(h.contains("https://example.com/a.png") && h.contains("window.print()"));
        assert!(!h.contains("<script>alert"));
        // XSS-safe: user text is escaped
        ws.page_mut(cur).unwrap().title = "<b>x</b>".into();
        assert!(page_html(&ws, &media, cur, false).contains("&lt;b&gt;x&lt;/b&gt;"));
        // database page prints too
        assert!(page_html(&ws, &media, road, false).contains("<th>Status</th>"));
    }
}
