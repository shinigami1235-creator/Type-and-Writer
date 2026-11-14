//! Ribbon icons from Microsoft's Fluent UI System Icons (MIT, `assets/fluent-icons/LICENSE`),
//! coloured the way Word colours its ribbon: grey outlines, with a pale fill in the theme's blue
//! (yellow for Format Painter, green for SmartArt) inside the main shapes, and blue lines on a
//! few. Each SVG is drawn once per size into a white mask and tinted. Icons without a Fluent
//! match keep their own drawing ([`crate::icons`]). `TAW_CLASSIC_ICONS` switches them off.

use std::sync::OnceLock;

use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2, vec2};

#[path = "fluent_map.rs"]
mod map;

/// Whether the Fluent icons are in use.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TAW_CLASSIC_ICONS").is_none())
}

/// How an icon takes colour.
#[derive(Clone, Copy)]
enum Accent {
    /// Grey outline only (text formatting, alignment, lists).
    None,
    /// A pale fill of this colour (the theme accent when `None`) under the grey outline.
    Fill(Option<Color32>),
    /// The outline itself in the theme accent.
    Line,
}

fn accent_of(name: &str) -> Accent {
    let yellow = Color32::from_rgb(0xF2, 0xB8, 0x11);
    let green = Color32::from_rgb(0x2E, 0x9E, 0x5B);
    match name {
        "painter" => Accent::Fill(Some(yellow)),
        "smartArt" => Accent::Fill(Some(green)),
        "paste" | "picture" | "shapes" | "chart" | "table" | "header" | "footer" | "pageNumber" | "coverPage" | "blankPage" | "pageBreak"
        | "textBox" | "newComment" | "dictate" | "signature" | "dateTime" | "object" | "icons" | "editor" | "copy" => Accent::Fill(None),
        "link" | "bookmark" | "crossRef" | "equation" | "symbol" => Accent::Line,
        _ => Accent::None,
    }
}

/// An SVG as a white mask `px` pixels square (its colour comes from the tint).
fn rasterize(svg: &[u8], px: u32) -> Option<ColorImage> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pix = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let s = px as f32 / size.width().max(size.height()).max(1.0);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pix.as_mut());
    let rgba: Vec<u8> = pix.data().chunks_exact(4).flat_map(|p| [255, 255, 255, p[3]]).collect();
    Some(ColorImage::from_rgba_unmultiplied([px as usize, px as usize], &rgba))
}

/// The mask of one SVG at `px`, made once and kept.
fn texture(ctx: &egui::Context, name: &str, which: &str, svg: &[u8], px: u32) -> Option<TextureHandle> {
    let key = egui::Id::unique(("fluent-icon", name, which, px));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(key)) {
        return Some(t);
    }
    let t = ctx.load_texture(format!("fluent-{name}-{which}-{px}"), rasterize(svg, px)?, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(key, t.clone()));
    Some(t)
}

/// Paint icon `name` into `r`, outline in `c`, accents from `accent`; false when it has no
/// Fluent icon.
pub fn paint(p: &Painter, r: Rect, name: &str, c: Color32, accent: Color32) -> bool {
    if !enabled() {
        return false;
    }
    let Some(&(_, small, large, small_fill, large_fill)) = map::MAP.iter().find(|(n, ..)| *n == name) else { return false };
    // Fluent icons fill their square, so they sit a little inside the button's icon box.
    let side = r.width().min(r.height()) * 0.92;
    let ctx = p.ctx();
    let px = (side * ctx.pixels_per_point()).round().clamp(8.0, 256.0) as u32;
    let large_one = side >= 24.0;
    let (outline, filled) = if large_one { (large, large_fill) } else { (small, small_fill) };
    let rect = Rect::from_center_size(r.center(), vec2(side, side));
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let kind = accent_of(name);
    if let Accent::Fill(colour) = kind
        && let Some(t) = texture(ctx, name, if large_one { "lf" } else { "sf" }, filled, px)
    {
        // A pale wash of the colour: the filled shape behind the outline.
        let tint = colour.unwrap_or(accent).gamma_multiply(0.5);
        p.image(t.id(), rect, uv, tint);
    }
    let Some(t) = texture(ctx, name, if large_one { "l" } else { "s" }, outline, px) else { return false };
    let line = if matches!(kind, Accent::Line) { accent } else { c };
    p.image(t.id(), rect, uv, line);
    true
}
