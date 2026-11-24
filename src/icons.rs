//! Icons: the Lucide icon font (embedded, registered in `theme::setup_fonts`) plus a couple of animated custom shapes.

use egui::{load::TexturePoll, pos2, vec2, Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, Ui, Vec2};

use crate::lucide as l;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Menu,
    Plus,
    Minus,
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
    Canvas,
    Flow,
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
    Underline,
    Images,
    Globe,
    Bookmark,
    Paperclip,
    Undo,
    Redo,
    Video,
    MoveUp,
    MoveDown,
    Zap,
    Bell,
}

impl Icon {
    pub fn glyph(self) -> &'static str {
        match self {
            Icon::Menu => l::MENU,
            Icon::Plus => l::PLUS,
            Icon::Minus => l::MINUS,
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
            Icon::Canvas => l::NETWORK,
            Icon::Flow => l::ZAP,
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
            Icon::Underline => l::UNDERLINE,
            Icon::Images => l::IMAGES,
            Icon::Globe => l::GLOBE,
            Icon::Bookmark => l::BOOKMARK,
            Icon::Paperclip => l::PAPERCLIP,
            Icon::Undo => l::UNDO_2,
            Icon::Redo => l::REDO_2,
            Icon::Video => l::VIDEO,
            Icon::MoveUp => l::ARROW_UP_FROM_LINE,
            Icon::MoveDown => l::ARROW_DOWN_FROM_LINE,
            Icon::Zap => l::ZAP,
            Icon::Bell => l::BELL,
        }
    }
}

/// Font used for Lucide glyphs.
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("lucide".into()))
}

/// Custom Lumen AI vector emblem (two-petal mark from `Asset-1.svg`), crisp at every size.
pub fn paint_ai_emblem(p: &Painter, c: Pos2, size: f32, color: Color32) {
    let s = size * 0.94;
    let mut mesh = egui::Mesh::default();
    let aa_stroke = Stroke::new((size * 0.024).clamp(0.45, 0.85), color);

    for (pts, indices) in [
        (AI_PETAL_TOP_PTS, AI_PETAL_TOP_IDX),
        (AI_PETAL_BOT_PTS, AI_PETAL_BOT_IDX),
    ] {
        let base = mesh.vertices.len() as u32;
        let mut outline = Vec::with_capacity(pts.len());
        for &(nx, ny) in pts {
            let pos = pos2(c.x + nx * s, c.y + ny * s);
            outline.push(pos);
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: egui::epaint::WHITE_UV,
                color,
            });
        }
        for &idx in indices {
            mesh.indices.push(base + idx);
        }
        p.add(Shape::Path(egui::epaint::PathShape::closed_line(
            outline, aa_stroke,
        )));
    }
    p.add(Shape::mesh(mesh));
}

const AI_PETAL_TOP_PTS: &[(f32, f32)] = &[
    (-0.45919, 0.42662),
    (-0.46201, 0.42466),
    (-0.46419, 0.42231),
    (-0.46595, 0.41967),
    (-0.4675, 0.41684),
    (-0.46904, 0.41392),
    (-0.47078, 0.41101),
    (-0.48692, 0.37621),
    (-0.49583, 0.3349),
    (-0.49942, 0.28987),
    (-0.49962, 0.24389),
    (-0.49833, 0.19975),
    (-0.49748, 0.16024),
    (-0.49748, -0.0081855),
    (-0.49706, -0.063818),
    (-0.49409, -0.11499),
    (-0.48595, -0.16259),
    (-0.47005, -0.2075),
    (-0.44377, -0.25061),
    (-0.40451, -0.29281),
    (-0.37524, -0.31612),
    (-0.34284, -0.33717),
    (-0.30847, -0.35656),
    (-0.2733, -0.37483),
    (-0.23848, -0.39258),
    (-0.20518, -0.41037),
    (-0.18803, -0.4199),
    (-0.17083, -0.42934),
    (-0.1536, -0.43872),
    (-0.13636, -0.4481),
    (-0.11913, -0.45751),
    (-0.10193, -0.46699),
    (-0.093048, -0.47221),
    (-0.084088, -0.47782),
    (-0.07502, -0.48349),
    (-0.065818, -0.48888),
    (-0.056459, -0.49365),
    (-0.046916, -0.49748),
    (-0.040768, -0.49889),
    (-0.032873, -0.49975),
    (-0.02413, -0.5),
    (-0.015437, -0.49957),
    (-0.0076899, -0.49839),
    (-0.0017878, -0.4964),
    (0.0026044, -0.49355),
    (0.006386, -0.48995),
    (0.0095609, -0.48576),
    (0.012133, -0.48114),
    (0.014107, -0.47625),
    (0.015487, -0.47125),
    (0.017017, -0.46106),
    (0.017546, -0.45035),
    (0.017397, -0.43937),
    (0.016892, -0.42835),
    (0.016352, -0.41753),
    (0.016101, -0.40715),
    (0.016101, -0.2529),
    (0.017227, -0.20561),
    (0.018479, -0.15897),
    (0.016668, -0.1136),
    (0.0086033, -0.070136),
    (-0.0089034, -0.029204),
    (-0.039042, 0.0085624),
    (-0.050428, 0.018415),
    (-0.062552, 0.027196),
    (-0.075197, 0.035221),
    (-0.088142, 0.042805),
    (-0.10117, 0.050261),
    (-0.11406, 0.057906),
    (-0.17684, 0.09382),
    (-0.24874, 0.13392),
    (-0.32155, 0.17917),
    (-0.38711, 0.23056),
    (-0.4372, 0.28906),
    (-0.46364, 0.35565),
    (-0.46495, 0.36749),
    (-0.46509, 0.37942),
    (-0.46434, 0.39135),
    (-0.46293, 0.40324),
    (-0.46113, 0.41502),
];
const AI_PETAL_TOP_IDX: &[u32] = &[
    79, 0, 1, 79, 1, 2, 79, 2, 3, 79, 3, 4, 79, 4, 5, 79, 5, 6, 79, 6, 7, 79, 7, 8, 79, 8, 9, 79,
    9, 10, 79, 10, 11, 12, 13, 14, 12, 14, 15, 12, 15, 16, 11, 12, 16, 11, 16, 17, 11, 17, 18, 11,
    18, 19, 11, 19, 20, 11, 20, 21, 11, 21, 22, 11, 22, 23, 11, 23, 24, 11, 24, 25, 11, 25, 26, 11,
    26, 27, 11, 27, 28, 11, 28, 29, 11, 29, 30, 11, 30, 31, 11, 31, 32, 11, 32, 33, 11, 33, 34, 11,
    34, 35, 11, 35, 36, 11, 36, 37, 11, 37, 38, 11, 38, 39, 11, 39, 40, 11, 40, 41, 11, 41, 42, 11,
    42, 43, 11, 43, 44, 11, 44, 45, 11, 45, 46, 11, 46, 47, 11, 47, 48, 11, 48, 49, 11, 49, 50, 11,
    50, 51, 11, 51, 52, 11, 52, 53, 11, 53, 54, 11, 54, 55, 11, 55, 56, 11, 56, 57, 11, 57, 58, 11,
    58, 59, 11, 59, 60, 11, 60, 61, 11, 61, 62, 11, 62, 63, 11, 63, 64, 11, 64, 65, 11, 65, 66, 11,
    66, 67, 11, 67, 68, 11, 68, 69, 11, 69, 70, 11, 70, 71, 11, 71, 72, 11, 72, 73, 11, 73, 74, 11,
    74, 75, 11, 75, 76, 11, 76, 77, 11, 77, 78, 11, 78, 79,
];

const AI_PETAL_BOT_PTS: &[(f32, f32)] = &[
    (-0.39041, 0.46039),
    (-0.38998, 0.45681),
    (-0.38922, 0.45311),
    (-0.38815, 0.44942),
    (-0.38676, 0.44585),
    (-0.38504, 0.44252),
    (-0.38299, 0.43955),
    (-0.37625, 0.43214),
    (-0.36871, 0.42528),
    (-0.36068, 0.41879),
    (-0.35247, 0.41247),
    (-0.3444, 0.40615),
    (-0.33678, 0.39962),
    (-0.32223, 0.38596),
    (-0.30792, 0.37196),
    (-0.2938, 0.35773),
    (-0.27979, 0.34339),
    (-0.26582, 0.32903),
    (-0.25184, 0.31477),
    (-0.20881, 0.2674),
    (-0.1671, 0.21746),
    (-0.12441, 0.16831),
    (-0.078455, 0.12332),
    (-0.02694, 0.085828),
    (0.032425, 0.059213),
    (0.062121, 0.05174),
    (0.092037, 0.047338),
    (0.12215, 0.045243),
    (0.15243, 0.044691),
    (0.18286, 0.04492),
    (0.21341, 0.045166),
    (0.24484, 0.044669),
    (0.29162, 0.043659),
    (0.3454, 0.042863),
    (0.39784, 0.043004),
    (0.44062, 0.044807),
    (0.46539, 0.048998),
    (0.47641, 0.055144),
    (0.48524, 0.062928),
    (0.49191, 0.072164),
    (0.49649, 0.082666),
    (0.49904, 0.094247),
    (0.49962, 0.10672),
    (0.49373, 0.1314),
    (0.47946, 0.15986),
    (0.45991, 0.18985),
    (0.43817, 0.2191),
    (0.41733, 0.24538),
    (0.40048, 0.26643),
    (0.35945, 0.31859),
    (0.3172, 0.36841),
    (0.27178, 0.41346),
    (0.22126, 0.45127),
    (0.16368, 0.4794),
    (0.097096, 0.49539),
    (0.066005, 0.49837),
    (0.034638, 0.49975),
    (0.0031154, 0.5),
    (-0.028442, 0.4996),
    (-0.059914, 0.49901),
    (-0.09118, 0.49871),
    (-0.32632, 0.49871),
    (-0.33917, 0.49814),
    (-0.35366, 0.49602),
    (-0.3679, 0.49177),
    (-0.38003, 0.48481),
    (-0.38816, 0.47454),
];
const AI_PETAL_BOT_IDX: &[u32] = &[
    66, 0, 1, 66, 1, 2, 66, 2, 3, 66, 3, 4, 66, 4, 5, 66, 5, 6, 66, 6, 7, 66, 7, 8, 66, 8, 9, 66,
    9, 10, 66, 10, 11, 66, 11, 12, 66, 12, 13, 66, 13, 14, 66, 14, 15, 66, 15, 16, 66, 16, 17, 66,
    17, 18, 66, 18, 19, 19, 20, 21, 19, 21, 22, 66, 19, 22, 66, 22, 23, 66, 23, 24, 66, 24, 25, 66,
    25, 26, 66, 26, 27, 66, 27, 28, 66, 28, 29, 66, 29, 30, 66, 30, 31, 66, 31, 32, 66, 32, 33, 66,
    33, 34, 66, 34, 35, 66, 35, 36, 66, 36, 37, 66, 37, 38, 66, 38, 39, 66, 39, 40, 66, 40, 41, 66,
    41, 42, 66, 42, 43, 66, 43, 44, 66, 44, 45, 66, 45, 46, 66, 46, 47, 66, 47, 48, 66, 48, 49, 66,
    49, 50, 66, 50, 51, 66, 51, 52, 66, 52, 53, 66, 53, 54, 66, 54, 55, 66, 55, 56, 66, 56, 57, 66,
    57, 58, 66, 58, 59, 66, 59, 60, 66, 60, 61, 66, 61, 62, 66, 62, 63, 66, 63, 64, 64, 65, 66,
];

/// Draws a Lucide glyph (any `lucide::*` constant) centred on `c`; `size` is the visual icon size in points.
pub fn glyph(p: &Painter, c: Pos2, size: f32, g: &str, color: Color32) {
    if g == l::SPARKLES {
        paint_ai_emblem(p, c, size, color);
        return;
    }
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
    let size = size * crate::theme::content_z();
    if icon == Icon::StarFilled {
        let k = size / 16.0;
        let center = pos2(c.x, c.y + 0.5 * k);
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(center, color);
        for i in 0..10 {
            let r = if i % 2 == 0 { 7.4 } else { 3.2 } * k;
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            mesh.colored_vertex(pos2(center.x + r * a.cos(), center.y + r * a.sin()), color);
        }
        for i in 0..10u32 {
            mesh.add_triangle(0, 1 + i, 1 + ((i + 1) % 10));
        }
        p.add(Shape::mesh(mesh));
        return;
    }
    glyph(p, c, size, icon.glyph(), color);
}

// ───────────────────────────── images & page icons ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImgState {
    Ready,
    Loading,
    Failed,
}

/// Natural size of an image if it is loaded (also kicks off loading).
pub fn image_size(ui: &Ui, uri: &str, hint: Vec2) -> (ImgState, Option<Vec2>) {
    match egui::Image::new(uri.to_owned()).load_for_size(ui.ctx(), hint) {
        Ok(TexturePoll::Ready { texture }) => (ImgState::Ready, Some(texture.size)),
        Ok(TexturePoll::Pending { .. }) => (ImgState::Loading, None),
        Err(_) => (ImgState::Failed, None),
    }
}

/// Paints an image stretched to `rect` (callers compute the aspect ratio first).
pub fn paint_image(ui: &Ui, rect: Rect, uri: &str, radius: f32) -> ImgState {
    let (st, _) = image_size(ui, uri, rect.size());
    if st == ImgState::Ready {
        egui::Image::new(uri.to_owned()).corner_radius(radius).paint_at(ui, rect);
    }
    st
}

/// Paints an image like CSS `object-fit: cover` (cropping), `pos` = vertical focus 0..1.
pub fn paint_cover(ui: &Ui, rect: Rect, uri: &str, pos: f32) -> ImgState {
    let (st, size) = image_size(ui, uri, rect.size());
    if let Some(ts) = size {
        let scale = (rect.width() / ts.x).max(rect.height() / ts.y).max(1e-4);
        let (uw, uh) = ((rect.width() / scale / ts.x).min(1.0), (rect.height() / scale / ts.y).min(1.0));
        let uv = Rect::from_min_size(pos2((1.0 - uw) / 2.0, (1.0 - uh) * pos.clamp(0.0, 1.0)), vec2(uw, uh));
        egui::Image::new(uri.to_owned()).uv(uv).paint_at(ui, rect);
    }
    st
}

pub fn is_emoji(icon: &str) -> bool {
    icon.chars().next().map_or(false, |c| (c as u32) >= 0x2190 && !(0xE000..=0xF8FF).contains(&(c as u32)))
}

/// Text that is safe to paint with a plain font for any page icon string (custom images fall back to a picture glyph).
pub fn icon_text(icon: &str) -> &str {
    if icon.starts_with("img:") {
        l::IMAGE
    } else {
        icon
    }
}

/// Paints a page icon centred on `c`: Lucide glyph, colour emoji (Twemoji image, glyph fallback) or a custom picture.
pub fn paint_page_icon(ui: &Ui, c: Pos2, icon: &str, size: f32, color: Color32) {
    if icon.is_empty() {
        return;
    }
    let r = Rect::from_center_size(c, vec2(size, size));
    if let Some(src) = icon.strip_prefix("img:") {
        if !ui.is_rect_visible(r) {
            return;
        }
        let st = paint_image(ui, r, &crate::media::src_uri(src), size * 0.2);
        if st == ImgState::Failed {
            glyph(ui.painter(), c, size * 0.8, l::IMAGE, color);
        }
        return;
    }
    if is_emoji(icon) {
        if !ui.is_rect_visible(r) {
            return;
        }
        // colour emoji come from the bundled Noto pack through the text engine (no network, same look as in text)
        ui.painter().text(c, Align2::CENTER_CENTER, icon, crate::theme::body(size * 0.82), color);
        return;
    }
    ui.painter().text(c, Align2::CENTER_CENTER, icon, crate::theme::body(size), color);
}
