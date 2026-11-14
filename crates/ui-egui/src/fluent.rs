//! Ribbon icons from Microsoft's Fluent UI System Icons (MIT, `assets/fluent-icons/LICENSE`):
//! the same style as current Office. Each icon is an SVG drawn once per size into a texture
//! and tinted with the icon colour; icons without a Fluent match keep their own drawing
//! ([`crate::icons`]). Off until chosen: `TAW_FLUENT_ICONS` switches them on.

use std::sync::OnceLock;

use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2, vec2};

#[path = "fluent_map.rs"]
mod map;

/// Whether the Fluent icons are in use.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TAW_FLUENT_ICONS").is_some())
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

/// Paint icon `name` into `r` in colour `c`; false when it has no Fluent icon.
pub fn paint(p: &Painter, r: Rect, name: &str, c: Color32) -> bool {
    if !enabled() {
        return false;
    }
    let Some((_, small, large)) = map::MAP.iter().find(|(n, _, _)| *n == name) else { return false };
    // The icon's own box: Fluent icons fill their square, so they sit a little inside it.
    let side = r.width().min(r.height()) * 0.92;
    let ctx = p.ctx();
    let px = (side * ctx.pixels_per_point()).round().clamp(8.0, 256.0) as u32;
    let large_one = side >= 24.0;
    let key = egui::Id::unique(("fluent-icon", name, px, large_one));
    let tex = match ctx.data(|d| d.get_temp::<TextureHandle>(key)) {
        Some(t) => t,
        None => {
            let Some(img) = rasterize(if large_one { large } else { small }, px) else { return false };
            let t = ctx.load_texture(format!("fluent-{name}-{px}"), img, TextureOptions::LINEAR);
            ctx.data_mut(|d| d.insert_temp(key, t.clone()));
            t
        }
    };
    let rect = Rect::from_center_size(r.center(), vec2(side, side));
    p.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), c);
    true
}
