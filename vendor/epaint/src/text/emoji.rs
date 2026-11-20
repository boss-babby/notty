//! Lumen: colour emoji inside text.
//!
//! egui's font atlas is a single-channel coverage image, so a normal font can only ever draw emoji as black-and-white glyphs
//! (and every emoji the bundled monochrome fonts do not contain was drawn as an empty box).  Lumen ships a colour emoji pack
//! (`assets/emoji-noto.pack`, Noto style) and registers a *provider* here.  The text layout recognises emoji sequences
//! (ZWJ families, skin tones, flags, keycaps, variation selectors …), asks the provider for the bitmap, resamples it to the
//! font size and writes it into the very same glyph atlas.  A colour texel is stored in an `f32` slot as a NaN whose payload
//! carries `A8 R5 G6 B5`; [`crate::FontImage::srgba_pixels`] decodes it, so no texture, shader or backend change is needed.
//! The quad of such a glyph is tinted with [`EMOJI_MARK`], which the tessellator turns into plain white (text colour overrides
//! must not touch it).
//!
//! The monochrome look is still available for the places that want a tintable glyph: use the font family
//! `FontFamily::Name("emoji-bw")` (the layout does not substitute colour bitmaps for families whose name starts with `emoji-bw`).

use crate::Color32;
use std::sync::OnceLock;

/// Vertex colour that marks the quad of a colour emoji. The tessellator replaces it by white.
pub const EMOJI_MARK: Color32 = Color32::from_rgba_premultiplied(255, 255, 254, 255);

/// A decoded emoji bitmap (straight, non-premultiplied RGBA8).
pub struct EmojiBitmap {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// Looks up a normalised key (lower-case hex code points joined by `-`, `fe0f` removed) and decodes the picture.
pub type EmojiProvider = fn(&str) -> Option<EmojiBitmap>;
/// Cheap existence test for a key (no decoding).
pub type EmojiExists = fn(&str) -> bool;

static PROVIDER: OnceLock<(EmojiProvider, EmojiExists)> = OnceLock::new();

/// Registers the colour emoji source. Call once at start-up; until then every emoji falls back to the monochrome fonts.
pub fn set_emoji_provider(load: EmojiProvider, exists: EmojiExists) {
    let _ = PROVIDER.set((load, exists));
}

pub(crate) fn provider() -> Option<(EmojiProvider, EmojiExists)> {
    PROVIDER.get().copied()
}

// ---------------------------------------------------------------------------------------------------------------------
// f32 slot encoding

/// `A8 | R5<<8 | G6<<13 | B5<<19` stored as sign + 23 mantissa bits of a NaN. Fully transparent texels stay `0.0`.
#[inline]
pub(crate) fn pack_texel(r: u8, g: u8, b: u8, a: u8) -> f32 {
    if a == 0 {
        return 0.0;
    }
    let p: u32 = a as u32 | ((r as u32 >> 3) << 8) | ((g as u32 >> 2) << 13) | ((b as u32 >> 3) << 19);
    f32::from_bits(0x7F80_0000 | ((p >> 23) << 31) | (p & 0x007F_FFFF))
}

/// Decodes a colour texel into premultiplied sRGBA. `None` for ordinary coverage values.
#[inline]
pub fn unpack_texel(v: f32) -> Option<Color32> {
    if !v.is_nan() {
        return None;
    }
    let bits = v.to_bits();
    let p = (bits & 0x007F_FFFF) | ((bits >> 31) << 23);
    let a = p & 0xFF;
    let r5 = (p >> 8) & 0x1F;
    let g6 = (p >> 13) & 0x3F;
    let b5 = (p >> 19) & 0x1F;
    let r = (r5 * 255 + 15) / 31;
    let g = (g6 * 255 + 31) / 63;
    let b = (b5 * 255 + 15) / 31;
    Some(Color32::from_rgba_premultiplied(
        ((r * a + 127) / 255) as u8,
        ((g * a + 127) / 255) as u8,
        ((b * a + 127) / 255) as u8,
        a as u8,
    ))
}

// ---------------------------------------------------------------------------------------------------------------------
// sequence recognition

#[inline]
fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

#[inline]
fn is_skin_tone(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

#[inline]
fn is_tag(c: char) -> bool {
    ('\u{E0020}'..='\u{E007F}').contains(&c)
}

/// Code points that are drawn as emoji by default (no variation selector needed).
fn default_emoji(c: char) -> bool {
    let u = c as u32;
    if u >= 0x1F000 {
        if is_regional(c) {
            return false;
        }
        // enclosed alphanumerics / ideographs are text-style unless they are one of the emoji ones
        if (0x1F100..=0x1F2FF).contains(&u) {
            return matches!(u, 0x1F18E | 0x1F191..=0x1F19A | 0x1F201 | 0x1F21A | 0x1F22F | 0x1F232..=0x1F236 | 0x1F238..=0x1F23A | 0x1F250 | 0x1F251);
        }
        return u <= 0x1FAFF;
    }
    matches!(
        u,
        0x231A | 0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD | 0x25FE | 0x2614 | 0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1
            | 0x26AA | 0x26AB | 0x26BD | 0x26BE | 0x26C4 | 0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2 | 0x26F3 | 0x26F5 | 0x26FA | 0x26FD
            | 0x2705 | 0x270A | 0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF
            | 0x2B1B | 0x2B1C | 0x2B50 | 0x2B55
    )
}

/// Code points that become emoji when followed by U+FE0F.
fn text_emoji(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x2199 | 0x21A9 | 0x21AA | 0x2328 | 0x23CF | 0x23ED..=0x23EF | 0x23F1
            | 0x23F2 | 0x23F8..=0x23FA | 0x24C2 | 0x25AA | 0x25AB | 0x25B6 | 0x25C0 | 0x25FB | 0x25FC | 0x2600..=0x2604 | 0x260E | 0x2611
            | 0x2618 | 0x261D | 0x2620 | 0x2622 | 0x2623 | 0x2626 | 0x262A | 0x262E | 0x262F | 0x2638..=0x263A | 0x2640 | 0x2642
            | 0x265F | 0x2660 | 0x2663 | 0x2665 | 0x2666 | 0x2668 | 0x267B | 0x267E | 0x2692 | 0x2694..=0x2697 | 0x2699 | 0x269B
            | 0x269C | 0x26A0 | 0x26A7 | 0x26B0 | 0x26B1 | 0x26C8 | 0x26CF | 0x26D1 | 0x26D3 | 0x26E9 | 0x26F0 | 0x26F1 | 0x26F4
            | 0x26F7..=0x26F9 | 0x2702 | 0x2708 | 0x2709 | 0x270C | 0x270D | 0x270F | 0x2712 | 0x2714 | 0x2716 | 0x271D | 0x2721
            | 0x2733 | 0x2734 | 0x2744 | 0x2747 | 0x2763 | 0x2764 | 0x27A1 | 0x2934 | 0x2935 | 0x2B05..=0x2B07 | 0x3030 | 0x303D | 0x3297
            | 0x3299
    )
}

/// May this code point follow a zero-width joiner inside an emoji sequence?
fn joinable(c: char) -> bool {
    let u = c as u32;
    (0x1F000..=0x1FAFF).contains(&u)
        || matches!(u, 0x2640 | 0x2642 | 0x2695 | 0x2696 | 0x2708 | 0x2764 | 0x2620 | 0x26A7 | 0x2B1B | 0x2744 | 0x26D3 | 0x2B05 | 0x27A1 | 0x2B06 | 0x2B07)
}

/// Quick reject used on the hot path of the layout: can an emoji sequence start with `c`?
#[inline]
pub fn may_start(c: char, next: Option<char>) -> bool {
    let u = c as u32;
    if u < 0x80 {
        // keycap bases: 0-9 # * followed by FE0F / 20E3
        return matches!(c, '0'..='9' | '#' | '*') && matches!(next, Some('\u{FE0F}') | Some('\u{20E3}'));
    }
    u == 0xA9 || u == 0xAE || (0x203C..=0x3299).contains(&u) || (0x1F000..=0x1FAFF).contains(&u)
}

/// Candidate sequence lengths at the start of `chars`, longest first (variation selectors included in the count).
fn candidates(chars: &[char]) -> Vec<usize> {
    let c = chars[0];
    let at = |i: usize| chars.get(i).copied();
    if is_regional(c) {
        return if at(1).map_or(false, is_regional) { vec![2] } else { vec![] };
    }
    if matches!(c, '0'..='9' | '#' | '*') {
        return match (at(1), at(2)) {
            (Some('\u{FE0F}'), Some('\u{20E3}')) => vec![3],
            (Some('\u{20E3}'), _) => vec![2],
            _ => vec![],
        };
    }
    let fe0f = at(1) == Some('\u{FE0F}');
    let fe0e = at(1) == Some('\u{FE0E}');
    if fe0e || !(default_emoji(c) || (fe0f && text_emoji(c))) {
        return vec![];
    }
    // base [FE0F] [skin tone] [tags] { ZWJ element [FE0F] [skin tone] }
    let mut i = 1;
    let mut stops: Vec<usize> = vec![];
    loop {
        if at(i) == Some('\u{FE0F}') {
            i += 1;
        }
        if at(i).map_or(false, is_skin_tone) {
            i += 1;
        }
        while at(i).map_or(false, is_tag) {
            i += 1;
        }
        stops.push(i);
        if at(i) == Some('\u{200D}') && at(i + 1).map_or(false, joinable) {
            i += 2;
            continue;
        }
        break;
    }
    stops.reverse();
    let mut out = stops;
    let base = if fe0f { 2 } else { 1 };
    if !out.contains(&base) {
        out.push(base);
    }
    if !out.contains(&1) {
        out.push(1);
    }
    out
}

/// Normalised provider key of `chars`: lower-case hex code points joined by `-`, variation selector-16 removed.
pub fn key(chars: &[char]) -> String {
    let mut s = String::with_capacity(chars.len() * 6);
    for &c in chars {
        if c == '\u{FE0F}' {
            continue;
        }
        if !s.is_empty() {
            s.push('-');
        }
        s.push_str(&format!("{:x}", c as u32));
    }
    s
}

/// Longest emoji sequence at the start of `chars` that the provider has a picture for: `(characters consumed, key)`.
pub(crate) fn recognise(chars: &[char], has: &dyn Fn(&str) -> bool) -> Option<(usize, String)> {
    if chars.is_empty() || !may_start(chars[0], chars.get(1).copied()) {
        return None;
    }
    for n in candidates(chars) {
        let n = n.min(chars.len());
        let k = key(&chars[..n]);
        if has(&k) {
            return Some((n, k));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------------
// resampling

/// Resamples straight RGBA to `dw × dh` (premultiplied maths; box filter when shrinking, bilinear when enlarging).
pub(crate) fn resample(src: &EmojiBitmap, dw: usize, dh: usize) -> Vec<[u8; 4]> {
    let (sw, sh) = (src.w, src.h);
    let px = |x: usize, y: usize| -> [f32; 4] {
        let i = (y * sw + x) * 4;
        let a = src.rgba[i + 3] as f32 / 255.0;
        [src.rgba[i] as f32 * a, src.rgba[i + 1] as f32 * a, src.rgba[i + 2] as f32 * a, a * 255.0]
    };
    let mut out = Vec::with_capacity(dw * dh);
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for y in 0..dh {
        for x in 0..dw {
            let acc = if fx > 1.0 || fy > 1.0 {
                let (x0, x1) = (x as f32 * fx, (x as f32 + 1.0) * fx);
                let (y0, y1) = (y as f32 * fy, (y as f32 + 1.0) * fy);
                let mut acc = [0.0f32; 4];
                let mut wsum = 0.0;
                let mut sy = y0.floor() as usize;
                while (sy as f32) < y1 && sy < sh {
                    let wy = (y1.min(sy as f32 + 1.0) - y0.max(sy as f32)).max(0.0);
                    let mut sx = x0.floor() as usize;
                    while (sx as f32) < x1 && sx < sw {
                        let wx = (x1.min(sx as f32 + 1.0) - x0.max(sx as f32)).max(0.0);
                        let w = wx * wy;
                        let p = px(sx, sy);
                        for k in 0..4 {
                            acc[k] += p[k] * w;
                        }
                        wsum += w;
                        sx += 1;
                    }
                    sy += 1;
                }
                if wsum > 0.0 {
                    for v in acc.iter_mut() {
                        *v /= wsum;
                    }
                }
                acc
            } else {
                let gx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
                let gy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
                let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
                let (tx, ty) = (gx - x0 as f32, gy - y0 as f32);
                let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
                let mut acc = [0.0f32; 4];
                for k in 0..4 {
                    acc[k] = (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
                }
                acc
            };
            let a = acc[3].clamp(0.0, 255.0);
            if a < 0.5 {
                out.push([0, 0, 0, 0]);
            } else {
                let inv = 255.0 / a;
                out.push([
                    (acc[0] * inv).clamp(0.0, 255.0) as u8,
                    (acc[1] * inv).clamp(0.0, 255.0) as u8,
                    (acc[2] * inv).clamp(0.0, 255.0) as u8,
                    a.round() as u8,
                ]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texel_roundtrip() {
        let v = pack_texel(255, 128, 0, 255);
        assert!(v.is_nan());
        let c = unpack_texel(v).unwrap();
        assert_eq!(c.a(), 255);
        assert!(c.r() > 240 && c.g() > 120 && c.g() < 136 && c.b() < 8);
        assert_eq!(unpack_texel(0.5), None);
        assert_eq!(pack_texel(1, 2, 3, 0), 0.0);
        let w = vec![v];
        assert_eq!(w[0].to_bits(), v.to_bits());
    }

    #[test]
    fn sequences() {
        let has = |s: &str| matches!(s, "1f600" | "2764" | "1f469-200d-1f4bb" | "1f1e9-1f1ea" | "31-20e3" | "1f44d-1f3fd" | "1f44d");
        let r = |s: &str| recognise(&s.chars().collect::<Vec<_>>(), &has);
        assert_eq!(r("😀x").map(|x| x.0), Some(1));
        assert_eq!(r("❤\u{fe0f}x").map(|x| x.0), Some(2));
        assert_eq!(r("❤x"), None);
        assert_eq!(r("👩\u{200d}💻!").map(|x| x.0), Some(3));
        assert_eq!(r("🇩🇪").map(|x| x.0), Some(2));
        assert_eq!(r("1\u{fe0f}\u{20e3}").map(|x| x.0), Some(3));
        assert_eq!(r("1"), None);
        assert_eq!(r("👍🏽").map(|x| x.0), Some(2));
        assert_eq!(r("abc"), None);
    }
}
