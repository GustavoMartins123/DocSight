use docsight_core::{
    Block, BlockContent, BlockKind, Document, DocumentFormat, DocumentMetadata, HeadingBlock,
    ObjectId, Page, ParagraphBlock, Rect, SourceSpan, TableBlock, TableCell, table_to_csv,
    table_to_html, table_to_markdown, table_to_tsv,
};

fn sample_document() -> Document {
    let doc_id = "doc_1234567890ab".to_owned();
    let digest = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_owned();
    let h1_id = ObjectId::new("h1", &digest, "/body/p[1]");
    let p1_id = ObjectId::new("p", &digest, "/body/p[2]");
    let tbl_id = ObjectId::new("tbl", &digest, "/body/tbl[1]");
    let c1_id = ObjectId::new("c", &digest, "/body/tbl[1]/r[1]/c[1]");
    let c2_id = ObjectId::new("c", &digest, "/body/tbl[1]/r[1]/c[2]");

    let cell1 = TableCell {
        id: c1_id,
        row: 0,
        column: 0,
        row_span: 1,
        column_span: 1,
        bbox: None,
        text: "Header A".to_owned(),
        blocks: Vec::new(),
        source: SourceSpan::new("/body/tbl[1]/r[1]/c[1]"),
    };
    let cell2 = TableCell {
        id: c2_id,
        row: 0,
        column: 1,
        row_span: 1,
        column_span: 1,
        bbox: None,
        text: "Header B".to_owned(),
        blocks: Vec::new(),
        source: SourceSpan::new("/body/tbl[1]/r[1]/c[2]"),
    };

    let table_block = TableBlock {
        rows: 1,
        columns: 2,
        header_rows: 1,
        cells: vec![cell1, cell2],
        column_widths_pt: None,
        detector: None,
    };

    let blocks = vec![
        Block {
            id: h1_id.clone(),
            kind: BlockKind::Heading,
            page: Some(1),
            bbox: Rect::new(50.0, 50.0, 300.0, 70.0).ok(),
            z_index: 0,
            reading_order: 1,
            source: SourceSpan::new("/body/p[1]"),
            confidence: 1.0,
            flags: Default::default(),
            content: BlockContent::Heading(HeadingBlock {
                level: 1,
                text: "Introduction".to_owned(),
                style_id: Some("Heading1".to_owned()),
            }),
        },
        Block {
            id: p1_id.clone(),
            kind: BlockKind::Paragraph,
            page: Some(1),
            bbox: Rect::new(50.0, 80.0, 400.0, 100.0).ok(),
            z_index: 0,
            reading_order: 2,
            source: SourceSpan::new("/body/p[2]"),
            confidence: 1.0,
            flags: Default::default(),
            content: BlockContent::Paragraph(ParagraphBlock {
                text: "This is a paragraph.".to_owned(),
                style_id: None,
            }),
        },
        Block {
            id: tbl_id.clone(),
            kind: BlockKind::Table,
            page: Some(1),
            bbox: Rect::new(50.0, 110.0, 450.0, 160.0).ok(),
            z_index: 0,
            reading_order: 3,
            source: SourceSpan::new("/body/tbl[1]"),
            confidence: 1.0,
            flags: Default::default(),
            content: BlockContent::Table(table_block),
        },
    ];

    let page1 = Page {
        number: 1,
        width_pt: 595.0,
        height_pt: 842.0,
        block_ids: vec![h1_id, p1_id, tbl_id],
        overlays: Vec::new(),
    };

    Document {
        id: doc_id,
        sha256: digest,
        format: DocumentFormat::Docx,
        size_bytes: 1024,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections: Vec::new(),
        pages: vec![page1],
        blocks,
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: docsight_core::TrackedChanges::default(),
        warnings: Vec::new(),
    }
}

#[test]
fn round_trips_document_ir_through_json() -> Result<(), Box<dyn std::error::Error>> {
    let doc = sample_document();
    let json = serde_json::to_string(&doc)?;
    let deserialized: Document = serde_json::from_str(&json)?;
    assert_eq!(doc, deserialized);
    Ok(())
}

#[test]
fn queries_document_blocks_by_kind() {
    let doc = sample_document();
    assert_eq!(doc.headings().count(), 1);
    assert_eq!(doc.paragraphs().count(), 1);
    assert_eq!(doc.tables().count(), 1);
    assert_eq!(doc.page_blocks(1).count(), 3);
    assert!(doc.page_blocks(2).next().is_none());

    let (block, heading) = doc.headings().next().unwrap_or_else(|| unreachable!());
    assert_eq!(heading.level, 1);
    assert_eq!(heading.text, "Introduction");
    assert_eq!(block.reading_order, 1);
}

#[test]
fn formats_canonical_table_to_markdown_csv_tsv_html() -> Result<(), Box<dyn std::error::Error>> {
    let doc = sample_document();
    let (_, table) = doc.tables().next().unwrap_or_else(|| unreachable!());

    let md = table_to_markdown(table)?;
    assert!(md.contains("| Header A | Header B |"));
    assert!(md.contains("| -------- | -------- |"));

    let csv = table_to_csv(table)?;
    assert_eq!(csv.trim(), "Header A,Header B");

    let tsv = table_to_tsv(table)?;
    assert_eq!(tsv.trim(), "Header A\tHeader B");

    let html = table_to_html(table)?;
    assert!(html.contains("<table>"));
    assert!(html.contains("<th>Header A</th>"));
    assert!(html.contains("<th>Header B</th>"));
    Ok(())
}

#[test]
fn preserves_block_ref_geometry() {
    let doc = sample_document();
    let block = &doc.blocks[0];
    let block_ref = block.to_ref().unwrap_or_else(|| unreachable!());
    assert_eq!(block_ref.page, 1);
    assert_eq!(block_ref.bbox_pt.x0, 50.0);
    assert_eq!(block_ref.bbox_pt.y0, 50.0);
    assert_eq!(block_ref.confidence, 1.0);
}
