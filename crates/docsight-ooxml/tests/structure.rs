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

fn package_full(
    document: &str,
    rels: Option<&str>,
    header: Option<&str>,
    footer: Option<&str>,
    footnotes: Option<&str>,
    endnotes: Option<&str>,
    comments: Option<&str>,
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
    if let Some(rels) = rels {
        writer.start_file("word/_rels/document.xml.rels", options)?;
        writer.write_all(rels.as_bytes())?;
    }
    if let Some(header) = header {
        writer.start_file("word/header1.xml", options)?;
        writer.write_all(header.as_bytes())?;
    }
    if let Some(footer) = footer {
        writer.start_file("word/footer1.xml", options)?;
        writer.write_all(footer.as_bytes())?;
    }
    if let Some(footnotes) = footnotes {
        writer.start_file("word/footnotes.xml", options)?;
        writer.write_all(footnotes.as_bytes())?;
    }
    if let Some(endnotes) = endnotes {
        writer.start_file("word/endnotes.xml", options)?;
        writer.write_all(endnotes.as_bytes())?;
    }
    if let Some(comments) = comments {
        writer.start_file("word/comments.xml", options)?;
        writer.write_all(comments.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

#[test]
fn parses_figures_headers_footers_notes_links_and_comments()
-> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
        <w:body>
          <w:p>
            <w:r><w:t>Check this link: </w:t></w:r>
            <w:hyperlink r:id="rIdLink"><w:r><w:t>Docsight Website</w:t></w:r></w:hyperlink>
          </w:p>
          <w:p>
            <w:r><w:t>Internal bookmark: </w:t></w:r>
            <w:hyperlink w:anchor="section2"><w:r><w:t>Jump to section 2</w:t></w:r></w:hyperlink>
          </w:p>
          <w:p>
            <w:r>
              <w:drawing>
                <wp:inline>
                  <wp:extent cx="1270000" cy="635000"/>
                  <wp:docPr id="1" name="Figure 1" descr="Architecture Diagram"/>
                  <a:graphic>
                    <a:graphicData>
                      <a:blip r:embed="rIdImg"/>
                    </a:graphicData>
                  </a:graphic>
                </wp:inline>
              </w:drawing>
            </w:r>
          </w:p>
          <w:p>
            <w:r><w:t>Normal text before edit.</w:t></w:r>
            <w:ins w:id="1" w:author="Author"><w:r><w:t> Inserted text.</w:t></w:r></w:ins>
            <w:del w:id="2" w:author="Author"><w:r><w:delText> Deleted text.</w:delText></w:r></w:del>
          </w:p>
          <w:sectPr>
            <w:headerReference r:id="rIdH" w:type="default"/>
            <w:footerReference r:id="rIdF" w:type="default"/>
            <w:pgSz w:w="12240" w:h="15840"/>
            <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
          </w:sectPr>
        </w:body></w:document>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rIdH" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
      <Relationship Id="rIdF" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
      <Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/diagram.png"/>
      <Relationship Id="rIdLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://docsight.dev"/>
    </Relationships>"#;
    let header = format!(
        r#"<w:hdr xmlns:w="{W_NS}"><w:p><w:r><w:t>Company Confidential</w:t></w:r></w:p></w:hdr>"#
    );
    let footer = format!(
        r#"<w:ftr xmlns:w="{W_NS}"><w:p><w:r><w:t>Page </w:t></w:r><w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:ftr>"#
    );
    let footnotes = format!(
        r#"<w:footnotes xmlns:w="{W_NS}">
          <w:footnote w:id="1"><w:p><w:r><w:t>First citation footnote</w:t></w:r></w:p></w:footnote>
        </w:footnotes>"#
    );
    let endnotes = format!(
        r#"<w:endnotes xmlns:w="{W_NS}">
          <w:endnote w:id="1"><w:p><w:r><w:t>Endnote bibliographic reference</w:t></w:r></w:p></w:endnote>
        </w:endnotes>"#
    );
    let comments = format!(
        r#"<w:comments xmlns:w="{W_NS}">
          <w:comment w:id="1" w:author="Editor" w:date="2026-09-03T10:00:00Z">
            <w:p><w:r><w:t>Please verify numbers</w:t></w:r></w:p>
          </w:comment>
        </w:comments>"#
    );

    let bytes = package_full(
        &document,
        Some(rels),
        Some(&header),
        Some(&footer),
        Some(&footnotes),
        Some(&endnotes),
        Some(&comments),
    )?;
    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;

    assert_eq!(doc.figures().count(), 1);
    let (_, fig) = doc.figures().next().ok_or("missing figure")?;
    assert_eq!(fig.alt_text.as_deref(), Some("Architecture Diagram"));
    assert_eq!(fig.width_pt, Some(100.0));
    assert_eq!(fig.height_pt, Some(50.0));
    assert_eq!(fig.resource_id.as_deref(), Some("rIdImg"));

    assert_eq!(doc.resources.len(), 1);
    assert_eq!(doc.resources[0].name, "media/diagram.png");
    assert_eq!(doc.resources[0].mime_type.as_deref(), Some("image/png"));

    assert_eq!(doc.links.len(), 2);
    assert_eq!(doc.links[0].text, "Docsight Website");
    assert_eq!(doc.links[0].target, "https://docsight.dev");
    assert!(doc.links[0].is_external);

    assert_eq!(doc.links[1].text, "Jump to section 2");
    assert_eq!(doc.links[1].target, "#section2");
    assert!(!doc.links[1].is_external);

    assert_eq!(doc.notes().count(), 2);
    assert_eq!(doc.comments.len(), 1);
    assert_eq!(doc.comments[0].author.as_deref(), Some("Editor"));
    assert_eq!(doc.comments[0].text, "Please verify numbers");

    assert_eq!(doc.tracked_changes.insertions, 1);
    assert_eq!(doc.tracked_changes.deletions, 1);

    assert_eq!(doc.sections.len(), 1);
    assert_eq!(
        doc.sections[0].header_text.as_deref(),
        Some("Company Confidential")
    );
    assert_eq!(doc.sections[0].footer_text.as_deref(), Some("Page [PAGE]"));

    Ok(())
}
