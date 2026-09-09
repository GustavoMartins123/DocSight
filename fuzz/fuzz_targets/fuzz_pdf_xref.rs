#![no_main]

use docsight_core::DocumentSource;
use docsight_pdf::PdfDocument;
use libfuzzer_sys::fuzz_target;

mod support;

const BASE: &[u8] = include_bytes!("../../fixtures/validation/sample_semantic.pdf");

fuzz_target!(|data: &[u8]| {
    let bytes = support::mutate_tail(BASE, data);
    let Ok(source) = DocumentSource::from_bytes(bytes) else {
        return;
    };
    if let Ok(document) = PdfDocument::open(&source) {
        let _ = document.info();
        let _ = document.to_document();
    }
});
