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

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
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
    let rest: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".to_owned());
    prop.insert(1, "lucide".to_owned());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(1.min(mono.len()), "lucide".to_owned());
    let mut b = vec!["inter-bold".to_owned(), "lucide".to_owned()];
    b.extend(rest);
    fonts.families.insert(FontFamily::Name("bold".into()), b);
    fonts.families.insert(FontFamily::Name("lucide".into()), vec!["lucide".to_owned()]);
    ctx.set_fonts(fonts);
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
