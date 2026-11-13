//! Colors, fonts and egui styling. Palettes are interpolated for smooth theme transitions.

use std::sync::Arc;

use egui::{
    epaint::Shadow, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub hover: Color32,
    pub active: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub callout: Color32,
    pub code_bg: Color32,
    pub popup: Color32,
    pub card: Color32,
    pub danger: Color32,
}

fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

impl Palette {
    pub fn light() -> Self {
        Self {
            bg: c(255, 255, 255),
            sidebar: c(247, 247, 245),
            text: c(55, 53, 47),
            muted: c(155, 154, 151),
            hover: c(236, 236, 234),
            active: c(228, 228, 225),
            border: c(233, 233, 231),
            accent: c(35, 131, 226),
            accent_soft: c(226, 238, 251),
            callout: c(241, 241, 239),
            code_bg: c(247, 246, 243),
            popup: c(255, 255, 255),
            card: c(255, 255, 255),
            danger: c(224, 62, 62),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: c(25, 25, 25),
            sidebar: c(32, 32, 32),
            text: c(235, 235, 234),
            muted: c(127, 127, 127),
            hover: c(45, 45, 45),
            active: c(55, 55, 55),
            border: c(47, 47, 47),
            accent: c(45, 140, 235),
            accent_soft: c(30, 50, 74),
            callout: c(40, 40, 40),
            code_bg: c(34, 34, 34),
            popup: c(37, 37, 37),
            card: c(37, 37, 37),
            danger: c(240, 90, 90),
        }
    }

    pub fn mix(t: f32) -> Self {
        let (a, b) = (Self::light(), Self::dark());
        let m = |x, y| lerp_color(x, y, t);
        Self {
            bg: m(a.bg, b.bg),
            sidebar: m(a.sidebar, b.sidebar),
            text: m(a.text, b.text),
            muted: m(a.muted, b.muted),
            hover: m(a.hover, b.hover),
            active: m(a.active, b.active),
            border: m(a.border, b.border),
            accent: m(a.accent, b.accent),
            accent_soft: m(a.accent_soft, b.accent_soft),
            callout: m(a.callout, b.callout),
            code_bg: m(a.code_bg, b.code_bg),
            popup: m(a.popup, b.popup),
            card: m(a.card, b.card),
            danger: m(a.danger, b.danger),
        }
    }
}

/// Notion-style label colors used for board columns and tags.
pub const TAG_NAMES: [&str; 9] = ["Gray", "Blue", "Green", "Orange", "Purple", "Red", "Yellow", "Pink", "Brown"];

pub fn tag_color(i: usize) -> Color32 {
    [
        c(155, 154, 151),
        c(51, 126, 169),
        c(68, 131, 97),
        c(217, 115, 13),
        c(144, 101, 176),
        c(212, 76, 71),
        c(203, 145, 47),
        c(193, 76, 138),
        c(159, 107, 83),
    ][i % 9]
}

/// Accent colour choices for the workspace.
pub const ACCENTS: [(&str, (u8, u8, u8)); 6] = [
    ("Blue", (35, 131, 226)),
    ("Purple", (144, 101, 176)),
    ("Green", (46, 160, 110)),
    ("Orange", (217, 115, 13)),
    ("Red", (212, 76, 71)),
    ("Pink", (193, 76, 138)),
];

pub fn with_accent(mut p: Palette, idx: usize, t: f32) -> Palette {
    let (_, (r, g, b)) = ACCENTS[idx % ACCENTS.len()];
    let base = c(r, g, b);
    p.accent = lerp_color(base, lerp_color(base, Color32::WHITE, 0.12), t);
    p.accent_soft = lerp_color(p.bg, p.accent, 0.14);
    p
}

pub const COVERS: [((u8, u8, u8), (u8, u8, u8)); 8] = [
    ((255, 154, 158), (250, 208, 196)),
    ((161, 140, 209), (251, 194, 235)),
    ((132, 250, 176), (143, 211, 244)),
    ((250, 177, 160), (255, 234, 167)),
    ((79, 172, 254), (0, 242, 254)),
    ((67, 233, 123), (56, 249, 215)),
    ((55, 59, 68), (66, 134, 244)),
    ((253, 160, 133), (246, 211, 101)),
];

thread_local! {
    static CONTENT_Z: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// Page "content size" while a database view (table / board / list / gallery / calendar) is drawn; 1.0 everywhere else.
/// `body()`, `bold()` and the icon painter multiply by it, so the text of a scaled view follows the page zoom automatically.
pub fn content_z() -> f32 {
    CONTENT_Z.with(|z| z.get())
}

/// Scales a layout dimension (row height, column width, card size …) by the current content size.
pub fn zs(v: f32) -> f32 {
    v * content_z()
}

/// RAII guard: the content size is `z` until it is dropped.
pub struct ContentScale(f32);

impl ContentScale {
    pub fn new(z: f32) -> Self {
        let z = if z.is_finite() && z > 0.0 { z.clamp(0.5, 2.5) } else { 1.0 };
        let old = CONTENT_Z.with(|c| c.replace(z));
        ContentScale(old)
    }
}

impl Drop for ContentScale {
    fn drop(&mut self) {
        CONTENT_Z.with(|c| c.set(self.0));
    }
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size * content_z(), FontFamily::Name("bold".into()))
}

pub fn body(size: f32) -> FontId {
    FontId::new(size * content_z(), FontFamily::Proportional)
}

/// Fonts downloaded at run time (web), see [`load_web_fonts`].
static EXTRA_FONTS: std::sync::Mutex<Vec<(String, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

/// Web: the Chinese / Japanese / Korean subsets (≈ 2.8 MB, cached by the service worker) are fetched in the background and registered
/// as soon as they arrive. Native builds use the operating system's fonts instead.
#[cfg(target_arch = "wasm32")]
pub fn load_web_fonts(ctx: &Context) {
    for (name, url) in [("cjk-sc", "fonts/NotoSansSC-subset.otf"), ("cjk-jp", "fonts/NotoSansJP-subset.otf"), ("cjk-kr", "fonts/NotoSansKR-subset.otf")] {
        let ctx = ctx.clone();
        ehttp::fetch(ehttp::Request::get(url), move |res| {
            if let Ok(r) = res {
                if r.ok && r.bytes.len() > 1000 {
                    if let Ok(mut g) = EXTRA_FONTS.lock() {
                        g.push((name.to_owned(), r.bytes));
                    }
                    setup_fonts(&ctx);
                    ctx.request_repaint();
                }
            }
        });
    }
}

/// Scripts whose fonts are loaded **on demand** (the first time text in that script appears): (name, code point ranges).
/// Keeping them out of the start-up font set saves memory (the system CJK fonts alone are 50+ MB on Windows / Android) and start-up time;
/// Latin, Cyrillic, Greek, Vietnamese and the icon font are always present.
const SCRIPTS: &[(&str, &[(u32, u32)])] = &[
    ("bengali", &[(0x980, 0x9FF)]),
    ("devanagari", &[(0x900, 0x97F), (0xA8E0, 0xA8FF)]),
    ("arabic", &[(0x600, 0x6FF), (0x750, 0x77F), (0x8A0, 0x8FF), (0xFB50, 0xFDFF), (0xFE70, 0xFEFF)]),
    ("hebrew", &[(0x590, 0x5FF), (0xFB1D, 0xFB4F)]),
    ("thai", &[(0xE00, 0xE7F)]),
    ("tamil", &[(0xB80, 0xBFF)]),
    ("telugu", &[(0xC00, 0xC7F)]),
    ("kannada", &[(0xC80, 0xCFF)]),
    ("malayalam", &[(0xD00, 0xD7F)]),
    ("gujarati", &[(0xA80, 0xAFF)]),
    ("gurmukhi", &[(0xA00, 0xA7F)]),
    ("oriya", &[(0xB00, 0xB7F)]),
    ("sinhala", &[(0xD80, 0xDFF)]),
    ("khmer", &[(0x1780, 0x17FF), (0x19E0, 0x19FF)]),
    ("lao", &[(0xE80, 0xEFF)]),
    ("myanmar", &[(0x1000, 0x109F)]),
    ("armenian", &[(0x530, 0x58F)]),
    ("georgian", &[(0x10A0, 0x10FF), (0x1C90, 0x1CBF)]),
    ("ethiopic", &[(0x1200, 0x139F)]),
    ("cjk", &[(0x2E80, 0x2FDF), (0x3000, 0x30FF), (0x3100, 0x31FF), (0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xAC00, 0xD7AF), (0xF900, 0xFAFF), (0xFF00, 0xFFEF), (0x20000, 0x2FFFF)]),
];

/// Bit set of the script groups whose fonts are currently registered.
static LOADED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn script_bit(c: char) -> Option<u32> {
    let u = c as u32;
    if u < 0x530 {
        return None;
    }
    SCRIPTS.iter().position(|(_, r)| r.iter().any(|(a, b)| u >= *a && u <= *b)).map(|i| 1u32 << i)
}

/// Makes sure the fonts for every script that occurs in `text` are loaded. Cheap for Latin text (one pass, no allocation); the first time a
/// new script shows up the font set is rebuilt (and a repaint is requested).
pub fn ensure_scripts(ctx: &Context, text: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    if text.is_ascii() {
        return;
    }
    let have = LOADED.load(Relaxed);
    let mut need = 0u32;
    for c in text.chars() {
        if let Some(bit) = script_bit(c) {
            if have & bit == 0 {
                need |= bit;
            }
        }
    }
    if need != 0 {
        LOADED.fetch_or(need, Relaxed);
        setup_fonts(ctx);
        ctx.request_repaint();
    }
}

fn group_loaded(name: &str) -> bool {
    SCRIPTS.iter().position(|(n, _)| *n == name).map_or(false, |i| LOADED.load(std::sync::atomic::Ordering::Relaxed) & (1 << i) != 0)
}

pub fn setup_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        Arc::new(FontData::from_static(include_bytes!("../assets/Inter-Regular.otf"))),
    );
    fonts.font_data.insert(
        "inter-bold".to_owned(),
        Arc::new(FontData::from_static(include_bytes!("../assets/Inter-Bold.otf"))),
    );
    // Lucide icon font: its own family and a last-resort fallback everywhere, so icon glyphs work in any text.
    fonts.font_data.insert("lucide".to_owned(), Arc::new(FontData::from_static(include_bytes!("../assets/lucide.ttf"))));
    // The Lucide glyphs live in the private-use area, which egui's bundled emoji/icon fonts also use for *different* shapes.
    // Inter v4 ships 700+ private-use stylistic alternates (e.g. U+E286 = an accented 'u'); they were stripped from the bundled
    // Inter files (assets/) so that they can no longer shadow Lucide. Lucide is also subset to the private-use range only. Otherwise some
    // icons render as other symbols, dots or empty boxes.
    // Every script with complex shaping (Indic, Arabic, Hebrew, Thai, Khmer, Myanmar …) is shaped by HarfBuzz (vendor/epaint), so each of
    // them needs its own font. They are registered lazily (see `ensure_scripts`). Order matters only for shared code points (the danda).
    let mut scripts: Vec<String> = vec![];
    for (name, bytes) in [
        ("noto-bengali", &include_bytes!("../assets/NotoSansBengali-Regular.ttf")[..]),
        ("noto-devanagari", &include_bytes!("../assets/NotoSansDevanagari-Regular.ttf")[..]),
        ("noto-arabic", &include_bytes!("../assets/NotoSansArabic-Regular.ttf")[..]),
        ("noto-hebrew", &include_bytes!("../assets/NotoSansHebrew-Regular.ttf")[..]),
        ("noto-thai", &include_bytes!("../assets/NotoSansThai-Regular.ttf")[..]),
        ("noto-tamil", &include_bytes!("../assets/NotoSansTamil-Regular.ttf")[..]),
        ("noto-telugu", &include_bytes!("../assets/NotoSansTelugu-Regular.ttf")[..]),
        ("noto-kannada", &include_bytes!("../assets/NotoSansKannada-Regular.ttf")[..]),
        ("noto-malayalam", &include_bytes!("../assets/NotoSansMalayalam-Regular.ttf")[..]),
        ("noto-gujarati", &include_bytes!("../assets/NotoSansGujarati-Regular.ttf")[..]),
        ("noto-gurmukhi", &include_bytes!("../assets/NotoSansGurmukhi-Regular.ttf")[..]),
        ("noto-oriya", &include_bytes!("../assets/NotoSansOriya-Regular.ttf")[..]),
        ("noto-sinhala", &include_bytes!("../assets/NotoSansSinhala-Regular.ttf")[..]),
        ("noto-khmer", &include_bytes!("../assets/NotoSansKhmer-Regular.ttf")[..]),
        ("noto-lao", &include_bytes!("../assets/NotoSansLao-Regular.ttf")[..]),
        ("noto-myanmar", &include_bytes!("../assets/NotoSansMyanmar-Regular.ttf")[..]),
        ("noto-armenian", &include_bytes!("../assets/NotoSansArmenian-Regular.ttf")[..]),
        ("noto-georgian", &include_bytes!("../assets/NotoSansGeorgian-Regular.ttf")[..]),
        ("noto-ethiopic", &include_bytes!("../assets/NotoSansEthiopic-Regular.ttf")[..]),
    ] {
        if !group_loaded(&name["noto-".len()..]) {
            continue;
        }
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        scripts.push(name.to_owned());
    }
    // fonts that arrived over the network (web: CJK subsets)
    if let Ok(extra) = EXTRA_FONTS.lock() {
        for (name, bytes) in extra.iter() {
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes.clone())));
            scripts.push(name.clone());
        }
    }
    scripts.extend(system_fonts(&mut fonts));
    let rest: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".to_owned());
    prop.insert(1, "lucide".to_owned());
    prop.extend(scripts.iter().cloned());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(1.min(mono.len()), "lucide".to_owned());
    mono.extend(scripts.iter().cloned());
    let mut b = vec!["inter-bold".to_owned(), "lucide".to_owned()];
    b.extend(rest);
    b.extend(scripts.iter().cloned());
    fonts.families.insert(FontFamily::Name("bold".into()), b);
    fonts.families.insert(FontFamily::Name("lucide".into()), vec!["lucide".to_owned()]);
    ctx.set_fonts(fonts);
}

/// Fonts of the operating system for the scripts that are not embedded. Registers the files it finds and returns their names
/// (in fallback order). Symbol fonts are small and always loaded; the big CJK files are read only once CJK text appears.
/// Web has no access to system fonts; the embedded Noto fonts cover the most used scripts there.
#[allow(unused_variables)]
fn system_fonts(fonts: &mut FontDefinitions) -> Vec<String> {
    let mut names = vec![];
    #[cfg(any(target_os = "android", windows))]
    {
        // (path, needs the CJK group)
        #[cfg(target_os = "android")]
        let candidates: &[(&str, bool)] = &[
            ("/system/fonts/NotoSansCJK-Regular.ttc", true),
            ("/system/fonts/NotoSansCJKjp-Regular.otf", true),
            ("/system/fonts/NotoSansCJKsc-Regular.otf", true),
            ("/system/fonts/NotoSansSymbols-Regular-Subsetted.ttf", false),
            ("/system/fonts/DroidSansFallback.ttf", true),
            ("/system/fonts/DroidSansFallbackFull.ttf", true),
        ];
        #[cfg(windows)]
        let candidates: &[(&str, bool)] = &[
            ("C:\\Windows\\Fonts\\msyh.ttc", true),
            ("C:\\Windows\\Fonts\\YuGothR.ttc", true),
            ("C:\\Windows\\Fonts\\meiryo.ttc", true),
            ("C:\\Windows\\Fonts\\malgun.ttf", true),
            ("C:\\Windows\\Fonts\\segoeui.ttf", false),
            ("C:\\Windows\\Fonts\\seguisym.ttf", false),
        ];
        let cjk = group_loaded("cjk");
        for (i, (path, is_cjk)) in candidates.iter().enumerate() {
            if *is_cjk && !cjk {
                continue;
            }
            let Ok(meta) = std::fs::metadata(path) else { continue };
            if meta.len() > 40 * 1024 * 1024 {
                continue;
            }
            let Ok(bytes) = std::fs::read(path) else { continue };
            let name = format!("system-{i}");
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
            names.push(name);
        }
    }
    names
}

pub fn apply(ctx: &Context, p: &Palette, dark: bool) {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.popup;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    let sh = Color32::from_black_alpha(if dark { 120 } else { 38 });
    v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: sh };
    v.popup_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: sh };
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.text_cursor.stroke = Stroke::new(1.6_f32, p.accent);
    v.override_text_color = Some(p.text);
    v.widgets.noninteractive.bg_fill = p.bg;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.inactive.weak_bg_fill = p.hover;
    v.widgets.inactive.bg_fill = p.hover;
    v.widgets.hovered.weak_bg_fill = p.active;
    v.widgets.hovered.bg_fill = p.active;
    v.widgets.active.weak_bg_fill = p.active;
    v.widgets.active.bg_fill = p.active;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.text_styles.insert(TextStyle::Body, body(15.0));
        s.text_styles.insert(TextStyle::Button, body(14.0));
        s.text_styles.insert(TextStyle::Small, body(12.0));
        s.text_styles.insert(TextStyle::Heading, bold(26.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.5));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.interaction.selectable_labels = true;
        s.animation_time = 0.12;
        // a tapped control stays "hovered" on touch screens — its tooltip would hang on screen until the next tap
        s.interaction.tooltip_delay = if crate::touch::touchy() { 1.0e6 } else { 0.5 };
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        if crate::touch::touchy() {
            // finger-sized targets: ≥ 38 pt rows, roomier padding, bigger check boxes / radio buttons
            s.spacing.interact_size = egui::vec2(40.0, 38.0);
            s.spacing.button_padding = egui::vec2(14.0, 9.0);
            s.spacing.item_spacing = egui::vec2(8.0, 8.0);
            s.spacing.icon_width = 24.0;
            s.spacing.icon_width_inner = 14.0;
            s.spacing.icon_spacing = 8.0;
            s.spacing.slider_rail_height = 8.0;
            s.interaction.interact_radius = 10.0;
            s.interaction.resize_grab_radius_side = 12.0;
            s.interaction.resize_grab_radius_corner = 16.0;
        }
    });
}
