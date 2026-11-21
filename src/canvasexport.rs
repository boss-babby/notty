//! Canvas → SVG. A whiteboard leaves the app as a crisp vector picture (shapes, frames, cards, connectors, pen and highlighter ink),
//! ready for slides, docs and design tools. Pure string building: no egui widgets involved, so it is unit tested and works everywhere.

use std::fmt::Write;

use crate::canvas::{self as cv, Arrow, Canvas, Kind};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn rgb(c: u8, fallback: &str) -> String {
    match cv::accent_rgb(c) {
        Some([r, g, b]) => format!("#{r:02x}{g:02x}{b:02x}"),
        None => fallback.to_owned(),
    }
}

/// Greedy word wrap using a rough glyph width (good enough for a picture; the app itself lays text out with real font metrics).
fn wrap(text: &str, max_px: f32, px: f32) -> Vec<String> {
    let per = (px * 0.55).max(1.0);
    let cols = ((max_px / per) as usize).max(4);
    let mut out = vec![];
    for line in text.lines() {
        let line = line.trim_start_matches(|c| c == '#' || c == '>' || c == '-').trim();
        let line = line.replace("[ ]", "☐").replace("[x]", "☑");
        let mut cur = String::new();
        for w in line.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > cols {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        out.push(cur);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn text_block(o: &mut String, lines: &[String], x: f32, y: f32, px: f32, fill: &str, anchor: &str, weight: &str) {
    for (i, l) in lines.iter().enumerate() {
        let _ = write!(o, "<text x=\"{x:.1}\" y=\"{:.1}\" font-size=\"{px:.1}\" fill=\"{fill}\" text-anchor=\"{anchor}\" font-weight=\"{weight}\">{}</text>", y + (i as f32 + 0.8) * px * 1.35, esc(l));
    }
}

/// `title_of` resolves the title of a workspace page for [`Kind::Page`] cards.
pub fn to_svg(c: &Canvas, dark: bool, title_of: &dyn Fn(u64) -> String) -> String {
    let (bg, fg, card, line) = if dark { ("#191919", "#ebebeb", "#252525", "#4a4a4a") } else { ("#f7f6f3", "#2b2b2b", "#ffffff", "#d6d3cd") };
    let b = c.bounds(None).unwrap_or(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0))).expand(48.0);
    let mut o = String::new();
    let _ = write!(o, "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{:.0} {:.0} {:.0} {:.0}\" width=\"{:.0}\" height=\"{:.0}\" font-family=\"Inter, 'Segoe UI', Roboto, Arial, sans-serif\">", b.min.x, b.min.y, b.width(), b.height(), b.width(), b.height());
    let _ = write!(o, "<rect x=\"{:.0}\" y=\"{:.0}\" width=\"{:.0}\" height=\"{:.0}\" fill=\"{bg}\"/>", b.min.x, b.min.y, b.width(), b.height());
    let node = |o: &mut String, n: &cv::Node| {
        let r = n.rect();
        let acc = rgb(n.color, fg);
        match n.kind {
            Kind::Group => {
                let _ = write!(o, "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"12\" fill=\"{acc}\" fill-opacity=\"0.07\" stroke=\"{acc}\" stroke-opacity=\"0.5\"/>", r.min.x, r.min.y, r.width(), r.height());
                let _ = write!(o, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"14\" font-weight=\"700\" fill=\"{acc}\">{}</text>", r.min.x + 12.0, r.min.y + 24.0, esc(&n.text));
            }
            Kind::Ink => {
                let pts: Vec<String> = n.pts.iter().map(|p| format!("{:.1},{:.1}", n.x + p[0], n.y + p[1])).collect();
                let w = 2.2 + n.size as f32 * 1.9;
                let (w, op) = if n.hl { (w * 4.5, 0.38) } else { (w, 1.0) };
                let _ = write!(o, "<polyline points=\"{}\" fill=\"none\" stroke=\"{acc}\" stroke-width=\"{w:.1}\" stroke-opacity=\"{op}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>", pts.join(" "));
            }
            Kind::Text => {
                let px = [20.0, 30.0, 44.0][n.size.min(2) as usize];
                text_block(o, &wrap(&n.text, r.width(), px), r.min.x + 4.0, r.min.y + 2.0, px, &acc, "start", "700");
            }
            Kind::Shape => {
                let pts: Vec<String> = cv::geo_points_px(n.geo, r, 6.0).iter().map(|p| format!("{:.1},{:.1}", p.x, p.y)).collect();
                let (fill, fo, sw) = match n.style {
                    1 | 3 => ("none".to_owned(), 1.0, 2.0),
                    2 => (acc.clone(), 1.0, 2.0),
                    _ => (acc.clone(), 0.16, 2.0),
                };
                let dash = match n.dash {
                    1 => " stroke-dasharray=\"10 7\"",
                    2 => " stroke-dasharray=\"2 6\" stroke-linecap=\"round\"",
                    _ => "",
                };
                if n.geo == 2 {
                    // a real ellipse element: exact at any zoom / print size
                    let _ = write!(o, "<ellipse cx=\"{:.2}\" cy=\"{:.2}\" rx=\"{:.2}\" ry=\"{:.2}\" fill=\"{fill}\" fill-opacity=\"{fo}\" stroke=\"{acc}\" stroke-width=\"{sw}\"{dash}/>", r.center().x, r.center().y, r.width() / 2.0, r.height() / 2.0);
                } else {
                    let _ = write!(o, "<polygon points=\"{}\" fill=\"{fill}\" fill-opacity=\"{fo}\" stroke=\"{acc}\" stroke-width=\"{sw}\" stroke-linejoin=\"round\"{dash}/>", pts.join(" "));
                }
                let px = [16.0, 22.0, 30.0][n.size.min(2) as usize];
                let tcol = if n.style == 2 { "#ffffff".to_owned() } else { fg.to_owned() };
                let lines = wrap(&n.text, r.width() * 0.8, px);
                let total = lines.len() as f32 * px * 1.35;
                text_block(o, &lines, r.center().x, r.center().y - total / 2.0, px, &tcol, "middle", "600");
            }
            Kind::Image => {
                if n.url.starts_with("http") {
                    let _ = write!(o, "<image x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" href=\"{}\" preserveAspectRatio=\"xMidYMid meet\"/>", r.min.x, r.min.y, r.width(), r.height(), esc(&n.url));
                } else {
                    let _ = write!(o, "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"8\" fill=\"{card}\" stroke=\"{line}\"/>", r.min.x, r.min.y, r.width(), r.height());
                    let _ = write!(o, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"13\" fill=\"{fg}\" text-anchor=\"middle\">image</text>", r.center().x, r.center().y);
                }
            }
            Kind::Note | Kind::Todo | Kind::Page | Kind::Link | Kind::Embed | Kind::Pdf | Kind::File | Kind::DbView | Kind::Widget | Kind::Dash => {
                let _ = write!(o, "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"10\" fill=\"{card}\" stroke=\"{}\"/>", r.min.x, r.min.y, r.width(), r.height(), if n.color > 0 { acc.clone() } else { line.to_owned() });
                if n.color > 0 {
                    let _ = write!(o, "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"3\" height=\"{:.1}\" fill=\"{acc}\"/>", r.min.x, r.min.y + 10.0, (r.height() - 20.0).max(1.0));
                }
                let text = match n.kind {
                    Kind::Page => n.page.map(title_of).unwrap_or_default(),
                    Kind::DbView => format!("{} (database)", n.page.map(title_of).unwrap_or_default()),
                    Kind::Pdf | Kind::File => format!("{} ({})", if n.text.is_empty() { &n.url } else { &n.text }, if n.kind == Kind::Pdf { "PDF" } else { "file" }),
                    Kind::Widget => crate::canvasx::search_text(n),
                    Kind::Dash => "Dashboard widget".to_owned(),
                    Kind::Link | Kind::Embed => {
                        if n.text.is_empty() {
                            n.url.clone()
                        } else {
                            n.text.clone()
                        }
                    }
                    _ => n.text.clone(),
                };
                text_block(o, &wrap(&text, r.width() - 28.0, 14.5), r.min.x + 14.0, r.min.y + 10.0, 14.5, fg, "start", "400");
            }
        }
    };
    for n in c.nodes.iter().filter(|n| n.kind == Kind::Group) {
        node(&mut o, n);
    }
    for e in &c.edges {
        let Some((pts, _, _)) = c.edge_route(e) else { continue };
        if pts.len() < 2 {
            continue;
        }
        let col = rgb(e.color, if dark { "#9b9b9b" } else { "#787774" });
        let p: Vec<String> = pts.iter().map(|p| format!("{:.1},{:.1}", p.x, p.y)).collect();
        let _ = write!(o, "<polyline points=\"{}\" fill=\"none\" stroke=\"{col}\" stroke-width=\"2\"{} stroke-linejoin=\"round\"/>", p.join(" "), if e.dashed { " stroke-dasharray=\"8 6\"" } else { "" });
        let head = |o: &mut String, tip: egui::Pos2, from: egui::Pos2| {
            let d = (tip - from).normalized();
            if !d.x.is_finite() {
                return;
            }
            let n = egui::vec2(-d.y, d.x);
            let (a, b) = (tip - d * 12.0 + n * 6.0, tip - d * 12.0 - n * 6.0);
            let _ = write!(o, "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{col}\"/>", tip.x, tip.y, a.x, a.y, b.x, b.y);
        };
        if e.arrow != Arrow::None {
            head(&mut o, pts[pts.len() - 1], pts[pts.len() - 2]);
        }
        if e.arrow == Arrow::Both {
            head(&mut o, pts[0], pts[1]);
        }
        if !e.label.is_empty() {
            let m = cv::path_mid(&pts);
            let n = e.label.chars().count() as f32;
            let _ = write!(o, "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"20\" rx=\"6\" fill=\"{bg}\"/>", m.x - 4.0 - n * 3.8, m.y - 10.0, 8.0 + n * 7.6);
            let _ = write!(o, "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" fill=\"{fg}\" text-anchor=\"middle\">{}</text>", m.x, m.y + 4.0, esc(&e.label));
        }
    }
    for n in c.nodes.iter().filter(|n| n.kind != Kind::Group) {
        node(&mut o, n);
    }
    o.push_str("</svg>");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn svg_has_shapes_edges_and_ink() {
        let mut c = Canvas::new();
        let a = c.add_node(Kind::Shape, pos2(0.0, 0.0));
        let b = c.add_node(Kind::Note, pos2(400.0, 0.0));
        c.node_mut(a).unwrap().geo = 3;
        c.node_mut(a).unwrap().text = "A & B".into();
        c.connect(a, b, cv::Side::Auto, cv::Side::Auto);
        c.add_stroke(&[pos2(0.0, 100.0), pos2(50.0, 130.0), pos2(90.0, 100.0)], 2, 1, true);
        let s = to_svg(&c, false, &|_| String::new());
        assert!(s.starts_with("<svg") && s.ends_with("</svg>"));
        assert!(s.contains("<polygon"), "shape outline");
        assert!(s.contains("A &amp; B"), "escaped text");
        assert!(s.contains("stroke-opacity=\"0.38\""), "highlighter");
        assert!(s.matches("<polyline").count() >= 2, "edge + ink");
    }

    #[test]
    fn geometry_hit_and_json_roundtrip() {
        let r = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        assert!(!cv::geo_contains(2, r, pos2(2.0, 2.0)), "corner of an ellipse is outside");
        assert!(cv::geo_contains(2, r, pos2(50.0, 50.0)));
        assert!(!cv::geo_contains(3, r, pos2(5.0, 5.0)), "corner of a diamond is outside");
        for g in 0..cv::GEO_NAMES.len() as u8 {
            assert!(cv::geo_points(g, r).len() >= 3);
        }
        let mut c = Canvas::new();
        let id = c.add_node(Kind::Shape, pos2(10.0, 10.0));
        {
            let n = c.node_mut(id).unwrap();
            n.geo = 6;
            n.style = 3;
            n.dash = 1;
        }
        let json = c.to_json(None);
        let mut d = Canvas::new();
        let ids = d.import_json(&json, pos2(0.0, 0.0));
        let n = d.node(ids[0]).unwrap();
        assert_eq!((n.kind, n.geo, n.style, n.dash), (Kind::Shape, 6, 3, 1));
    }

    #[test]
    fn new_templates_build_boards() {
        for t in [cv::Tpl::Diagram, cv::Tpl::Retro, cv::Tpl::Org] {
            let mut c = Canvas::new();
            let ids = c.insert_template(t, pos2(0.0, 0.0));
            assert!(ids.len() >= 6, "{:?}", t);
            assert!(c.nodes.iter().any(|n| n.kind == Kind::Shape || n.kind == Kind::Group));
        }
    }
}
