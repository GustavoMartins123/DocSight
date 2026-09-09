#![no_main]

use libfuzzer_sys::fuzz_target;

mod support;

const RELATIONSHIPS: &[u8] = b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdNumbering\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/></Relationships>";
const NUMBERING: &[u8] = b"<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl></w:abstractNum><w:num w:numId=\"7\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>";

fuzz_target!(|data: &[u8]| {
    let raw_parts = [
        ("word/_rels/document.xml.rels", RELATIONSHIPS),
        ("word/numbering.xml", support::bounded(data)),
    ];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &raw_parts) {
        support::parse_docx_bytes(package);
    }

    let mutated = support::mutate_base(NUMBERING, data);
    let mutated_parts = [
        ("word/_rels/document.xml.rels", RELATIONSHIPS),
        ("word/numbering.xml", mutated.as_slice()),
    ];
    if let Some(package) = support::docx_package(support::fixed_document_xml(), &mutated_parts) {
        support::parse_docx_bytes(package);
    }
});
