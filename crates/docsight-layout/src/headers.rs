use crate::font::text_width;
use crate::layout::{LaidOutPage, TextRunLayout};
use crate::paginate::PageRecord;
use docsight_core::{
    Diagnostic, DiagnosticSeverity, DocsightError, HeaderFooterKind, HeaderFooterVariant, ObjectId,
    Overlay, OverlayKind, Page, Rect, Section, SectionHeaderFooter, SourceSpan,
};
use std::collections::BTreeSet;

const HEADER_FOOTER_FONT_SIZE: f32 = 9.0;
const HEADER_FOOTER_LINE_HEIGHT: f32 = 12.0;
const HEADER_FOOTER_COLOR: u32 = 0xFF606060;
const PAGE_PLACEHOLDER: &str = "[PAGE]";
const NUMPAGES_PLACEHOLDER: &str = "[NUMPAGES]";
const SECTIONPAGES_PLACEHOLDER: &str = "[SECTIONPAGES]";
const MAX_ROMAN_PAGE_NUMBER: u32 = 3_999;
const MAX_LETTER_PAGE_NUMBER: u32 = 780;

pub(crate) struct HeaderFooterContext<'a> {
    pub document_digest: &'a str,
    pub sections: &'a [Section],
    pub records: &'a [PageRecord],
}

pub(crate) fn project_headers_footers(
    context: &HeaderFooterContext<'_>,
    doc_pages: &mut [Page],
    laid_pages: &mut [LaidOutPage],
    warnings: &mut Vec<Diagnostic>,
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
    let mut unsupported_formats = BTreeSet::new();
    let mut approximated_entries = BTreeSet::new();
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
            let display = match format_page_number(
                record.display_number,
                section.page_number_format.as_deref(),
            ) {
                Some(display) => display,
                None => {
                    if entry.text.contains(PAGE_PLACEHOLDER)
                        && unsupported_formats.insert(record.section)
                    {
                        warnings.push(unsupported_format_warning(section));
                    }
                    record.display_number.to_string()
                }
            };
            let text = entry
                .text
                .replace(PAGE_PLACEHOLDER, &display)
                .replace(NUMPAGES_PLACEHOLDER, &total_pages.to_string())
                .replace(SECTIONPAGES_PLACEHOLDER, &section_pages.to_string());
            if text.trim().is_empty() {
                continue;
            }
            if approximated_entries.insert((record.section, entry.part.clone())) {
                warnings.push(approximated_layout_warning(section, entry));
            }
            project_entry(
                context.document_digest,
                record,
                entry,
                kind,
                text,
                doc_page,
                laid_page,
                warnings,
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

#[allow(clippy::too_many_arguments)]
fn project_entry(
    document_digest: &str,
    record: &PageRecord,
    entry: &SectionHeaderFooter,
    kind: HeaderFooterKind,
    text: String,
    doc_page: &mut Page,
    laid_page: &mut LaidOutPage,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(), DocsightError> {
    let geometry = record.geometry;
    let lines: Vec<&str> = text.split('\n').collect();
    let height = lines.len() as f32 * HEADER_FOOTER_LINE_HEIGHT;
    let (top, overlay_kind, label) = match kind {
        HeaderFooterKind::Header => (geometry.header_distance, OverlayKind::Header, "header"),
        HeaderFooterKind::Footer => (
            geometry.page_height - geometry.footer_distance - height,
            OverlayKind::Footer,
            "footer",
        ),
    };
    let left = geometry.margin_left;
    let bbox = Rect::new(left, top, left + geometry.content_width(), top + height)?;
    let id = ObjectId::new(
        if kind == HeaderFooterKind::Header {
            "hdr"
        } else {
            "ftr"
        },
        document_digest,
        &format!("page[{}]/{label}", record.number),
    );
    let overlaps_body = match kind {
        HeaderFooterKind::Header => bbox.y1 > geometry.margin_top,
        HeaderFooterKind::Footer => bbox.y0 < geometry.content_bottom(),
    };
    if overlaps_body {
        warnings.push(Diagnostic {
            code: "DOCX_HEADER_FOOTER_OVERLAPS_BODY".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "the {label} on page {} extends into the body text area",
                record.number
            ),
            effect: "body text is not moved to make room, so the header or footer overlaps content that Word would push away".to_owned(),
            object: Some(id.clone()),
            page: Some(record.number),
        });
    }
    if bbox.y0 < 0.0 || bbox.y1 > geometry.page_height {
        warnings.push(Diagnostic {
            code: "DOCX_HEADER_FOOTER_OUTSIDE_PAGE".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "the {label} on page {} extends outside the physical page",
                record.number
            ),
            effect: "content outside the page boundary is clipped from rendered evidence"
                .to_owned(),
            object: Some(id.clone()),
            page: Some(record.number),
        });
    }
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_top = top + index as f32 * HEADER_FOOTER_LINE_HEIGHT;
        laid_page.runs.push(TextRunLayout {
            text: (*line).to_owned(),
            font_size: HEADER_FOOTER_FONT_SIZE,
            bold: false,
            bbox: Rect::new(
                left,
                line_top,
                left + text_width(line, HEADER_FOOTER_FONT_SIZE).max(1.0),
                line_top + HEADER_FOOTER_LINE_HEIGHT,
            )?,
            color_argb: HEADER_FOOTER_COLOR,
        });
    }
    doc_page.overlays.push(Overlay {
        id,
        kind: overlay_kind,
        page: record.number,
        bbox: Some(bbox),
        text,
        source: SourceSpan::new(entry.part.clone()),
    });
    Ok(())
}

fn format_page_number(number: u32, format: Option<&str>) -> Option<String> {
    match format {
        None | Some("decimal") => Some(number.to_string()),
        Some("lowerRoman") => roman(number).map(|value| value.to_lowercase()),
        Some("upperRoman") => roman(number),
        Some("lowerLetter") => letters(number).map(|value| value.to_lowercase()),
        Some("upperLetter") => letters(number),
        Some(_) => None,
    }
}

fn roman(mut number: u32) -> Option<String> {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if !(1..=MAX_ROMAN_PAGE_NUMBER).contains(&number) {
        return None;
    }
    let mut output = String::new();
    for (value, numeral) in NUMERALS {
        while number >= value {
            output.push_str(numeral);
            number -= value;
        }
    }
    Some(output)
}

fn letters(number: u32) -> Option<String> {
    if number > MAX_LETTER_PAGE_NUMBER {
        return None;
    }
    let index = number.checked_sub(1)?;
    let letter = char::from_u32(u32::from(b'A') + index % 26)?;
    let repeat = usize::try_from(index / 26 + 1).ok()?;
    Some(std::iter::repeat_n(letter, repeat).collect())
}

fn unsupported_format_warning(section: &Section) -> Diagnostic {
    Diagnostic {
        code: "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "section {} numbers pages with format {}",
            section.section_index,
            section.page_number_format.as_deref().unwrap_or_default()
        ),
        effect: "page number fields in this section are shown as decimal numbers where the format or number is unsupported".to_owned(),
        object: Some(section.id.clone()),
        page: None,
    }
}

fn approximated_layout_warning(section: &Section, entry: &SectionHeaderFooter) -> Diagnostic {
    Diagnostic {
        code: "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "header or footer part {} is projected as plain text",
            entry.part
        ),
        effect: "source paragraph and run formatting, tab stops, tables, drawings, and positioned content are not reproduced; projected evidence uses deterministic 9 pt text lines"
            .to_owned(),
        object: Some(section.id.clone()),
        page: None,
    }
}
