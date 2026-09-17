use crate::font::wrap_text;
use crate::layout::{LaidOutDocument, layout_docx};
use docsight_core::{Block, BlockContent, Diagnostic, Document, DocsightError, LayoutFlags, ObjectId, OverlayKind, Section, Style, validate_canonical};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutSection { pub start_block: usize, pub end_block: usize, pub section: Section }

pub fn layout_docx_productized(mut document: Document, mut sections: Vec<LayoutSection>) -> Result<LaidOutDocument, DocsightError> {
    if sections.is_empty() { return layout_docx(document); }
    normalize_sections(&mut sections, document.blocks.len())?;
    let original_blocks = document.blocks.clone();
    let original_links = document.links.clone();
    let original_comments = document.comments.clone();
    let mut final_blocks = Vec::new();
    let mut final_pages = Vec::new();
    let mut final_laid_pages = Vec::new();
    let mut final_links = Vec::new();
    let mut final_comments = Vec::new();
    let mut warnings = document.warnings.iter().filter(|warning| warning.code != "DOCX_SECTIONS_COLLAPSED").cloned().collect::<Vec<_>>();
    let mut page_offset = 0_u32;
    let mut reading_order = 0_u32;
    let mut seen_links = BTreeSet::new();
    let mut seen_comments = BTreeSet::new();

    for span in &sections {
        let source_blocks = &original_blocks[span.start_block..span.end_block];
        let mut section_document = document.clone();
        section_document.sections = vec![span.section.clone()];
        section_document.pages.clear();
        section_document.blocks = fragment_text_blocks(source_blocks, &span.section, &document.styles, &document.sha256)?;
        section_document.links = original_links.iter().filter(|link| anchor_in_blocks(link.anchor_path.as_deref(), source_blocks)).cloned().collect();
        section_document.comments = original_comments.iter().filter(|comment| anchor_in_blocks(comment.anchor_path.as_deref(), source_blocks)).cloned().collect();
        section_document.warnings.clear();
        let laid = layout_docx(section_document)?;
        let section_pages = u32::try_from(laid.document.pages.len()).map_err(|_| DocsightError::ResourceLimit { resource: "DOCX productized section pages".to_owned(), limit: u32::MAX as u64 })?;

        let mut section_blocks = laid.document.blocks;
        for block in &mut section_blocks {
            reading_order = reading_order.checked_add(1).ok_or_else(|| DocsightError::ResourceLimit { resource: "DOCX productized reading order".to_owned(), limit: u32::MAX as u64 })?;
            block.reading_order = reading_order;
            if let Some(page) = block.page.as_mut() { *page = page.checked_add(page_offset).ok_or_else(page_limit)?; }
        }
        let mut section_doc_pages = laid.document.pages;
        for page in &mut section_doc_pages {
            page.number = page.number.checked_add(page_offset).ok_or_else(page_limit)?;
            for overlay in &mut page.overlays {
                overlay.page = page.number;
                overlay.id = ObjectId::new(overlay_prefix(overlay.kind), &document.sha256, &format!("productized/page[{}]/{}/{}", page.number, overlay_prefix(overlay.kind), overlay.id));
            }
        }
        let mut section_laid_pages = laid.pages;
        for page in &mut section_laid_pages { page.number = page.number.checked_add(page_offset).ok_or_else(page_limit)?; }
        let mut section_links = laid.document.links;
        for link in &mut section_links {
            if let Some(page) = link.page.as_mut() { *page = page.checked_add(page_offset).ok_or_else(page_limit)?; }
            seen_links.insert(link.id.clone());
        }
        final_links.extend(section_links);
        let mut section_comments = laid.document.comments;
        for comment in &mut section_comments {
            if let Some(page) = comment.page.as_mut() { *page = page.checked_add(page_offset).ok_or_else(page_limit)?; }
            seen_comments.insert(comment.id.clone());
        }
        final_comments.extend(section_comments);
        for mut warning in laid.document.warnings {
            if warning.code == "DOCX_PAGINATION_BLOCK_GRANULAR" { continue; }
            if let Some(page) = warning.page.as_mut() { *page = page.checked_add(page_offset).ok_or_else(page_limit)?; }
            warnings.push(warning);
        }
        final_blocks.extend(section_blocks);
        final_pages.extend(section_doc_pages);
        final_laid_pages.extend(section_laid_pages);
        page_offset = page_offset.checked_add(section_pages).ok_or_else(page_limit)?;
    }

    final_links.extend(original_links.into_iter().filter(|link| !seen_links.contains(&link.id)));
    final_comments.extend(original_comments.into_iter().filter(|comment| !seen_comments.contains(&comment.id)));
    warnings.push(Diagnostic::warning("DOCX_MULTI_SECTION_LAYOUT", format!("laid out {} DOCX sections with independent geometry", sections.len()), "page size, margins, headers and footers follow the active section instead of the first section"));
    if final_blocks.len() > original_blocks.len() {
        warnings.push(Diagnostic::warning("DOCX_LINE_LEVEL_PAGINATION", "long paragraphs were split into deterministic line groups before pagination".to_owned(), "paragraph fragments preserve source paths while allowing page breaks between line groups with at least two lines per fragment"));
    }
    deduplicate_warnings(&mut warnings);
    document.sections = sections.into_iter().map(|span| span.section).collect();
    document.blocks = final_blocks;
    document.pages = final_pages;
    document.links = final_links;
    document.comments = final_comments;
    document.warnings = warnings;
    validate_canonical(&document)?;
    Ok(LaidOutDocument { document, pages: final_laid_pages })
}

fn normalize_sections(sections: &mut [LayoutSection], block_count: usize) -> Result<(), DocsightError> {
    let mut cursor = 0_usize;
    for (index, span) in sections.iter_mut().enumerate() {
        if span.start_block != cursor || span.end_block < span.start_block || span.end_block > block_count {
            return Err(DocsightError::MalformedDocument { message: "DOCX section layout plan is not contiguous or exceeds the block set".to_owned() });
        }
        span.section.section_index = u32::try_from(index + 1).map_err(|_| DocsightError::ResourceLimit { resource: "DOCX section count".to_owned(), limit: u32::MAX as u64 })?;
        cursor = span.end_block;
    }
    if cursor < block_count {
        let last = sections.last_mut().ok_or_else(|| DocsightError::MalformedDocument { message: "DOCX section layout plan is empty".to_owned() })?;
        last.end_block = block_count;
    }
    Ok(())
}

fn fragment_text_blocks(blocks: &[Block], section: &Section, styles: &[Style], digest: &str) -> Result<Vec<Block>, DocsightError> {
    let width = content_width(section)?;
    let mut output = Vec::new();
    for block in blocks {
        let BlockContent::Paragraph(paragraph) = &block.content else { output.push(block.clone()); continue; };
        if block.flags.keep_lines { output.push(block.clone()); continue; }
        let font_size = paragraph.style_id.as_deref().and_then(|style_id| styles.iter().find(|style| style.id == style_id)).and_then(|style| style.font_size_pt).filter(|size| size.is_finite() && *size > 0.0).unwrap_or(11.0);
        let indents = block.format.indent_left_pt.unwrap_or(0.0).max(0.0) + block.format.indent_right_pt.unwrap_or(0.0).max(0.0);
        let lines = wrap_text(&paragraph.text, font_size, (width - indents).max(36.0));
        if lines.len() <= 4 { output.push(block.clone()); continue; }
        let groups = line_groups(lines.len());
        for (fragment_index, (start, end)) in groups.iter().copied().enumerate() {
            let mut fragment = block.clone();
            let first = fragment_index == 0;
            let last = fragment_index + 1 == groups.len();
            fragment.id = ObjectId::new("frag", digest, &format!("{}#lines[{}-{}]", block.source.path, start + 1, end));
            fragment.page = None;
            fragment.bbox = None;
            fragment.reading_order = 0;
            fragment.flags = LayoutFlags { page_break_before: first && block.flags.page_break_before, break_after: last && block.flags.break_after, keep_with_next: last && block.flags.keep_with_next, keep_lines: true };
            if let BlockContent::Paragraph(value) = &mut fragment.content { value.text = lines[start..end].join(" "); }
            output.push(fragment);
        }
    }
    Ok(output)
}

fn line_groups(line_count: usize) -> Vec<(usize, usize)> {
    if line_count <= 4 { return vec![(0, line_count)]; }
    let mut groups = Vec::new();
    let mut start = 0_usize;
    while line_count - start > 4 { groups.push((start, start + 2)); start += 2; }
    groups.push((start, line_count));
    groups
}

fn content_width(section: &Section) -> Result<f32, DocsightError> {
    let width = section.page_width_pt.unwrap_or(612.0) - section.margin_left_pt.unwrap_or(72.0) - section.margin_right_pt.unwrap_or(72.0);
    if !width.is_finite() || width <= 0.0 { return Err(DocsightError::MalformedDocument { message: "DOCX section margins leave no positive page width".to_owned() }); }
    Ok(width)
}
fn anchor_in_blocks(anchor: Option<&str>, blocks: &[Block]) -> bool { anchor.is_some_and(|path| blocks.iter().any(|block| block.source.path == path)) }
fn overlay_prefix(kind: OverlayKind) -> &'static str { match kind { OverlayKind::Header => "hdr", OverlayKind::Footer => "ftr", OverlayKind::Watermark => "wm", OverlayKind::CommentMarker => "cmt", OverlayKind::Annotation => "ann" } }
fn page_limit() -> DocsightError { DocsightError::ResourceLimit { resource: "DOCX productized page number".to_owned(), limit: u32::MAX as u64 } }
fn deduplicate_warnings(warnings: &mut Vec<Diagnostic>) { let mut seen = BTreeSet::new(); warnings.retain(|warning| seen.insert((warning.code.clone(), warning.message.clone(), warning.object.clone(), warning.page))); }

#[cfg(test)]
mod tests {
    use super::line_groups;
    #[test]
    fn line_groups_never_leave_single_line_widows() {
        for count in 5..40 {
            let groups = line_groups(count);
            assert_eq!(groups.first().map(|group| group.0), Some(0));
            assert_eq!(groups.last().map(|group| group.1), Some(count));
            assert!(groups.iter().all(|(start, end)| end - start >= 2));
        }
    }
}
