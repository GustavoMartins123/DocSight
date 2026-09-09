use docsight_core::{
    Block, BlockContent, Document, DocumentFormat, DocumentMetadata, HeadingBlock, LayoutFlags,
    ObjectId, ParagraphBlock, Section, SourceSpan, TableBlock, TableCell,
};
use docsight_layout::{layout_docx, text_width, wrap_text};

const DIGEST: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

fn dummy_document(blocks: Vec<Block>, section: Option<Section>) -> Document {
    Document {
        id: "doc_1234567890ab".to_owned(),
        sha256: DIGEST.to_owned(),
        format: DocumentFormat::Docx,
        size_bytes: 100,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections: section.into_iter().collect(),
        pages: Vec::new(),
        blocks,
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: docsight_core::TrackedChanges::default(),
        warnings: Vec::new(),
    }
}

#[test]
fn wraps_text_deterministically() {
    let text = "The quick brown fox jumps over the lazy dog";
    let lines = wrap_text(text, 11.0, 100.0);
    assert!(lines.len() > 1);
    let combined = lines.join(" ");
    assert_eq!(combined, text);

    let short_lines = wrap_text("hello", 11.0, 200.0);
    assert_eq!(short_lines, vec!["hello"]);
    assert!(text_width("W", 10.0) > text_width("i", 10.0));
}

#[test]
fn paginates_paragraphs_and_computes_deterministic_geometry()
-> Result<(), Box<dyn std::error::Error>> {
    let b1 = Block {
        id: ObjectId::new("h", DIGEST, "/word/document.xml::body/p[1]"),
        kind: docsight_core::BlockKind::Heading,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: 1,
        source: SourceSpan::new("/word/document.xml::body/p[1]"),
        confidence: 1.0,
        flags: Default::default(),
        content: BlockContent::Heading(HeadingBlock {
            level: 1,
            text: "Document Title".to_owned(),
            style_id: None,
        }),
    };
    let b2 = Block {
        id: ObjectId::new("p", DIGEST, "/word/document.xml::body/p[2]"),
        kind: docsight_core::BlockKind::Paragraph,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: 2,
        source: SourceSpan::new("/word/document.xml::body/p[2]"),
        confidence: 1.0,
        flags: Default::default(),
        content: BlockContent::Paragraph(ParagraphBlock {
            text: "This is a sample paragraph describing the deterministic layout implementation."
                .to_owned(),
            style_id: None,
        }),
    };
    let section = Section {
        id: ObjectId::new("sect", DIGEST, "/word/document.xml::body/sectPr[1]"),
        section_index: 1,
        page_width_pt: Some(612.0),
        page_height_pt: Some(792.0),
        margin_top_pt: Some(72.0),
        margin_right_pt: Some(72.0),
        margin_bottom_pt: Some(72.0),
        margin_left_pt: Some(72.0),
        header_text: None,
        footer_text: None,
    };

    let doc = dummy_document(vec![b1, b2], Some(section));
    let laid_out = layout_docx(doc)?;

    assert_eq!(laid_out.pages.len(), 1);
    assert_eq!(laid_out.document.pages.len(), 1);

    let page1 = &laid_out.document.pages[0];
    assert_eq!(page1.number, 1);
    assert_eq!(page1.width_pt, 612.0);
    assert_eq!(page1.height_pt, 792.0);
    assert_eq!(page1.block_ids.len(), 2);

    let blk1 = &laid_out.document.blocks[0];
    assert_eq!(blk1.page, Some(1));
    let bbox1 = blk1.bbox.ok_or("missing bbox 1")?;
    assert_eq!(bbox1.x0, 72.0);
    assert!(bbox1.y0 >= 72.0);
    assert!(bbox1.y1 > bbox1.y0);

    let blk2 = &laid_out.document.blocks[1];
    assert_eq!(blk2.page, Some(1));
    let bbox2 = blk2.bbox.ok_or("missing bbox 2")?;
    assert_eq!(bbox2.x0, 72.0);
    assert!(bbox2.y0 >= bbox1.y1);

    assert!(!laid_out.pages[0].runs.is_empty());
    Ok(())
}

#[test]
fn paginates_large_content_into_multiple_pages() -> Result<(), Box<dyn std::error::Error>> {
    let mut blocks = Vec::new();
    for i in 1..=50 {
        blocks.push(Block {
            id: ObjectId::new("p", DIGEST, &format!("/word/document.xml::body/p[{i}]")),
            kind: docsight_core::BlockKind::Paragraph,
            page: None,
            bbox: None,
            z_index: 0,
            reading_order: i,
            source: SourceSpan::new(format!("/word/document.xml::body/p[{i}]")),
            confidence: 1.0,
            flags: Default::default(),
            content: BlockContent::Paragraph(ParagraphBlock {
                text: format!("Paragraph {i}: This is substantial content to fill space on the page and force deterministic pagination breaks."),
                style_id: None,
            }),
        });
    }

    let section = Section {
        id: ObjectId::new("sect", DIGEST, "/word/document.xml::body/sectPr[1]"),
        section_index: 1,
        page_width_pt: Some(612.0),
        page_height_pt: Some(400.0),
        margin_top_pt: Some(50.0),
        margin_right_pt: Some(50.0),
        margin_bottom_pt: Some(50.0),
        margin_left_pt: Some(50.0),
        header_text: None,
        footer_text: None,
    };

    let doc = dummy_document(blocks, Some(section));
    let laid_out = layout_docx(doc)?;

    assert!(laid_out.pages.len() > 1);
    assert_eq!(laid_out.document.pages.len(), laid_out.pages.len());
    for (i, p) in laid_out.document.pages.iter().enumerate() {
        assert_eq!(p.number, (i + 1) as u32);
        assert!(!p.block_ids.is_empty());
    }
    Ok(())
}

#[test]
fn lays_out_tables_with_cells_and_borders() -> Result<(), Box<dyn std::error::Error>> {
    let cells = vec![
        TableCell {
            id: ObjectId::new("c", DIGEST, "c1"),
            row: 0,
            column: 0,
            row_span: 1,
            column_span: 1,
            bbox: None,
            text: "Header A".to_owned(),
            blocks: Vec::new(),
            source: SourceSpan::new("c1"),
        },
        TableCell {
            id: ObjectId::new("c", DIGEST, "c2"),
            row: 0,
            column: 1,
            row_span: 1,
            column_span: 1,
            bbox: None,
            text: "Header B".to_owned(),
            blocks: Vec::new(),
            source: SourceSpan::new("c2"),
        },
        TableCell {
            id: ObjectId::new("c", DIGEST, "c3"),
            row: 1,
            column: 0,
            row_span: 1,
            column_span: 1,
            bbox: None,
            text: "Data 1".to_owned(),
            blocks: Vec::new(),
            source: SourceSpan::new("c3"),
        },
        TableCell {
            id: ObjectId::new("c", DIGEST, "c4"),
            row: 1,
            column: 1,
            row_span: 1,
            column_span: 1,
            bbox: None,
            text: "Data 2".to_owned(),
            blocks: Vec::new(),
            source: SourceSpan::new("c4"),
        },
    ];
    let tbl_block = Block {
        id: ObjectId::new("tbl", DIGEST, "/word/document.xml::body/tbl[1]"),
        kind: docsight_core::BlockKind::Table,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: 1,
        source: SourceSpan::new("/word/document.xml::body/tbl[1]"),
        confidence: 1.0,
        flags: Default::default(),
        content: BlockContent::Table(TableBlock {
            rows: 2,
            columns: 2,
            header_rows: 1,
            cells,
            column_widths_pt: None,
            detector: None,
        }),
    };

    let doc = dummy_document(vec![tbl_block], None);
    let laid_out = layout_docx(doc)?;

    assert_eq!(laid_out.pages.len(), 1);
    let laid_table = match &laid_out.document.blocks[0].content {
        BlockContent::Table(t) => t,
        _ => return Err("expected table".into()),
    };
    for cell in &laid_table.cells {
        assert!(cell.bbox.is_some());
    }
    assert_eq!(laid_out.pages[0].borders.len(), 4);
    Ok(())
}

#[test]
fn lays_out_figures_notes_headers_and_footers() -> Result<(), Box<dyn std::error::Error>> {
    let fig_block = Block {
        id: ObjectId::new("fig", DIGEST, "/word/document.xml::fig[1]"),
        kind: docsight_core::BlockKind::Figure,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: 1,
        source: SourceSpan::new("/word/document.xml::fig[1]"),
        confidence: 1.0,
        flags: Default::default(),
        content: BlockContent::Figure(docsight_core::FigureBlock {
            alt_text: Some("Chart Diagram".to_owned()),
            caption: None,
            resource_id: Some("rId1".to_owned()),
            width_pt: Some(300.0),
            height_pt: Some(150.0),
        }),
    };

    let note_block = Block {
        id: ObjectId::new("fn", DIGEST, "/word/footnotes.xml::fn[1]"),
        kind: docsight_core::BlockKind::Note,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: 2,
        source: SourceSpan::new("/word/footnotes.xml::fn[1]"),
        confidence: 1.0,
        flags: Default::default(),
        content: BlockContent::Note(docsight_core::NoteBlock {
            kind: docsight_core::NoteKind::Footnote,
            note_id: "1".to_owned(),
            text: "Footnote explanation text.".to_owned(),
            anchor_path: None,
        }),
    };

    let section = Section {
        id: ObjectId::new("sect", DIGEST, "/word/document.xml::body/sectPr[1]"),
        section_index: 1,
        page_width_pt: Some(612.0),
        page_height_pt: Some(792.0),
        margin_top_pt: Some(72.0),
        margin_right_pt: Some(72.0),
        margin_bottom_pt: Some(72.0),
        margin_left_pt: Some(72.0),
        header_text: Some("Top Header".to_owned()),
        footer_text: Some("Bottom Page [PAGE]".to_owned()),
    };

    let mut doc = dummy_document(vec![fig_block, note_block], Some(section));
    doc.links.push(docsight_core::Hyperlink {
        id: ObjectId::new("lnk", DIGEST, "link[1]"),
        text: "Footnote explanation".to_owned(),
        target: "https://example.com".to_owned(),
        is_external: true,
        page: None,
        anchor_path: Some("/word/document.xml::fig[1]".to_owned()),
        source: SourceSpan::new("link[1]"),
    });

    let laid_out = layout_docx(doc)?;

    assert_eq!(laid_out.pages.len(), 1);
    assert_eq!(laid_out.document.pages.len(), 1);
    let page = &laid_out.document.pages[0];
    assert_eq!(page.overlays.len(), 2);
    assert_eq!(page.overlays[0].kind, docsight_core::OverlayKind::Header);
    assert_eq!(page.overlays[0].text, "Top Header");
    assert_eq!(page.overlays[1].kind, docsight_core::OverlayKind::Footer);
    assert_eq!(page.overlays[1].text, "Bottom Page 1");

    assert!(laid_out.document.blocks[0].bbox.is_some());
    assert_eq!(laid_out.document.blocks[0].page, Some(1));
    assert!(laid_out.document.blocks[1].bbox.is_some());
    assert_eq!(laid_out.document.blocks[1].page, Some(1));

    assert_eq!(laid_out.document.links[0].page, Some(1));
    assert_eq!(laid_out.pages[0].borders.len(), 1);

    Ok(())
}

fn paragraph_block(index: u32, text: &str, flags: LayoutFlags) -> Block {
    let source_path = format!("/word/document.xml::body/p[{index}]");
    Block {
        id: ObjectId::new("p", DIGEST, &source_path),
        kind: docsight_core::BlockKind::Paragraph,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: index,
        source: SourceSpan::new(source_path),
        confidence: 1.0,
        flags,
        content: BlockContent::Paragraph(ParagraphBlock {
            text: text.to_owned(),
            style_id: None,
        }),
    }
}

fn explicit_page_document(page_count: u32) -> Document {
    let blocks = (1..=page_count)
        .map(|index| {
            paragraph_block(
                index,
                "x",
                LayoutFlags {
                    page_break_before: index > 1,
                    ..Default::default()
                },
            )
        })
        .collect();
    dummy_document(blocks, None)
}

#[test]
fn enforces_layout_page_limit_at_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let accepted = layout_docx(explicit_page_document(10_000))?;
    assert_eq!(accepted.pages.len(), 10_000);

    let error = match layout_docx(explicit_page_document(10_001)) {
        Err(error) => error,
        Ok(_) => return Err(std::io::Error::other("layout page limit was not enforced").into()),
    };
    assert!(matches!(
        error,
        docsight_core::DocsightError::ResourceLimit { .. }
    ));
    Ok(())
}

#[test]
fn explicit_page_break_before_moves_block_to_next_page() -> Result<(), Box<dyn std::error::Error>> {
    let flags = LayoutFlags {
        page_break_before: true,
        ..Default::default()
    };
    let doc = dummy_document(
        vec![
            paragraph_block(1, "First page body text.", LayoutFlags::default()),
            paragraph_block(2, "Second page body text.", flags),
        ],
        None,
    );
    let laid_out = layout_docx(doc)?;
    assert_eq!(laid_out.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[0].page, Some(1));
    assert_eq!(laid_out.document.blocks[1].page, Some(2));
    Ok(())
}

#[test]
fn break_after_flushes_the_current_page() -> Result<(), Box<dyn std::error::Error>> {
    let flags = LayoutFlags {
        break_after: true,
        ..Default::default()
    };
    let doc = dummy_document(
        vec![
            paragraph_block(1, "Before the explicit break.", flags),
            paragraph_block(2, "After the explicit break.", LayoutFlags::default()),
        ],
        None,
    );
    let laid_out = layout_docx(doc)?;
    assert_eq!(laid_out.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[0].page, Some(1));
    assert_eq!(laid_out.document.blocks[1].page, Some(2));
    Ok(())
}

#[test]
fn keep_with_next_moves_heading_with_its_follower() -> Result<(), Box<dyn std::error::Error>> {
    let keep = LayoutFlags {
        keep_with_next: true,
        ..Default::default()
    };
    let long_text = "filler paragraph ".repeat(292);
    let doc = dummy_document(
        vec![
            paragraph_block(1, &long_text, LayoutFlags::default()),
            paragraph_block(2, "Heading kept with next", keep),
            paragraph_block(3, "Follower body paragraph.", LayoutFlags::default()),
        ],
        None,
    );
    let laid_out = layout_docx(doc)?;
    assert_eq!(laid_out.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[0].page, Some(1));
    assert_eq!(laid_out.document.blocks[1].page, Some(2));
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    Ok(())
}

#[test]
fn block_taller_than_page_overflows_with_diagnostic() -> Result<(), Box<dyn std::error::Error>> {
    let huge = "overflow line of text ".repeat(2000);
    let doc = dummy_document(
        vec![paragraph_block(1, &huge, LayoutFlags::default())],
        None,
    );
    let laid_out = layout_docx(doc)?;
    assert_eq!(laid_out.pages.len(), 1);
    assert!(
        laid_out
            .document
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_BLOCK_TALLER_THAN_PAGE")
    );
    Ok(())
}

#[test]
fn block_granular_pagination_is_diagnosed() -> Result<(), Box<dyn std::error::Error>> {
    let doc = dummy_document(
        vec![paragraph_block(
            1,
            "Single paragraph.",
            LayoutFlags::default(),
        )],
        None,
    );
    let laid_out = layout_docx(doc)?;
    assert!(
        laid_out
            .document
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_PAGINATION_BLOCK_GRANULAR")
    );
    Ok(())
}
