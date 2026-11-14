//! Open a PDF with OCR on: `TYPE_AND_WRITER_OCR_DIR=models pdf_ocr in.pdf` prints Markdown.
fn main() {
    let Some(path) = std::env::args().nth(1) else { return };
    match wordcraft_ocr::install() {
        Some(d) => eprintln!("OCR models: {}", d.display()),
        None => eprintln!("no OCR models found"),
    }
    let Ok(bytes) = std::fs::read(&path) else { return };
    let t = std::time::Instant::now();
    match wordcraft_formats::pdf::import(&bytes) {
        Ok(doc) => {
            eprintln!("read in {:?}", t.elapsed());
            print!("{}", wordcraft_formats::markdown::export(&doc));
        }
        Err(e) => eprintln!("{e}"),
    }
}
