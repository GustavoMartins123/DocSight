use docsight_core::{
    Block, BlockContent, BlockKind, DocsightError, Document, DocumentFormat, DocumentMetadata,
    HeaderFooterKind, HeaderFooterVariant, IrVersion, LayoutFlags, ObjectId, OverlayKind,
    ParagraphBlock, Section, SectionHeaderFooter, SectionStart, SourceSpan, TrackedChanges,
    canonical_violations,
};
use docsight_layout::{LaidOutDocument, layout_docx};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DIGEST: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const LINE_HEIGHT: f32 = 13.97;
const PARAGRAPH_AFTER: f32 = 4.0;
const FILLER_HEIGHT: f32 = LINE_HEIGHT + PARAGRAPH_AFTER;

fn section(index: u32, width: f32, height: f32) -> Section {
    Section {
        id: ObjectId::new("sect", DIGEST, &format!("section[{index}]")),
        section_index: index,
        page_width_pt: Some(width),
        page_height_pt: Some(height),
        margin_top_pt: Some(72.0),
        margin_right_pt: Some(72.0),
        margin_bottom_pt: Some(72.0),
        margin_left_pt: Some(72.0),
        header_text: None,
        footer_text: None,
        start: SectionStart::NextPage,
        title_page: false,
        even_and_odd_headers: false,
        header_distance_pt: Some(36.0),
        footer_distance_pt: Some(36.0),
        columns: 1,
        page_number_start: None,
        page_number_format: None,
        headers_footers: Vec::new(),
        last_block_id: None,
    }
}

fn entry(kind: HeaderFooterKind, variant: HeaderFooterVariant, text: &str) -> SectionHeaderFooter {
    SectionHeaderFooter {
        kind,
        variant,
        part: format!("/word/{text}.xml").replace(' ', "_"),
        text: text.to_owned(),
        inherited: false,
    }
}

fn paragraph(index: u32, text: &str, flags: LayoutFlags) -> Block {
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
        format: Default::default(),
        continuations: Vec::new(),
        content: BlockContent::Paragraph(ParagraphBlock {
            text: text.to_owned(),
            style_id: None,
        }),
    }
}

fn fillers(count: u32) -> Vec<Block> {
    (1..=count)
        .map(|index| paragraph(index, "Filler.", LayoutFlags::default()))
        .collect()
}

fn lines(count: usize) -> String {
    (1..=count)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn widow(enabled: bool) -> LayoutFlags {
    LayoutFlags {
        widow_control: enabled,
        ..Default::default()
    }
}

fn document(blocks: Vec<Block>, sections: Vec<Section>) -> Document {
    Document {
        version: IrVersion::current(),
        id: "doc_pagination".to_owned(),
        sha256: DIGEST.to_owned(),
        format: DocumentFormat::Docx,
        size_bytes: 1,
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

fn page_height_for(content: f32) -> f32 {
    content + 144.0
}

fn laid(blocks: Vec<Block>, sections: Vec<Section>) -> Result<LaidOutDocument, DocsightError> {
    let laid_out = layout_docx(document(blocks, sections))?;
    assert!(
        canonical_violations(&laid_out.document).is_empty(),
        "{:?}",
        canonical_violations(&laid_out.document)
    );
    Ok(laid_out)
}

fn char_start(text: &str, needle: &str) -> Option<usize> {
    let byte = text.find(needle)?;
    Some(text[..byte].chars().count())
}

fn has_warning(laid_out: &LaidOutDocument, code: &str) -> bool {
    laid_out
        .document
        .warnings
        .iter()
        .any(|warning| warning.code == code)
}

#[test]
fn long_paragraph_is_split_by_lines_with_indexed_continuations() -> TestResult {
    let text = lines(30);
    let block = paragraph(1, &text, widow(true));
    let id = block.id.clone();
    let laid_out = laid(
        vec![block],
        vec![section(1, 612.0, page_height_for(11.0 * LINE_HEIGHT + 1.0))],
    )?;
    let placed = &laid_out.document.blocks[0];
    assert_eq!(placed.page, Some(1));
    assert_eq!(laid_out.document.pages.len(), 3);
    let pages: Vec<u32> = placed.continuations.iter().map(|part| part.page).collect();
    assert_eq!(pages, vec![2, 3]);
    assert_eq!(
        placed.continuations[0].text_start_char,
        char_start(&text, "line 12").ok_or("line 12")?
    );
    assert_eq!(
        placed.continuations[1].text_start_char,
        char_start(&text, "line 23").ok_or("line 23")?
    );
    assert_eq!(laid_out.document.pages[0].block_ids, vec![id.clone()]);
    assert_eq!(
        laid_out.document.pages[1].continued_block_ids,
        vec![id.clone()]
    );
    assert_eq!(laid_out.document.pages[2].continued_block_ids, vec![id]);
    let runs: usize = laid_out.pages.iter().map(|page| page.runs.len()).sum();
    assert_eq!(runs, 30);
    assert_eq!(placed.fragment_at_char(0).map(|(page, _)| page), Some(1));
    assert_eq!(
        placed
            .fragment_at_char(char_start(&text, "line 25").ok_or("line 25")?)
            .map(|(page, _)| page),
        Some(3)
    );
    let mut fragments = Vec::new();
    for page in 1..=3 {
        fragments.push(placed.text_on_page(page)?.ok_or("missing text fragment")?);
    }
    assert_eq!(fragments.concat(), text);
    assert!(fragments[0].contains("line 11"));
    assert!(fragments[1].starts_with("line 12"));
    assert!(fragments[2].starts_with("line 23"));
    Ok(())
}

#[test]
fn orphan_control_moves_a_paragraph_when_only_one_line_fits() -> TestResult {
    let content = 5.0 * FILLER_HEIGHT + LINE_HEIGHT + 6.0;
    let geometry = || vec![section(1, 612.0, page_height_for(content))];

    let mut controlled = fillers(5);
    controlled.push(paragraph(6, &lines(5), widow(true)));
    let laid_out = laid(controlled, geometry())?;
    let block = &laid_out.document.blocks[5];
    assert_eq!(block.page, Some(2));
    assert!(block.continuations.is_empty());

    let mut uncontrolled = fillers(5);
    uncontrolled.push(paragraph(6, &lines(5), widow(false)));
    let laid_out = laid(uncontrolled, geometry())?;
    let block = &laid_out.document.blocks[5];
    assert_eq!(block.page, Some(1));
    assert_eq!(block.continuations.len(), 1);
    assert_eq!(
        block.continuations[0].text_start_char,
        char_start(&lines(5), "line 2").ok_or("line 2")?
    );
    Ok(())
}

#[test]
fn widow_control_leaves_two_lines_for_the_next_page() -> TestResult {
    let content = 2.0 * FILLER_HEIGHT + 4.0 * LINE_HEIGHT + 6.0;
    let text = lines(5);
    for (enabled, continuation) in [(true, "line 4"), (false, "line 5")] {
        let mut blocks = fillers(2);
        blocks.push(paragraph(3, &text, widow(enabled)));
        let laid_out = laid(blocks, vec![section(1, 612.0, page_height_for(content))])?;
        let block = &laid_out.document.blocks[2];
        assert_eq!(block.page, Some(1));
        assert_eq!(block.continuations.len(), 1);
        assert_eq!(block.continuations[0].page, 2);
        assert_eq!(
            block.continuations[0].text_start_char,
            char_start(&text, continuation).ok_or("continuation")?
        );
    }
    Ok(())
}

#[test]
fn unsatisfiable_widow_control_on_an_empty_page_is_diagnosed() -> TestResult {
    let content = 2.0 * LINE_HEIGHT + 6.0;
    let laid_out = laid(
        vec![paragraph(1, &lines(3), widow(true))],
        vec![section(1, 612.0, page_height_for(content))],
    )?;
    assert_eq!(laid_out.document.pages.len(), 2);
    assert!(has_warning(&laid_out, "DOCX_WIDOW_CONTROL_RELAXED"));
    Ok(())
}

#[test]
fn keep_lines_moves_a_paragraph_that_fits_on_a_fresh_page() -> TestResult {
    let content = 2.0 * FILLER_HEIGHT + 6.0 * LINE_HEIGHT + 6.0;
    let keep = LayoutFlags {
        keep_lines: true,
        ..Default::default()
    };
    let mut fitting = fillers(2);
    fitting.push(paragraph(3, &lines(7), keep));
    let laid_out = laid(fitting, vec![section(1, 612.0, page_height_for(content))])?;
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    assert!(laid_out.document.blocks[2].continuations.is_empty());

    let mut oversized = fillers(2);
    oversized.push(paragraph(3, &lines(20), keep));
    let laid_out = laid(oversized, vec![section(1, 612.0, page_height_for(content))])?;
    assert_eq!(laid_out.document.blocks[2].page, Some(1));
    assert!(!laid_out.document.blocks[2].continuations.is_empty());
    Ok(())
}

#[test]
fn keep_with_next_chains_move_together_with_the_follower() -> TestResult {
    let keep = LayoutFlags {
        keep_with_next: true,
        ..Default::default()
    };
    let content = 4.0 * FILLER_HEIGHT + 2.0 * FILLER_HEIGHT + 6.0;
    let mut blocks = fillers(4);
    blocks.push(paragraph(5, "Chapter", keep));
    blocks.push(paragraph(6, "Section", keep));
    blocks.push(paragraph(7, &lines(4), widow(true)));
    let laid_out = laid(blocks, vec![section(1, 612.0, page_height_for(content))])?;
    let pages: Vec<Option<u32>> = laid_out
        .document
        .blocks
        .iter()
        .map(|block| block.page)
        .collect();
    assert_eq!(
        pages,
        vec![
            Some(1),
            Some(1),
            Some(1),
            Some(1),
            Some(2),
            Some(2),
            Some(2)
        ]
    );
    Ok(())
}

fn two_section_document(second: Section) -> Result<LaidOutDocument, DocsightError> {
    let blocks = vec![
        paragraph(1, "portrait body", LayoutFlags::default()),
        paragraph(2, "portrait end", LayoutFlags::default()),
        paragraph(3, "second section body", LayoutFlags::default()),
    ];
    let mut first = section(1, 612.0, 792.0);
    first.last_block_id = Some(blocks[1].id.clone());
    laid(blocks, vec![first, second])
}

#[test]
fn every_section_uses_its_own_page_geometry_and_page_linkage() -> TestResult {
    let mut landscape = section(2, 792.0, 612.0);
    landscape.margin_left_pt = Some(54.0);
    landscape.margin_right_pt = Some(54.0);
    let laid_out = two_section_document(landscape)?;
    let pages = &laid_out.document.pages;
    assert_eq!(pages.len(), 2);
    assert_eq!(
        (
            pages[0].width_pt,
            pages[0].height_pt,
            pages[0].section_index
        ),
        (612.0, 792.0, Some(1))
    );
    assert_eq!(
        (
            pages[1].width_pt,
            pages[1].height_pt,
            pages[1].section_index
        ),
        (792.0, 612.0, Some(2))
    );
    let body = laid_out.document.blocks[2]
        .bbox
        .ok_or("second section bbox")?;
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    assert_eq!((body.x0, body.x1), (54.0, 738.0));
    assert_eq!(laid_out.pages[1].width_pt, 792.0);
    assert_eq!(laid_out.document.section_pages(2).count(), 1);
    Ok(())
}

#[test]
fn continuous_sections_share_a_page_only_when_the_page_size_matches() -> TestResult {
    let mut continuous = section(2, 612.0, 792.0);
    continuous.start = SectionStart::Continuous;
    continuous.margin_left_pt = Some(144.0);
    let laid_out = two_section_document(continuous)?;
    assert_eq!(laid_out.document.pages.len(), 1);
    assert_eq!(laid_out.document.pages[0].section_index, Some(1));
    let body = laid_out.document.blocks[2].bbox.ok_or("continuous bbox")?;
    assert_eq!(body.x0, 144.0);
    assert!(body.y0 > laid_out.document.blocks[1].bbox.ok_or("first bbox")?.y0);
    assert_eq!(
        laid_out
            .document
            .section_for_block(&laid_out.document.blocks[2].id)?
            .ok_or("block section")?
            .section_index,
        2
    );

    let mut resized = section(2, 792.0, 612.0);
    resized.start = SectionStart::Continuous;
    let laid_out = two_section_document(resized)?;
    assert_eq!(laid_out.document.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    Ok(())
}

#[test]
fn continuous_sections_apply_new_vertical_margins_on_the_shared_page() -> TestResult {
    let mut continuous = section(2, 612.0, 792.0);
    continuous.start = SectionStart::Continuous;
    continuous.margin_top_pt = Some(180.0);
    continuous.margin_bottom_pt = Some(500.0);
    let laid_out = two_section_document(continuous)?;
    let body = laid_out.document.blocks[2]
        .bbox
        .ok_or("continuous section body")?;
    assert_eq!(body.y0, 180.0);
    assert_eq!(laid_out.document.blocks[2].page, Some(1));

    let mut constrained = section(2, 612.0, 792.0);
    constrained.start = SectionStart::Continuous;
    constrained.margin_top_pt = Some(72.0);
    constrained.margin_bottom_pt = Some(690.0);
    let laid_out = two_section_document(constrained)?;
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    Ok(())
}

#[test]
fn odd_and_even_page_sections_insert_blank_pages_only_when_needed() -> TestResult {
    let mut odd = section(2, 612.0, 792.0);
    odd.start = SectionStart::OddPage;
    let laid_out = two_section_document(odd)?;
    let pages = &laid_out.document.pages;
    assert_eq!(pages.len(), 3);
    assert!(pages[1].block_ids.is_empty());
    assert_eq!(pages[1].section_index, Some(1));
    assert_eq!(pages[2].section_index, Some(2));
    assert_eq!(laid_out.document.blocks[2].page, Some(3));

    let mut even = section(2, 612.0, 792.0);
    even.start = SectionStart::EvenPage;
    let laid_out = two_section_document(even)?;
    assert_eq!(laid_out.document.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[2].page, Some(2));

    let mut restarted = section(2, 612.0, 792.0);
    restarted.start = SectionStart::EvenPage;
    restarted.page_number_start = Some(1);
    let laid_out = two_section_document(restarted)?;
    assert_eq!(laid_out.document.pages.len(), 2);
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    Ok(())
}

#[test]
fn next_column_sections_start_a_page_with_an_explicit_diagnostic() -> TestResult {
    let mut next_column = section(2, 612.0, 792.0);
    next_column.start = SectionStart::NextColumn;
    let laid_out = two_section_document(next_column)?;
    assert_eq!(laid_out.document.blocks[2].page, Some(2));
    let warning = laid_out
        .document
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED")
        .ok_or("next column warning")?;
    assert_eq!(
        warning.object.as_ref(),
        Some(&laid_out.document.sections[1].id)
    );
    Ok(())
}

fn overlay_texts(laid_out: &LaidOutDocument, kind: OverlayKind) -> Vec<String> {
    laid_out
        .document
        .pages
        .iter()
        .map(|page| {
            page.overlays
                .iter()
                .filter(|overlay| overlay.kind == kind)
                .map(|overlay| overlay.text.clone())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

#[test]
fn page_fields_resolve_restarts_formats_and_totals_per_page() -> TestResult {
    let footer = "[PAGE] of [NUMPAGES] ([SECTIONPAGES])";
    let blocks = vec![
        paragraph(1, "front matter", LayoutFlags::default()),
        paragraph(2, "chapter one", LayoutFlags::default()),
        paragraph(
            3,
            "chapter two",
            LayoutFlags {
                page_break_before: true,
                ..Default::default()
            },
        ),
    ];
    let mut front = section(1, 612.0, 792.0);
    front.last_block_id = Some(blocks[0].id.clone());
    front.page_number_format = Some("upperRoman".to_owned());
    front.headers_footers.push(entry(
        HeaderFooterKind::Footer,
        HeaderFooterVariant::Default,
        footer,
    ));
    let mut body = section(2, 612.0, 792.0);
    body.page_number_start = Some(1);
    body.page_number_format = Some("lowerLetter".to_owned());
    body.headers_footers.push(SectionHeaderFooter {
        inherited: true,
        ..entry(
            HeaderFooterKind::Footer,
            HeaderFooterVariant::Default,
            footer,
        )
    });
    let laid_out = laid(blocks, vec![front, body])?;
    assert_eq!(
        overlay_texts(&laid_out, OverlayKind::Footer),
        vec!["I of 3 (1)", "a of 3 (2)", "b of 3 (2)"]
    );
    assert!(has_warning(
        &laid_out,
        "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED"
    ));
    assert!(!has_warning(
        &laid_out,
        "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED"
    ));
    Ok(())
}

#[test]
fn section_page_totals_include_a_shared_continuous_page() -> TestResult {
    let blocks = vec![
        paragraph(1, "first section", LayoutFlags::default()),
        paragraph(2, "second section start", LayoutFlags::default()),
        paragraph(
            3,
            "second section continuation",
            LayoutFlags {
                page_break_before: true,
                ..Default::default()
            },
        ),
    ];
    let mut first = section(1, 612.0, 792.0);
    first.last_block_id = Some(blocks[0].id.clone());
    let mut second = section(2, 612.0, 792.0);
    second.start = SectionStart::Continuous;
    second.headers_footers.push(entry(
        HeaderFooterKind::Footer,
        HeaderFooterVariant::Default,
        "[SECTIONPAGES]",
    ));
    let laid_out = laid(blocks, vec![first, second])?;
    assert_eq!(laid_out.document.pages.len(), 2);
    assert_eq!(overlay_texts(&laid_out, OverlayKind::Footer), vec!["", "2"]);
    Ok(())
}

#[test]
fn unsupported_page_number_formats_are_rendered_decimal_and_diagnosed_once() -> TestResult {
    let blocks = vec![
        paragraph(1, "one", LayoutFlags::default()),
        paragraph(
            2,
            "two",
            LayoutFlags {
                page_break_before: true,
                ..Default::default()
            },
        ),
    ];
    let mut numbered = section(1, 612.0, 792.0);
    numbered.page_number_format = Some("chineseCounting".to_owned());
    numbered.headers_footers.push(entry(
        HeaderFooterKind::Footer,
        HeaderFooterVariant::Default,
        "[PAGE]",
    ));
    let laid_out = laid(blocks, vec![numbered])?;
    assert_eq!(
        overlay_texts(&laid_out, OverlayKind::Footer),
        vec!["1", "2"]
    );
    let count = laid_out
        .document
        .warnings
        .iter()
        .filter(|warning| warning.code == "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED")
        .count();
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn title_page_and_even_page_variants_are_selected_per_page() -> TestResult {
    let blocks: Vec<Block> = (1..=3)
        .map(|index| {
            paragraph(
                index,
                "body",
                LayoutFlags {
                    page_break_before: index > 1,
                    ..Default::default()
                },
            )
        })
        .collect();
    let mut varied = section(1, 612.0, 792.0);
    varied.title_page = true;
    varied.even_and_odd_headers = true;
    varied.headers_footers = vec![
        entry(
            HeaderFooterKind::Header,
            HeaderFooterVariant::Default,
            "Odd",
        ),
        entry(
            HeaderFooterKind::Header,
            HeaderFooterVariant::First,
            "First",
        ),
        entry(HeaderFooterKind::Header, HeaderFooterVariant::Even, "Even"),
    ];
    let laid_out = laid(blocks.clone(), vec![varied.clone()])?;
    assert_eq!(
        overlay_texts(&laid_out, OverlayKind::Header),
        vec!["First", "Even", "Odd"]
    );
    let first_page = laid_out.document.pages[0]
        .overlays
        .first()
        .ok_or("first page header")?;
    assert_eq!(first_page.source.path, "/word/First.xml");

    varied
        .headers_footers
        .retain(|entry| entry.variant != HeaderFooterVariant::First);
    let laid_out = laid(blocks, vec![varied])?;
    assert_eq!(
        overlay_texts(&laid_out, OverlayKind::Header),
        vec!["", "Even", "Odd"]
    );
    Ok(())
}

#[test]
fn header_distance_positions_overlays_and_overlaps_are_diagnosed() -> TestResult {
    let mut placed = section(1, 612.0, 792.0);
    placed.header_distance_pt = Some(20.0);
    placed.footer_distance_pt = Some(30.0);
    placed.headers_footers = vec![
        entry(
            HeaderFooterKind::Header,
            HeaderFooterVariant::Default,
            "Header",
        ),
        entry(
            HeaderFooterKind::Footer,
            HeaderFooterVariant::Default,
            "Footer",
        ),
    ];
    let laid_out = laid(
        vec![paragraph(1, "body", LayoutFlags::default())],
        vec![placed.clone()],
    )?;
    let overlays = &laid_out.document.pages[0].overlays;
    let header = overlays[0].bbox.ok_or("header bbox")?;
    let footer = overlays[1].bbox.ok_or("footer bbox")?;
    assert_eq!((header.y0, header.y1), (20.0, 32.0));
    assert_eq!((footer.y0, footer.y1), (750.0, 762.0));
    assert!(!has_warning(&laid_out, "DOCX_HEADER_FOOTER_OVERLAPS_BODY"));

    placed.headers_footers[0].text = lines(6);
    let laid_out = laid(
        vec![paragraph(1, "body", LayoutFlags::default())],
        vec![placed],
    )?;
    assert!(has_warning(&laid_out, "DOCX_HEADER_FOOTER_OVERLAPS_BODY"));

    let mut outside = section(1, 612.0, 792.0);
    outside.footer_distance_pt = Some(800.0);
    outside.headers_footers.push(entry(
        HeaderFooterKind::Footer,
        HeaderFooterVariant::Default,
        "Footer",
    ));
    let laid_out = laid(
        vec![paragraph(1, "body", LayoutFlags::default())],
        vec![outside],
    )?;
    assert!(has_warning(&laid_out, "DOCX_HEADER_FOOTER_OUTSIDE_PAGE"));
    Ok(())
}

#[test]
fn inconsistent_section_boundaries_fail_closed() {
    let blocks = vec![paragraph(1, "only", LayoutFlags::default())];
    let mut first = section(1, 612.0, 792.0);
    first.last_block_id = Some(ObjectId::new("p", DIGEST, "missing"));
    let result = layout_docx(document(
        blocks.clone(),
        vec![first, section(2, 612.0, 792.0)],
    ));
    assert!(matches!(
        result,
        Err(DocsightError::MalformedDocument { .. })
    ));
    assert!(matches!(
        layout_docx(document(blocks, Vec::new())),
        Err(DocsightError::MalformedDocument { .. })
    ));
}

#[test]
fn repeated_layout_is_identical() -> TestResult {
    let build = || {
        let mut blocks = fillers(3);
        blocks.push(paragraph(4, &lines(40), widow(true)));
        let mut first = section(1, 612.0, 400.0);
        first.last_block_id = Some(blocks[1].id.clone());
        first.headers_footers.push(entry(
            HeaderFooterKind::Footer,
            HeaderFooterVariant::Default,
            "[PAGE]",
        ));
        let mut second = section(2, 400.0, 612.0);
        second.start = SectionStart::OddPage;
        layout_docx(document(blocks, vec![first, second]))
    };
    assert_eq!(build()?, build()?);
    Ok(())
}
