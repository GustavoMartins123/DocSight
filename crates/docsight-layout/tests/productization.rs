use docsight_core::{Block, BlockContent, BlockKind, Document, DocumentFormat, DocumentMetadata, LayoutFlags, ObjectId, ParagraphBlock, ParagraphFormat, Section, SourceSpan, TrackedChanges};
use docsight_layout::{LayoutSection, layout_docx_productized};

const DIGEST: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

fn paragraph(index: u32, text: &str, flags: LayoutFlags, format: ParagraphFormat) -> Block {
    let path = format!("/word/document.xml::body/p[{index}]");
    Block {
        id: ObjectId::new("p", DIGEST, &path),
        kind: BlockKind::Paragraph,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: index,
        source: SourceSpan::new(path),
        confidence: 1.0,
        flags,
        format,
        content: BlockContent::Paragraph(ParagraphBlock { text: text.to_owned(), style_id: None }),
    }
}

fn section(index: u32, width: f32, height: f32, header: &str) -> Section {
    Section {
        id: ObjectId::new("sect", DIGEST, &format!("section-{index}")),
        section_index: index,
        page_width_pt: Some(width),
        page_height_pt: Some(height),
        margin_top_pt: Some(36.0),
        margin_right_pt: Some(36.0),
        margin_bottom_pt: Some(36.0),
        margin_left_pt: Some(36.0),
        header_text: Some(header.to_owned()),
        footer_text: Some("Page [PAGE]".to_owned()),
    }
}

fn document(blocks: Vec<Block>, sections: Vec<Section>) -> Document {
    Document {
        version: docsight_core::IrVersion::current(),
        id: "doc_1234567890ab".to_owned(),
        sha256: DIGEST.to_owned(),
        format: DocumentFormat::Docx,
        size_bytes: 100,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections,
        pages: Vec::new(),
        blocks,
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: TrackedChanges::default(),
        warnings: Vec::new(),
    }
}

#[test]
fn applies_geometry_and_overlays_per_section() -> Result<(), Box<dyn std::error::Error>> {
    let first = section(1, 612.0, 792.0, "First");
    let second = section(2, 792.0, 612.0, "Second");
    let doc = document(
        vec![
            paragraph(1, "first section", LayoutFlags::default(), ParagraphFormat::default()),
            paragraph(2, "second section", LayoutFlags::default(), ParagraphFormat::default()),
        ],
        vec![first.clone(), second.clone()],
    );
    let laid = layout_docx_productized(doc, vec![
        LayoutSection { start_block: 0, end_block: 1, section: first },
        LayoutSection { start_block: 1, end_block: 2, section: second },
    ])?;
    assert_eq!(laid.document.pages.len(), 2);
    assert_eq!((laid.document.pages[0].width_pt, laid.document.pages[0].height_pt), (612.0, 792.0));
    assert_eq!((laid.document.pages[1].width_pt, laid.document.pages[1].height_pt), (792.0, 612.0));
    assert_eq!(laid.document.blocks[0].page, Some(1));
    assert_eq!(laid.document.blocks[1].page, Some(2));
    assert!(laid.document.pages[0].overlays.iter().any(|overlay| overlay.text == "First"));
    assert!(laid.document.pages[1].overlays.iter().any(|overlay| overlay.text == "Second"));
    assert!(laid.document.pages[0].overlays.iter().any(|overlay| overlay.text == "Page 1"));
    assert!(laid.document.pages[1].overlays.iter().any(|overlay| overlay.text == "Page 1"));
    Ok(())
}

#[test]
fn fragments_preserve_only_boundary_spacing_and_keep_flags() -> Result<(), Box<dyn std::error::Error>> {
    let section = Section {
        id: ObjectId::new("sect", DIGEST, "section-1"),
        section_index: 1,
        page_width_pt: Some(220.0),
        page_height_pt: Some(260.0),
        margin_top_pt: Some(30.0),
        margin_right_pt: Some(30.0),
        margin_bottom_pt: Some(30.0),
        margin_left_pt: Some(30.0),
        header_text: None,
        footer_text: None,
    };
    let flags = LayoutFlags { page_break_before: true, break_after: true, keep_with_next: true, keep_lines: false };
    let format = ParagraphFormat {
        alignment: None,
        space_before_pt: Some(12.0),
        space_after_pt: Some(18.0),
        line_spacing: Some(1.2),
        indent_left_pt: Some(8.0),
        indent_right_pt: Some(8.0),
        indent_first_line_pt: Some(16.0),
    };
    let text = (0..80).map(|index| format!("word{index}")).collect::<Vec<_>>().join(" ");
    let doc = document(vec![paragraph(1, &text, flags, format)], vec![section.clone()]);
    let laid = layout_docx_productized(doc, vec![LayoutSection { start_block: 0, end_block: 1, section }])?;
    assert!(laid.document.blocks.len() > 1);
    let first = laid.document.blocks.first().ok_or("missing first fragment")?;
    let last = laid.document.blocks.last().ok_or("missing last fragment")?;
    assert_eq!(first.format.space_before_pt, Some(12.0));
    assert_eq!(first.format.indent_first_line_pt, Some(16.0));
    assert!(first.flags.page_break_before);
    assert!(!first.flags.break_after);
    for middle in laid.document.blocks.iter().skip(1) {
        assert_eq!(middle.format.space_before_pt, None);
        assert_eq!(middle.format.indent_first_line_pt, None);
        assert!(!middle.flags.page_break_before);
    }
    for fragment in laid.document.blocks.iter().take(laid.document.blocks.len() - 1) {
        assert_eq!(fragment.format.space_after_pt, None);
        assert!(!fragment.flags.keep_with_next);
        assert!(!fragment.flags.break_after);
    }
    assert_eq!(last.format.space_after_pt, Some(18.0));
    assert!(last.flags.keep_with_next);
    assert!(last.flags.break_after);
    assert!(laid.document.warnings.iter().any(|warning| warning.code == "DOCX_LINE_LEVEL_PAGINATION"));
    Ok(())
}
