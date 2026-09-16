use crate::font::{text_width, wrap_text};
use docsight_core::{
    Block, BlockContent, Diagnostic, DiagnosticSeverity, DocsightError, Document, ObjectId,
    Overlay, OverlayKind, Page, ParagraphFormat, Rect, SourceSpan, Style, TextAlignment,
    validate_canonical,
};

pub const MAX_LAYOUT_PAGES: u32 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct TextRunLayout {
    pub text: String,
    pub font_size: f32,
    pub bold: bool,
    pub bbox: Rect,
    pub color_argb: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BorderLayout {
    pub rect: Rect,
    pub color_argb: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaidOutPage {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub runs: Vec<TextRunLayout>,
    pub borders: Vec<BorderLayout>,
    pub images: Vec<ImageLayout>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageLayout {
    pub rect: Rect,
    pub target: String,
    pub object_id: ObjectId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaidOutDocument {
    pub document: Document,
    pub pages: Vec<LaidOutPage>,
}

struct Placement {
    page: u32,
    y: f32,
}

pub fn layout_docx(mut doc: Document) -> Result<LaidOutDocument, DocsightError> {
    let (page_width, page_height, margin_top, margin_bottom, margin_left, margin_right) =
        if let Some(section) = doc.sections.first() {
            (
                section.page_width_pt.unwrap_or(612.0),
                section.page_height_pt.unwrap_or(792.0),
                section.margin_top_pt.unwrap_or(72.0),
                section.margin_bottom_pt.unwrap_or(72.0),
                section.margin_left_pt.unwrap_or(72.0),
                section.margin_right_pt.unwrap_or(72.0),
            )
        } else {
            (612.0, 792.0, 72.0, 72.0, 72.0, 72.0)
        };

    let content_width = page_width - margin_left - margin_right;
    let content_height = page_height - margin_top - margin_bottom;
    if !content_width.is_finite()
        || !content_height.is_finite()
        || content_width <= 0.0
        || content_height <= 0.0
    {
        return Err(DocsightError::MalformedDocument {
            message: "DOCX section margins leave no positive page content area".to_owned(),
        });
    }
    let content_bottom = margin_top + content_height;

    let mut warnings = vec![
        Diagnostic {
            code: "DOCX_LAYOUT_PAGINATED".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "DOCX was paginated using deterministic layout engine".to_owned(),
            effect: "line wrapping and page boundaries are computed approximations".to_owned(),
            object: None,
            page: None,
        },
        Diagnostic {
            code: "DOCX_FONT_SUBSTITUTED".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "layout used deterministic proportional fallback font".to_owned(),
            effect: "glyph metrics follow standard proportional reference widths".to_owned(),
            object: None,
            page: None,
        },
        Diagnostic {
            code: "DOCX_PAGINATION_BLOCK_GRANULAR".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "pagination never splits a block across pages".to_owned(),
            effect: "widow, orphan and keep-lines rules are approximated by whole-block moves; page breaks can occur earlier than in Word"
                .to_owned(),
            object: None,
            page: None,
        },
    ];

    let block_count = doc.blocks.len();
    let styles = doc.styles.clone();
    let resources = doc.resources.clone();
    let mut heights = Vec::with_capacity(block_count);
    for block in &doc.blocks {
        heights.push(measure_height(block, content_width, &styles, &resources)?);
    }

    let mut placements: Vec<Placement> = Vec::with_capacity(block_count);
    let mut current_page_number = 1_u32;
    let mut current_y = margin_top;
    let mut page_has_content = false;

    for index in 0..block_count {
        let flags = doc.blocks[index].flags;
        if flags.page_break_before && page_has_content {
            current_page_number = advance_page(current_page_number)?;
            current_y = margin_top;
            page_has_content = false;
        }

        let mut chain_end = index;
        while chain_end + 1 < block_count && doc.blocks[chain_end].flags.keep_with_next {
            chain_end += 1;
        }
        let chain_height =
            heights[index..=chain_end]
                .iter()
                .try_fold(0.0_f32, |total, height| {
                    let next = total + height;
                    next.is_finite().then_some(next).ok_or_else(|| {
                        DocsightError::MalformedDocument {
                            message: "DOCX keep-with-next chain height is non-finite".to_owned(),
                        }
                    })
                })?;
        let remaining = content_bottom - current_y;
        let block_fits = heights[index] <= remaining;
        let chain_fits = chain_end == index || chain_height <= remaining;

        if page_has_content && (!block_fits || !chain_fits) {
            current_page_number = advance_page(current_page_number)?;
            current_y = margin_top;
        }

        if heights[index] > content_height {
            warnings.push(Diagnostic {
                code: "DOCX_BLOCK_TALLER_THAN_PAGE".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "block {} is taller than the content area and overflows the page",
                    doc.blocks[index].id
                ),
                effect: "the block geometry extends beyond the page bottom edge".to_owned(),
                object: Some(doc.blocks[index].id.clone()),
                page: Some(current_page_number),
            });
        }

        placements.push(Placement {
            page: current_page_number,
            y: current_y,
        });
        current_y += heights[index];
        page_has_content = true;

        if flags.break_after {
            current_page_number = advance_page(current_page_number)?;
            current_y = margin_top;
            page_has_content = false;
        }
    }

    let total_pages = placements
        .last()
        .map(|placement| placement.page)
        .unwrap_or(current_page_number);

    let total_pages_capacity =
        usize::try_from(total_pages).map_err(|_| DocsightError::ResourceLimit {
            resource: "DOCX layout pages".to_owned(),
            limit: u64::from(MAX_LAYOUT_PAGES),
        })?;
    let mut laid_pages: Vec<LaidOutPage> = Vec::with_capacity(total_pages_capacity);
    let mut doc_pages: Vec<Page> = Vec::with_capacity(total_pages_capacity);
    for number in 1..=total_pages {
        laid_pages.push(LaidOutPage {
            number,
            width_pt: page_width,
            height_pt: page_height,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
        });
        doc_pages.push(Page {
            number,
            width_pt: page_width,
            height_pt: page_height,
            block_ids: Vec::new(),
            overlays: Vec::new(),
        });
    }

    let mut updated_blocks: Vec<Block> = Vec::with_capacity(block_count);
    let source_blocks = std::mem::take(&mut doc.blocks);
    for (placement_index, (mut block, placement)) in
        source_blocks.into_iter().zip(placements.iter()).enumerate()
    {
        let emitted = emit_block(
            &mut block,
            content_width,
            margin_left,
            placement.y,
            &mut warnings,
            &styles,
            &resources,
        )?;
        block.page = Some(placement.page);
        block.bbox = Some(Rect::new(
            margin_left,
            placement.y,
            margin_left + content_width,
            placement.y + emitted.height,
        )?);
        block.reading_order =
            u32::try_from(placement_index + 1).map_err(|_| DocsightError::ResourceLimit {
                resource: "DOCX layout blocks".to_owned(),
                limit: u64::from(u32::MAX),
            })?;

        let page_index = usize::try_from(placement.page.checked_sub(1).ok_or_else(|| {
            DocsightError::MalformedDocument {
                message: "DOCX layout produced page zero".to_owned(),
            }
        })?)
        .map_err(|_| DocsightError::ResourceLimit {
            resource: "DOCX layout page index".to_owned(),
            limit: u64::from(MAX_LAYOUT_PAGES),
        })?;
        let laid_page =
            laid_pages
                .get_mut(page_index)
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "DOCX layout page index is outside the allocated page set".to_owned(),
                })?;
        let doc_page =
            doc_pages
                .get_mut(page_index)
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "DOCX document page index is outside the allocated page set"
                        .to_owned(),
                })?;
        doc_page.block_ids.push(block.id.clone());
        laid_page.runs.extend(emitted.runs);
        laid_page.borders.extend(emitted.borders);
        laid_page.images.extend(emitted.images);

        updated_blocks.push(block);
    }

    anchor_links_and_comments(
        &doc.sha256,
        &updated_blocks,
        &mut doc.links,
        &mut doc.comments,
        &mut doc_pages,
        &mut warnings,
    );

    project_headers_footers(
        &doc.sha256,
        &doc.sections,
        &mut doc_pages,
        &mut laid_pages,
        page_height,
        margin_top,
        margin_bottom,
        margin_left,
        content_width,
    );

    doc.pages = doc_pages;
    doc.blocks = updated_blocks;
    doc.warnings.append(&mut warnings);

    validate_canonical(&doc)?;

    Ok(LaidOutDocument {
        document: doc,
        pages: laid_pages,
    })
}

fn advance_page(current: u32) -> Result<u32, DocsightError> {
    if current >= MAX_LAYOUT_PAGES {
        return Err(DocsightError::ResourceLimit {
            resource: "layout page count".to_owned(),
            limit: u64::from(MAX_LAYOUT_PAGES),
        });
    }
    current
        .checked_add(1)
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "page count".to_owned(),
            limit: u32::MAX as u64,
        })
}

#[allow(clippy::too_many_arguments)]
fn project_headers_footers(
    document_digest: &str,
    sections: &[docsight_core::Section],
    doc_pages: &mut [Page],
    laid_pages: &mut [LaidOutPage],
    page_height: f32,
    margin_top: f32,
    margin_bottom: f32,
    margin_left: f32,
    content_width: f32,
) {
    let header_template = sections.first().and_then(|s| s.header_text.clone());
    let footer_template = sections.first().and_then(|s| s.footer_text.clone());

    for (page_idx, page) in doc_pages.iter_mut().enumerate() {
        if let Some(ref hdr) = header_template {
            let hdr_top = (margin_top - 28.0).max(10.0);
            let hdr_bottom = (margin_top - 10.0).max(20.0);
            if let Ok(hdr_bbox) = Rect::new(
                margin_left,
                hdr_top,
                margin_left + content_width,
                hdr_bottom,
            ) {
                let overlay_id = ObjectId::new(
                    "hdr",
                    document_digest,
                    &format!("page[{}]/header", page.number),
                );
                page.overlays.push(Overlay {
                    id: overlay_id,
                    kind: OverlayKind::Header,
                    page: page.number,
                    bbox: Some(hdr_bbox),
                    text: hdr.clone(),
                    source: SourceSpan::new("header"),
                });
                laid_pages[page_idx].runs.push(TextRunLayout {
                    text: hdr.clone(),
                    font_size: 9.0,
                    bold: false,
                    bbox: hdr_bbox,
                    color_argb: 0xFF606060,
                });
            }
        }

        if let Some(ref ftr) = footer_template {
            let resolved_ftr = ftr.replace("[PAGE]", &format!("{}", page.number));
            let ftr_top = (page_height - margin_bottom + 10.0).min(page_height - 24.0);
            let ftr_bottom = (page_height - margin_bottom + 26.0).min(page_height - 6.0);
            if let Ok(ftr_bbox) = Rect::new(
                margin_left,
                ftr_top,
                margin_left + content_width,
                ftr_bottom,
            ) {
                let overlay_id = ObjectId::new(
                    "ftr",
                    document_digest,
                    &format!("page[{}]/footer", page.number),
                );
                page.overlays.push(Overlay {
                    id: overlay_id,
                    kind: OverlayKind::Footer,
                    page: page.number,
                    bbox: Some(ftr_bbox),
                    text: resolved_ftr.clone(),
                    source: SourceSpan::new("footer"),
                });
                laid_pages[page_idx].runs.push(TextRunLayout {
                    text: resolved_ftr,
                    font_size: 9.0,
                    bold: false,
                    bbox: ftr_bbox,
                    color_argb: 0xFF606060,
                });
            }
        }
    }
}

fn anchor_links_and_comments(
    document_digest: &str,
    blocks: &[Block],
    links: &mut [docsight_core::Hyperlink],
    comments: &mut [docsight_core::Comment],
    doc_pages: &mut [Page],
    warnings: &mut Vec<Diagnostic>,
) {
    for link in links.iter_mut() {
        let anchor_page = link
            .anchor_path
            .as_deref()
            .and_then(|path| blocks.iter().find(|b| b.source.path == path))
            .and_then(|block| block.page);
        match anchor_page {
            Some(page) => link.page = Some(page),
            None => warnings.push(Diagnostic {
                code: "DOCX_LINK_PAGE_UNRESOLVED".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "hyperlink {} could not be anchored to a placed block",
                    link.id
                ),
                effect: "the link has no page attribution; geometry is unknown".to_owned(),
                object: Some(link.id.clone()),
                page: None,
            }),
        }
    }
    for comment in comments.iter_mut() {
        let anchor = comment
            .anchor_path
            .as_deref()
            .and_then(|path| blocks.iter().find(|b| b.source.path == path));
        let anchor_page = anchor.and_then(|block| block.page);
        comment.page = anchor_page;
        if let (Some(page), Some(anchor_block)) = (anchor_page, anchor) {
            let bbox_top = anchor_block
                .bbox
                .as_ref()
                .map(|bbox| bbox.y0)
                .unwrap_or(0.0);
            let bbox_bottom = (bbox_top + 10.0).min(
                anchor_block
                    .bbox
                    .as_ref()
                    .map_or(0.0, |b| b.y1)
                    .max(bbox_top + 10.0),
            );
            let bbox = Rect::new(0.0, bbox_top, 12.0, bbox_bottom).ok();
            if let Some(doc_page) = doc_pages
                .iter_mut()
                .find(|candidate| candidate.number == page)
            {
                doc_page.overlays.push(Overlay {
                    id: ObjectId::new(
                        "cmt",
                        document_digest,
                        &format!("page[{page}]/{}", comment.id),
                    ),
                    kind: OverlayKind::CommentMarker,
                    page,
                    bbox,
                    text: comment.text.clone(),
                    source: comment.source.clone(),
                });
            }
        }
    }
}

struct ParagraphMetrics {
    line_height: f32,
    space_before: f32,
    space_after: f32,
    indent_left: f32,
    indent_first_line: f32,
    text_width: f32,
    alignment: TextAlignment,
}

fn paragraph_metrics(
    format: &ParagraphFormat,
    content_width: f32,
    default_line_height: f32,
    default_space_before: f32,
    default_space_after: f32,
) -> ParagraphMetrics {
    let line_height = match format.line_spacing {
        Some(multiple) if multiple.is_finite() && multiple > 0.0 => default_line_height * multiple,
        _ => default_line_height,
    };
    let indent_left = format.indent_left_pt.unwrap_or(0.0).max(0.0);
    let indent_right = format.indent_right_pt.unwrap_or(0.0).max(0.0);
    let text_width = (content_width - indent_left - indent_right).max(1.0);
    ParagraphMetrics {
        line_height,
        space_before: format
            .space_before_pt
            .unwrap_or(default_space_before)
            .max(0.0),
        space_after: format
            .space_after_pt
            .unwrap_or(default_space_after)
            .max(0.0),
        indent_left,
        indent_first_line: format.indent_first_line_pt.unwrap_or(0.0),
        text_width,
        alignment: format.alignment.unwrap_or(TextAlignment::Left),
    }
}

fn aligned_line_x(
    metrics: &ParagraphMetrics,
    margin_left: f32,
    line_index: usize,
    line_width: f32,
) -> f32 {
    let first_line_offset = if line_index == 0 {
        metrics.indent_first_line
    } else {
        0.0
    };
    let left = margin_left + metrics.indent_left + first_line_offset.max(-metrics.indent_left);
    let available = (metrics.text_width - first_line_offset.max(0.0)).max(1.0);
    match metrics.alignment {
        TextAlignment::Left | TextAlignment::Justify => left,
        TextAlignment::Center => left + ((available - line_width) / 2.0).max(0.0),
        TextAlignment::Right => left + (available - line_width).max(0.0),
    }
}

fn wrap_paragraph(text: &str, font_size: f32, metrics: &ParagraphMetrics) -> Vec<String> {
    let first_width = (metrics.text_width - metrics.indent_first_line.max(0.0)).max(1.0);
    if (first_width - metrics.text_width).abs() < f32::EPSILON {
        return wrap_text(text, font_size, metrics.text_width);
    }
    let mut lines = wrap_text(text, font_size, first_width);
    if lines.len() <= 1 {
        return lines;
    }
    let remainder = lines.split_off(1).join(" ");
    lines.extend(wrap_text(&remainder, font_size, metrics.text_width));
    lines
}

fn measure_height(
    block: &Block,
    content_width: f32,
    styles: &[Style],
    resources: &[docsight_core::Resource],
) -> Result<f32, DocsightError> {
    let mut copy = block.clone();
    Ok(emit_block(
        &mut copy,
        content_width,
        0.0,
        0.0,
        &mut Vec::new(),
        styles,
        resources,
    )?
    .height)
}

struct EmittedBlock {
    height: f32,
    runs: Vec<TextRunLayout>,
    borders: Vec<BorderLayout>,
    images: Vec<ImageLayout>,
}

impl EmittedBlock {
    fn new(height: f32, runs: Vec<TextRunLayout>, borders: Vec<BorderLayout>) -> Self {
        Self {
            height,
            runs,
            borders,
            images: Vec::new(),
        }
    }
}

fn emit_block(
    block: &mut Block,
    content_width: f32,
    margin_left: f32,
    base_y: f32,
    warnings: &mut Vec<Diagnostic>,
    styles: &[Style],
    resources: &[docsight_core::Resource],
) -> Result<EmittedBlock, DocsightError> {
    let format = block.format;
    let block_id = block.id.clone();
    match &mut block.content {
        BlockContent::Paragraph(p) => {
            let style = find_style(styles, p.style_id.as_deref());
            let font_size = style.and_then(|value| value.font_size_pt).unwrap_or(11.0);
            let bold = style.and_then(|value| value.bold).unwrap_or(false);
            let metrics = paragraph_metrics(
                &format,
                content_width,
                (font_size * 1.27).max(font_size + 2.0),
                0.0,
                4.0,
            );
            let lines = wrap_paragraph(&p.text, font_size, &metrics);
            let height = (lines.len() as f32 * metrics.line_height
                + metrics.space_before
                + metrics.space_after)
                .max(1.0);
            let mut runs = Vec::with_capacity(lines.len());
            for (i, line) in lines.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line_y = base_y + metrics.space_before + i as f32 * metrics.line_height;
                let line_w = text_width(&line, font_size).max(1.0);
                let line_x = aligned_line_x(&metrics, margin_left, i, line_w);
                let bbox = Rect::new(
                    line_x,
                    line_y,
                    line_x + line_w,
                    line_y + metrics.line_height,
                )?;
                runs.push(TextRunLayout {
                    text: line,
                    font_size,
                    bold,
                    bbox,
                    color_argb: 0xFF000000,
                });
            }
            Ok(EmittedBlock::new(height, runs, Vec::new()))
        }
        BlockContent::Heading(h) => {
            let (default_font_size, default_line_height, space_before, space_after) = match h.level
            {
                1 => (16.0_f32, 20.0_f32, 10.0_f32, 5.0_f32),
                2 => (13.0_f32, 17.0_f32, 8.0_f32, 4.0_f32),
                _ => (12.0_f32, 15.0_f32, 6.0_f32, 3.0_f32),
            };
            let style = find_style(styles, h.style_id.as_deref());
            let font_size = style
                .and_then(|value| value.font_size_pt)
                .unwrap_or(default_font_size);
            let line_height = style
                .and_then(|value| value.font_size_pt)
                .map(|size| (size * 1.25).max(size + 2.0))
                .unwrap_or(default_line_height);
            let bold = style.and_then(|value| value.bold).unwrap_or(true);
            let metrics = paragraph_metrics(
                &format,
                content_width,
                line_height,
                space_before,
                space_after,
            );
            let lines = wrap_paragraph(&h.text, font_size, &metrics);
            let height = (lines.len() as f32 * metrics.line_height
                + metrics.space_before
                + metrics.space_after)
                .max(1.0);
            let mut runs = Vec::with_capacity(lines.len());
            for (i, line) in lines.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line_y = base_y + metrics.space_before + i as f32 * metrics.line_height;
                let line_w = text_width(&line, font_size).max(1.0);
                let line_x = aligned_line_x(&metrics, margin_left, i, line_w);
                let bbox = Rect::new(
                    line_x,
                    line_y,
                    line_x + line_w,
                    line_y + metrics.line_height,
                )?;
                runs.push(TextRunLayout {
                    text: line,
                    font_size,
                    bold,
                    bbox,
                    color_argb: 0xFF000000,
                });
            }
            Ok(EmittedBlock::new(height, runs, Vec::new()))
        }
        BlockContent::ListItem(li) => {
            let style = find_style(styles, li.style_id.as_deref());
            let font_size = style.and_then(|value| value.font_size_pt).unwrap_or(11.0);
            let bold = style.and_then(|value| value.bold).unwrap_or(false);
            let line_height = (font_size * 1.27).max(font_size + 2.0);
            let space_after = 3.0_f32;
            let indent = (li.level as f32 + 1.0) * 18.0;
            let item_w = (content_width - indent).max(50.0);
            let lines = wrap_text(&li.text, font_size, item_w);
            let height = (lines.len() as f32 * line_height + space_after).max(1.0);
            let mut runs = Vec::with_capacity(lines.len() + 1);

            let marker_text = li.marker.as_deref().unwrap_or("-");
            let marker_w = text_width(marker_text, font_size).max(1.0);
            let marker_bbox = Rect::new(
                margin_left + indent - 14.0,
                base_y,
                margin_left + indent - 14.0 + marker_w,
                base_y + line_height,
            )?;
            runs.push(TextRunLayout {
                text: marker_text.to_owned(),
                font_size,
                bold: true,
                bbox: marker_bbox,
                color_argb: 0xFF000000,
            });

            for (i, line) in lines.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line_y = base_y + i as f32 * line_height;
                let line_w = text_width(&line, font_size).max(1.0);
                let bbox = Rect::new(
                    margin_left + indent,
                    line_y,
                    margin_left + indent + line_w,
                    line_y + line_height,
                )?;
                runs.push(TextRunLayout {
                    text: line,
                    font_size,
                    bold,
                    bbox,
                    color_argb: 0xFF000000,
                });
            }
            Ok(EmittedBlock::new(height, runs, Vec::new()))
        }
        BlockContent::Table(tbl) => {
            let cols = tbl.columns.max(1);
            let col_widths = match tbl.column_widths_pt.as_ref() {
                Some(widths) if widths.len() == cols as usize => {
                    let sum: f32 = widths.iter().sum();
                    if sum > 0.0 {
                        widths.iter().map(|w| w / sum * content_width).collect()
                    } else {
                        vec![content_width / cols as f32; cols as usize]
                    }
                }
                Some(widths) => {
                    warnings.push(Diagnostic {
                        code: "DOCX_TABLE_GRID_WIDTHS_UNUSABLE".to_owned(),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "table grid declares {} columns but layout computed {cols}",
                            widths.len()
                        ),
                        effect: "columns fall back to equal widths".to_owned(),
                        object: Some(block.id.clone()),
                        page: block.page,
                    });
                    vec![content_width / cols as f32; cols as usize]
                }
                None => vec![content_width / cols as f32; cols as usize],
            };
            let col_w = |index: u32| -> f32 {
                col_widths
                    .get(index as usize)
                    .copied()
                    .unwrap_or(content_width / cols as f32)
            };
            let padding = 4.0_f32;
            let font_size = 9.5_f32;
            let line_height = 12.0_f32;

            let row_count = tbl.rows.max(1) as usize;
            let mut row_heights = vec![18.0_f32; row_count];

            for cell in &tbl.cells {
                let cell_w = (cell.column..cell.column + cell.column_span)
                    .map(col_w)
                    .sum::<f32>();
                let usable_w = (cell_w - padding * 2.0).max(10.0);
                let lines = wrap_text(&cell.text, font_size, usable_w);
                let cell_h = lines.len() as f32 * line_height + padding * 2.0;
                let r = cell.row as usize;
                if r < row_heights.len() {
                    let per_row = cell_h / cell.row_span.max(1) as f32;
                    if per_row > row_heights[r] {
                        row_heights[r] = per_row;
                    }
                }
            }

            let mut row_y = Vec::with_capacity(row_heights.len() + 1);
            let mut acc_y = base_y;
            for h in &row_heights {
                row_y.push(acc_y);
                acc_y += *h;
            }
            row_y.push(acc_y);
            let total_table_h = acc_y - base_y + 6.0;

            let mut runs = Vec::new();
            let mut borders = Vec::new();

            for cell in &mut tbl.cells {
                let cell_x0 = margin_left + (0..cell.column).map(col_w).sum::<f32>();
                let cell_x1 = cell_x0
                    + (cell.column..cell.column + cell.column_span)
                        .map(col_w)
                        .sum::<f32>();
                let r0 = (cell.row as usize).min(row_y.len() - 1);
                let r1 = ((cell.row + cell.row_span) as usize).min(row_y.len() - 1);
                let cell_y0 = row_y[r0];
                let cell_y1 = row_y[r1];

                let cell_rect = Rect::new(cell_x0, cell_y0, cell_x1, cell_y1)?;
                cell.bbox = Some(cell_rect);

                borders.push(BorderLayout {
                    rect: cell_rect,
                    color_argb: 0xFFB0B0B0,
                });

                let usable_w = (cell_x1 - cell_x0 - padding * 2.0).max(10.0);
                let lines = wrap_text(&cell.text, font_size, usable_w);
                for (i, line) in lines.into_iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let text_y = cell_y0 + padding + i as f32 * line_height;
                    let text_w = text_width(&line, font_size).max(1.0);
                    let max_x1 = (cell_x0 + padding + text_w).min(cell_x1 - 1.0);
                    let text_x1 = if max_x1 > cell_x0 + padding {
                        max_x1
                    } else {
                        cell_x0 + padding + 1.0
                    };
                    let text_bbox =
                        Rect::new(cell_x0 + padding, text_y, text_x1, text_y + line_height)?;
                    runs.push(TextRunLayout {
                        text: line,
                        font_size,
                        bold: cell.row < tbl.header_rows,
                        bbox: text_bbox,
                        color_argb: 0xFF000000,
                    });
                }
            }

            Ok(EmittedBlock::new(total_table_h, runs, borders))
        }
        BlockContent::Figure(fig) => {
            let width = fig.width_pt.unwrap_or(240.0).min(content_width).max(20.0);
            let height = fig.height_pt.unwrap_or(120.0).max(20.0);
            let total_height = height + 12.0;
            let fig_rect = Rect::new(
                margin_left,
                base_y + 6.0,
                margin_left + width,
                base_y + 6.0 + height,
            )?;
            let border = BorderLayout {
                rect: fig_rect,
                color_argb: 0xFF808080,
            };
            let label = fig
                .caption
                .as_deref()
                .or(fig.alt_text.as_deref())
                .unwrap_or("[FIGURE]");
            let label_w = text_width(label, 9.0).max(1.0);
            let label_bbox = Rect::new(
                margin_left + 4.0,
                base_y + 10.0,
                (margin_left + 4.0 + label_w).min(margin_left + width - 2.0),
                base_y + 22.0,
            )?;
            let target = fig.resource_id.as_deref().and_then(|id| {
                resources
                    .iter()
                    .find(|resource| resource.name == id)
                    .map(|resource| resource.target.clone())
            });
            if let Some(target) = target {
                return Ok(EmittedBlock {
                    height: total_height,
                    runs: Vec::new(),
                    borders: vec![border],
                    images: vec![ImageLayout {
                        rect: fig_rect,
                        target,
                        object_id: block_id,
                    }],
                });
            }
            let run = TextRunLayout {
                text: label.to_owned(),
                font_size: 9.0,
                bold: true,
                bbox: label_bbox,
                color_argb: 0xFF404040,
            };
            Ok(EmittedBlock::new(total_height, vec![run], vec![border]))
        }
        BlockContent::Note(note) => {
            let prefix = match note.kind {
                docsight_core::NoteKind::Footnote => format!("[Footnote {}] ", note.note_id),
                docsight_core::NoteKind::Endnote => format!("[Endnote {}] ", note.note_id),
            };
            let full_text = format!("{}{}", prefix, note.text);
            let font_size = 9.0_f32;
            let line_height = 12.0_f32;
            let space_after = 3.0_f32;
            let lines = wrap_text(&full_text, font_size, content_width);
            let height = (lines.len() as f32 * line_height + space_after).max(1.0);
            let mut runs = Vec::with_capacity(lines.len());
            for (i, line) in lines.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line_y = base_y + i as f32 * line_height;
                let line_w = text_width(&line, font_size).max(1.0);
                let bbox = Rect::new(
                    margin_left,
                    line_y,
                    margin_left + line_w,
                    line_y + line_height,
                )?;
                runs.push(TextRunLayout {
                    text: line,
                    font_size,
                    bold: false,
                    bbox,
                    color_argb: 0xFF404040,
                });
            }
            Ok(EmittedBlock::new(height, runs, Vec::new()))
        }
        BlockContent::Shape(_) | BlockContent::Unknown(_) => {
            let height = 24.0_f32;
            Ok(EmittedBlock::new(height, Vec::new(), Vec::new()))
        }
    }
}

fn find_style<'a>(styles: &'a [Style], style_id: Option<&str>) -> Option<&'a Style> {
    let id = style_id?;
    styles.iter().find(|style| style.id == id)
}
