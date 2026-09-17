use docsight_core::DocumentSource;
use docsight_ooxml::build_layout_plan;
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
fn section_spans_follow_page_break_segments() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p>
            <w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr>
            <w:r><w:t>alpha</w:t><w:br w:type="page"/><w:t>beta</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>gamma</w:t></w:r></w:p>
        <w:sectPr><w:pgSz w:w="15840" w:h="12240"/></w:sectPr>
    </w:body></w:document>"#);
    let source = DocumentSource::from_bytes(package(&document)?)?;
    let plan = build_layout_plan(&source)?;
    assert_eq!(plan.sections.len(), 2);
    assert_eq!((plan.sections[0].start_block, plan.sections[0].end_block), (0, 2));
    assert_eq!((plan.sections[1].start_block, plan.sections[1].end_block), (2, 3));
    assert_eq!(plan.sections[0].section.page_width_pt, Some(612.0));
    assert_eq!(plan.sections[1].section.page_width_pt, Some(792.0));
    Ok(())
}

#[test]
fn later_sections_inherit_omitted_geometry() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p>
            <w:pPr><w:sectPr><w:pgSz w:w="14400" w:h="18000"/><w:pgMar w:top="1000" w:right="1200" w:bottom="1400" w:left="1600"/></w:sectPr></w:pPr>
            <w:r><w:t>first</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>second</w:t></w:r></w:p>
        <w:sectPr><w:pgMar w:top="2000"/></w:sectPr>
    </w:body></w:document>"#);
    let source = DocumentSource::from_bytes(package(&document)?)?;
    let plan = build_layout_plan(&source)?;
    assert_eq!(plan.sections.len(), 2);
    let first = &plan.sections[0].section;
    let second = &plan.sections[1].section;
    assert_eq!(first.page_width_pt, Some(720.0));
    assert_eq!(first.page_height_pt, Some(900.0));
    assert_eq!(second.page_width_pt, first.page_width_pt);
    assert_eq!(second.page_height_pt, first.page_height_pt);
    assert_eq!(second.margin_left_pt, first.margin_left_pt);
    assert_eq!(second.margin_right_pt, first.margin_right_pt);
    assert_eq!(second.margin_bottom_pt, first.margin_bottom_pt);
    assert_eq!(second.margin_top_pt, Some(100.0));
    Ok(())
}
