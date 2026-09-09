#![no_main]

use docsight_core::DocumentSource;
use docsight_ooxml::parse_docx;
use libfuzzer_sys::fuzz_target;

mod support;

const BASE: &[u8] = include_bytes!("../../fixtures/validation/sample_headings.docx");

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = DocumentSource::from_bytes(support::bounded(data).to_vec()) {
        let _ = parse_docx(&source);
    }
    support::parse_docx_bytes(support::mutate_base(BASE, data));
});
