//! Open a PDF the way Type and Writer does and print it as Markdown: `pdf_reflow in.pdf`.
fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: pdf_reflow in.pdf");
        return;
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            return;
        }
    };
    let t = std::time::Instant::now();
    match wordcraft_formats::pdf::import(&bytes) {
        Ok(doc) => {
            eprintln!("read in {:?}", t.elapsed());
            print!("{}", wordcraft_formats::markdown::export(&doc));
        }
        Err(e) => eprintln!("{path}: {e}"),
    }
}
