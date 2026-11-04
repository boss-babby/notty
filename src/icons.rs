//! Icons: the Lucide icon font (embedded, registered in `theme::setup_fonts`) plus a couple of animated custom shapes.

use egui::{pos2, Align2, Color32, FontFamily, FontId, Painter, Pos2, Shape, Stroke};

use crate::lucide as l;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Menu,
    Plus,
    Dots,
    Search,
    Sun,
    Moon,
    Lock,
    Trash,
    Star,
    StarFilled,
    Close,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Page,
    PagePlus,
    Board,
    Grip,
    Duplicate,
    Restore,
    Shield,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    Pencil,
    Indent,
    Outdent,
    Up,
    Down,
    Download,
    Upload,
    Keyboard,
    Settings,
    Sparkles,
    Palette,
    History,
    Image,
    Smile,
    Folder,
    Table,
    Check,
    Info,
    Warning,
    External,
    Command,
    Type,
    Template,
    Panel,
    Focus,
    Eraser,
    Highlight,
    Home,
    Book,
    Calendar,
    Filter,
    Sort,
    Eye,
    Unlock,
    Archive,
    Replace,
}

impl Icon {
    pub fn glyph(self) -> &'static str {
        match self {
            Icon::Menu => l::MENU,
            Icon::Plus => l::PLUS,
            Icon::Dots => l::ELLIPSIS,
            Icon::Search => l::SEARCH,
            Icon::Sun => l::SUN,
            Icon::Moon => l::MOON,
            Icon::Lock => l::LOCK,
            Icon::Trash => l::TRASH_2,
            Icon::Star | Icon::StarFilled => l::STAR,
            Icon::Close => l::X,
            Icon::ChevronLeft => l::CHEVRON_LEFT,
            Icon::ChevronRight => l::CHEVRON_RIGHT,
            Icon::ChevronDown => l::CHEVRON_DOWN,
            Icon::Page => l::FILE_TEXT,
            Icon::PagePlus => l::FILE_PLUS,
            Icon::Board => l::KANBAN,
            Icon::Grip => l::GRIP_VERTICAL,
            Icon::Duplicate => l::COPY_PLUS,
            Icon::Restore => l::ROTATE_CCW,
            Icon::Shield => l::SHIELD_CHECK,
            Icon::Copy => l::COPY,
            Icon::Cut => l::SCISSORS,
            Icon::Paste => l::CLIPBOARD_PASTE,
            Icon::SelectAll => l::TEXT_SELECT,
            Icon::Bold => l::BOLD,
            Icon::Italic => l::ITALIC,
            Icon::Strike => l::STRIKETHROUGH,
            Icon::Code => l::CODE,
            Icon::Link => l::LINK_2,
            Icon::Pencil => l::PENCIL,
            Icon::Indent => l::INDENT_INCREASE,
            Icon::Outdent => l::INDENT_DECREASE,
            Icon::Up => l::ARROW_UP,
            Icon::Down => l::ARROW_DOWN,
            Icon::Download => l::DOWNLOAD,
            Icon::Upload => l::UPLOAD,
            Icon::Keyboard => l::KEYBOARD,
            Icon::Settings => l::SETTINGS,
            Icon::Sparkles => l::SPARKLES,
            Icon::Palette => l::PALETTE,
            Icon::History => l::HISTORY,
            Icon::Image => l::IMAGE,
            Icon::Smile => l::SMILE,
            Icon::Folder => l::FOLDER,
            Icon::Table => l::TABLE,
            Icon::Check => l::CHECK,
            Icon::Info => l::INFO,
            Icon::Warning => l::TRIANGLE_ALERT,
            Icon::External => l::EXTERNAL_LINK,
            Icon::Command => l::COMMAND,
            Icon::Type => l::TYPE,
            Icon::Template => l::LAYOUT_TEMPLATE,
            Icon::Panel => l::PANEL_LEFT,
            Icon::Focus => l::FOCUS,
            Icon::Eraser => l::ERASER,
            Icon::Highlight => l::HIGHLIGHTER,
            Icon::Home => l::HOUSE,
            Icon::Book => l::BOOK_OPEN,
            Icon::Calendar => l::CALENDAR_DAYS,
            Icon::Filter => l::FILTER,
            Icon::Sort => l::ARROW_UP_DOWN,
            Icon::Eye => l::EYE,
            Icon::Unlock => l::LOCK_OPEN,
            Icon::Archive => l::ARCHIVE,
            Icon::Replace => l::REPLACE,
        }
    }
}

/// Font used for Lucide glyphs.
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("lucide".into()))
}

/// Draws a Lucide glyph (any `lucide::*` constant) centred on `c`; `size` is the visual icon size in points.
pub fn glyph(p: &Painter, c: Pos2, size: f32, g: &str, color: Color32) {
    p.text(c, Align2::CENTER_CENTER, g, font(size * 1.04), color);
}

/// Draws a rotated chevron (`angle` = 0 points right, PI/2 points down). Custom shape so it can rotate smoothly.
pub fn chevron(p: &Painter, c: Pos2, size: f32, angle: f32, color: Color32) {
    let k = size / 16.0;
    let (s, co) = angle.sin_cos();
    let rot = |x: f32, y: f32| pos2(c.x + (x * co - y * s) * k, c.y + (x * s + y * co) * k);
    let line = vec![rot(-2.5, -4.5), rot(2.5, 0.0), rot(-2.5, 4.5)];
    p.add(Shape::line(line, Stroke::new(1.7_f32, color)));
}

pub fn paint(p: &Painter, c: Pos2, size: f32, icon: Icon, color: Color32) {
    if icon == Icon::StarFilled {
        let k = size / 16.0;
        let mut v = Vec::new();
        for i in 0..10 {
            let r = if i % 2 == 0 { 7.2 } else { 3.2 } * k;
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            v.push(pos2(c.x + r * a.cos(), c.y + 0.5 * k + r * a.sin()));
        }
        p.add(Shape::convex_polygon(v, color, Stroke::new(1.4_f32, color)));
        return;
    }
    glyph(p, c, size, icon.glyph(), color);
}
