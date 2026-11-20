//! Inline text formatting for canvas nodes (notes, text labels, todo cards, groups).
//!
//! Canvas text is stored as lightweight Markdown (`**bold**`, `*italic*`, `__underline__`, `~~strike~~`, `==highlight==`,
//! `` `code` ``). Before this module the canvas painted those markers literally and the editing overlay was a plain text box, so
//! none of the formatting worked. Now
//! * the text of every node is *rendered* (markers removed, styles applied) — see `canvasdraw::layout_md`;
//! * while editing, a live layouter shows the styled text with the markers dimmed, the cursor positions stay byte-exact;
//! * a floating format bar (touch-sized) and the shortcuts Ctrl/⌘ + B, I, U, Shift+S, E, H wrap / unwrap the selection
//!   and toggle line styles (heading, bullet, check box, quote).

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{pos2, vec2, Color32, Context, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui};

use crate::lucide as l;
use crate::theme::Palette;
use crate::widgets::with_alpha;

/// One inline style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Highlight,
}

impl Mark {
    pub fn marker(self) -> &'static str {
        match self {
            Mark::Bold => "**",
            Mark::Italic => "*",
            Mark::Underline => "__",
            Mark::Strike => "~~",
            Mark::Code => "`",
            Mark::Highlight => "==",
        }
    }
}

/// Line-level styles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineStyle {
    Heading,
    Bullet,
    Check,
    Quote,
}

impl LineStyle {
    fn prefix(self) -> &'static str {
        match self {
            LineStyle::Heading => "## ",
            LineStyle::Bullet => "- ",
            LineStyle::Check => "- [ ] ",
            LineStyle::Quote => "> ",
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

/// Pure text transform: wraps / unwraps `[a, b)` (char indices) with `mark`. Returns the new text and selection.
pub fn toggle_mark(text: &str, a: usize, b: usize, mark: Mark) -> (String, usize, usize) {
    let m: Vec<char> = mark.marker().chars().collect();
    let ml = m.len();
    let mut ch: Vec<char> = text.chars().collect();
    let (mut a, mut b) = (a.min(b).min(ch.len()), a.max(b).min(ch.len()));
    if a == b {
        // no selection: format the word under the caret
        while a > 0 && is_word(ch[a - 1]) {
            a -= 1;
        }
        while b < ch.len() && is_word(ch[b]) {
            b += 1;
        }
    }
    let starts = |ch: &[char], at: usize| at + ml <= ch.len() && ch[at..at + ml] == m[..];
    // for `*` a neighbouring `**` is bold, not italic
    let lone = |ch: &[char], at: usize| -> bool {
        if mark != Mark::Italic {
            return true;
        }
        let before = at > 0 && ch[at - 1] == '*';
        let after = at + 1 < ch.len() && ch[at + 1] == '*';
        !before && !after
    };
    // already wrapped just outside the selection?
    if a >= ml && starts(&ch, a - ml) && starts(&ch, b) && lone(&ch, a - ml) && lone(&ch, b) {
        ch.drain(b..b + ml);
        ch.drain(a - ml..a);
        return (ch.iter().collect(), a - ml, b - ml);
    }
    // markers are part of the selection
    if b - a >= 2 * ml && starts(&ch, a) && b >= ml && starts(&ch, b - ml) && lone(&ch, a) && lone(&ch, b - ml) {
        ch.drain(b - ml..b);
        ch.drain(a..a + ml);
        return (ch.iter().collect(), a, b - 2 * ml);
    }
    if a == b {
        // empty: drop an empty pair at the caret
        for (i, c) in m.iter().enumerate() {
            ch.insert(a + i, *c);
        }
        for (i, c) in m.iter().enumerate() {
            ch.insert(a + ml + i, *c);
        }
        return (ch.iter().collect(), a + ml, a + ml);
    }
    for (i, c) in m.iter().enumerate() {
        ch.insert(b + i, *c);
    }
    for (i, c) in m.iter().enumerate() {
        ch.insert(a + i, *c);
    }
    (ch.iter().collect(), a + ml, b + ml)
}

/// Pure text transform: toggles a line style on every line touched by `[a, b]`. Returns the new text and caret.
pub fn toggle_line(text: &str, a: usize, b: usize, style: LineStyle) -> (String, usize, usize) {
    let ch: Vec<char> = text.chars().collect();
    let (a, b) = (a.min(b).min(ch.len()), a.max(b).min(ch.len()));
    let line_start = |i: usize| ch[..i].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1);
    let first = line_start(a);
    let mut out: Vec<char> = ch[..first].to_vec();
    let mut pos = first;
    let mut na = a;
    let mut nb = b;
    let all: Vec<&[char]> = ch[first..].split(|c| *c == '\n').collect();
    for (k, line) in all.iter().enumerate() {
        let end = pos + line.len();
        let touched = pos <= b;
        let s: String = line.iter().collect();
        let mut new_line = s.clone();
        if touched && !s.trim().is_empty() || (touched && pos == a) {
            let trimmed = s.trim_start();
            let indent = &s[..s.len() - trimmed.len()];
            let heading = |t: &str| -> Option<usize> { ["### ", "## ", "# "].iter().find(|p| t.starts_with(**p)).map(|p| p.len()) };
            let strip = |t: &str| -> String {
                let mut t = t;
                if let Some(n) = heading(t) {
                    t = &t[n..];
                }
                for p in ["- [ ] ", "- [x] ", "- [X] ", "[ ] ", "[x] ", "- ", "* ", "> ", "• "] {
                    if let Some(r) = t.strip_prefix(p) {
                        return r.to_owned();
                    }
                }
                t.to_owned()
            };
            let has = match style {
                LineStyle::Heading => heading(trimmed).is_some(),
                LineStyle::Bullet => trimmed.starts_with("- ") && !trimmed.starts_with("- [") || trimmed.starts_with("* ") || trimmed.starts_with("• "),
                LineStyle::Check => trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [x] ") || trimmed.starts_with("[ ] ") || trimmed.starts_with("[x] "),
                LineStyle::Quote => trimmed.starts_with("> "),
            };
            let body = strip(trimmed);
            new_line = if has { format!("{indent}{body}") } else { format!("{indent}{}{body}", style.prefix()) };
        }
        let delta = new_line.chars().count() as isize - line.len() as isize;
        // keep the selection roughly where it was
        if touched {
            if a >= pos && a <= end {
                na = (a as isize + delta).max(pos as isize) as usize;
            } else if a > end {
                na = (na as isize + delta) as usize;
            }
            if b >= pos && b <= end {
                nb = (b as isize + delta).max(pos as isize) as usize;
            } else if b > end {
                nb = (nb as isize + delta) as usize;
            }
        }
        out.extend(new_line.chars());
        if k + 1 < all.len() {
            out.push('\n');
        }
        pos = end + 1;
    }
    let total = out.len();
    (out.into_iter().collect(), na.min(total), nb.min(total))
}

/// Current selection of the text field `eid` as ordered char indices.
fn selection(ctx: &Context, eid: Id) -> Option<(usize, usize)> {
    let s = TextEditState::load(ctx, eid)?;
    let r = s.cursor.char_range()?;
    Some((r.primary.index.min(r.secondary.index), r.primary.index.max(r.secondary.index)))
}

fn set_selection(ctx: &Context, eid: Id, a: usize, b: usize) {
    let mut s = TextEditState::load(ctx, eid).unwrap_or_default();
    s.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    s.store(ctx, eid);
}

/// Applies an inline style to the selection of the text field `eid`. Returns true when `text` changed.
pub fn apply_mark(ctx: &Context, eid: Id, text: &mut String, mark: Mark) -> bool {
    let (a, b) = selection(ctx, eid).unwrap_or_else(|| {
        let n = text.chars().count();
        (n, n)
    });
    let (t, na, nb) = toggle_mark(text, a, b, mark);
    *text = t;
    set_selection(ctx, eid, na, nb);
    true
}

/// Toggles a line style for the lines of the selection. Returns true when `text` changed.
pub fn apply_line(ctx: &Context, eid: Id, text: &mut String, style: LineStyle) -> bool {
    let (a, b) = selection(ctx, eid).unwrap_or_else(|| {
        let n = text.chars().count();
        (n, n)
    });
    let (t, na, nb) = toggle_line(text, a, b, style);
    *text = t;
    set_selection(ctx, eid, na, nb);
    true
}

/// Keyboard shortcuts while the field is focused. Returns true when the text changed.
pub fn shortcuts(ctx: &Context, eid: Id, text: &mut String) -> bool {
    if !ctx.memory(|m| m.has_focus(eid)) {
        return false;
    }
    let mut changed = false;
    let table: [(Modifiers, Key, Mark); 6] = [
        (Modifiers::COMMAND, Key::B, Mark::Bold),
        (Modifiers::COMMAND, Key::I, Mark::Italic),
        (Modifiers::COMMAND, Key::U, Mark::Underline),
        (Modifiers::COMMAND | Modifiers::SHIFT, Key::S, Mark::Strike),
        (Modifiers::COMMAND, Key::E, Mark::Code),
        (Modifiers::COMMAND | Modifiers::SHIFT, Key::H, Mark::Highlight),
    ];
    for (m, k, mark) in table {
        if ctx.input_mut(|i| i.consume_key(m, k)) {
            changed |= apply_mark(ctx, eid, text, mark);
        }
    }
    changed
}

/// Which inline styles are active at the caret / selection (for the bar's pressed state).
pub fn active_marks(text: &str, a: usize, b: usize) -> [bool; 6] {
    // a style is active when toggling it would remove markers (the text shrinks)
    let probe = |mark: Mark| -> bool { toggle_mark(text, a, b, mark).0.chars().count() < text.chars().count() };
    [Mark::Bold, Mark::Italic, Mark::Underline, Mark::Strike, Mark::Code, Mark::Highlight].map(probe)
}

pub struct BarOut {
    pub changed: bool,
    pub rect: Rect,
}

/// Floating format bar above (or below) `anchor`, kept inside `bounds`. Buttons are 38 px on touch screens.
pub fn format_bar(ctx: &Context, pal: &Palette, eid: Id, text: &mut String, anchor: Rect, bounds: Rect, line_styles: bool) -> BarOut {
    let touchy = crate::touch::touchy();
    let bs = if touchy { 40.0 } else { 30.0 };
    let gap = 2.0;
    let marks = [
        (l::BOLD, Mark::Bold, "Bold  Ctrl+B"),
        (l::ITALIC, Mark::Italic, "Italic  Ctrl+I"),
        (l::UNDERLINE, Mark::Underline, "Underline  Ctrl+U"),
        (l::STRIKETHROUGH, Mark::Strike, "Strikethrough  Ctrl+Shift+S"),
        (l::CODE, Mark::Code, "Code  Ctrl+E"),
        (l::HIGHLIGHTER, Mark::Highlight, "Highlight  Ctrl+Shift+H"),
    ];
    let lines = [(l::HEADING_2, LineStyle::Heading, "Heading"), (l::LIST, LineStyle::Bullet, "Bullet list"), (l::LIST_CHECKS, LineStyle::Check, "To-do"), (l::QUOTE, LineStyle::Quote, "Quote")];
    let n = marks.len() + if line_styles { lines.len() } else { 0 };
    let w = n as f32 * (bs + gap) + 12.0 + if line_styles { 8.0 } else { 0.0 };
    let h = bs + 10.0;
    let mut x = (anchor.center().x - w / 2.0).clamp(bounds.left() + 6.0, (bounds.right() - w - 6.0).max(bounds.left() + 6.0));
    if bounds.width() < w + 12.0 {
        x = bounds.left() + 6.0;
    }
    let mut y = anchor.top() - h - 8.0;
    if y < bounds.top() + 6.0 {
        y = (anchor.bottom() + 8.0).min(bounds.bottom() - h - 6.0).max(bounds.top() + 6.0);
    }
    let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
    let (a, b) = selection(ctx, eid).unwrap_or((0, 0));
    let active = active_marks(text, a, b);
    let layer = egui::Area::new(Id::new(("cnv_fmt_bar", eid))).order(egui::Order::Foreground).fixed_pos(rect.min).interactable(true);
    let mut changed = false;
    layer.show(ctx, |ui| {
        let p = ui.painter();
        p.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), 14.0, Color32::from_black_alpha(if pal.text.r() > 128 { 90 } else { 30 }));
        p.rect_filled(rect, 13.0, pal.popup);
        p.rect_stroke(rect, 13.0, Stroke::new(1.0_f32, pal.border), StrokeKind::Inside);
        let mut cx = rect.left() + 6.0;
        let mut click: Option<Result<Mark, LineStyle>> = None;
        let mut item = |ui: &mut Ui, glyph: &str, on: bool, tip: &str, key: Result<Mark, LineStyle>, cx: &mut f32| {
            let r = Rect::from_min_size(pos2(*cx, rect.top() + 5.0), vec2(bs, bs));
            let resp = ui.interact(r, Id::new(("cnv_fmt_btn", glyph)), Sense::click());
            if on {
                ui.painter().rect_filled(r, 9.0, pal.accent_soft);
            } else if resp.hovered() || resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(r, 9.0, pal.hover);
            }
            crate::icons::glyph(ui.painter(), r.center(), if touchy { 20.0 } else { 17.0 }, glyph, if on { pal.accent } else { pal.text });
            if !touchy {
                resp.clone().on_hover_text(tip);
            }
            if resp.clicked() {
                click = Some(key);
            }
            *cx += bs + gap;
        };
        for (g, m, tip) in marks {
            let on = active[marks.iter().position(|x| x.1 == m).unwrap_or(0)];
            item(ui, g, on, tip, Ok(m), &mut cx);
        }
        if line_styles {
            let sx = cx + 1.0;
            ui.painter().vline(sx, rect.top() + 9.0..=rect.bottom() - 9.0, Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)));
            cx += 8.0;
            for (g, s, tip) in lines {
                item(ui, g, false, tip, Err(s), &mut cx);
            }
        }
        if let Some(c) = click {
            changed = match c {
                Ok(m) => apply_mark(ctx, eid, text, m),
                Err(s) => apply_line(ctx, eid, text, s),
            };
        }
    });
    BarOut { changed, rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_unwrap() {
        let (t, a, b) = toggle_mark("hello world", 0, 5, Mark::Bold);
        assert_eq!((t.as_str(), a, b), ("**hello** world", 2, 7));
        let (t2, a2, b2) = toggle_mark(&t, a, b, Mark::Bold);
        assert_eq!((t2.as_str(), a2, b2), ("hello world", 0, 5));
        // caret inside a word formats the word
        let (t, _, _) = toggle_mark("say hello now", 6, 6, Mark::Underline);
        assert_eq!(t, "say __hello__ now");
        // empty selection between spaces inserts an empty pair
        let (t, a, b) = toggle_mark("a  b", 2, 2, Mark::Code);
        assert_eq!((t.as_str(), a, b), ("a `` b", 3, 3));
        // italic next to bold is independent
        let (t, a, b) = toggle_mark("**x**", 2, 3, Mark::Italic);
        assert_eq!((t.as_str(), a, b), ("***x***", 3, 4));
        // markers inside the selection are removed
        let (t, _, _) = toggle_mark("~~gone~~", 0, 8, Mark::Strike);
        assert_eq!(t, "gone");
        // unicode / emoji
        let (t, _, _) = toggle_mark("héllo 😀", 0, 5, Mark::Highlight);
        assert_eq!(t, "==héllo== 😀");
    }

    #[test]
    fn line_styles() {
        let (t, _, _) = toggle_line("one\ntwo", 0, 0, LineStyle::Heading);
        assert_eq!(t, "## one\ntwo");
        let (t, _, _) = toggle_line(&t, 0, 0, LineStyle::Heading);
        assert_eq!(t, "one\ntwo");
        let (t, _, _) = toggle_line("one\ntwo", 0, 7, LineStyle::Check);
        assert_eq!(t, "- [ ] one\n- [ ] two");
        let (t, _, _) = toggle_line("- [ ] one", 0, 0, LineStyle::Bullet);
        assert_eq!(t, "- one");
    }

    #[test]
    fn active() {
        assert_eq!(active_marks("**a** b", 2, 3)[0], true);
        assert_eq!(active_marks("**a** b", 6, 7)[0], false);
    }
}
