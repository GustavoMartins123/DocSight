#![no_main]

use libfuzzer_sys::fuzz_target;

mod support;

const TABLE: &[u8] = b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:tbl><w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>";

fuzz_target!(|data: &[u8]| {
    if let Some(package) = support::docx_package(support::bounded(data), &[]) {
        support::parse_docx_bytes(package);
    }
    if let Some(package) = support::docx_package(&support::mutate_base(TABLE, data), &[]) {
        support::parse_docx_bytes(package);
    }
});
