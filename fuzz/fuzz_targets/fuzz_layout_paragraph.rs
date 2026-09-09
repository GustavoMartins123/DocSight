#![no_main]

use docsight_core::DocumentSource;
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use libfuzzer_sys::fuzz_target;

mod support;

fuzz_target!(|data: &[u8]| {
    let document_xml = support::paragraph_document(data);
    let Some(package) = support::docx_package(&document_xml, &[]) else {
        return;
    };
    let Ok(source) = DocumentSource::from_bytes(package) else {
        return;
    };
    let Ok(document) = parse_docx(&source) else {
        return;
    };
    let _ = layout_docx(document);
});
