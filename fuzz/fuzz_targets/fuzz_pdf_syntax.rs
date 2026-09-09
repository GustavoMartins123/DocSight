#![no_main]

use docsight_core::DocumentSource;
use docsight_pdf::PdfDocument;
use libfuzzer_sys::fuzz_target;

mod support;

const BASE: &[u8] = include_bytes!("../../fixtures/validation/sample_semantic.pdf");

fn exercise(bytes: Vec<u8>) {
    let Ok(source) = DocumentSource::from_bytes(bytes) else {
        return;
    };
    let Ok(document) = PdfDocument::open(&source) else {
        return;
    };
    let _ = document.info();
    let _ = document.to_document();
}

fuzz_target!(|data: &[u8]| {
    let mut syntax = b"%PDF-1.7\n".to_vec();
    syntax.extend_from_slice(support::bounded(data));
    exercise(syntax);
    exercise(support::mutate_base(BASE, data));
});
