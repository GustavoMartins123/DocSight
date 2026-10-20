use crate::geometry::{SectionGeometry, section_ranges};
use crate::headers::{HeaderFooterContext, project_headers_footers};
use crate::measure::{Measured, emit_atomic, measure_block};
use crate::paginate::{Pagination, paginate};
use docsight_core::{
    Block, BlockContinuation, Diagnostic, DiagnosticSeverity, DocsightError, Document, ObjectId,
    Overlay, OverlayKind, Page, Rect, validate_canonical,
};

pub const MAX_LAYOUT_PAGES: u32 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct TextRunLayout {
    pub text: String,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub font_family: String,
    pub bbox: Rect,
    pub color_argb: u32,
    pub baseline: f32,
    pub underline: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BorderLayout {
    pub rect: Rect,
    pub color_argb: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FillLayout {
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
    pub fills: Vec<FillLayout>,
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
    pub fonts: docsight_fonts::FontBook,
}

pub fn layout_docx(doc: Document) -> Result<LaidOutDocument, DocsightError> {
    let fonts = docsight_fonts::FontBook::normalized()?;
    let mut hints = crate::headers::LayoutHints {
        total: 1,
        sections: vec![1; doc.sections.len()],
    };
    for _ in 0..16 {
        let (laid_out, actual) = layout_pass(doc.clone(), &hints, &fonts)?;
        if hints == actual {
            return Ok(laid_out);
        }
        hints = actual;
    }
    Err(DocsightError::ResourceLimit {
        resource: "DOCX pagination stabilization passes".to_owned(),
        limit: 16,
    })
}

fn layout_pass(
    mut doc: Document,
    hints: &crate::headers::LayoutHints,
    fonts: &docsight_fonts::FontBook,
) -> Result<(LaidOutDocument, crate::headers::LayoutHints), DocsightError> {
    let ranges = section_ranges(&doc)?;
    let geometries = doc
        .sections
        .iter()
        .map(SectionGeometry::from_section)
        .collect::<Result<Vec<_>, _>>()?;
    let mut measured = Vec::with_capacity(doc.blocks.len());
    for range in &ranges {
        for block in &doc.blocks[range.blocks.clone()] {
            measured.push(
                measure_block(
                    block,
                    &geometries[range.section],
                    &doc.styles,
                    &doc.resources,
                    fonts,
                )
                .map_err(|error| {
                    error.with_error_location(docsight_core::ErrorLocation {
                        object: Some(block.id.to_string()),
                        ..Default::default()
                    })
                })?,
            );
        }
    }
    for range in &ranges {
        for index in range.blocks.start + 1..range.blocks.end {
            let previous = &doc.blocks[index - 1];
            let current = &doc.blocks[index];
            let style_id = |block: &Block| match &block.content {
                docsight_core::BlockContent::Paragraph(value) => value.style_id.clone(),
                docsight_core::BlockContent::Heading(value) => value.style_id.clone(),
                docsight_core::BlockContent::ListItem(value) => value.style_id.clone(),
                _ => None,
            };
            if style_id(previous) == style_id(current) {
                let (before, after) = measured.split_at_mut(index);
                if let Measured::Text(metrics) = &mut before[index - 1]
                    && previous.format.contextual_spacing == Some(true)
                {
                    metrics.space_after = 0.0;
                }
                if let Measured::Text(metrics) = &mut after[0]
                    && current.format.contextual_spacing == Some(true)
                {
                    metrics.space_before = 0.0;
                }
            }
        }
    }
    let Pagination {
        pages: records,
        placements,
        warnings: pagination_warnings,
    } = paginate(&doc, &ranges, &geometries, &measured, fonts, hints)?;

    let mut warnings = vec![Diagnostic {
        code: "DOCX_LAYOUT_PAGINATED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: "DOCX pagination was computed from source formatting and resolved OpenType fonts".to_owned(),
        effect: "page geometry is inferred by DocSight; equivalence to an authoring application has not been established".to_owned(),
        object: None,
        page: None,
        occurrences: None,
    }];
    warnings.push(Diagnostic::warning(
        "DOCX_FONT_NORMALIZED",
        format!("DOCX visual text uses bundled DejaVu Sans, Liberation Serif and Liberation Mono under {}", docsight_fonts::NORMALIZED_PROFILE),
        "source font names remain in the IR; line breaks, pagination and geometry describe the normalized view, not the authoring application's appearance",
    ));
    warnings.extend(pagination_warnings);

    let mut laid_pages: Vec<LaidOutPage> = Vec::with_capacity(records.len());
    let mut doc_pages: Vec<Page> = Vec::with_capacity(records.len());
    for record in &records {
        laid_pages.push(LaidOutPage {
            number: record.number,
            width_pt: record.geometry.page_width,
            height_pt: record.geometry.page_height,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
            fills: Vec::new(),
        });
        doc_pages.push(Page {
            number: record.number,
            width_pt: record.geometry.page_width,
            height_pt: record.geometry.page_height,
            section_index: Some(doc.sections[record.section].section_index),
            block_ids: Vec::new(),
            continued_block_ids: Vec::new(),
            overlays: Vec::new(),
        });
    }

    let source_blocks = std::mem::take(&mut doc.blocks);
    if source_blocks.len() != placements.len() || measured.len() != placements.len() {
        return Err(DocsightError::BackendFailure {
            backend: "docsight-layout".to_owned(),
            message: "pagination did not place every DOCX block exactly once".to_owned(),
        });
    }
    let mut updated_blocks: Vec<Block> = Vec::with_capacity(source_blocks.len());
    for (index, ((mut block, placement), metrics)) in source_blocks
        .into_iter()
        .zip(placements)
        .zip(&measured)
        .enumerate()
    {
        let geometry = geometries[placement.section];
        block.reading_order =
            u32::try_from(index + 1).map_err(|_| DocsightError::ResourceLimit {
                resource: "DOCX layout blocks".to_owned(),
                limit: u64::from(u32::MAX),
            })?;
        block.continuations.clear();
        for (fragment_index, fragment) in placement.fragments.iter().enumerate() {
            let page_index = page_position(fragment.page)?;
            let x_offset =
                fragment.column as f32 * (geometry.content_width() + geometry.column_gap);
            let mut fills = Vec::new();
            let (height, mut runs, mut borders, mut images) = match metrics {
                Measured::Text(text) => (
                    text.fragment_height(&fragment.lines),
                    text.emit(&fragment.lines, fragment.top, fonts)?,
                    Vec::new(),
                    Vec::new(),
                ),
                Measured::Table(table) => {
                    let (start, end) =
                        fragment
                            .table_slice
                            .ok_or_else(|| DocsightError::BackendFailure {
                                backend: "docsight-layout".to_owned(),
                                message: "table fragment has no measured slice".to_owned(),
                            })?;
                    let delta = fragment.top - start;
                    if fragment_index == 0
                        && let docsight_core::BlockContent::Table(current) = &mut block.content
                    {
                        *current = table.table.clone();
                        crate::tables::clear_placements(current);
                    }
                    if let docsight_core::BlockContent::Table(current) = &mut block.content {
                        crate::tables::project_cells(
                            current,
                            &table.table,
                            table,
                            &crate::tables::TableFragment {
                                page: fragment.page,
                                start,
                                end,
                                dx: x_offset,
                                dy: delta,
                            },
                        )?;
                    }
                    let mut runs = Vec::new();
                    for run in &table.emitted.runs {
                        if run.bbox.y0 >= start - 0.001 && run.bbox.y1 <= end + 0.001 {
                            let mut run = run.clone();
                            crate::tables::translate_run(&mut run, 0.0, delta);
                            runs.push(run);
                        }
                    }
                    let mut borders = Vec::new();
                    for border in &table.emitted.borders {
                        let y0 = border.rect.y0.max(start);
                        let y1 = border.rect.y1.min(end);
                        if y1 > y0 {
                            borders.push(BorderLayout {
                                rect: Rect::new(
                                    border.rect.x0,
                                    y0 + delta,
                                    border.rect.x1,
                                    y1 + delta,
                                )?,
                                color_argb: border.color_argb,
                            });
                        }
                    }
                    let mut images = Vec::new();
                    for fill in &table.emitted.fills {
                        let y0 = fill.rect.y0.max(start);
                        let y1 = fill.rect.y1.min(end);
                        if y1 > y0 {
                            fills.push(FillLayout {
                                rect: Rect::new(
                                    fill.rect.x0,
                                    y0 + delta,
                                    fill.rect.x1,
                                    y1 + delta,
                                )?,
                                color_argb: fill.color_argb,
                            });
                        }
                    }
                    for image in &table.emitted.images {
                        if image.rect.y0 >= start && image.rect.y1 <= end {
                            let mut image = image.clone();
                            image.rect = crate::tables::translate(image.rect, 0.0, delta)?;
                            images.push(image);
                        }
                    }
                    (end - start, runs, borders, images)
                }
                Measured::Atomic { .. } => {
                    block.page = Some(fragment.page);
                    let emitted = emit_atomic(&mut block, &geometry, fragment.top, &doc.resources)?;
                    (
                        emitted.height,
                        emitted.runs,
                        emitted.borders,
                        emitted.images,
                    )
                }
            };
            let bbox = if matches!(block.content, docsight_core::BlockContent::Figure(_)) {
                let image = images
                    .first()
                    .ok_or_else(|| DocsightError::BackendFailure {
                        backend: "docsight-layout".to_owned(),
                        message: format!("figure {} has no image placement", block.id),
                    })?;
                crate::tables::translate(image.rect, x_offset, 0.0)?
            } else {
                Rect::new(
                    geometry.margin_left + x_offset,
                    fragment.top,
                    geometry.margin_left + x_offset + geometry.content_width(),
                    fragment.top + height,
                )?
            };
            let doc_page = doc_pages
                .get_mut(page_index)
                .ok_or_else(|| page_outside_allocation(fragment.page))?;
            if fragment_index == 0 {
                block.page = Some(fragment.page);
                block.bbox = Some(bbox);
                doc_page.block_ids.push(block.id.clone());
            } else if block.page == Some(fragment.page) {
                if let Some(primary) = block.bbox {
                    block.bbox = Some(Rect::new(
                        primary.x0.min(bbox.x0),
                        primary.y0.min(bbox.y0),
                        primary.x1.max(bbox.x1),
                        primary.y1.max(bbox.y1),
                    )?);
                }
            } else if block
                .continuations
                .last()
                .is_some_and(|previous| previous.page == fragment.page)
            {
                if let Some(previous) = block.continuations.last_mut() {
                    previous.bbox = Rect::new(
                        previous.bbox.x0.min(bbox.x0),
                        previous.bbox.y0.min(bbox.y0),
                        previous.bbox.x1.max(bbox.x1),
                        previous.bbox.y1.max(bbox.y1),
                    )?;
                }
            } else {
                let text_start_char = match metrics {
                    Measured::Text(text) => text
                        .lines
                        .get(fragment.lines.start)
                        .map(|line| line.start_char)
                        .ok_or_else(|| DocsightError::BackendFailure {
                            backend: "docsight-layout".to_owned(),
                            message: format!(
                                "continuation of block {} has no first line",
                                block.id
                            ),
                        })?,
                    Measured::Atomic { .. } => {
                        return Err(DocsightError::BackendFailure {
                            backend: "docsight-layout".to_owned(),
                            message: format!("atomic block {} was split across pages", block.id),
                        });
                    }
                    Measured::Table(table) => table.text_start(
                        fragment
                            .table_slice
                            .ok_or_else(|| DocsightError::BackendFailure {
                                backend: "docsight-layout".to_owned(),
                                message: "table continuation has no source slice".to_owned(),
                            })?
                            .0,
                    ),
                };
                block.continuations.push(BlockContinuation {
                    page: fragment.page,
                    bbox,
                    text_start_char,
                });
                doc_page.continued_block_ids.push(block.id.clone());
            }
            for run in &mut runs {
                crate::tables::translate_run(run, x_offset, 0.0);
            }
            for border in &mut borders {
                border.rect = crate::tables::translate(border.rect, x_offset, 0.0)?;
            }
            for image in &mut images {
                image.rect = crate::tables::translate(image.rect, x_offset, 0.0)?;
            }
            for fill in &mut fills {
                fill.rect = crate::tables::translate(fill.rect, x_offset, 0.0)?;
            }
            let laid_page = laid_pages
                .get_mut(page_index)
                .ok_or_else(|| page_outside_allocation(fragment.page))?;
            laid_page.runs.extend(runs);
            laid_page.borders.extend(borders);
            laid_page.images.extend(images);
            laid_page.fills.extend(fills);
        }
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
        &HeaderFooterContext {
            document_digest: &doc.sha256,
            sections: &doc.sections,
            records: &records,
            styles: &doc.styles,
            resources: &doc.resources,
            fonts,
        },
        &mut doc_pages,
        &mut laid_pages,
    )?;

    doc.pages = doc_pages;
    doc.blocks = updated_blocks;
    doc.warnings.append(&mut warnings);

    let mut section_counts = vec![0_u32; doc.sections.len()];
    for record in &records {
        for section in &record.sections {
            section_counts[*section] += 1;
        }
    }
    let counts = crate::headers::LayoutHints {
        total: u32::try_from(records.len()).map_err(|_| page_outside_allocation(u32::MAX))?,
        sections: section_counts,
    };
    let mut used = std::collections::BTreeSet::new();
    for page in &mut laid_pages {
        for run in &mut page.runs {
            let face = fonts.resolve(&run.font_family, run.bold, run.italic)?;
            run.text = face.decoded_text(&run.text)?;
            run.font_family = face.family.clone();
            if used.insert(face.sha256.clone()) {
                let target = format!("opentype:{}", face.sha256);
                doc.resources.push(docsight_core::Resource {
                    id: ObjectId::new("font", &doc.sha256, &target),
                    kind: docsight_core::ResourceKind::Font,
                    name: format!("{};bold={};italic={}", face.family, face.bold, face.italic),
                    target,
                    mime_type: Some("font/ttf".to_owned()),
                    content_sha256: Some(face.sha256),
                });
            }
        }
    }
    validate_canonical(&doc)?;
    Ok((
        LaidOutDocument {
            document: doc,
            pages: laid_pages,
            fonts: fonts.clone(),
        },
        counts,
    ))
}

fn page_position(page: u32) -> Result<usize, DocsightError> {
    let index = page
        .checked_sub(1)
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "DOCX layout produced page zero".to_owned(),
        })?;
    usize::try_from(index).map_err(|_| DocsightError::ResourceLimit {
        resource: "DOCX layout page index".to_owned(),
        limit: u64::from(MAX_LAYOUT_PAGES),
    })
}

fn page_outside_allocation(page: u32) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("DOCX layout page {page} is outside the allocated page set"),
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
                occurrences: None,
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
