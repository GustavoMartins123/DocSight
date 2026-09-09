#![no_main]

use libfuzzer_sys::fuzz_target;

mod support;

const RELATIONSHIPS: &[u8] = b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
const STYLES: &[u8] = b"<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:style w:type=\"paragraph\" w:styleId=\"A\"><w:basedOn w:val=\"B\"/></w:style><w:style w:type=\"paragraph\" w:styleId=\"B\"><w:basedOn w:val=\"A\"/></w:style></w:styles>";

fuzz_target!(|data: &[u8]| {
    let raw_parts = [
        ("word/_rels/document.xml.rels", RELATIONSHIPS),
        ("word/styles.xml", support::bounded(data)),
    ];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &raw_parts) {
        support::parse_docx_bytes(package);
    }

    let mutated = support::mutate_base(STYLES, data);
    let mutated_parts = [
        ("word/_rels/document.xml.rels", RELATIONSHIPS),
        ("word/styles.xml", mutated.as_slice()),
    ];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &mutated_parts) {
        support::parse_docx_bytes(package);
    }
});
