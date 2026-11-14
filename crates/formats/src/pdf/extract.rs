//! What a PDF page draws, as positioned pieces: every glyph with its text, place, size and font
//! look; every picture with its box; and the thin lines that may rule a table. Positions are
//! points from the page's top left corner (y grows down), whatever the page's rotation.

use std::collections::HashMap;
use std::sync::Arc;

use hayro_interpret::font::Glyph;
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, ImageData, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask,
    TransformExt, interpret_page,
};
use kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};
use wordcraft_doc::Rgb;

/// Most glyphs read from one page; the rest of a hostile page is left out.
const MAX_GLYPHS: usize = 200_000;
/// Most pictures kept from one page.
const MAX_IMAGES: usize = 200;
/// Most ruling lines kept from one page.
const MAX_RULES: usize = 20_000;
/// Largest picture decoded, in pixels.
const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// Pictures smaller than this, points, on both sides, are decoration (bullets, rules drawn as
/// pictures) and are left out.
const MIN_IMAGE_PT: f64 = 6.0;

/// One glyph as drawn.
#[derive(Clone, Debug)]
pub struct G {
    pub text: String,
    /// Left and right edge, points.
    pub x0: f32,
    pub x1: f32,
    /// Baseline, points from the top.
    pub base: f32,
    /// Font size (the em), points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub family: Option<Arc<str>>,
    pub color: Rgb,
    /// A space was drawn right after this glyph.
    pub space_after: bool,
    /// Drawn upright (not turned or set vertically).
    pub upright: bool,
    /// Read by OCR: its box is approximate, and words are farther apart than letters.
    pub ocr: bool,
}

impl G {
    pub fn is_space(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }
}

/// A picture as drawn: PNG bytes and its box on the page.
#[derive(Clone, Debug)]
pub struct PImg {
    pub png: Arc<Vec<u8>>,
    pub rect: Rect,
}

/// A thin straight line: horizontal (`y0 == y1`) or vertical (`x0 == x1`).
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rule {
    pub fn horizontal(&self) -> bool {
        (self.y1 - self.y0).abs() < 0.5
    }
}

/// Everything read from one page.
#[derive(Debug, Default)]
pub struct PageData {
    /// Page size, points.
    pub w: f32,
    pub h: f32,
    pub glyphs: Vec<G>,
    pub images: Vec<PImg>,
    pub rules: Vec<Rule>,
    /// Glyphs drawn whose text the file doesn't give (fonts without a character map).
    pub unreadable: usize,
}

/// How a font looks, worked out once per font.
#[derive(Clone, Debug, Default)]
struct Look {
    bold: bool,
    italic: bool,
    mono: bool,
    family: Option<Arc<str>>,
}

/// A font's own character map turned around: glyph id → character. For fonts a PDF gives
/// without a ToUnicode map or glyph names (common in subset TrueType fonts).
fn reverse_cmap(data: &[u8]) -> HashMap<u32, char> {
    use skrifa::MetadataProvider;
    let mut m = HashMap::new();
    let Ok(font) = skrifa::FontRef::new(data) else { return m };
    for (cp, gid) in font.charmap().mappings().take(70_000) {
        // Symbol-encoded fonts put their characters at U+F020..U+F0FF.
        let cp = if (0xF020..=0xF0FF).contains(&cp) { cp - 0xF000 } else { cp };
        if let Some(c) = char::from_u32(cp).filter(|c| !c.is_control()) {
            m.entry(gid.to_u32()).or_insert(c);
        }
    }
    m
}

/// Fonts a page names in its resources (and its forms'), by the interpreter's cache key for
/// them: Type 1 fonts don't tell the interpreter their name, but their dictionary does.
fn named_fonts(page: &hayro_syntax::page::Page<'_>) -> HashMap<u128, Look> {
    use hayro_interpret::CacheKey;
    use hayro_syntax::object::{Dict, Name, Stream};
    fn add(fonts: &Dict<'_>, out: &mut HashMap<u128, Look>) {
        for key in fonts.keys().take(500) {
            let Some(font) = fonts.get::<Dict<'_>>(key.as_ref()) else { continue };
            let base = font.get::<Name<'_>>(b"BaseFont".as_slice()).map(|n| n.as_str().to_string()).unwrap_or_default();
            let desc = font.get::<Dict<'_>>(b"FontDescriptor".as_slice());
            let flags = desc.as_ref().and_then(|d| d.get::<i32>(b"Flags".as_slice())).unwrap_or(0);
            let weight = desc.as_ref().and_then(|d| d.get::<f32>(b"FontWeight".as_slice()));
            let lower = base.to_ascii_lowercase();
            let style = lower.split_once(['-', ',']).map(|(_, s)| s).unwrap_or("");
            // TeX's Computer Modern names its weights and slants: cmbx (bold), cmti (italic).
            let tex = lower.split_once('+').map(|(_, r)| r).unwrap_or(&lower);
            let bold = flags & (1 << 18) != 0
                || weight.is_some_and(|w| w >= 600.0)
                || ["bold", "black", "heavy", "semibold", "demi", "medi"].iter().any(|k| style.contains(k))
                || tex.starts_with("cmbx")
                || tex.starts_with("cmb10")
                || tex.starts_with("sfbx");
            let italic = flags & (1 << 6) != 0
                || ["italic", "oblique", "ital", "slant"].iter().any(|k| style.contains(k))
                || tex.starts_with("cmti")
                || tex.starts_with("cmsl")
                || tex.starts_with("sfti");
            let mono = flags & 1 != 0 || lower.contains("courier") || lower.contains("mono") || tex.starts_with("cmtt");
            out.insert(font.cache_key(), Look { bold, italic, mono, family: family_of(&base) });
        }
    }
    let mut out = HashMap::new();
    let r = page.resources();
    add(&r.fonts, &mut out);
    for key in r.x_objects.keys().take(200) {
        if let Some(form) = r.x_objects.get::<Stream<'_>>(key.as_ref())
            && let Some(res) = form.dict().get::<Dict<'_>>(b"Resources".as_slice())
            && let Some(fonts) = res.get::<Dict<'_>>(b"Font".as_slice())
        {
            add(&fonts, &mut out);
        }
    }
    out
}

struct Collector {
    page: PageData,
    looks: HashMap<u128, Look>,
    cmaps: HashMap<u128, Arc<HashMap<u32, char>>>,
    want_images: bool,
}

/// Read every page of `pdf`. `want_images`: decode pictures too.
pub fn read_pages(pdf: &hayro_syntax::Pdf, max_pages: usize, want_images: bool) -> Vec<PageData> {
    let cache = InterpreterCache::new();
    let mut looks = HashMap::new();
    let mut cmaps = HashMap::new();
    let mut out = Vec::new();
    for page in pdf.pages().iter().take(max_pages) {
        let (w, h) = page.render_dimensions();
        let initial = page.initial_transform(true).to_kurbo();
        let mut ctx = Context::new(initial, Rect::new(0.0, 0.0, w as f64, h as f64), &cache, pdf.xref(), InterpreterSettings::default());
        // Type 1 fonts: their look from the page's font dictionaries.
        for (k, v) in named_fonts(page) {
            looks.entry(k).or_insert(v);
        }
        let mut c = Collector {
            page: PageData { w, h, ..Default::default() },
            looks: std::mem::take(&mut looks),
            cmaps: std::mem::take(&mut cmaps),
            want_images,
        };
        interpret_page(page, &mut ctx, &mut c);
        looks = std::mem::take(&mut c.looks);
        cmaps = std::mem::take(&mut c.cmaps);
        out.push(c.page);
    }
    out
}

/// The text a glyph stands for, ligatures spelled out.
fn glyph_text(g: &Glyph<'_>) -> Option<String> {
    use hayro_interpret::hayro_cmap::BfString;
    let s = match g.as_unicode()? {
        BfString::Char(c) => c.to_string(),
        BfString::String(s) => s,
    };
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' => out.push(' '),
            '\u{00AD}' | '\u{200B}' | '\u{FEFF}' => {}
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A font family name from a PostScript name: `ABCDEF+TimesNewRomanPS-BoldItalicMT` → `Times
/// New Roman`; the PDF base fonts become the fonts Windows has for them.
pub fn family_of(ps: &str) -> Option<Arc<str>> {
    let ps = match ps.split_once('+') {
        Some((p, rest)) if p.len() == 6 => rest,
        _ => ps,
    };
    let base = ps.split([',', '-']).next().unwrap_or(ps);
    let mut base = base.trim();
    for suffix in ["PSMT", "PS", "MT"] {
        if let Some(b) = base.strip_suffix(suffix)
            && b.len() >= 3
        {
            base = b;
        }
    }
    let lower = base.to_ascii_lowercase();
    let starts = |ps: &[&str]| ps.iter().any(|p| lower.starts_with(p));
    // The PDF base fonts, and the free look-alikes TeX and Linux tools embed (URW Nimbus, TeX
    // Gyre, Computer Modern), as the fonts Windows has.
    let mapped = if matches!(lower.as_str(), "symbol" | "zapfdingbats") || starts(&["cmsy", "cmmi", "cmex", "msam", "msbm", "rsfs"]) {
        return None;
    } else if starts(&["helvetica", "arial", "nimbussan", "texgyreheros", "phv", "liberationsans", "freesans"]) {
        "Arial"
    } else if starts(&[
        "times",
        "nimbusrom",
        "texgyretermes",
        "ptm",
        "liberationserif",
        "freeserif",
        "cmr",
        "cmbx",
        "cmti",
        "cmsl",
        "cmcsc",
        "lmroman",
        "sfrm",
        "sfbx",
        "sfti",
    ]) {
        "Times New Roman"
    } else if starts(&["courier", "nimbusmon", "texgyrecursor", "pcr", "cmtt", "lmmono", "liberationmono", "freemono", "sftt"]) {
        "Courier New"
    } else if starts(&["palatino", "pplr", "pplb", "texgyrepagella", "urwpalladio"]) {
        "Palatino Linotype"
    } else if starts(&["bookman", "texgyrebonum", "urwbookman"]) {
        "Bookman Old Style"
    } else if starts(&["century", "texgyreschola", "ncenturyschlbk"]) {
        "Century Schoolbook"
    } else {
        ""
    };
    if !mapped.is_empty() {
        return Some(Arc::from(mapped));
    }
    if base.is_empty() || base.len() > 64 || !base.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ') {
        return None;
    }
    // CamelCase to words: "SourceSansPro" → "Source Sans Pro".
    let mut name = String::with_capacity(base.len() + 4);
    let chars: Vec<char> = base.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            Some(p) if c.is_ascii_uppercase() => p.is_ascii_lowercase() || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase())),
            _ => false,
        };
        if boundary && !name.ends_with(' ') {
            name.push(' ');
        }
        name.push(c);
    }
    Some(Arc::from(name.trim()))
}

fn look_of(g: &Glyph<'_>) -> (u128, Option<Look>) {
    match g {
        Glyph::Outline(o) => {
            let key = o.font_cache_key();
            (key, None)
        }
        Glyph::Type3(_) => (0, Some(Look::default())),
    }
}

fn compute_look(g: &Glyph<'_>) -> Look {
    let Glyph::Outline(o) = g else { return Look::default() };
    let Some(d) = o.font_data() else { return Look::default() };
    let name = d.postscript_name.clone().unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let style = lower.rsplit(['-', ',']).next().unwrap_or("");
    let bold =
        d.weight.is_some_and(|w| w >= 600) || ["bold", "black", "heavy", "semibold", "demi"].iter().any(|k| style.contains(k) || lower.ends_with(k));
    let italic = d.is_italic || style.contains("italic") || style.contains("oblique");
    Look { bold, italic, mono: d.is_monospace || lower.contains("mono") || lower.contains("courier"), family: family_of(&name) }
}

fn rgb_of(paint: &Paint<'_>) -> Rgb {
    match paint {
        Paint::Color(c) => {
            let [r, g, b, _] = c.to_rgba().to_rgba8();
            Rgb(r, g, b)
        }
        Paint::Pattern(_) => Rgb(0, 0, 0),
    }
}

fn finite(v: f64) -> f32 {
    if v.is_finite() { v as f32 } else { 0.0 }
}

impl Collector {
    fn add_glyph(&mut self, g: &Glyph<'_>, t: Affine, paint: &Paint<'_>, synthetic_bold: bool) {
        if self.page.glyphs.len() >= MAX_GLYPHS {
            return;
        }
        let text = match glyph_text(g) {
            Some(t) => t,
            None => {
                // No ToUnicode and no glyph name: look the glyph up in the font's own cmap.
                let Glyph::Outline(o) = g else {
                    self.page.unreadable += 1;
                    return;
                };
                let map = self
                    .cmaps
                    .entry(o.font_cache_key())
                    .or_insert_with(|| Arc::new(o.font_data().map(|d| reverse_cmap(d.data.as_ref().as_ref())).unwrap_or_default()))
                    .clone();
                match map.get(&o.glyph_id().to_u32()) {
                    Some(c) => c.to_string(),
                    None => {
                        self.page.unreadable += 1;
                        return;
                    }
                }
            }
        };
        let [a, b, c, d, _, _] = t.as_coeffs();
        let origin = t * Point::ORIGIN;
        let upright = a > 0.0 && b.abs() < 0.05 * a.abs();
        // Glyph space is 1000 units to the em.
        let size = finite(if upright { d.abs() * 1000.0 } else { (c * c + d * d).sqrt() * 1000.0 });
        if !(0.5..=2000.0).contains(&size) {
            return;
        }
        let advance = match g {
            Glyph::Outline(o) => o.advance_width().unwrap_or(500.0) as f64,
            Glyph::Type3(_) => 500.0,
        };
        let end = t * Point::new(advance, 0.0);
        let (x0, x1) = (finite(origin.x), finite(end.x.max(origin.x + 0.01)));
        let (key, fixed) = look_of(g);
        let look = match fixed {
            Some(l) => l,
            None => self.looks.entry(key).or_insert_with(|| compute_look(g)).clone(),
        };
        // A slanted em (sheared transform) reads as italic.
        let slanted = upright && c.abs() > 0.15 * d.abs();
        if text.chars().all(char::is_whitespace) {
            if let Some(prev) = self.page.glyphs.last_mut() {
                prev.space_after = true;
            }
            return;
        }
        self.page.glyphs.push(G {
            text,
            x0,
            x1,
            base: finite(origin.y),
            size,
            bold: look.bold || synthetic_bold,
            italic: look.italic || slanted,
            mono: look.mono,
            family: look.family,
            color: rgb_of(paint),
            space_after: false,
            upright,
            ocr: false,
        });
    }

    fn add_rules(&mut self, path: &BezPath, t: Affine, stroke_width: Option<f32>) {
        if self.page.rules.len() >= MAX_RULES {
            return;
        }
        let p = t * path.clone();
        match stroke_width {
            // Stroked: each straight segment that runs across or down.
            Some(_) => {
                let mut last = Point::ORIGIN;
                let mut start = Point::ORIGIN;
                for el in p.elements() {
                    match *el {
                        PathEl::MoveTo(q) => {
                            last = q;
                            start = q;
                        }
                        PathEl::LineTo(q) => {
                            self.push_segment(last, q);
                            last = q;
                        }
                        PathEl::ClosePath => {
                            self.push_segment(last, start);
                            last = start;
                        }
                        PathEl::QuadTo(_, q) | PathEl::CurveTo(_, _, q) => last = q,
                    }
                }
            }
            // Filled: a thin rectangle is a line; a box's edges rule a cell.
            None => {
                let bb = p.bounding_box();
                let (w, h) = (bb.width(), bb.height());
                if w > 3.0 && h <= 2.5 {
                    self.push_segment(Point::new(bb.x0, bb.center().y), Point::new(bb.x1, bb.center().y));
                } else if h > 3.0 && w <= 2.5 {
                    self.push_segment(Point::new(bb.center().x, bb.y0), Point::new(bb.center().x, bb.y1));
                }
            }
        }
    }

    fn push_segment(&mut self, a: Point, b: Point) {
        let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        if dy < 0.6 && dx > 3.0 {
            let y = finite((a.y + b.y) / 2.0);
            self.page.rules.push(Rule { x0: finite(a.x.min(b.x)), y0: y, x1: finite(a.x.max(b.x)), y1: y });
        } else if dx < 0.6 && dy > 3.0 {
            let x = finite((a.x + b.x) / 2.0);
            self.page.rules.push(Rule { x0: x, y0: finite(a.y.min(b.y)), x1: x, y1: finite(a.y.max(b.y)) });
        }
    }
}

impl<'a> Device<'a> for Collector {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, path: &BezPath, transform: Affine, _: &Paint<'a>, mode: &PathDrawMode) {
        match mode {
            PathDrawMode::Stroke(s) if s.line_width <= 3.0 => self.add_rules(path, transform, Some(s.line_width)),
            PathDrawMode::Fill(_) => self.add_rules(path, transform, None),
            PathDrawMode::Stroke(_) => {}
        }
    }
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph(&mut self, g: &Glyph<'a>, transform: Affine, glyph_transform: Affine, paint: &Paint<'a>, mode: &GlyphDrawMode) {
        let t = transform * glyph_transform;
        if let GlyphDrawMode::Stroke(_) = mode {
            // Fill-then-stroke draws a glyph twice to fake bold: mark the first, add nothing.
            let o = t * Point::ORIGIN;
            if let Some(prev) = self.page.glyphs.last_mut()
                && (prev.x0 - finite(o.x)).abs() < 0.05
                && (prev.base - finite(o.y)).abs() < 0.05
            {
                prev.bold = true;
                return;
            }
        }
        self.add_glyph(g, t, paint, false);
    }
    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        if !self.want_images || self.page.images.len() >= MAX_IMAGES {
            return;
        }
        let (w, h) = (image.width(), image.height());
        let rect = (transform * Rect::new(0.0, 0.0, w as f64, h as f64).to_path(0.1)).bounding_box();
        if rect.width() < MIN_IMAGE_PT && rect.height() < MIN_IMAGE_PT {
            return;
        }
        if (w as u64).saturating_mul(h as u64) > MAX_IMAGE_PIXELS {
            return;
        }
        let Image::Raster(r) = image else { return };
        let mut png = None;
        r.with_rgba(|data, alpha| png = encode_png(&data, alpha.as_ref()), None);
        if let Some(png) = png {
            self.page.images.push(PImg { png: Arc::new(png), rect });
        }
    }
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// An image the interpreter decoded, as PNG (with its alpha when it has one of the same size).
fn encode_png(data: &ImageData, alpha: Option<&hayro_interpret::LumaData>) -> Option<Vec<u8>> {
    let (w, h) = (data.width(), data.height());
    let px = (w as usize).checked_mul(h as usize)?;
    let alpha = alpha.filter(|a| a.width == w && a.height == h && a.data.len() == px);
    let mut rgba = Vec::with_capacity(px.checked_mul(4)?);
    match data {
        ImageData::Rgb(d) if d.data.len() == px * 3 => {
            for (i, c) in d.data.chunks_exact(3).enumerate() {
                rgba.extend_from_slice(c);
                rgba.push(alpha.and_then(|a| a.data.get(i).copied()).unwrap_or(255));
            }
        }
        ImageData::Luma(d) if d.data.len() == px => {
            for (i, &v) in d.data.iter().enumerate() {
                rgba.extend_from_slice(&[v, v, v, alpha.and_then(|a| a.data.get(i).copied()).unwrap_or(255)]);
            }
        }
        _ => return None,
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let opaque = alpha.is_none();
    let mut out = std::io::Cursor::new(Vec::new());
    if opaque {
        image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, image::ImageFormat::Png).ok()?;
    } else {
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    }
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::family_of;

    #[test]
    fn family_names() {
        assert_eq!(family_of("ABCDEF+TimesNewRomanPS-BoldMT").as_deref(), Some("Times New Roman"));
        assert_eq!(family_of("Helvetica-Bold").as_deref(), Some("Arial"));
        assert_eq!(family_of("SourceSansPro-Regular").as_deref(), Some("Source Sans Pro"));
        assert_eq!(family_of("Calibri,Bold").as_deref(), Some("Calibri"));
        assert_eq!(family_of("ArialMT").as_deref(), Some("Arial"));
    }
}
