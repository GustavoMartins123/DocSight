use docsight_core::{DocumentSource, canonical_violations};
use docsight_ingest::ingest_docx;
use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn package(document: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    Ok(writer.finish()?.into_inner())
}

#[test]
fn ingests_multi_section_geometry_and_internal_page_breaks() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p>
            <w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:pPr>
            <w:r><w:t>alpha</w:t><w:br w:type="page"/><w:t>beta</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>gamma</w:t></w:r></w:p>
        <w:sectPr><w:pgSz w:w="15840" w:h="12240"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>
    </w:body></w:document>"#);
    let source = DocumentSource::from_bytes(package(&document)?)?;
    let laid = ingest_docx(&source)?;
    assert_eq!(laid.document.sections.len(), 2);
    assert_eq!(laid.document.sections[0].page_width_pt, Some(612.0));
    assert_eq!(laid.document.sections[1].page_width_pt, Some(792.0));
    assert_eq!(laid.document.blocks.len(), 3);
    assert_eq!(laid.document.blocks[0].page, Some(1));
    assert_eq!(laid.document.blocks[1].page, Some(2));
    assert_eq!(laid.document.blocks[2].page, Some(3));
    assert_eq!(laid.document.pages[0].width_pt, 612.0);
    assert_eq!(laid.document.pages[1].width_pt, 612.0);
    assert_eq!(laid.document.pages[2].width_pt, 792.0);
    assert!(!laid.document.warnings.iter().any(|warning| warning.code == "DOCX_SECTIONS_COLLAPSED"));
    assert!(laid.document.warnings.iter().any(|warning| warning.code == "DOCX_MULTI_SECTION_LAYOUT"));
    assert!(canonical_violations(&laid.document).is_empty());
    Ok(())
}
