use crate::font::WrappedLine;
use crate::geometry::SectionGeometry;
use crate::layout::{BorderLayout, ImageLayout, TextRunLayout};
use docsight_core::{
    Block, BlockContent, DocsightError, NoteKind, ParagraphFormat, Rect, Resource, Style,
    TextAlignment,
};
use docsight_fonts::{DEFAULT_FAMILY, FontBook};
use std::ops::Range;

const BODY_COLOR: u32 = 0xFF000000;
const NOTE_COLOR: u32 = 0xFF404040;

#[derive(Clone, Debug)]
pub(crate) struct TextLine {
    pub text: String,
    pub x: f32,
    pub width: f32,
    pub start_char: usize,
    pub runs: Vec<docsight_core::InlineRun>,
    pub height: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct Marker {
    pub text: String,
    pub x: f32,
    pub width: f32,
    pub format: docsight_core::RunFormat,
}

#[derive(Clone, Debug)]
pub(crate) struct TextMetrics {
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub font_family: String,
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
        (before
            + self.lines[lines.clone()]
                .iter()
                .map(|line| line.height)
                .sum::<f32>()
            + after)
            .max(1.0)
    }

    pub fn full_height(&self) -> f32 {
        self.fragment_height(&(0..self.lines.len()))
    }

    pub fn leading_height(&self, minimum_lines: usize) -> f32 {
        let lines = minimum_lines.min(self.lines.len());
        self.space_before
            + self.lines[..lines]
                .iter()
                .map(|line| line.height)
                .sum::<f32>()
    }

    pub fn lines_fitting(&self, start: usize, available: f32) -> usize {
        let mut height = 0.0;
        self.lines[start..]
            .iter()
            .take_while(|line| {
                height += line.height;
                height <= available + 0.001
            })
            .count()
    }

    pub fn emit(
        &self,
        lines: &Range<usize>,
        top: f32,
        fonts: &FontBook,
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
            let family = marker
                .format
                .font_family
                .as_deref()
                .unwrap_or(&self.font_family);
            let size = marker.format.font_size_pt.unwrap_or(self.font_size);
            let bold = marker.format.bold.unwrap_or(self.bold);
            let italic = marker.format.italic.unwrap_or(self.italic);
            runs.push(TextRunLayout {
                text: marker.text.clone(),
                font_size: size,
                bold,
                italic,
                font_family: family.to_owned(),
                bbox: Rect::new(
                    marker.x,
                    top + before,
                    marker.x + marker.width,
                    top + before + self.line_height,
                )?,
                color_argb: self.color_argb,
                baseline: top
                    + before
                    + fonts.resolve(family, bold, italic)?.shape("", size)?.ascent,
                underline: None,
            });
        }
        let mut y = top + before;
        for line in &self.lines[lines.clone()] {
            if line.text.is_empty() {
                y += line.height;
                continue;
            }
            if !line.runs.is_empty() {
                let book = fonts;
                let ascent = line.runs.iter().try_fold(0.0_f32, |ascent, run| {
                    Ok::<_, DocsightError>(
                        ascent.max(
                            book.resolve(
                                run.format
                                    .font_family
                                    .as_deref()
                                    .unwrap_or(&self.font_family),
                                run.format.bold.unwrap_or(self.bold),
                                run.format.italic.unwrap_or(self.italic),
                            )?
                            .shape("", run.format.font_size_pt.unwrap_or(self.font_size))?
                            .ascent,
                        ),
                    )
                })?;
                let mut x = line.x;
                for run in &line.runs {
                    let family = run
                        .format
                        .font_family
                        .as_deref()
                        .unwrap_or(&self.font_family);
                    let size = run.format.font_size_pt.unwrap_or(self.font_size);
                    let bold = run.format.bold.unwrap_or(self.bold);
                    let italic = run.format.italic.unwrap_or(self.italic);
                    let width = book
                        .resolve(family, bold, italic)?
                        .shape(&run.text, size)?
                        .width;
                    runs.push(TextRunLayout {
                        text: run.text.clone(),
                        font_size: size,
                        bold,
                        italic,
                        font_family: family.to_owned(),
                        bbox: Rect::new(x, y, x + width.max(0.01), y + line.height)?,
                        color_argb: run.format.color_argb.unwrap_or(self.color_argb),
                        baseline: y + ascent,
                        underline: if run.format.underline == Some(true) {
                            let (offset, thickness) = book
                                .resolve(family, bold, italic)?
                                .underline_metrics(size)?;
                            Some(Rect::new(
                                x,
                                y + ascent + offset,
                                x + width.max(0.01),
                                y + ascent + offset + thickness,
                            )?)
                        } else {
                            None
                        },
                    });
                    x += width;
                }
                y += line.height;
                continue;
            }
            runs.push(TextRunLayout {
                text: line.text.clone(),
                font_size: self.font_size,
                bold: self.bold,
                italic: self.italic,
                font_family: self.font_family.clone(),
                bbox: Rect::new(line.x, y, line.x + line.width, y + line.height)?,
                color_argb: self.color_argb,
                baseline: y + fonts
                    .resolve(&self.font_family, self.bold, self.italic)?
                    .shape("", self.font_size)?
                    .ascent,
                underline: None,
            });
            y += line.height;
        }
        Ok(runs)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Measured {
    Text(TextMetrics),
    Table(crate::tables::TableMetrics),
    Atomic { height: f32 },
}

impl Measured {
    pub fn full_height(&self) -> f32 {
        match self {
            Self::Text(metrics) => metrics.full_height(),
            Self::Table(metrics) => metrics.height,
            Self::Atomic { height } => *height,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EmittedBlock {
    pub height: f32,
    pub runs: Vec<TextRunLayout>,
    pub borders: Vec<BorderLayout>,
    pub images: Vec<ImageLayout>,
    pub fills: Vec<crate::layout::FillLayout>,
}

pub(crate) fn measure_block(
    block: &Block,
    geometry: &SectionGeometry,
    styles: &[Style],
    resources: &[Resource],
    fonts: &FontBook,
) -> Result<Measured, DocsightError> {
    let content_width = geometry.content_width();
    let margin_left = geometry.margin_left;
    if !block.inline_runs.is_empty() {
        return Ok(Measured::Text(rich_text(block, geometry, fonts)?));
    }
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
                    italic: style.and_then(|value| value.italic).unwrap_or(false),
                    font_family: style
                        .and_then(|value| value.font_family.clone())
                        .unwrap_or_else(|| DEFAULT_FAMILY.to_owned()),
                    color_argb: BODY_COLOR,
                },
                spacing,
                margin_left,
                content_width,
                fonts,
            )?))
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
                    italic: style.and_then(|value| value.italic).unwrap_or(false),
                    font_family: style
                        .and_then(|value| value.font_family.clone())
                        .unwrap_or_else(|| DEFAULT_FAMILY.to_owned()),
                    color_argb: BODY_COLOR,
                },
                ParagraphSpacing {
                    line_height,
                    space_before,
                    space_after,
                },
                margin_left,
                content_width,
                fonts,
            )?))
        }
        BlockContent::ListItem(item) => {
            let style = find_style(styles, item.style_id.as_deref());
            let font_size = style.and_then(|value| value.font_size_pt).unwrap_or(11.0);
            let indent = (item.level as f32 + 1.0) * 18.0;
            let item_width = (content_width - indent).max(50.0);
            let wrapped = crate::font::wrap_with_face(
                &item.text,
                font_size,
                item_width,
                item_width,
                &fonts.resolve(DEFAULT_FAMILY, false, false)?,
            )?;
            let marker = item
                .marker
                .as_deref()
                .map(|marker_text| {
                    Ok::<_, DocsightError>(Marker {
                        text: marker_text.to_owned(),
                        x: margin_left + indent - 14.0,
                        width: fonts
                            .resolve(DEFAULT_FAMILY, false, false)?
                            .shape(marker_text, font_size)?
                            .width,
                        format: item.marker_format.clone().unwrap_or_default(),
                    })
                })
                .transpose()?;
            Ok(Measured::Text(TextMetrics {
                font_size,
                bold: style.and_then(|value| value.bold).unwrap_or(false),
                italic: style.and_then(|value| value.italic).unwrap_or(false),
                font_family: style
                    .and_then(|value| value.font_family.clone())
                    .unwrap_or_else(|| DEFAULT_FAMILY.to_owned()),
                color_argb: BODY_COLOR,
                line_height: (font_size * 1.27).max(font_size + 2.0),
                space_before: 0.0,
                space_after: 3.0,
                lines: positioned_lines(wrapped, font_size, 0, |_, _| margin_left + indent, fonts)?,
                marker,
            }))
        }
        BlockContent::Note(note) => {
            let prefix = match note.kind {
                NoteKind::Footnote => format!("[Footnote {}] ", note.note_id),
                NoteKind::Endnote => format!("[Endnote {}] ", note.note_id),
            };
            let font_size = 9.0_f32;
            let displayed = format!("{prefix}{}", note.text);
            let wrapped = crate::font::wrap_with_face(
                &displayed,
                font_size,
                content_width,
                content_width,
                &fonts.resolve(DEFAULT_FAMILY, false, false)?,
            )?;
            Ok(Measured::Text(TextMetrics {
                font_size,
                bold: false,
                italic: false,
                font_family: DEFAULT_FAMILY.to_owned(),
                color_argb: NOTE_COLOR,
                line_height: 12.0,
                space_before: 0.0,
                space_after: 3.0,
                lines: positioned_lines(
                    wrapped,
                    font_size,
                    prefix.chars().count(),
                    |_, _| margin_left,
                    fonts,
                )?,
                marker: None,
            }))
        }
        BlockContent::Table(_) => Ok(Measured::Table(crate::tables::measure(
            block, geometry, styles, resources, fonts,
        )?)),
        BlockContent::Figure(figure) => Ok(Measured::Atomic {
            height: figure_dimensions(figure)?.1 + 12.0,
        }),
        BlockContent::Shape(_) => Ok(Measured::Atomic { height: 1.0 }),
        BlockContent::Unknown(_) => Ok(Measured::Atomic { height: 24.0 }),
    }
}

struct TextStyle {
    font_size: f32,
    bold: bool,
    italic: bool,
    font_family: String,
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
    fonts: &FontBook,
) -> Result<TextMetrics, DocsightError> {
    let line_height = match format.line_height_pt {
        Some(height) if format.exact_line_height => height,
        Some(height) => height.max(defaults.line_height),
        None => match format.line_spacing {
            Some(multiple) if multiple.is_finite() && multiple > 0.0 => {
                defaults.line_height * multiple
            }
            _ => defaults.line_height,
        },
    };
    let indent_left = format.indent_left_pt.unwrap_or(0.0).max(0.0);
    let indent_right = format.indent_right_pt.unwrap_or(0.0).max(0.0);
    let indent_first_line = format.indent_first_line_pt.unwrap_or(0.0);
    let text_area = (content_width - indent_left - indent_right).max(1.0);
    let first_line_area = (text_area - indent_first_line.max(0.0)).max(1.0);
    let alignment = format.alignment.unwrap_or(TextAlignment::Left);
    let book = fonts;
    let face = book.resolve(&style.font_family, style.bold, style.italic)?;
    let wrapped =
        crate::font::wrap_with_face(text, style.font_size, first_line_area, text_area, &face)?;
    let mut lines = positioned_lines(
        wrapped,
        style.font_size,
        0,
        |index, width| {
            let first_line_offset = if index == 0 { indent_first_line } else { 0.0 };
            let left = margin_left + indent_left + first_line_offset.max(-indent_left);
            let available = (text_area - first_line_offset.max(0.0)).max(1.0);
            match alignment {
                TextAlignment::Left | TextAlignment::Justify => left,
                TextAlignment::Center => left + ((available - width) / 2.0).max(0.0),
                TextAlignment::Right => left + (available - width).max(0.0),
            }
        },
        fonts,
    )?;
    for line in &mut lines {
        line.height = line_height;
    }
    Ok(TextMetrics {
        font_size: style.font_size,
        bold: style.bold,
        italic: style.italic,
        font_family: style.font_family,
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
    })
}

fn positioned_lines(
    wrapped: Vec<WrappedLine>,
    font_size: f32,
    displayed_prefix_chars: usize,
    x_for: impl Fn(usize, f32) -> f32,
    fonts: &FontBook,
) -> Result<Vec<TextLine>, DocsightError> {
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let width = fonts
                .resolve(DEFAULT_FAMILY, false, false)?
                .shape(&line.text, font_size)?
                .width
                .max(1.0);
            Ok(TextLine {
                x: x_for(index, width),
                width,
                start_char: line.start_char.saturating_sub(displayed_prefix_chars),
                text: line.text,
                runs: Vec::new(),
                height: font_size * 1.27,
            })
        })
        .collect()
}

fn rich_text(
    block: &Block,
    geometry: &SectionGeometry,
    fonts: &FontBook,
) -> Result<TextMetrics, DocsightError> {
    let text = block.text();
    if block
        .inline_runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>()
        != text
    {
        return Err(DocsightError::MalformedDocument {
            message: format!("inline runs disagree with the text of {}", block.id),
        });
    }
    let book = fonts;
    let characters: Vec<char> = text.chars().collect();
    let mut widths = vec![0.0_f32; characters.len()];
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut line_height = 0.0_f32;
    let mut largest_size = 0.0_f32;
    for run in &block.inline_runs {
        let family = run.format.font_family.as_deref().unwrap_or(DEFAULT_FAMILY);
        let size = run.format.font_size_pt.unwrap_or(11.0);
        let shaped = book
            .resolve(
                family,
                run.format.bold.unwrap_or(false),
                run.format.italic.unwrap_or(false),
            )?
            .shape(&run.text, size)?;
        line_height = line_height.max(shaped.ascent + shaped.descent + shaped.line_gap);
        largest_size = largest_size.max(size);
        let offsets: Vec<usize> = run.text.char_indices().map(|(offset, _)| offset).collect();
        for glyph in &shaped.glyphs {
            let index = offsets
                .partition_point(|offset| *offset <= glyph.cluster as usize)
                .saturating_sub(1);
            if let Some(width) = widths.get_mut(start + index) {
                *width += glyph.advance;
            }
        }
        let end = start + offsets.len();
        ranges.push((start..end, run));
        start = end;
    }
    let format = &block.format;
    line_height = match format.line_height_pt {
        Some(height) if format.exact_line_height => height,
        Some(height) => height.max(line_height),
        None => line_height * format.line_spacing.unwrap_or(1.0),
    }
    .max(0.1);
    let list = match &block.content {
        BlockContent::ListItem(list) => Some(list),
        _ => None,
    };
    let left = format
        .indent_left_pt
        .unwrap_or_else(|| list.map_or(0.0, |list| 18.0 * (f32::from(list.level) + 1.0)));
    let right = format.indent_right_pt.unwrap_or(0.0);
    let first_indent = if list.is_some() {
        0.0
    } else {
        format.indent_first_line_pt.unwrap_or(0.0)
    };
    let available = geometry.content_width() - left - right;
    if available <= 0.0 {
        return Err(DocsightError::MalformedDocument {
            message: format!("paragraph {} indents leave no text area", block.id),
        });
    }
    let mut lines = Vec::new();
    let mut start = 0_usize;
    while start < characters.len() {
        let indent = if lines.is_empty() { first_indent } else { 0.0 };
        let limit = available - indent;
        let mut end = start;
        let mut width = 0.0;
        let mut last_break = None;
        while end < characters.len() && characters[end] != '\n' {
            if width + widths[end] > limit && end > start {
                break;
            }
            width += widths[end];
            end += 1;
            if characters[end - 1].is_whitespace() && characters[end - 1] != '\u{a0}' {
                last_break = Some(end);
            }
        }
        if end < characters.len()
            && characters[end] != '\n'
            && let Some(boundary) = last_break
        {
            end = boundary;
        }
        let displayed: String = characters[start..end].iter().collect();
        let mut runs = Vec::new();
        let mut width = 0.0;
        let mut height = 0.0_f32;
        for (range, run) in &ranges {
            let a = start.max(range.start);
            let b = end.min(range.end);
            if a >= b {
                continue;
            }
            let mut fragment = (*run).clone();
            fragment.text = characters[a..b].iter().collect();
            let face = book.resolve(
                fragment
                    .format
                    .font_family
                    .as_deref()
                    .unwrap_or(DEFAULT_FAMILY),
                fragment.format.bold.unwrap_or(false),
                fragment.format.italic.unwrap_or(false),
            )?;
            let shaped =
                face.shape(&fragment.text, fragment.format.font_size_pt.unwrap_or(11.0))?;
            width += shaped.width;
            height = height.max(shaped.ascent + shaped.descent + shaped.line_gap);
            runs.push(fragment);
        }
        let x = geometry.margin_left
            + left
            + indent
            + match format.alignment.unwrap_or(TextAlignment::Left) {
                TextAlignment::Center => ((limit - width) / 2.0).max(0.0),
                TextAlignment::Right => (limit - width).max(0.0),
                _ => 0.0,
            };
        lines.push(TextLine {
            text: displayed,
            x,
            width: width.max(0.01),
            start_char: start,
            runs,
            height: match format.line_height_pt {
                Some(value) if format.exact_line_height => value,
                Some(value) => value.max(height),
                None => height * format.line_spacing.unwrap_or(1.0),
            }
            .max(0.1),
        });
        start = end;
        if start < characters.len() && characters[start] == '\n' {
            start += 1;
        }
    }
    if lines.is_empty() {
        lines.push(TextLine {
            text: String::new(),
            x: geometry.margin_left,
            width: 0.01,
            start_char: 0,
            runs: Vec::new(),
            height: line_height,
        });
    }
    let marker = if let Some(list) = list {
        if let Some(text) = &list.marker {
            let mut marker_format = list.marker_format.clone().unwrap_or_default();
            let family = marker_format
                .font_family
                .as_deref()
                .or_else(|| {
                    block
                        .inline_runs
                        .first()
                        .and_then(|run| run.format.font_family.as_deref())
                })
                .unwrap_or(DEFAULT_FAMILY);
            let size = marker_format.font_size_pt.unwrap_or(largest_size);
            let family = family.to_owned();
            marker_format.font_family = Some(family.clone());
            marker_format.font_size_pt = Some(size);
            if let Some(first) = block.inline_runs.first() {
                marker_format.bold = marker_format.bold.or(first.format.bold);
                marker_format.italic = marker_format.italic.or(first.format.italic);
                marker_format.color_argb = marker_format.color_argb.or(first.format.color_argb);
            }
            let shaped = book
                .resolve(
                    &family,
                    marker_format.bold.unwrap_or(false),
                    marker_format.italic.unwrap_or(false),
                )?
                .shape(text, size)?;
            Some(Marker {
                text: text.clone(),
                x: geometry.margin_left + left + format.indent_first_line_pt.unwrap_or(-18.0),
                width: shaped.width,
                format: marker_format,
            })
        } else {
            None
        }
    } else {
        None
    };
    Ok(TextMetrics {
        font_size: largest_size,
        bold: false,
        italic: false,
        font_family: DEFAULT_FAMILY.to_owned(),
        color_argb: BODY_COLOR,
        line_height,
        space_before: format.space_before_pt.unwrap_or(0.0),
        space_after: format.space_after_pt.unwrap_or(0.0),
        lines,
        marker,
    })
}

pub(crate) fn emit_atomic(
    block: &mut Block,
    geometry: &SectionGeometry,
    base_y: f32,
    resources: &[Resource],
) -> Result<EmittedBlock, DocsightError> {
    let margin_left = geometry.margin_left;
    let block_id = block.id.clone();
    match &mut block.content {
        BlockContent::Table(_) => Err(DocsightError::BackendFailure {
            backend: "docsight-layout".to_owned(),
            message: "table emission requires measured row fragments".to_owned(),
        }),
        BlockContent::Figure(figure) => {
            let (width, height) = figure_dimensions(figure)?;
            let total_height = height + 12.0;
            let figure_rect = Rect::new(
                margin_left,
                base_y + 6.0,
                margin_left + width,
                base_y + 6.0 + height,
            )?;
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
                    borders: Vec::new(),
                    images: vec![ImageLayout {
                        rect: figure_rect,
                        target,
                        object_id: block_id,
                    }],
                    fills: Vec::new(),
                });
            }
            Err(DocsightError::MalformedDocument {
                message: format!("figure {block_id} has no resolved image resource"),
            })
        }
        BlockContent::Shape(_) => Ok(EmittedBlock {
            height: 1.0,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
            fills: Vec::new(),
        }),
        BlockContent::Unknown(_) => Ok(EmittedBlock {
            height: 24.0,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
            fills: Vec::new(),
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

fn figure_dimensions(figure: &docsight_core::FigureBlock) -> Result<(f32, f32), DocsightError> {
    let (Some(width), Some(height)) = (figure.width_pt, figure.height_pt) else {
        return Err(DocsightError::UnsupportedFeature {
            feature: "DOCX figure without explicit dimensions".to_owned(),
        });
    };
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(DocsightError::MalformedDocument {
            message: "DOCX figure dimensions must be finite and positive".to_owned(),
        });
    }
    Ok((width, height))
}
