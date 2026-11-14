//! Opening a PDF (Type and Writer): read its pages back into an editable document, the way
//! Word's PDF Reflow does. The text keeps its fonts, sizes, colours, bold and italic; headings,
//! lists, columns, ruled tables and pictures come back as such; running headers, footers and page
//! numbers are left out. Scanned pages go through the OCR engine the app installs
//! ([`set_ocr`]), when it has one.
//!
//! [`page_pictures`] instead keeps each page's exact look, as a picture.

pub mod extract;
pub mod reflow;

use std::sync::{Arc, OnceLock};

use wordcraft_doc::{Document, SectionProps};

use crate::model::{FBlock, Flow, Inline, Meta, Para, make_img, to_doc_in};

/// Most pages read from one file.
pub const MAX_PAGES: usize = 2000;
/// Most pages run through OCR in one file (it takes a few seconds a page).
pub const MAX_OCR_PAGES: usize = 300;
/// Rendering scale for OCR: pixels per point (3 ≈ 216 dpi).
const OCR_SCALE: f32 = 3.0;
/// Rendering scale for page pictures (2 ≈ 144 dpi).
const PICTURE_SCALE: f32 = 2.0;

/// A word an OCR engine read, with its box in the page picture's pixels.
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    /// Which line of the page the engine put the word on.
    pub line: u32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Reads the words in a page picture.
pub trait Ocr: Send + Sync {
    fn read(&self, page: &image::RgbaImage) -> Vec<OcrWord>;
}

static OCR: OnceLock<Box<dyn Ocr>> = OnceLock::new();

/// Install the OCR engine scanned pages go through (once per process).
pub fn set_ocr(engine: Box<dyn Ocr>) {
    let _ = OCR.set(engine);
}

/// Whether an OCR engine is installed.
pub fn has_ocr() -> bool {
    OCR.get().is_some()
}

fn load(bytes: &[u8]) -> Result<hayro_syntax::Pdf, String> {
    use hayro_syntax::LoadPdfError;
    hayro_syntax::Pdf::new(bytes.to_vec()).map_err(|e| match e {
        LoadPdfError::Decryption(_) => "This PDF is password protected.".to_string(),
        LoadPdfError::Invalid => "This PDF could not be read.".to_string(),
    })
}

/// Render one page to RGBA at `scale` pixels per point.
fn render_page(page: &hayro_syntax::page::Page<'_>, scale: f32) -> Option<image::RgbaImage> {
    let (w, h) = page.render_dimensions();
    let (pw, ph) = ((w * scale).ceil(), (h * scale).ceil());
    if !(1.0..=16_000.0).contains(&pw) || !(1.0..=16_000.0).contains(&ph) || (pw * ph) > 120_000_000.0 {
        return None;
    }
    let cache = hayro::RenderCache::new();
    let settings =
        hayro::RenderSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() };
    let pix = hayro::render(page, &cache, &hayro_interpret::InterpreterSettings::default(), &settings);
    let (iw, ih) = (pix.width() as u32, pix.height() as u32);
    let data: Vec<u8> = pix.take_unpremultiplied().into_iter().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
    image::RgbaImage::from_raw(iw, ih, data)
}

/// OCR words as glyphs on the page (points), so the reflow reads them like drawn text. Each
/// line's words share one size and baseline (word boxes vary with ascenders and descenders), and
/// sizes near the page's usual size are set to it, so ordinary lines don't read as headings.
fn ocr_glyphs(words: Vec<OcrWord>, scale: f32) -> Vec<extract::G> {
    use std::collections::BTreeMap;
    let mut lines: BTreeMap<u32, Vec<OcrWord>> = BTreeMap::new();
    for w in words.into_iter().filter(|w| !w.text.trim().is_empty() && w.x1 > w.x0 && w.y1 > w.y0) {
        lines.entry(w.line).or_default().push(w);
    }
    let median = |mut v: Vec<f32>| -> f32 {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    // A word's box runs about 1.15 em from ascender to descender.
    let line_size: Vec<(u32, f32, f32)> = lines
        .iter()
        .map(|(k, ws)| {
            let h = median(ws.iter().map(|w| (w.y1 - w.y0) / scale).collect());
            let bottom = median(ws.iter().map(|w| w.y1 / scale).collect());
            (*k, h / 1.15, bottom)
        })
        .collect();
    let usual = median(line_size.iter().map(|l| l.1).collect());
    let mut out = Vec::new();
    for (k, size, bottom) in line_size {
        let size = if usual > 0.0 && (size / usual - 1.0).abs() < 0.2 { usual } else { size };
        let size = (size * 2.0).round().max(4.0) / 2.0;
        let base = bottom - 0.2 * size;
        for w in lines.remove(&k).unwrap_or_default() {
            out.push(extract::G {
                text: w.text.trim().to_string(),
                x0: w.x0 / scale,
                x1: w.x1 / scale,
                base,
                size,
                bold: false,
                italic: false,
                mono: false,
                family: None,
                color: wordcraft_doc::Rgb(0, 0, 0),
                space_after: true,
                upright: true,
                ocr: true,
            });
        }
    }
    out
}

/// Does this page need OCR: a scan (next to no text, a picture covering most of it), or text
/// drawn in fonts that don't say which letters they draw?
fn is_scan(p: &extract::PageData) -> bool {
    let text: usize = p.glyphs.iter().filter(|g| !g.is_space()).count();
    let area = (p.w * p.h).max(1.0);
    let scan = text < 8 && p.images.iter().any(|i| (i.rect.width() * i.rect.height()) as f32 > 0.5 * area);
    scan || p.unreadable > 40 && p.unreadable * 3 > text
}

/// The document's properties from its Info dictionary.
fn meta_of(pdf: &hayro_syntax::Pdf) -> Meta {
    let m = pdf.metadata();
    let s = |v: &Option<Vec<u8>>| v.as_deref().map(decode_text_string).unwrap_or_default();
    Meta { title: s(&m.title), author: s(&m.author), subject: s(&m.subject), keywords: s(&m.keywords), ..Default::default() }
}

/// A PDF text string: UTF-16BE with a byte order mark, else PDFDocEncoding (Latin-1 for the
/// characters that matter here).
fn decode_text_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units).trim().to_string();
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).trim().to_string();
    }
    b.iter().map(|&c| c as char).collect::<String>().trim().to_string()
}

/// Open a PDF as an editable document.
pub fn import(bytes: &[u8]) -> Result<Document, String> {
    let pdf = load(bytes)?;
    let mut pages = extract::read_pages(&pdf, MAX_PAGES, true);
    if pages.is_empty() {
        return Err("This PDF has no pages.".into());
    }
    let (w, h) = pages.first().map(|p| (p.w, p.h)).unwrap_or((612.0, 792.0));
    // Scanned pages: read their words with OCR when an engine is installed.
    if let Some(ocr) = OCR.get() {
        let mut done = 0;
        for (i, page) in pdf.pages().iter().enumerate() {
            let Some(p) = pages.get_mut(i) else { break };
            if done >= MAX_OCR_PAGES || !is_scan(p) {
                continue;
            }
            if let Some(img) = render_page(page, OCR_SCALE) {
                p.glyphs = ocr_glyphs(ocr.read(&img), OCR_SCALE);
                done += 1;
            }
        }
    }
    let scanned_without_ocr = !has_ocr() && pages.iter().any(is_scan);
    let r = reflow::reflow(pages);
    let mut blocks = r.blocks;
    if scanned_without_ocr {
        let mut p = Para::default();
        p.push_text(
            "Some pages are scans and came in without their text since the OCR models are missing. Reinstall Type and Writer to read them.",
            &Default::default(),
        );
        blocks.insert(0, FBlock::Para(p));
    }
    let clamp = |v: f32| if v.is_finite() { (v - 2.0).clamp(18.0, 144.0) } else { 72.0 };
    let section = SectionProps {
        page_w: w,
        page_h: h,
        landscape: w > h,
        margin_left: clamp(r.margins[0]),
        margin_right: clamp(r.margins[1]),
        margin_top: clamp(r.margins[2]),
        margin_bottom: clamp(r.margins[3]),
        ..Default::default()
    };
    let flow = Flow { blocks, meta: meta_of(&pdf) };
    let mut doc = to_doc_in(&flow, section);
    doc.ensure_nonempty();
    Ok(doc)
}

/// Every page as a PNG picture with its size in points, for keeping a PDF's exact look.
pub fn page_pictures(bytes: &[u8]) -> Result<Vec<(Arc<Vec<u8>>, f32, f32)>, String> {
    let pdf = load(bytes)?;
    let mut out = Vec::new();
    for page in pdf.pages().iter().take(MAX_PAGES) {
        let (w, h) = page.render_dimensions();
        let Some(img) = render_page(page, PICTURE_SCALE) else { continue };
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        out.push((Arc::new(png.into_inner()), w, h));
    }
    if out.is_empty() {
        return Err("No page of this PDF could be drawn.".into());
    }
    Ok(out)
}

/// Paragraphs holding each page of a PDF as a picture `width` points wide, a page break
/// between them.
pub fn picture_blocks(bytes: &[u8], width: f32) -> Result<Vec<FBlock>, String> {
    let pics = page_pictures(bytes)?;
    let mut blocks = Vec::with_capacity(pics.len());
    let n = pics.len();
    for (i, (png, w, h)) in pics.into_iter().enumerate() {
        let s = if w > 0.0 { width / w } else { 1.0 };
        let Some(img) = make_img(png, Some(w * s), Some(h * s), &format!("Page {}", i + 1)) else { continue };
        let mut p = Para::default();
        p.inlines.push(Inline::Image(img));
        p.spacing.before = Some(0.0);
        p.spacing.after = Some(0.0);
        p.page_break = i > 0 && i < n;
        blocks.push(FBlock::Para(p));
    }
    Ok(blocks)
}

/// Open a PDF keeping each page's exact look: one picture per page, on pages of the PDF's size
/// with no margins. Not editable as text.
pub fn import_pictures(bytes: &[u8]) -> Result<Document, String> {
    let pdf = load(bytes)?;
    let (w, h) = pdf.pages().iter().next().map(|p| p.render_dimensions()).unwrap_or((612.0, 792.0));
    // A sliver less than the page, so the picture never spills onto a page of its own.
    let blocks = picture_blocks(bytes, w - 1.0)?;
    let section = SectionProps {
        page_w: w,
        page_h: h,
        landscape: w > h,
        margin_left: 0.5,
        margin_right: 0.5,
        margin_top: 0.0,
        margin_bottom: 0.0,
        header: 0.0,
        footer: 0.0,
        ..Default::default()
    };
    let mut doc = to_doc_in(&Flow { blocks, meta: meta_of(&pdf) }, section);
    doc.ensure_nonempty();
    Ok(doc)
}
