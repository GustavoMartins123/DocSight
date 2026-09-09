#![no_main]

use docsight_core::DocumentSource;
use docsight_pdf::PdfDocument;
use libfuzzer_sys::fuzz_target;

mod support;

fuzz_target!(|data: &[u8]| {
    let bytes = support::pdf_with_content(data);
    let Ok(source) = DocumentSource::from_bytes(bytes) else {
        return;
    };
    let Ok(document) = PdfDocument::open(&source) else {
        return;
    };
    let _ = document.page(1);
    let _ = document.to_document();
});
