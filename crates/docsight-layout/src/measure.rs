use crate::font::{WrappedLine, text_width, wrap_text, wrap_text_indexed};
use crate::geometry::SectionGeometry;
use crate::layout::{BorderLayout, ImageLayout, TextRunLayout};
use docsight_core::{
    Block, BlockContent, Diagnostic, DiagnosticSeverity, DocsightError, NoteKind, ParagraphFormat,
    Rect, Resource, Style, TextAlignment,
};
use std::ops::Range;

const BODY_COLOR: u32 = 0xFF000000;
const NOTE_COLOR: u32 = 0xFF404040;

#[derive(Clone, Debug)]
pub(crate) struct TextLine {
    pub text: String,
    pub x: f32,
    pub width: f32,
    pub start_char: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Marker {
    pub text: String,
    pub x: f32,
    pub width: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct TextMetrics {
    pub font_size: f32,
    pub bold: bool,
    pub color_argb: u32,
    pub line_height: f32,
    pub space_before: f32,
    pub space_after: f32,
    pub lines: Vec<TextLine>,
    pub marker: Option<Marker>,
}

impl TextMetrics {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn fragment_height(&self, lines: &Range<usize>) -> f32 {
        let before = if lines.start == 0 {
            self.space_before
        } else {
            0.0
        };
        let after = if lines.end == self.lines.len() {
            self.space_after
        } else {
            0.0
        };
        (before + lines.len() as f32 * self.line_height + after).max(1.0)
    }

    pub fn full_height(&self) -> f32 {
        self.fragment_height(&(0..self.lines.len()))
    }

    pub fn leading_height(&self, minimum_lines: usize) -> f32 {
        let lines = minimum_lines.min(self.lines.len());
        self.space_before + lines as f32 * self.line_height
    }

    pub fn emit(
        &self,
        lines: &Range<usize>,
        top: f32,
    ) -> Result<Vec<TextRunLayout>, DocsightError> {
        let before = if lines.start == 0 {
            self.space_before
        } else {
            0.0
        };
        let mut runs = Vec::with_capacity(lines.len() + 1);
        if lines.start == 0
            && let Some(marker) = &self.marker
        {
            runs.push(TextRunLayout {
                text: marker.text.clone(),
                font_size: self.font_size,
                bold: true,
                bbox: Rect::new(
                    marker.x,
                    top + before,
                    marker.x + marker.width,
                    top + before + self.line_height,
                )?,
                color_argb: self.color_argb,
            });
        }
        for (offset, line) in self.lines[lines.clone()].iter().enumerate() {
            if line.text.is_empty() {
                continue;
            }
            let y = top + before + offset as f32 * self.line_height;
            runs.push(TextRunLayout {
                text: line.text.clone(),
                font_size: self.font_size,
                bold: self.bold,
                bbox: Rect::new(line.x, y, line.x + line.width, y + self.line_height)?,
                color_argb: self.color_argb,
            });
        }
        Ok(runs)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Measured {
    Text(TextMetrics),
    Atomic { height: f32 },
}

impl Measured {
    pub fn full_height(&self) -> f32 {
        match self {
            Self::Text(metrics) => metrics.full_height(),
            Self::Atomic { height } => *height,
        }
    }
}

pub(crate) struct EmittedBlock {
    pub height: f32,
    pub runs: Vec<TextRunLayout>,
    pub borders: Vec<BorderLayout>,
    pub images: Vec<ImageLayout>,
}

pub(crate) fn measure_block(
    block: &Block,
    geometry: &SectionGeometry,
    styles: &[Style],
    resources: &[Resource],
) -> Result<Measured, DocsightError> {
    let content_width = geometry.content_width();
    let margin_left = geometry.margin_left;
    match &block.content {
        BlockContent::Paragraph(paragraph) => {
            let style = find_style(styles, paragraph.style_id.as_deref());
            let font_size = style.and_then(|value| value.font_size_pt).unwrap_or(11.0);
            let spacing = ParagraphSpacing {
                line_height: (font_size * 1.27).max(font_size + 2.0),
                space_before: 0.0,
                space_after: 4.0,
            };
            Ok(Measured::Text(formatted_text(
                &paragraph.text,
                &block.format,
                TextStyle {
                    font_size,
                    bold: style.and_then(|value| value.bold).unwrap_or(false),
                    color_argb: BODY_COLOR,
                },
                spacing,
                margin_left,
                content_width,
            )))
        }
        BlockContent::Heading(heading) => {
            let (default_font_size, default_line_height, space_before, space_after) =
                match heading.level {
                    1 => (16.0_f32, 20.0_f32, 10.0_f32, 5.0_f32),
                    2 => (13.0_f32, 17.0_f32, 8.0_f32, 4.0_f32),
                    _ => (12.0_f32, 15.0_f32, 6.0_f32, 3.0_f32),
                };
            let style = find_style(styles, heading.style_id.as_deref());
            let font_size = style
                .and_then(|value| value.font_size_pt)
                .unwrap_or(default_font_size);
            let line_height = style
                .and_then(|value| value.font_size_pt)
                .map(|size| (size * 1.25).max(size + 2.0))
                .unwrap_or(default_line_height);
            Ok(Measured::Text(formatted_text(
                &heading.text,
                &block.format,
                TextStyle {
                    font_size,
                    bold: style.and_then(|value| value.bold).unwrap_or(true),
                    color_argb: BODY_COLOR,
                },
                ParagraphSpacing {
                    line_height,
                    space_before,
                    space_after,
                },
                margin_left,
                content_width,
            )))
        }
        BlockContent::ListItem(item) => {
            let style = find_style(styles, item.style_id.as_deref());
            let font_size = style.and_then(|value| value.font_size_pt).unwrap_or(11.0);
            let indent = (item.level as f32 + 1.0) * 18.0;
            let item_width = (content_width - indent).max(50.0);
            let wrapped = wrap_text_indexed(&item.text, font_size, item_width, item_width);
            let marker_text = item.marker.as_deref().unwrap_or("-");
            Ok(Measured::Text(TextMetrics {
                font_size,
                bold: style.and_then(|value| value.bold).unwrap_or(false),
                color_argb: BODY_COLOR,
                line_height: (font_size * 1.27).max(font_size + 2.0),
                space_before: 0.0,
                space_after: 3.0,
                lines: positioned_lines(wrapped, font_size, 0, |_, _| margin_left + indent),
                marker: Some(Marker {
                    text: marker_text.to_owned(),
                    x: margin_left + indent - 14.0,
                    width: text_width(marker_text, font_size).max(1.0),
                }),
            }))
        }
        BlockContent::Note(note) => {
            let prefix = match note.kind {
                NoteKind::Footnote => format!("[Footnote {}] ", note.note_id),
                NoteKind::Endnote => format!("[Endnote {}] ", note.note_id),
            };
            let font_size = 9.0_f32;
            let displayed = format!("{prefix}{}", note.text);
            let wrapped = wrap_text_indexed(&displayed, font_size, content_width, content_width);
            Ok(Measured::Text(TextMetrics {
                font_size,
                bold: false,
                color_argb: NOTE_COLOR,
                line_height: 12.0,
                space_before: 0.0,
                space_after: 3.0,
                lines: positioned_lines(wrapped, font_size, prefix.chars().count(), |_, _| {
                    margin_left
                }),
                marker: None,
            }))
        }
        BlockContent::Table(_) => {
            let mut copy = block.clone();
            let emitted = emit_atomic(&mut copy, geometry, 0.0, &mut Vec::new(), resources)?;
            Ok(Measured::Atomic {
                height: emitted.height,
            })
        }
        BlockContent::Figure(figure) => Ok(Measured::Atomic {
            height: figure.height_pt.unwrap_or(120.0).max(20.0) + 12.0,
        }),
        BlockContent::Shape(_) => Ok(Measured::Atomic { height: 1.0 }),
        BlockContent::Unknown(_) => Ok(Measured::Atomic { height: 24.0 }),
    }
}

struct TextStyle {
    font_size: f32,
    bold: bool,
    color_argb: u32,
}

struct ParagraphSpacing {
    line_height: f32,
    space_before: f32,
    space_after: f32,
}

fn formatted_text(
    text: &str,
    format: &ParagraphFormat,
    style: TextStyle,
    defaults: ParagraphSpacing,
    margin_left: f32,
    content_width: f32,
) -> TextMetrics {
    let line_height = match format.line_spacing {
        Some(multiple) if multiple.is_finite() && multiple > 0.0 => defaults.line_height * multiple,
        _ => defaults.line_height,
    };
    let indent_left = format.indent_left_pt.unwrap_or(0.0).max(0.0);
    let indent_right = format.indent_right_pt.unwrap_or(0.0).max(0.0);
    let indent_first_line = format.indent_first_line_pt.unwrap_or(0.0);
    let text_area = (content_width - indent_left - indent_right).max(1.0);
    let first_line_area = (text_area - indent_first_line.max(0.0)).max(1.0);
    let alignment = format.alignment.unwrap_or(TextAlignment::Left);
    let wrapped = wrap_text_indexed(text, style.font_size, first_line_area, text_area);
    let lines = positioned_lines(wrapped, style.font_size, 0, |index, width| {
        let first_line_offset = if index == 0 { indent_first_line } else { 0.0 };
        let left = margin_left + indent_left + first_line_offset.max(-indent_left);
        let available = (text_area - first_line_offset.max(0.0)).max(1.0);
        match alignment {
            TextAlignment::Left | TextAlignment::Justify => left,
            TextAlignment::Center => left + ((available - width) / 2.0).max(0.0),
            TextAlignment::Right => left + (available - width).max(0.0),
        }
    });
    TextMetrics {
        font_size: style.font_size,
        bold: style.bold,
        color_argb: style.color_argb,
        line_height,
        space_before: format
            .space_before_pt
            .unwrap_or(defaults.space_before)
            .max(0.0),
        space_after: format
            .space_after_pt
            .unwrap_or(defaults.space_after)
            .max(0.0),
        lines,
        marker: None,
    }
}

fn positioned_lines(
    wrapped: Vec<WrappedLine>,
    font_size: f32,
    displayed_prefix_chars: usize,
    x_for: impl Fn(usize, f32) -> f32,
) -> Vec<TextLine> {
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let width = text_width(&line.text, font_size).max(1.0);
            TextLine {
                x: x_for(index, width),
                width,
                start_char: line.start_char.saturating_sub(displayed_prefix_chars),
                text: line.text,
            }
        })
        .collect()
}

pub(crate) fn emit_atomic(
    block: &mut Block,
    geometry: &SectionGeometry,
    base_y: f32,
    warnings: &mut Vec<Diagnostic>,
    resources: &[Resource],
) -> Result<EmittedBlock, DocsightError> {
    let content_width = geometry.content_width();
    let margin_left = geometry.margin_left;
    let block_id = block.id.clone();
    let block_page = block.page;
    match &mut block.content {
        BlockContent::Table(table) => {
            let columns = table.columns.max(1);
            let column_widths = match table.column_widths_pt.as_ref() {
                Some(widths) if widths.len() == columns as usize => {
                    let sum: f32 = widths.iter().sum();
                    if sum > 0.0 {
                        widths.iter().map(|w| w / sum * content_width).collect()
                    } else {
                        vec![content_width / columns as f32; columns as usize]
                    }
                }
                Some(widths) => {
                    warnings.push(Diagnostic {
                        code: "DOCX_TABLE_GRID_WIDTHS_UNUSABLE".to_owned(),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "table grid declares {} columns but layout computed {columns}",
                            widths.len()
                        ),
                        effect: "columns fall back to equal widths".to_owned(),
                        object: Some(block_id.clone()),
                        page: block_page,
                        occurrences: None,
                    });
                    vec![content_width / columns as f32; columns as usize]
                }
                None => vec![content_width / columns as f32; columns as usize],
            };
            let column_width = |index: u32| -> f32 {
                column_widths
                    .get(index as usize)
                    .copied()
                    .unwrap_or(content_width / columns as f32)
            };
            let padding = 4.0_f32;
            let font_size = 9.5_f32;
            let line_height = 12.0_f32;

            let row_count = table.rows.max(1) as usize;
            let mut row_heights = vec![18.0_f32; row_count];
            for cell in &table.cells {
                let cell_width = (cell.column..cell.column + cell.column_span)
                    .map(column_width)
                    .sum::<f32>();
                let usable_width = (cell_width - padding * 2.0).max(10.0);
                let lines = wrap_text(&cell.text, font_size, usable_width);
                let cell_height = lines.len() as f32 * line_height + padding * 2.0;
                let row = cell.row as usize;
                if row < row_heights.len() {
                    let per_row = cell_height / cell.row_span.max(1) as f32;
                    if per_row > row_heights[row] {
                        row_heights[row] = per_row;
                    }
                }
            }

            let mut row_y = Vec::with_capacity(row_heights.len() + 1);
            let mut accumulated = base_y;
            for height in &row_heights {
                row_y.push(accumulated);
                accumulated += *height;
            }
            row_y.push(accumulated);
            let total_height = accumulated - base_y + 6.0;

            let mut runs = Vec::new();
            let mut borders = Vec::new();
            for cell in &mut table.cells {
                let cell_x0 = margin_left + (0..cell.column).map(column_width).sum::<f32>();
                let cell_x1 = cell_x0
                    + (cell.column..cell.column + cell.column_span)
                        .map(column_width)
                        .sum::<f32>();
                let top_row = (cell.row as usize).min(row_y.len() - 1);
                let bottom_row = ((cell.row + cell.row_span) as usize).min(row_y.len() - 1);
                let cell_rect = Rect::new(cell_x0, row_y[top_row], cell_x1, row_y[bottom_row])?;
                cell.bbox = Some(cell_rect);
                borders.push(BorderLayout {
                    rect: cell_rect,
                    color_argb: 0xFFB0B0B0,
                });
                let usable_width = (cell_x1 - cell_x0 - padding * 2.0).max(10.0);
                for (index, line) in wrap_text(&cell.text, font_size, usable_width)
                    .into_iter()
                    .enumerate()
                {
                    if line.is_empty() {
                        continue;
                    }
                    let text_y = row_y[top_row] + padding + index as f32 * line_height;
                    let line_width = text_width(&line, font_size).max(1.0);
                    let max_x1 = (cell_x0 + padding + line_width).min(cell_x1 - 1.0);
                    let text_x1 = if max_x1 > cell_x0 + padding {
                        max_x1
                    } else {
                        cell_x0 + padding + 1.0
                    };
                    runs.push(TextRunLayout {
                        text: line,
                        font_size,
                        bold: cell.row < table.header_rows,
                        bbox: Rect::new(cell_x0 + padding, text_y, text_x1, text_y + line_height)?,
                        color_argb: BODY_COLOR,
                    });
                }
            }
            Ok(EmittedBlock {
                height: total_height,
                runs,
                borders,
                images: Vec::new(),
            })
        }
        BlockContent::Figure(figure) => {
            let width = figure
                .width_pt
                .unwrap_or(240.0)
                .min(content_width)
                .max(20.0);
            let height = figure.height_pt.unwrap_or(120.0).max(20.0);
            let total_height = height + 12.0;
            let figure_rect = Rect::new(
                margin_left,
                base_y + 6.0,
                margin_left + width,
                base_y + 6.0 + height,
            )?;
            let border = BorderLayout {
                rect: figure_rect,
                color_argb: 0xFF808080,
            };
            let target = figure.resource_id.as_deref().and_then(|id| {
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
                        rect: figure_rect,
                        target,
                        object_id: block_id,
                    }],
                });
            }
            let label = figure
                .caption
                .as_deref()
                .or(figure.alt_text.as_deref())
                .unwrap_or("[FIGURE]");
            let label_width = text_width(label, 9.0).max(1.0);
            let label_bbox = Rect::new(
                margin_left + 4.0,
                base_y + 10.0,
                (margin_left + 4.0 + label_width).min(margin_left + width - 2.0),
                base_y + 22.0,
            )?;
            Ok(EmittedBlock {
                height: total_height,
                runs: vec![TextRunLayout {
                    text: label.to_owned(),
                    font_size: 9.0,
                    bold: true,
                    bbox: label_bbox,
                    color_argb: NOTE_COLOR,
                }],
                borders: vec![border],
                images: Vec::new(),
            })
        }
        BlockContent::Shape(_) => Ok(EmittedBlock {
            height: 1.0,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
        }),
        BlockContent::Unknown(_) => Ok(EmittedBlock {
            height: 24.0,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
        }),
        BlockContent::Paragraph(_)
        | BlockContent::Heading(_)
        | BlockContent::ListItem(_)
        | BlockContent::Note(_) => Err(DocsightError::BackendFailure {
            backend: "docsight-layout".to_owned(),
            message: format!("text block {block_id} was routed to atomic emission"),
        }),
    }
}

fn find_style<'a>(styles: &'a [Style], style_id: Option<&str>) -> Option<&'a Style> {
    let id = style_id?;
    styles.iter().find(|style| style.id == id)
}
