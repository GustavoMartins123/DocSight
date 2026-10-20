use docsight_core::{BlockContent, DocumentSource, canonical_violations};
use docsight_ingest::ingest_docx;
use docsight_render::{RenderRequest, RenderTarget, render_document};
use std::io::{Cursor, Write};
use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn package(
    body: &str,
    styles: &str,
    header: Option<&str>,
) -> Result<DocumentSource, Box<dyn std::error::Error>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        let document = format!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}</w:body></w:document>"#
        );
        for (name, bytes) in [
            ("[Content_Types].xml", "<Types/>"),
            ("word/document.xml", document.as_str()),
            ("word/styles.xml", styles),
        ] {
            archive.start_file(name, options)?;
            archive.write_all(bytes.as_bytes())?;
        }
        if let Some(header) = header {
            archive.start_file("word/header1.xml", options)?;
            archive.write_all(header.as_bytes())?;
            archive.start_file("word/_rels/document.xml.rels", options)?;
            archive.write_all(br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#)?;
        }
        archive.finish()?;
    }
    Ok(DocumentSource::from_bytes(cursor.into_inner())?)
}

const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#;
const SECTION: &str = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="360" w:footer="360"/></w:sectPr>"#;

fn add_part(
    source: &DocumentSource,
    name: &str,
    contents: &str,
) -> Result<DocumentSource, Box<dyn std::error::Error>> {
    let mut existing = zip::ZipArchive::new(Cursor::new(source.bytes()))?;
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut output);
        for index in 0..existing.len() {
            let mut entry = existing.by_index(index)?;
            writer.start_file(entry.name(), zip::write::SimpleFileOptions::default())?;
            std::io::copy(&mut entry, &mut writer)?;
        }
        writer.start_file(name, zip::write::SimpleFileOptions::default())?;
        writer.write_all(contents.as_bytes())?;
        writer.finish()?;
    }
    Ok(DocumentSource::from_bytes(output.into_inner())?)
}

#[test]
fn numbering_counts_lists_independently_and_renders_hanging_markers() -> TestResult {
    let paragraph = |id, text| {
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{id}"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let source = package(
        &format!(
            "{}{}{}{}{SECTION}",
            paragraph(1, "First"),
            paragraph(1, "Second"),
            paragraph(2, "Restart"),
            paragraph(1, "Third")
        ),
        STYLES,
        None,
    )?;
    let numbering = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="7"/></w:lvlOverride></w:num></w:numbering>"#;
    let source = add_part(&source, "word/numbering.xml", numbering)?;
    let laid = ingest_docx(&source)?;
    let markers = laid
        .document
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::ListItem(item) => item.marker.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(markers, vec!["1.", "2.", "7.", "3."]);
    for marker in ["1.", "2.", "7.", "3."] {
        let run = laid.pages[0]
            .runs
            .iter()
            .find(|run| run.text == marker)
            .ok_or("marker pixels missing")?;
        assert!((run.bbox.x0 - 90.0).abs() < 0.001);
    }
    for body in ["First", "Second", "Restart", "Third"] {
        let run = laid.pages[0]
            .runs
            .iter()
            .find(|run| run.text == body)
            .ok_or("list body missing")?;
        assert!((run.bbox.x0 - 108.0).abs() < 0.001);
    }
    Ok(())
}

#[test]
fn runs_use_real_metrics_faces_colors_and_antialiased_pixels() -> TestResult {
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:b/><w:i/><w:sz w:val="40"/><w:color w:val="163C7A"/></w:rPr><w:t>Café Office Wi</w:t></w:r></w:p>{SECTION}"#
    );
    let source = package(&body, STYLES, None)?;
    let laid = ingest_docx(&source)?;
    assert_eq!(laid.document.version.schema_version, "1.5");
    let run = laid.pages[0].runs.first().ok_or("missing glyph run")?;
    assert_eq!(run.font_family, "DejaVu Sans");
    assert_eq!(run.font_size, 20.0);
    assert!(run.bold && run.italic);
    assert_eq!(run.color_argb, 0xff163c7a);
    let expected = laid
        .fonts
        .resolve(&run.font_family, true, true)?
        .shape(&run.text, 20.0)?;
    assert!((run.bbox.width() - expected.width).abs() < 0.001);
    assert!(laid.document.resources.iter().any(|resource| resource.kind
        == docsight_core::ResourceKind::Font
        && resource.content_sha256.is_some()));
    let rendered = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 144,
        },
    )?;
    assert!(
        rendered
            .pixels()
            .chunks_exact(3)
            .any(|pixel| pixel[0] < pixel[2] && pixel[2] < 255)
    );
    assert_eq!(
        rendered.png(),
        render_document(
            &source,
            &RenderRequest {
                target: RenderTarget::Page { page: 1 },
                dpi: 144
            }
        )?
        .png()
    );
    Ok(())
}

#[test]
fn table_rows_keep_text_cell_pages_padding_and_fills() -> TestResult {
    let rows=(0..90).map(|row|format!(r#"<w:tr><w:tc><w:tcPr><w:shd w:fill="EEF1FF"/><w:tcMar><w:top w:w="80"/><w:bottom w:w="80"/></w:tcMar></w:tcPr><w:p><w:r><w:t>Row {row:03}</w:t></w:r></w:p></w:tc></w:tr>"#)).collect::<String>();
    let source = package(
        &format!(r#"<w:tbl><w:tblGrid><w:gridCol w:w="9360"/></w:tblGrid>{rows}</w:tbl>{SECTION}"#),
        STYLES,
        None,
    )?;
    let laid = ingest_docx(&source)?;
    assert!(laid.pages.len() > 1);
    assert!(canonical_violations(&laid.document).is_empty());
    let block = &laid.document.blocks[0];
    let BlockContent::Table(table) = &block.content else {
        return Err("expected table".into());
    };
    assert_eq!(table.cells.len(), 90);
    assert!(!block.continuations.is_empty());
    assert!(!laid.document.warnings.iter().any(|warning| matches!(
        warning.code.as_str(),
        "DOCX_BLOCK_TALLER_THAN_PAGE" | "DOCX_PAGINATION_BLOCK_GRANULAR"
    )));
    for cell in &table.cells {
        let page = cell.page.ok_or("missing cell page")?;
        let bbox = cell.bbox.ok_or("missing cell box")?;
        assert!(bbox.y0 >= 72.0 && bbox.y1 <= 720.001);
        assert_eq!(
            laid.document
                .resolve_object(cell.id.as_str())
                .ok_or("cell object")?
                .page(),
            Some(page)
        );
        assert_eq!(
            cell.text_on_page(page, block.page)?,
            Some(cell.text.clone())
        );
    }
    for page in &laid.pages {
        assert!(!page.fills.is_empty());
    }
    let cell = table.cells.last().ok_or("last cell")?;
    let crop = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Object {
                id: cell.id.to_string(),
            },
            dpi: 72,
        },
    )?;
    assert_eq!(crop.metadata.page, cell.page.ok_or("page")?);
    assert!(
        crop.pixels()
            .chunks_exact(3)
            .any(|pixel| pixel == [238, 241, 255])
    );
    Ok(())
}

#[test]
fn contextual_spacing_is_applied_only_between_equal_styles() -> TestResult {
    let style = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Tight"><w:pPr><w:contextualSpacing/><w:spacing w:before="200" w:after="200"/></w:pPr><w:rPr><w:rFonts w:ascii="Liberation Sans"/></w:rPr></w:style></w:styles>"#;
    let paragraph = |text| {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Tight"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let source = package(
        &format!("{}{}{SECTION}", paragraph("One"), paragraph("Two")),
        style,
        None,
    )?;
    let laid = ingest_docx(&source)?;
    let runs = &laid.pages[0].runs;
    assert_eq!(runs.len(), 2);
    assert!((runs[1].bbox.y0 - runs[0].bbox.y1).abs() < 0.001);
    assert_eq!(
        laid.document.blocks[0].format.contextual_spacing,
        Some(true)
    );
    Ok(())
}

#[test]
fn header_tables_keep_format_fields_and_reserve_body_space() -> TestResult {
    let header = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tbl><w:tblGrid><w:gridCol w:w="9360"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t>Pages </w:t></w:r><w:fldSimple w:instr="NUMPAGES"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:tc></w:tr></w:tbl></w:hdr>"#;
    let paragraphs = (0..100)
        .map(|index| format!(r#"<w:p><w:r><w:t>Body {index}</w:t></w:r></w:p>"#))
        .collect::<String>();
    let section = r#"<w:sectPr><w:headerReference w:type="default" r:id="rHeader"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="400" w:right="1440" w:bottom="1440" w:left="1440" w:header="200" w:footer="360"/></w:sectPr>"#;
    let source = package(&format!("{paragraphs}{section}"), STYLES, Some(header))?;
    let laid = ingest_docx(&source)?;
    assert!(laid.pages.len() > 1);
    for (page, visual) in laid.document.pages.iter().zip(&laid.pages) {
        let overlay = page.overlays.first().ok_or("missing header")?;
        assert_eq!(overlay.text, format!("Pages {}", laid.pages.len()));
        let header_box = overlay.bbox.ok_or("header box")?;
        for id in &page.block_ids {
            let block = laid.document.find_block(id.as_str()).ok_or("body block")?;
            assert!(block.bbox.ok_or("body box")?.y0 >= header_box.y1 - 0.001);
        }
        assert!(
            visual
                .runs
                .iter()
                .any(|run| run.bold && run.font_size == 24.0 && run.text == "Pages ")
        );
    }
    Ok(())
}

#[test]
fn columns_and_gutters_use_page_local_geometry_without_losing_text() -> TestResult {
    let text = (0..140)
        .map(|index| format!("Line {index}\n"))
        .collect::<String>();
    let xml_text = text.replace('\n', "</w:t><w:br/><w:t>");
    let section = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:gutter="240"/><w:cols w:num="2" w:space="360"/></w:sectPr>"#;
    let source = package(
        &format!("<w:p><w:r><w:t>{xml_text}</w:t></w:r></w:p>{section}"),
        STYLES,
        None,
    )?;
    let laid = ingest_docx(&source)?;
    assert!(canonical_violations(&laid.document).is_empty());
    assert!(
        laid.pages[0]
            .runs
            .iter()
            .any(|run| (run.bbox.x0 - 84.0).abs() < 0.001)
    );
    assert!(
        laid.pages[0]
            .runs
            .iter()
            .any(|run| (run.bbox.x0 - 321.0).abs() < 0.001)
    );
    let block = &laid.document.blocks[0];
    let reconstructed = laid
        .document
        .pages
        .iter()
        .map(|page| block.text_on_page(page.number))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<String>();
    assert_eq!(reconstructed, text);
    Ok(())
}

#[test]
fn source_font_names_are_normalized_without_loading_external_fonts() -> TestResult {
    let directory = tempfile::tempdir()?;
    let source = package(
        &format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="DocSight Missing Face"/><w:b/></w:rPr><w:t>Unavailable</w:t></w:r></w:p>{SECTION}"#
        ),
        STYLES,
        None,
    )?;
    let path = directory.path().join("missing-font.docx");
    std::fs::write(&path, source.bytes())?;
    let png = directory.path().join("page.png");
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .env("DOCSIGHT_FONT_DIR", directory.path())
        .args(["--agent", "render"])
        .arg(&path)
        .args(["--page", "1", "--out"])
        .arg(&png)
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty() && png.is_file());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        result["warnings"]
            .as_array()
            .ok_or("warnings")?
            .iter()
            .any(|warning| warning["code"] == "DOCX_FONT_NORMALIZED")
    );
    let laid = ingest_docx(&source)?;
    assert_eq!(
        laid.document.blocks[0].inline_runs[0]
            .format
            .font_family
            .as_deref(),
        Some("DocSight Missing Face")
    );
    assert!(
        laid.pages[0]
            .runs
            .iter()
            .all(|run| run.font_family == "DejaVu Sans")
    );
    Ok(())
}

#[test]
fn oversized_cells_preserve_child_geometry_and_fragment_text() -> TestResult {
    let text = (0..120)
        .map(|index| format!("line-{index:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let runs = text
        .split('\n')
        .map(|line| format!("<w:r><w:t>{line}</w:t><w:br/></w:r>"))
        .collect::<String>();
    let body = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="9360"/></w:tblGrid><w:tr><w:tc><w:p>{runs}</w:p></w:tc></w:tr></w:tbl>{SECTION}"#
    );
    let source = package(&body, STYLES, None)?;
    let laid = ingest_docx(&source)?;
    let table_block = &laid.document.blocks[0];
    let BlockContent::Table(table) = &table_block.content else {
        return Err("table".into());
    };
    let cell = &table.cells[0];
    let child = &cell.blocks[0];
    assert!(laid.pages.len() > 1);
    assert_eq!(child.page, cell.page);
    assert!(child.bbox.is_some());
    assert!(!child.continuations.is_empty());
    let object = laid
        .document
        .resolve_object(child.id.as_str())
        .ok_or("child object")?;
    assert_eq!(object.fragments().len(), laid.pages.len());
    let mut cell_text = String::new();
    let mut child_text = String::new();
    for page in &laid.pages {
        cell_text.push_str(
            &cell
                .text_on_page(page.number, table_block.page)?
                .ok_or("cell fragment")?,
        );
        child_text.push_str(&child.text_on_page(page.number)?.ok_or("child fragment")?);
        let bbox = object
            .bbox_on_page(page.number)
            .ok_or("child page geometry")?;
        assert!(bbox.y0 >= 72.0 && bbox.y1 <= 720.001);
        let query = docsight_render::HitQuery::Point(bbox.x0 + 1.0, bbox.y0 + 1.0);
        let hit = docsight_render::hit_test(&laid.document, page.number, &query)?;
        assert!(hit.targets.iter().any(|target| target.cell.is_some()));
    }
    assert_eq!(cell_text, cell.text);
    assert_eq!(child_text, child.text());
    Ok(())
}

#[test]
fn nested_tables_retain_source_order_geometry_and_rendered_text() -> TestResult {
    let body = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="9360"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>nested</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{SECTION}"#
    );
    let source = package(&body, STYLES, None)?;
    let laid = ingest_docx(&source)?;
    let BlockContent::Table(table) = &laid.document.blocks[0].content else {
        return Err("table".into());
    };
    let cell = &table.cells[0];
    assert_eq!(cell.text, "before\nnested\nafter");
    assert_eq!(cell.blocks.len(), 3);
    let mut previous_bottom = 0.0;
    for child in &cell.blocks {
        assert_eq!(child.page, Some(1));
        let bbox = child.bbox.ok_or("child geometry")?;
        assert!(bbox.y0 >= previous_bottom);
        previous_bottom = bbox.y1;
    }
    let BlockContent::Table(nested) = &cell.blocks[1].content else {
        return Err("nested table".into());
    };
    let nested_cell = &nested.cells[0];
    assert_eq!(nested_cell.page, Some(1));
    assert!(nested_cell.bbox.is_some());
    assert!(nested_cell.blocks[0].bbox.is_some());
    assert_eq!(
        laid.pages[0]
            .runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>(),
        ["before", "nested", "after"]
    );
    assert!(canonical_violations(&laid.document).is_empty());
    Ok(())
}

#[test]
fn inherited_underlines_can_be_disabled_and_mixed_runs_share_a_baseline() -> TestResult {
    let styles = STYLES.replace("</w:styles>", r#"<w:style w:type="paragraph" w:styleId="Underlined"><w:rPr><w:u w:val="single"/><w:color w:val="2468AC"/></w:rPr></w:style></w:styles>"#);
    let body = format!(
        r#"<w:p><w:pPr><w:pStyle w:val="Underlined"/></w:pPr><w:r><w:rPr><w:sz w:val="40"/></w:rPr><w:t>Large</w:t></w:r><w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>Small</w:t></w:r></w:p>{SECTION}"#
    );
    let source = package(&body, &styles, None)?;
    let laid = ingest_docx(&source)?;
    let runs = &laid.pages[0].runs;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].baseline, runs[1].baseline);
    assert!(runs[0].underline.is_some());
    assert!(runs[1].underline.is_none());
    assert_eq!(runs[0].color_argb, 0xff2468ac);
    assert_eq!(runs[1].color_argb, 0xff2468ac);
    let trace = docsight_render::trace::record_trace(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    assert!(
        trace
            .manifest
            .display_list
            .operations
            .iter()
            .any(|operation| matches!(
                operation,
                docsight_render::trace::TraceDisplayOperation::Fill {
                    color_argb: 0xff2468ac,
                    ..
                }
            ))
    );
    Ok(())
}

#[test]
fn semantic_text_extraction_does_not_invoke_layout_or_font_discovery() -> TestResult {
    let directory = tempfile::tempdir()?;
    let source = package(
        &format!("<w:p><w:r><w:t>Sample \u{10ffff}</w:t></w:r></w:p>{SECTION}"),
        STYLES,
        None,
    )?;
    let path = directory.path().join("unrenderable.docx");
    std::fs::write(&path, source.bytes())?;
    let text = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .env("DOCSIGHT_FONT_DIR", directory.path().join("nonexistent"))
        .args(["--agent", "--sandbox", "text"])
        .arg(&path)
        .output()?;
    assert_eq!(
        text.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(text.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&text.stdout)?;
    assert_eq!(value["result"]["blocks"][0]["text"], "Sample \u{10ffff}");
    assert!(
        !value["warnings"]
            .as_array()
            .ok_or("warnings")?
            .iter()
            .any(|warning| warning["code"] == "DOCX_LAYOUT_PAGINATED")
    );
    let png = directory.path().join("page.png");
    let render = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .args(["--agent", "--sandbox", "render"])
        .arg(&path)
        .args(["--page", "1", "--out"])
        .arg(&png)
        .output()?;
    assert_eq!(render.status.code(), Some(20));
    assert!(render.stdout.is_empty() && !png.exists());
    Ok(())
}

#[test]
fn static_header_content_controls_preserve_text_and_fields() -> TestResult {
    let section = SECTION.replace(
        "<w:sectPr>",
        "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rHeader\"/>",
    );
    let header = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdt><w:sdtPr><w:alias w:val="Sample header"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Page </w:t></w:r><w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:sdtContent></w:sdt></w:hdr>"#;
    let source = package(
        &format!("<w:p><w:r><w:t>Sample body</w:t></w:r></w:p>{section}"),
        STYLES,
        Some(header),
    )?;
    let laid = ingest_docx(&source)?;
    assert!(laid.pages[0].runs.iter().any(|run| run.text == "Page "));
    assert!(laid.pages[0].runs.iter().any(|run| run.text == "1"));
    let bound = header.replace(
        "<w:alias w:val=\"Sample header\"/>",
        "<w:dataBinding w:storeItemID=\"sample\" w:xpath=\"/sample\"/>",
    );
    let source = package(
        &format!("<w:p><w:r><w:t>Sample body</w:t></w:r></w:p>{section}"),
        STYLES,
        Some(&bound),
    )?;
    assert!(matches!(
        ingest_docx(&source),
        Err(docsight_core::DocsightError::UnsupportedFeature { .. })
    ));
    Ok(())
}

#[test]
fn empty_table_paragraphs_anchor_once_when_rows_continue() -> TestResult {
    let rows = (0..80).map(|index| {
        let text = "Sample recorded condition. ".repeat(1+index%9);
        format!("<w:tr><w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc><w:tc><w:p><w:pPr><w:spacing w:line=\"600\" w:lineRule=\"exact\"/></w:pPr></w:p></w:tc></w:tr>")
    }).collect::<String>();
    let table = format!(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"4200\"/><w:gridCol w:w=\"4200\"/></w:tblGrid>{rows}</w:tbl>{SECTION}"
    );
    let source = package(&table, STYLES, None)?;
    let laid = ingest_docx(&source)?;
    assert!(laid.pages.len() > 1);
    assert!(canonical_violations(&laid.document).is_empty());
    let BlockContent::Table(table) = &laid.document.blocks[0].content else {
        return Err("table".into());
    };
    assert!(
        table
            .cells
            .iter()
            .flat_map(|cell| &cell.blocks)
            .filter(|block| block.text().is_empty())
            .all(|block| block.page.is_some() && block.continuations.is_empty())
    );
    Ok(())
}
