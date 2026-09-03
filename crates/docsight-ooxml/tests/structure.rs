use docsight_core::{BlockKind, DocsightError, DocumentSource};
use docsight_ooxml::parse_docx;
use std::io::{Cursor, Write};
use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn package(
    document: &str,
    styles: Option<&str>,
    numbering: Option<&str>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(
        b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
    )?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    if let Some(styles) = styles {
        writer.start_file("word/styles.xml", options)?;
        writer.write_all(styles.as_bytes())?;
    }
    if let Some(numbering) = numbering {
        writer.start_file("word/numbering.xml", options)?;
        writer.write_all(numbering.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

fn package_with_extra_part(name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p/></w:body></w:document>"#);
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file(name, options)?;
    writer.write_all(b"hostile")?;
    Ok(writer.finish()?.into_inner())
}

#[test]
fn parses_headings_lists_and_merged_tables() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p><w:pPr><w:pStyle w:val="CustomHeading"/></w:pPr><w:r><w:t>Scope</w:t></w:r></w:p>
        <w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>First</w:t></w:r></w:p>
        <w:tbl>
          <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>Combined</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Nested</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:tc></w:tr>
          <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr>
        </w:tbl><w:sectPr/>
        </w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles xmlns:w="{W_NS}">
        <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
        <w:style w:type="paragraph" w:styleId="CustomHeading"><w:name w:val="custom"/><w:basedOn w:val="Heading1"/></w:style>
        <w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:pPr><w:numPr><w:numId w:val="7"/></w:numPr></w:pPr></w:style>
        </w:styles>"#
    );
    let numbering = format!(
        r#"<w:numbering xmlns:w="{W_NS}">
        <w:abstractNum w:abstractNumId="4"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
        <w:num w:numId="7"><w:abstractNumId w:val="4"/></w:num>
        </w:numbering>"#
    );
    let source = DocumentSource::from_bytes(package(&document, Some(&styles), Some(&numbering))?)?;
    let parsed = parse_docx(&source)?;
    let headings: Vec<_> = parsed.headings().collect();
    assert_eq!(headings.len(), 1);
    assert_eq!(headings[0].1.level, 1);
    let list_items: Vec<_> = parsed.list_items().collect();
    assert_eq!(list_items.len(), 1);
    assert_eq!(list_items[0].1.format.as_deref(), Some("decimal"));
    let (_, table) = parsed.tables().next().ok_or("table missing")?;
    assert_eq!(table.rows, 2);
    assert_eq!(table.columns, 2);
    assert_eq!(table.cells.len(), 1);
    assert_eq!(table.cells[0].row_span, 2);
    assert_eq!(table.cells[0].column_span, 2);
    assert_eq!(table.cells[0].nested_tables().count(), 1);
    let (_, nested_table) = table.cells[0]
        .nested_tables()
        .next()
        .ok_or("nested table missing")?;
    assert_eq!(nested_table.cells[0].text, "Nested");
    assert_eq!(parsed.blocks[0].kind, BlockKind::Heading);
    assert_eq!(parsed.sections.len(), 1);
    Ok(())
}

#[test]
fn rejects_style_cycles() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:pStyle w:val="A"/></w:pPr><w:r><w:t>Cycle</w:t></w:r></w:p></w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles xmlns:w="{W_NS}"><w:style w:type="paragraph" w:styleId="A"><w:basedOn w:val="B"/></w:style><w:style w:type="paragraph" w:styleId="B"><w:basedOn w:val="A"/></w:style></w:styles>"#
    );
    let source = DocumentSource::from_bytes(package(&document, Some(&styles), None)?)?;
    let error = parse_docx(&source);
    assert!(error.is_err());
    Ok(())
}

#[test]
fn excludes_deleted_text_and_keeps_insertions() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:del><w:r><w:delText>old</w:delText></w:r></w:del><w:ins><w:r><w:t>new</w:t></w:r></w:ins></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, None, None)?)?;
    let parsed = parse_docx(&source)?;
    let text = parsed
        .paragraphs()
        .next()
        .ok_or("paragraph missing")?
        .1
        .text
        .as_str();
    assert_eq!(text, "new");
    Ok(())
}

#[test]
fn rejects_package_path_traversal() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(package_with_extra_part("../outside.xml")?)?;
    let error = parse_docx(&source);
    assert!(matches!(
        error,
        Err(DocsightError::MalformedDocument { .. })
    ));
    Ok(())
}

#[test]
fn rejects_excessive_compression_ratio() -> Result<(), Box<dyn std::error::Error>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(b"<w:document>")?;
    writer.write_all(&vec![b'a'; 2 * 1024 * 1024])?;
    writer.write_all(b"</w:document>")?;
    let source = DocumentSource::from_bytes(writer.finish()?.into_inner())?;
    let error = parse_docx(&source);
    assert!(matches!(error, Err(DocsightError::ResourceLimit { .. })));
    Ok(())
}
