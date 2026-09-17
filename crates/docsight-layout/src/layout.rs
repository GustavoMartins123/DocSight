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

pub fn layout_docx(mut doc: Document) -> Result<LaidOutDocument, DocsightError> {
    let ranges = section_ranges(&doc)?;
    let geometries = doc
        .sections
        .iter()
        .map(SectionGeometry::from_section)
        .collect::<Result<Vec<_>, _>>()?;
    let mut measured = Vec::with_capacity(doc.blocks.len());
    for range in &ranges {
        for block in &doc.blocks[range.blocks.clone()] {
            measured.push(measure_block(
                block,
                &geometries[range.section],
                &doc.styles,
                &doc.resources,
            )?);
        }
    }
    let Pagination {
        pages: records,
        placements,
        warnings: pagination_warnings,
    } = paginate(&doc, &ranges, &geometries, &measured)?;

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
    ];
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
            let (height, runs, borders, images) = match metrics {
                Measured::Text(text) => (
                    text.fragment_height(&fragment.lines),
                    text.emit(&fragment.lines, fragment.top)?,
                    Vec::new(),
                    Vec::new(),
                ),
                Measured::Atomic { .. } => {
                    block.page = Some(fragment.page);
                    let emitted = emit_atomic(
                        &mut block,
                        &geometry,
                        fragment.top,
                        &mut warnings,
                        &doc.resources,
                    )?;
                    (
                        emitted.height,
                        emitted.runs,
                        emitted.borders,
                        emitted.images,
                    )
                }
            };
            let bbox = Rect::new(
                geometry.margin_left,
                fragment.top,
                geometry.margin_left + geometry.content_width(),
                fragment.top + height,
            )?;
            let doc_page = doc_pages
                .get_mut(page_index)
                .ok_or_else(|| page_outside_allocation(fragment.page))?;
            if fragment_index == 0 {
                block.page = Some(fragment.page);
                block.bbox = Some(bbox);
                doc_page.block_ids.push(block.id.clone());
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
                };
                block.continuations.push(BlockContinuation {
                    page: fragment.page,
                    bbox,
                    text_start_char,
                });
                doc_page.continued_block_ids.push(block.id.clone());
            }
            let laid_page = laid_pages
                .get_mut(page_index)
                .ok_or_else(|| page_outside_allocation(fragment.page))?;
            laid_page.runs.extend(runs);
            laid_page.borders.extend(borders);
            laid_page.images.extend(images);
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
        },
        &mut doc_pages,
        &mut laid_pages,
        &mut warnings,
    )?;

    doc.pages = doc_pages;
    doc.blocks = updated_blocks;
    doc.warnings.append(&mut warnings);

    validate_canonical(&doc)?;

    Ok(LaidOutDocument {
        document: doc,
        pages: laid_pages,
    })
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
