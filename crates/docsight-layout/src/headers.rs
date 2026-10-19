use crate::geometry::SectionGeometry;
use crate::layout::LaidOutPage;
use crate::measure::{EmittedBlock, Measured, emit_atomic, measure_block};
use crate::paginate::PageRecord;
use docsight_core::{
    DocsightError, HeaderFooterKind, HeaderFooterVariant, ObjectId, Overlay, OverlayKind, Page,
    Rect, Section, SectionHeaderFooter, SourceSpan,
};

const PAGE_PLACEHOLDER: &str = "[PAGE]";
const NUMPAGES_PLACEHOLDER: &str = "[NUMPAGES]";
const SECTIONPAGES_PLACEHOLDER: &str = "[SECTIONPAGES]";

pub(crate) struct HeaderFooterContext<'a> {
    pub document_digest: &'a str,
    pub sections: &'a [Section],
    pub records: &'a [PageRecord],
    pub styles: &'a [docsight_core::Style],
    pub resources: &'a [docsight_core::Resource],
    pub fonts: &'a docsight_fonts::FontBook,
}

pub(crate) fn project_headers_footers(
    context: &HeaderFooterContext<'_>,
    doc_pages: &mut [Page],
    laid_pages: &mut [LaidOutPage],
) -> Result<(), DocsightError> {
    let total_pages =
        u32::try_from(context.records.len()).map_err(|_| DocsightError::ResourceLimit {
            resource: "layout page count".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
    let mut section_pages = vec![0_u32; context.sections.len()];
    for record in context.records {
        for section in &record.sections {
            let count =
                section_pages
                    .get_mut(*section)
                    .ok_or_else(|| DocsightError::BackendFailure {
                        backend: "docsight-layout".to_owned(),
                        message: "page record references an undeclared section".to_owned(),
                    })?;
            *count = count
                .checked_add(1)
                .ok_or_else(|| DocsightError::ResourceLimit {
                    resource: "layout section page count".to_owned(),
                    limit: u64::from(u32::MAX),
                })?;
        }
    }

    for ((record, doc_page), laid_page) in context
        .records
        .iter()
        .zip(doc_pages.iter_mut())
        .zip(laid_pages.iter_mut())
    {
        let section = &context.sections[record.section];
        let section_pages = section_pages[record.section];
        for kind in [HeaderFooterKind::Header, HeaderFooterKind::Footer] {
            let Some(entry) = select_entry(section, record, kind) else {
                continue;
            };
            let display = docsight_core::format_ooxml_number(
                record.display_number,
                section.page_number_format.as_deref(),
            )?;
            let text = entry
                .text
                .replace(PAGE_PLACEHOLDER, &display)
                .replace(NUMPAGES_PLACEHOLDER, &total_pages.to_string())
                .replace(SECTIONPAGES_PLACEHOLDER, &section_pages.to_string());
            if text.trim().is_empty() {
                continue;
            }
            project_entry(
                context.document_digest,
                record,
                entry,
                kind,
                text,
                doc_page,
                laid_page,
                context,
                total_pages,
                section_pages,
            )?;
        }
    }
    Ok(())
}

fn select_entry<'a>(
    section: &'a Section,
    record: &PageRecord,
    kind: HeaderFooterKind,
) -> Option<&'a SectionHeaderFooter> {
    let variant = if section.title_page && record.first_of_section {
        HeaderFooterVariant::First
    } else if section.even_and_odd_headers && record.display_number.is_multiple_of(2) {
        HeaderFooterVariant::Even
    } else {
        HeaderFooterVariant::Default
    };
    section.header_footer(kind, variant)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutHints {
    pub total: u32,
    pub sections: Vec<u32>,
}

#[allow(clippy::too_many_arguments)]
fn entry_layout(
    entry: &SectionHeaderFooter,
    record: &PageRecord,
    section: &Section,
    total: u32,
    section_pages: u32,
    styles: &[docsight_core::Style],
    resources: &[docsight_core::Resource],
    fonts: &docsight_fonts::FontBook,
) -> Result<EmittedBlock, DocsightError> {
    let display = docsight_core::format_ooxml_number(
        record.display_number,
        section.page_number_format.as_deref(),
    )?;
    if entry.blocks.is_empty() && !entry.text.is_empty() {
        return Err(DocsightError::MalformedDocument {
            message: format!(
                "header or footer {} has text without structural blocks",
                entry.part
            ),
        });
    }
    let replace = |text: &str| {
        text.replace(PAGE_PLACEHOLDER, &display)
            .replace(NUMPAGES_PLACEHOLDER, &total.to_string())
            .replace(SECTIONPAGES_PLACEHOLDER, &section_pages.to_string())
    };
    let mut output = EmittedBlock {
        height: 0.0,
        runs: Vec::new(),
        borders: Vec::new(),
        images: Vec::new(),
        fills: Vec::new(),
    };
    let geometry = SectionGeometry {
        columns: 1,
        column_gap: 0.0,
        ..record.geometry
    };
    for block in &entry.blocks {
        let mut block = block.clone();
        replace_fields(&mut block, &replace);
        let measured = measure_block(&block, &geometry, styles, resources, fonts)?;
        let emitted = match measured {
            Measured::Text(text) => EmittedBlock {
                height: text.full_height(),
                runs: text.emit(&(0..text.line_count()), output.height, fonts)?,
                borders: Vec::new(),
                images: Vec::new(),
                fills: Vec::new(),
            },
            Measured::Table(table) => {
                let mut emitted = table.emitted;
                for run in &mut emitted.runs {
                    crate::tables::translate_run(run, 0.0, output.height);
                }
                for border in &mut emitted.borders {
                    border.rect = crate::tables::translate(border.rect, 0.0, output.height)?;
                }
                for fill in &mut emitted.fills {
                    fill.rect = crate::tables::translate(fill.rect, 0.0, output.height)?;
                }
                for image in &mut emitted.images {
                    image.rect = crate::tables::translate(image.rect, 0.0, output.height)?;
                }
                emitted
            }
            Measured::Atomic { .. } => {
                emit_atomic(&mut block, &geometry, output.height, resources)?
            }
        };
        output.height += emitted.height;
        output.runs.extend(emitted.runs);
        output.borders.extend(emitted.borders);
        output.images.extend(emitted.images);
        output.fills.extend(emitted.fills);
    }
    Ok(output)
}

fn replace_fields(block: &mut docsight_core::Block, replace: &impl Fn(&str) -> String) {
    match &mut block.content {
        docsight_core::BlockContent::Paragraph(paragraph) => {
            paragraph.text = replace(&paragraph.text)
        }
        docsight_core::BlockContent::Table(table) => {
            for cell in &mut table.cells {
                cell.text = replace(&cell.text);
                for child in &mut cell.blocks {
                    replace_fields(child, replace);
                }
            }
        }
        _ => {}
    }
    for run in &mut block.inline_runs {
        run.text = replace(&run.text);
    }
}

pub(crate) fn content_geometry(
    section: &Section,
    record: &PageRecord,
    document: &docsight_core::Document,
    fonts: &docsight_fonts::FontBook,
    hints: &LayoutHints,
) -> Result<SectionGeometry, DocsightError> {
    let mut geometry = record.geometry;
    for kind in [HeaderFooterKind::Header, HeaderFooterKind::Footer] {
        if let Some(entry) = select_entry(section, record, kind) {
            let measured = entry_layout(
                entry,
                record,
                section,
                hints.total,
                hints.sections[record.section],
                &document.styles,
                &document.resources,
                fonts,
            )?;
            if kind == HeaderFooterKind::Header {
                geometry.margin_top = geometry
                    .margin_top
                    .max(geometry.header_distance + measured.height);
            } else {
                geometry.margin_bottom = geometry
                    .margin_bottom
                    .max(geometry.footer_distance + measured.height);
            }
        }
    }
    if geometry.content_height() <= 0.0 {
        return Err(DocsightError::MalformedDocument {
            message: format!(
                "headers and footers leave no body area in section {}",
                section.section_index
            ),
        });
    }
    Ok(geometry)
}

#[allow(clippy::too_many_arguments)]
fn project_entry(
    document_digest: &str,
    record: &PageRecord,
    entry: &SectionHeaderFooter,
    kind: HeaderFooterKind,
    text: String,
    doc_page: &mut Page,
    laid_page: &mut LaidOutPage,
    context: &HeaderFooterContext<'_>,
    total_pages: u32,
    section_pages: u32,
) -> Result<(), DocsightError> {
    let measured = entry_layout(
        entry,
        record,
        &context.sections[record.section],
        total_pages,
        section_pages,
        context.styles,
        context.resources,
        context.fonts,
    )?;
    let geometry = record.geometry;
    let height = measured.height;
    let (top, overlay_kind, label) = match kind {
        HeaderFooterKind::Header => (geometry.header_distance, OverlayKind::Header, "header"),
        HeaderFooterKind::Footer => (
            geometry.page_height - geometry.footer_distance - height,
            OverlayKind::Footer,
            "footer",
        ),
    };
    let bbox = Rect::new(
        geometry.margin_left,
        top,
        geometry.margin_left + geometry.full_content_width(),
        top + height,
    )?;
    if top < 0.0 || top + height > geometry.page_height {
        return Err(DocsightError::MalformedDocument {
            message: format!("{label} on page {} extends outside its page", record.number),
        });
    }
    for mut run in measured.runs {
        crate::tables::translate_run(&mut run, 0.0, top);
        laid_page.runs.push(run);
    }
    for mut border in measured.borders {
        border.rect = crate::tables::translate(border.rect, 0.0, top)?;
        laid_page.borders.push(border);
    }
    for mut image in measured.images {
        image.rect = crate::tables::translate(image.rect, 0.0, top)?;
        laid_page.images.push(image);
    }
    for mut fill in measured.fills {
        fill.rect = crate::tables::translate(fill.rect, 0.0, top)?;
        laid_page.fills.push(fill);
    }
    doc_page.overlays.push(Overlay {
        id: ObjectId::new(
            if kind == HeaderFooterKind::Header {
                "hdr"
            } else {
                "ftr"
            },
            document_digest,
            &format!("page[{}]/{label}", record.number),
        ),
        kind: overlay_kind,
        page: record.number,
        bbox: Some(bbox),
        text,
        source: SourceSpan::new(entry.part.clone()),
    });
    Ok(())
}
