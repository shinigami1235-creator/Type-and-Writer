//! Reading scanned PDF pages (Type and Writer): an [`Ocr`](wordcraft_formats::pdf::Ocr) engine
//! for the PDF importer, built on ocrs. Its two model files (`text-detection.rten`,
//! `text-recognition.rten`) live in an `ocr` folder beside the program, or in the folder named by
//! `TYPE_AND_WRITER_OCR_DIR`; without them scanned pages stay pictures.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use wordcraft_formats::pdf::{Ocr, OcrWord};

/// File names of the two models.
pub const DETECTION: &str = "text-detection.rten";
pub const RECOGNITION: &str = "text-recognition.rten";

/// Folders searched for the models, in order.
pub fn model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("TYPE_AND_WRITER_OCR_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("ocr"));
        // A Linux/macOS package keeps data apart from the program.
        dirs.push(dir.join("../share/type-and-writer/ocr"));
        dirs.push(dir.join("../Resources/ocr"));
    }
    dirs
}

fn find_models() -> Option<PathBuf> {
    model_dirs().into_iter().find(|d| d.join(DETECTION).is_file() && d.join(RECOGNITION).is_file())
}

/// The ocrs engine, loaded the first time a page needs it.
struct Engine {
    dir: PathBuf,
    engine: OnceLock<Option<Mutex<OcrEngine>>>,
}

fn load(dir: &Path) -> Option<OcrEngine> {
    let det = rten::Model::load_file(dir.join(DETECTION)).map_err(|e| log::warn!("OCR detection model: {e}")).ok()?;
    let rec = rten::Model::load_file(dir.join(RECOGNITION)).map_err(|e| log::warn!("OCR recognition model: {e}")).ok()?;
    OcrEngine::new(OcrEngineParams { detection_model: Some(det), recognition_model: Some(rec), ..Default::default() })
        .map_err(|e| log::warn!("OCR engine: {e}"))
        .ok()
}

/// Read the words of `img` with `engine`, boxes in its pixels.
pub fn read_words(engine: &OcrEngine, img: &image::RgbImage) -> Result<Vec<OcrWord>, String> {
    let src = ImageSource::from_bytes(img.as_raw(), img.dimensions()).map_err(|e| e.to_string())?;
    let input = engine.prepare_input(src).map_err(|e| e.to_string())?;
    let words = engine.detect_words(&input).map_err(|e| e.to_string())?;
    let lines = engine.find_text_lines(&input, &words);
    let texts = engine.recognize_text(&input, &lines).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (li, line) in texts.iter().flatten().enumerate() {
        for w in line.words() {
            let text: String = w.chars().iter().map(|c| c.char).collect();
            if text.trim().is_empty() {
                continue;
            }
            let r = w.bounding_rect();
            out.push(OcrWord { text, line: li as u32, x0: r.left() as f32, y0: r.top() as f32, x1: r.right() as f32, y1: r.bottom() as f32 });
        }
    }
    Ok(out)
}

impl Ocr for Engine {
    fn read(&self, page: &image::RgbaImage) -> Vec<OcrWord> {
        let Some(engine) = self.engine.get_or_init(|| load(&self.dir).map(Mutex::new)) else { return Vec::new() };
        let Ok(engine) = engine.lock() else { return Vec::new() };
        let rgb = image::DynamicImage::ImageRgba8(page.clone()).to_rgb8();
        read_words(&engine, &rgb).unwrap_or_else(|e| {
            log::warn!("OCR: {e}");
            Vec::new()
        })
    }
}

/// Install OCR for PDF opening when the models are found. Returns where they were found.
pub fn install() -> Option<PathBuf> {
    let dir = find_models()?;
    wordcraft_formats::pdf::set_ocr(Box::new(Engine { dir: dir.clone(), engine: OnceLock::new() }));
    Some(dir)
}
