use crate::font::{text_width, wrap_text};
use docsight_core::{
    Block, BlockContent, Diagnostic, DiagnosticSeverity, DocsightError, Document, ObjectId,
    Overlay, OverlayKind, Page, Rect, SourceSpan,
};

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaidOutDocument {
    pub document: Document,
    pub pages: Vec<LaidOutPage>,
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

    let content_width = (page_width - margin_left - margin_right).max(100.0);
    let content_height = (page_height - margin_top - margin_bottom).max(100.0);

    let mut warnings = vec![
        Diagnostic {
            code: "DOCX_LAYOUT_PAGINATED".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "DOCX was paginated using deterministic layout engine".to_owned(),
            effect: "line wrapping and page boundaries are computed approximations".to_owned(),
        },
        Diagnostic {
            code: "DOCX_FONT_SUBSTITUTED".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "layout used deterministic proportional fallback font".to_owned(),
            effect: "glyph metrics follow standard proportional reference widths".to_owned(),
        },
    ];

    let mut laid_pages = Vec::new();
    let mut doc_pages = Vec::new();
    let mut current_page_number = 1_u32;
    let mut current_y = margin_top;
    let mut current_block_ids = Vec::new();
    let mut current_runs = Vec::new();
    let mut current_borders = Vec::new();

    let mut updated_blocks = Vec::with_capacity(doc.blocks.len());

    for (index, mut block) in doc.blocks.into_iter().enumerate() {
        let (height, runs, borders) =
            measure_and_layout_block(&mut block, content_width, margin_left, current_y)?;

        if current_y + height > margin_top + content_height && current_y > margin_top {
            doc_pages.push(Page {
                number: current_page_number,
                width_pt: page_width,
                height_pt: page_height,
                block_ids: std::mem::take(&mut current_block_ids),
                overlays: Vec::new(),
            });
            laid_pages.push(LaidOutPage {
                number: current_page_number,
                width_pt: page_width,
                height_pt: page_height,
                runs: std::mem::take(&mut current_runs),
                borders: std::mem::take(&mut current_borders),
            });
            current_page_number =
                current_page_number
                    .checked_add(1)
                    .ok_or_else(|| DocsightError::ResourceLimit {
                        resource: "page count".to_owned(),
                        limit: u32::MAX as u64,
                    })?;
            current_y = margin_top;

            let (re_height, re_runs, re_borders) =
                measure_and_layout_block(&mut block, content_width, margin_left, current_y)?;
            let block_bbox = Rect::new(
                margin_left,
                current_y,
                margin_left + content_width,
                current_y + re_height,
            )?;
            block.page = Some(current_page_number);
            block.bbox = Some(block_bbox);
            block.reading_order = (index + 1) as u32;
            block.confidence = 0.95;

            current_block_ids.push(block.id.clone());
            current_runs.extend(re_runs);
            current_borders.extend(re_borders);
            current_y += re_height;
        } else {
            let block_bbox = Rect::new(
                margin_left,
                current_y,
                margin_left + content_width,
                current_y + height,
            )?;
            block.page = Some(current_page_number);
            block.bbox = Some(block_bbox);
            block.reading_order = (index + 1) as u32;
            block.confidence = 0.95;

            current_block_ids.push(block.id.clone());
            current_runs.extend(runs);
            current_borders.extend(borders);
            current_y += height;
        }

        updated_blocks.push(block);
    }

    doc_pages.push(Page {
        number: current_page_number,
        width_pt: page_width,
        height_pt: page_height,
        block_ids: current_block_ids,
        overlays: Vec::new(),
    });
    laid_pages.push(LaidOutPage {
        number: current_page_number,
        width_pt: page_width,
        height_pt: page_height,
        runs: current_runs,
        borders: current_borders,
    });

    let header_template = doc.sections.first().and_then(|s| s.header_text.clone());
    let footer_template = doc.sections.first().and_then(|s| s.footer_text.clone());

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
                let overlay_id =
                    ObjectId::new("hdr", &doc.sha256, &format!("page[{}]/header", page.number));
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
                let overlay_id =
                    ObjectId::new("ftr", &doc.sha256, &format!("page[{}]/footer", page.number));
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

    for link in &mut doc.links {
        let matched_page = updated_blocks.iter().find_map(|b| {
            if b.text().contains(&link.text) {
                b.page
            } else {
                None
            }
        });
        link.page = matched_page.or(Some(1));
    }

    doc.pages = doc_pages;
    doc.blocks = updated_blocks;
    doc.warnings.append(&mut warnings);

    Ok(LaidOutDocument {
        document: doc,
        pages: laid_pages,
    })
}

fn measure_and_layout_block(
    block: &mut Block,
    content_width: f32,
    margin_left: f32,
    base_y: f32,
) -> Result<(f32, Vec<TextRunLayout>, Vec<BorderLayout>), DocsightError> {
    match &mut block.content {
        BlockContent::Paragraph(p) => {
            let font_size = 11.0_f32;
            let line_height = 14.0_f32;
            let space_after = 4.0_f32;
            let lines = wrap_text(&p.text, font_size, content_width);
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
                    color_argb: 0xFF000000,
                });
            }
            Ok((height, runs, Vec::new()))
        }
        BlockContent::Heading(h) => {
            let (font_size, line_height, space_before, space_after) = match h.level {
                1 => (16.0_f32, 20.0_f32, 10.0_f32, 5.0_f32),
                2 => (13.0_f32, 17.0_f32, 8.0_f32, 4.0_f32),
                _ => (12.0_f32, 15.0_f32, 6.0_f32, 3.0_f32),
            };
            let lines = wrap_text(&h.text, font_size, content_width);
            let height = (lines.len() as f32 * line_height + space_before + space_after).max(1.0);
            let mut runs = Vec::with_capacity(lines.len());
            for (i, line) in lines.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line_y = base_y + space_before + i as f32 * line_height;
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
                    bold: true,
                    bbox,
                    color_argb: 0xFF000000,
                });
            }
            Ok((height, runs, Vec::new()))
        }
        BlockContent::ListItem(li) => {
            let font_size = 11.0_f32;
            let line_height = 14.0_f32;
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
                    bold: false,
                    bbox,
                    color_argb: 0xFF000000,
                });
            }
            Ok((height, runs, Vec::new()))
        }
        BlockContent::Table(tbl) => {
            let cols = tbl.columns.max(1);
            let col_w = content_width / cols as f32;
            let padding = 4.0_f32;
            let font_size = 9.5_f32;
            let line_height = 12.0_f32;

            let row_count = tbl.rows.max(1) as usize;
            let mut row_heights = vec![18.0_f32; row_count];

            for cell in &tbl.cells {
                let cell_w = col_w * cell.column_span as f32;
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
                let cell_x0 = margin_left + cell.column as f32 * col_w;
                let cell_x1 = cell_x0 + cell.column_span as f32 * col_w;
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

            Ok((total_table_h, runs, borders))
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
            let run = TextRunLayout {
                text: label.to_owned(),
                font_size: 9.0,
                bold: true,
                bbox: label_bbox,
                color_argb: 0xFF404040,
            };
            Ok((total_height, vec![run], vec![border]))
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
            Ok((height, runs, Vec::new()))
        }
        BlockContent::Shape(_) | BlockContent::Unknown(_) => {
            let height = 24.0_f32;
            Ok((height, Vec::new(), Vec::new()))
        }
    }
}
