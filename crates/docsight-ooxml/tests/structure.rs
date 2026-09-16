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

fn stored_package(document: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    Ok(writer.finish()?.into_inner())
}

fn package_with_entry_count(entries: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body/></w:document>"#);
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    for index in 2..entries {
        writer.start_file(format!("custom/item-{index}.bin"), options)?;
    }
    Ok(writer.finish()?.into_inner())
}

fn resource_name(error: &DocsightError) -> Option<&str> {
    match error {
        DocsightError::ResourceLimit { resource, .. } => Some(resource),
        _ => None,
    }
}

fn parse_error(
    source: &DocumentSource,
    message: &'static str,
) -> Result<DocsightError, Box<dyn std::error::Error>> {
    match parse_docx(source) {
        Ok(_) => Err(message.into()),
        Err(error) => Ok(error),
    }
}

#[test]
fn rejects_dtd_and_entity_declarations() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<!DOCTYPE w:document [<!ENTITY external SYSTEM "file:///never-read">]><w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>&external;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(stored_package(&document)?)?;
    let error = parse_error(&source, "DTD must be rejected")?;
    assert!(matches!(error, DocsightError::MalformedDocument { .. }));
    assert!(error.to_string().contains("DTD or entity declarations"));
    Ok(())
}

#[test]
fn rejects_excessive_xml_depth() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>"#);
    document.push_str(&"<w:sdt>".repeat(63));
    document.push_str(&"</w:sdt>".repeat(63));
    document.push_str("</w:body></w:document>");
    let source = DocumentSource::from_bytes(stored_package(&document)?)?;
    let error = parse_error(&source, "deep XML must be rejected")?;
    assert_eq!(resource_name(&error), Some("XML element depth"));
    Ok(())
}

#[test]
fn rejects_excessive_xml_node_count() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>"#);
    document.push_str(&"<w:r/>".repeat(65_535));
    document.push_str("</w:body></w:document>");
    let source = DocumentSource::from_bytes(stored_package(&document)?)?;
    let error = parse_error(&source, "wide XML must be rejected")?;
    assert_eq!(resource_name(&error), Some("XML node count"));
    Ok(())
}

#[test]
fn accepts_xml_resource_boundaries() -> Result<(), Box<dyn std::error::Error>> {
    let mut depth_document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>"#);
    depth_document.push_str(&"<w:sdt>".repeat(62));
    depth_document.push_str(&"</w:sdt>".repeat(62));
    depth_document.push_str("</w:body></w:document>");
    let depth_source = DocumentSource::from_bytes(stored_package(&depth_document)?)?;
    parse_docx(&depth_source)?;

    let text = "x".repeat(1024 * 1024);
    let token_document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    );
    let token_source = DocumentSource::from_bytes(stored_package(&token_document)?)?;
    parse_docx(&token_source)?;

    let mut node_document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>"#);
    node_document.push_str(&"<!--x-->".repeat(65_533));
    node_document.push_str("</w:body></w:document>");
    let node_source = DocumentSource::from_bytes(stored_package(&node_document)?)?;
    parse_docx(&node_source)?;
    Ok(())
}

#[test]
fn rejects_excessive_xml_token_size() -> Result<(), Box<dyn std::error::Error>> {
    let text = "x".repeat(1024 * 1024 + 1);
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(stored_package(&document)?)?;
    let error = parse_error(&source, "large XML token must be rejected")?;
    assert_eq!(resource_name(&error), Some("XML token bytes"));
    Ok(())
}

#[test]
fn preserves_macro_and_ole_parts_as_inert_resources() -> Result<(), Box<dyn std::error::Error>> {
    for (path, code) in [
        ("word/vbaProject.bin", "DOCX_ACTIVE_CONTENT_INERT"),
        (
            "word/embeddings/oleObject1.bin",
            "DOCX_EMBEDDED_OBJECT_INERT",
        ),
    ] {
        let source = DocumentSource::from_bytes(package_with_extra_part(path)?)?;
        let document = parse_docx(&source)?;
        let resource = document
            .resources
            .iter()
            .find(|resource| resource.target == path)
            .ok_or("inert resource missing")?;
        assert_eq!(resource.kind, docsight_core::ResourceKind::EmbeddedObject);
        assert_eq!(resource.content_sha256.as_deref().map(str::len), Some(64));
        let warning = document
            .warnings
            .iter()
            .find(|warning| warning.code == code)
            .ok_or("inert-content warning missing")?;
        assert_eq!(warning.object.as_ref(), Some(&resource.id));
        assert!(warning.effect.contains("not executed"));
    }
    Ok(())
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

#[test]
fn enforces_package_entry_limit_at_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let accepted = DocumentSource::from_bytes(package_with_entry_count(2_048)?)?;
    parse_docx(&accepted)?;

    let rejected = DocumentSource::from_bytes(package_with_entry_count(2_049)?)?;
    let error = parse_error(&rejected, "package entry limit must be enforced")?;
    assert_eq!(resource_name(&error), Some("package entry count"));
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
    writer.start_file("word/media/diagram.png", options)?;
    writer.write_all(b"fixture image bytes")?;
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
      <Relationship Id="rIdLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://docsight.dev" TargetMode="External"/>
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
    assert_eq!(doc.resources[0].name, "rIdImg");
    assert_eq!(doc.resources[0].target, "word/media/diagram.png");
    assert_eq!(doc.resources[0].mime_type.as_deref(), Some("image/png"));
    assert_eq!(
        doc.resources[0].content_sha256.as_ref().map(String::len),
        Some(64)
    );

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

#[test]
fn preserves_unknown_body_elements_as_opaque_blocks() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p><w:r><w:t>Before</w:t></w:r></w:p>
        <w:bgPict><w:shapeId w:val="7"/></w:bgPict>
        <w:p><w:r><w:t>After</w:t></w:r></w:p>
        <w:sectPr/>
        </w:body></w:document>"#
    );
    let bytes = package(&document, None, None)?;
    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;

    let unknown: Vec<_> = doc
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Unknown)
        .collect();
    assert_eq!(unknown.len(), 1);
    assert!(unknown[0].id.as_str().starts_with("unk_"));
    let docsight_core::BlockContent::Unknown(block) = &unknown[0].content else {
        unreachable!("expected unknown content");
    };
    assert_eq!(block.raw_tag, "bgPict");
    assert_eq!(block.details.as_deref(), Some("shapeId"));
    let diagnostic = doc
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_BODY_ELEMENT_UNSUPPORTED")
        .ok_or("missing opaque element diagnostic")?;
    assert_eq!(diagnostic.object.as_ref(), Some(&unknown[0].id));

    Ok(())
}

#[test]
fn warns_when_section_is_defaulted_and_run_content_is_uninterpreted()
-> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p><w:r><w:t>Body</w:t></w:r><w:r><w:object w:dxaOrig="1"/></w:r></w:p>
        </w:body></w:document>"#
    );
    let bytes = package(&document, None, None)?;
    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;

    assert!(
        doc.warnings
            .iter()
            .any(|warning| warning.code == "DOCX_SECTION_DEFAULTED")
    );
    let run_warning = doc
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_RUN_ELEMENT_UNSUPPORTED")
        .ok_or("missing run element diagnostic")?;
    assert!(run_warning.message.contains("r/object"));

    Ok(())
}

#[test]
fn reads_table_grid_widths_and_header_rows() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:tbl>
          <w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="1440"/></w:tblGrid>
          <w:tr><w:tc><w:tcPr><w:tblHeader/></w:tcPr><w:p><w:r><w:t>Head</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>H2</w:t></w:r></w:p></w:tc></w:tr>
          <w:tr><w:tc><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>
        <w:sectPr/>
        </w:body></w:document>"#
    );
    let bytes = package(&document, None, None)?;
    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;

    let (_, table) = doc.tables().next().ok_or("missing table")?;
    assert_eq!(table.header_rows, 1);
    let widths = table.column_widths_pt.as_ref().ok_or("missing widths")?;
    assert_eq!(widths.len(), 2);
    assert!((widths[0] - 144.0).abs() < 0.01, "{widths:?}");
    assert!((widths[1] - 72.0).abs() < 0.01, "{widths:?}");

    Ok(())
}

#[test]
fn header_without_reference_is_not_assigned() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body>
        <w:p><w:r><w:t>Body</w:t></w:r></w:p>
        <w:sectPr/>
        </w:body></w:document>"#
    );
    let header = format!(
        r#"<w:hdr xmlns:w="{W_NS}"><w:p><w:r><w:t>Orphan header</w:t></w:r></w:p></w:hdr>"#
    );
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/header1.xml", options)?;
    writer.write_all(header.as_bytes())?;
    let bytes = writer.finish()?.into_inner();

    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;
    assert_eq!(doc.sections[0].header_text, None);

    Ok(())
}

#[test]
fn resolves_package_absolute_image_targets() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><w:body>
        <w:p><w:r><w:drawing><a:graphic><a:graphicData><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></w:drawing></w:r></w:p>
        </w:body></w:document>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="/media/image.png"/>
    </Relationships>"#;
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(rels.as_bytes())?;
    writer.start_file("media/image.png", options)?;
    writer.write_all(b"root image bytes")?;
    let bytes = writer.finish()?.into_inner();

    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;
    assert_eq!(doc.figures().count(), 1);
    assert_eq!(doc.resources.len(), 1);
    assert_eq!(doc.resources[0].target, "media/image.png");
    assert!(
        !doc.warnings
            .iter()
            .any(|warning| warning.code == "DOCX_IMAGE_UNRESOLVED")
    );
    Ok(())
}

#[test]
fn preserves_document_with_unresolved_image_as_warning() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><w:body>
        <w:p><w:r><w:t>Text survives</w:t></w:r></w:p>
        <w:p><w:r><w:drawing><a:graphic><a:graphicData><a:blip r:embed="rIdMissing"/></a:graphicData></a:graphic></w:drawing></w:r></w:p>
        </w:body></w:document>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rIdMissing" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/missing.png"/>
    </Relationships>"#;
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(rels.as_bytes())?;
    let bytes = writer.finish()?.into_inner();

    let source = DocumentSource::from_bytes(bytes)?;
    let doc = parse_docx(&source)?;
    assert!(
        doc.paragraphs()
            .any(|(_, block)| block.text == "Text survives")
    );
    assert_eq!(doc.figures().count(), 1);
    assert!(
        doc.warnings
            .iter()
            .any(|warning| warning.code == "DOCX_IMAGE_UNRESOLVED")
    );
    Ok(())
}

fn package_with_properties(
    core: Option<&str>,
    app: Option<&str>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let document = format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p/></w:body></w:document>"#);
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    if let Some(core) = core {
        writer.start_file("docProps/core.xml", options)?;
        writer.write_all(core.as_bytes())?;
    }
    if let Some(app) = app {
        writer.start_file("docProps/app.xml", options)?;
        writer.write_all(app.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

#[test]
fn reads_package_core_and_extended_properties() -> Result<(), Box<dyn std::error::Error>> {
    let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Quarterly Report</dc:title><dc:creator>Ada Lovelace</dc:creator><dc:subject>Revenue</dc:subject></cp:coreProperties>"#;
    let app = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Microsoft Office Word</Application></Properties>"#;
    let source = DocumentSource::from_bytes(package_with_properties(Some(core), Some(app))?)?;
    let document = parse_docx(&source)?;

    assert_eq!(document.metadata.title.as_deref(), Some("Quarterly Report"));
    assert_eq!(document.metadata.author.as_deref(), Some("Ada Lovelace"));
    assert_eq!(document.metadata.subject.as_deref(), Some("Revenue"));
    assert_eq!(
        document.metadata.producer.as_deref(),
        Some("Microsoft Office Word")
    );
    Ok(())
}

#[test]
fn reports_absent_package_properties_as_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(package_with_properties(None, None)?)?;
    let document = parse_docx(&source)?;

    assert_eq!(document.metadata.title, None);
    assert_eq!(document.metadata.author, None);
    assert_eq!(document.metadata.subject, None);
    assert_eq!(document.metadata.producer, None);
    Ok(())
}

#[test]
fn ignores_empty_package_property_elements() -> Result<(), Box<dyn std::error::Error>> {
    let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>   </dc:title><dc:creator></dc:creator></cp:coreProperties>"#;
    let source = DocumentSource::from_bytes(package_with_properties(Some(core), None)?)?;
    let document = parse_docx(&source)?;

    assert_eq!(document.metadata.title, None);
    assert_eq!(document.metadata.author, None);
    Ok(())
}

#[test]
fn reads_direct_paragraph_alignment_spacing_and_indentation()
-> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:jc w:val="center"/><w:spacing w:before="240" w:after="120" w:line="360" w:lineRule="auto"/><w:ind w:left="720" w:right="360" w:firstLine="180"/></w:pPr><w:r><w:t>Centered</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, None, None)?)?;
    let parsed = parse_docx(&source)?;
    let format = parsed.blocks[0].format;

    assert_eq!(format.alignment, Some(docsight_core::TextAlignment::Center));
    assert_eq!(format.space_before_pt, Some(12.0));
    assert_eq!(format.space_after_pt, Some(6.0));
    assert_eq!(format.line_spacing, Some(1.5));
    assert_eq!(format.indent_left_pt, Some(36.0));
    assert_eq!(format.indent_right_pt, Some(18.0));
    assert_eq!(format.indent_first_line_pt, Some(9.0));
    Ok(())
}

#[test]
fn reads_hanging_indentation_as_a_negative_first_line_offset()
-> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:r><w:t>Hanging</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, None, None)?)?;
    let parsed = parse_docx(&source)?;

    assert_eq!(parsed.blocks[0].format.indent_first_line_pt, Some(-18.0));
    Ok(())
}

#[test]
fn paragraph_formatting_follows_the_style_cascade() -> Result<(), Box<dyn std::error::Error>> {
    let styles = format!(
        r#"<w:styles xmlns:w="{W_NS}"><w:style w:type="paragraph" w:styleId="Base"><w:name w:val="Base"/><w:pPr><w:jc w:val="right"/><w:spacing w:after="200"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Derived"><w:name w:val="Derived"/><w:basedOn w:val="Base"/><w:pPr><w:spacing w:before="100"/></w:pPr></w:style></w:styles>"#
    );
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:pStyle w:val="Derived"/></w:pPr><w:r><w:t>Inherited</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, Some(&styles), None)?)?;
    let parsed = parse_docx(&source)?;
    let format = parsed.blocks[0].format;

    assert_eq!(format.alignment, Some(docsight_core::TextAlignment::Right));
    assert_eq!(format.space_after_pt, Some(10.0));
    assert_eq!(format.space_before_pt, Some(5.0));
    Ok(())
}

#[test]
fn direct_paragraph_properties_win_over_the_style() -> Result<(), Box<dyn std::error::Error>> {
    let styles = format!(
        r#"<w:styles xmlns:w="{W_NS}"><w:style w:type="paragraph" w:styleId="Base"><w:name w:val="Base"/><w:pPr><w:jc w:val="right"/></w:pPr></w:style></w:styles>"#
    );
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:pStyle w:val="Base"/><w:jc w:val="center"/></w:pPr><w:r><w:t>Direct</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, Some(&styles), None)?)?;
    let parsed = parse_docx(&source)?;

    assert_eq!(
        parsed.blocks[0].format.alignment,
        Some(docsight_core::TextAlignment::Center)
    );
    Ok(())
}

#[test]
fn rejects_non_numeric_paragraph_measurements() -> Result<(), Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:spacing w:after="wide"/></w:pPr><w:r><w:t>Broken</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, None, None)?)?;

    assert!(matches!(
        parse_docx(&source),
        Err(DocsightError::MalformedDocument { .. })
    ));
    Ok(())
}

#[test]
fn reports_contextual_spacing_as_an_unapplied_declaration() -> Result<(), Box<dyn std::error::Error>>
{
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:contextualSpacing/></w:pPr><w:r><w:t>Tight</w:t></w:r></w:p></w:body></w:document>"#
    );
    let source = DocumentSource::from_bytes(package(&document, None, None)?)?;
    let parsed = parse_docx(&source)?;

    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_CONTEXTUAL_SPACING_IGNORED")
    );
    Ok(())
}

fn package_with_image(name: &str, bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="1270000" cy="635000"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#
    );
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/{name}"/></Relationships>"#
    );
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(rels.as_bytes())?;
    writer.start_file(format!("word/media/{name}"), options)?;
    writer.write_all(bytes)?;
    Ok(writer.finish()?.into_inner())
}

#[test]
fn a_png_figure_is_not_reported_as_a_placeholder() -> Result<(), Box<dyn std::error::Error>> {
    let png = docsight_core::encode_png(2, 2, &[10_u8; 12])?;
    let source = DocumentSource::from_bytes(package_with_image("logo.png", &png)?)?;
    let parsed = parse_docx(&source)?;

    assert_eq!(parsed.figures().count(), 1);
    assert!(
        !parsed
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
    );
    Ok(())
}

#[test]
fn a_non_png_figure_names_the_format_it_cannot_rasterize() -> Result<(), Box<dyn std::error::Error>>
{
    let jpeg = [
        0xFF_u8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46,
    ];
    let source = DocumentSource::from_bytes(package_with_image("photo.jpg", &jpeg)?)?;
    let parsed = parse_docx(&source)?;

    let warning = parsed
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
        .ok_or("placeholder diagnostic missing")?;
    assert!(
        warning.message.contains("jpeg"),
        "the diagnostic must name the format: {}",
        warning.message
    );
    Ok(())
}
