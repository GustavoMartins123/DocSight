#![no_main]

use libfuzzer_sys::fuzz_target;

mod support;

const RELATIONSHIPS: &[u8] = b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rIdExternal\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.invalid/never-fetch\" TargetMode=\"External\"/></Relationships>";

fuzz_target!(|data: &[u8]| {
    let raw_parts = [("word/_rels/document.xml.rels", support::bounded(data))];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &raw_parts) {
        support::parse_docx_bytes(package);
    }

    let mutated = support::mutate_base(RELATIONSHIPS, data);
    let mutated_parts = [("word/_rels/document.xml.rels", mutated.as_slice())];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &mutated_parts) {
        support::parse_docx_bytes(package);
    }
});
