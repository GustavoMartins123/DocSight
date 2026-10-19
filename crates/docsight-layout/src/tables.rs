use crate::geometry::SectionGeometry;
use crate::layout::{FillLayout, TextRunLayout};
use crate::measure::{EmittedBlock, Measured, emit_atomic, measure_block};
use docsight_core::{Block, BlockContent, DocsightError, Rect, Resource, Style, TableBlock};

#[derive(Clone, Debug)]
pub(crate) struct TableMetrics {
    pub height: f32,
    pub rows: Vec<f32>,
    pub table: TableBlock,
    pub emitted: EmittedBlock,
    pub text_lines: Vec<(usize, usize, Rect)>,
    pub cell_offsets: Vec<usize>,
    pub block_lines: std::collections::BTreeMap<docsight_core::ObjectId, Vec<(usize, Rect)>>,
}

impl TableMetrics {
    pub fn cell_text_start(&self, cell: usize, y: f32) -> usize {
        self.text_lines
            .iter()
            .filter(|(index, _, bbox)| *index == cell && bbox.y0 >= y - 0.001)
            .map(|(_, start, _)| *start)
            .min()
            .unwrap_or_else(|| self.table.cells[cell].text.chars().count())
    }

    pub fn text_start(&self, y: f32) -> usize {
        self.table
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.bbox.is_some_and(|bbox| bbox.y1 > y))
            .map(|(index, _)| self.cell_offsets[index] + self.cell_text_start(index, y))
            .min()
            .unwrap_or_else(|| {
                self.table
                    .cells
                    .iter()
                    .map(|cell| cell.text.chars().count())
                    .sum()
            })
    }
}

fn failure(message: &str) -> DocsightError {
    DocsightError::MalformedDocument {
        message: message.to_owned(),
    }
}

pub(crate) fn measure(
    block: &Block,
    geometry: &SectionGeometry,
    styles: &[Style],
    resources: &[Resource],
    fonts: &docsight_fonts::FontBook,
) -> Result<TableMetrics, DocsightError> {
    let BlockContent::Table(source) = &block.content else {
        return Err(failure("table measurement requires a table"));
    };
    let rows = usize::try_from(source.rows)
        .map_err(|_| failure("table row count exceeds platform bounds"))?;
    let columns = usize::try_from(source.columns)
        .map_err(|_| failure("table column count exceeds platform bounds"))?;
    if rows == 0 || columns == 0 || rows > 65536 || columns > 1024 {
        return Err(failure("table grid dimensions exceed supported bounds"));
    }
    let widths = match &source.column_widths_pt {
        Some(widths)
            if widths.len() == columns
                && widths.iter().all(|width| width.is_finite() && *width > 0.0) =>
        {
            let total: f32 = widths.iter().sum();
            let scale = (geometry.content_width() / total).min(1.0);
            widths
                .iter()
                .map(|width| *width * scale)
                .collect::<Vec<_>>()
        }
        Some(_) => return Err(failure("table grid widths disagree with its columns")),
        None => vec![geometry.content_width() / columns as f32; columns],
    };
    let mut table = source.clone();
    let mut block_lines = std::collections::BTreeMap::new();
    let mut cell_lines = Vec::new();
    let mut row_heights = vec![8.0_f32; rows];
    let mut cell_content = Vec::with_capacity(source.cells.len());
    for cell in &mut table.cells {
        let start = usize::try_from(cell.column)
            .map_err(|_| failure("cell column exceeds platform bounds"))?;
        let end = usize::try_from(
            cell.column
                .checked_add(cell.column_span)
                .ok_or_else(|| failure("cell column span overflows"))?,
        )
        .map_err(|_| failure("cell column span exceeds platform bounds"))?;
        let width = widths
            .get(start..end)
            .ok_or_else(|| failure("cell extends beyond table grid"))?
            .iter()
            .sum::<f32>();
        let [top, right, bottom, left] = cell.format.padding_pt;
        if cell
            .format
            .padding_pt
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
            || width <= left + right
        {
            return Err(failure("table cell leaves no text area after margins"));
        }
        let cell_geometry = SectionGeometry {
            page_width: width,
            margin_left: left,
            margin_right: right,
            columns: 1,
            column_gap: 0.0,
            ..*geometry
        };
        let mut content = EmittedBlock {
            height: 0.0,
            runs: Vec::new(),
            borders: Vec::new(),
            images: Vec::new(),
            fills: Vec::new(),
        };
        if cell.blocks.is_empty() && !cell.text.is_empty() {
            return Err(failure("DOCX table text has no structural paragraphs"));
        }
        let mut lines = Vec::new();
        let mut source_offset = 0;
        let mut has_text = false;
        for child in &mut cell.blocks {
            let child_text = child.text();
            if !child_text.is_empty() && has_text {
                source_offset += 1;
            }
            let mut child_lines = Vec::new();
            let child_metrics = measure_block(child, &cell_geometry, styles, resources, fonts)?;
            let emitted = match child_metrics {
                Measured::Text(text) => {
                    let mut y = content.height + text.space_before;
                    for line in &text.lines {
                        child_lines.push((
                            line.start_char,
                            Rect::new(
                                line.x,
                                y,
                                line.x + cell_geometry.content_width(),
                                y + line.height,
                            )?,
                        ));
                        y += line.height;
                    }
                    EmittedBlock {
                        height: text.full_height(),
                        runs: text.emit(&(0..text.line_count()), 0.0, fonts)?,
                        borders: Vec::new(),
                        images: Vec::new(),
                        fills: Vec::new(),
                    }
                }
                Measured::Table(nested) => {
                    for (cell_index, offset, bbox) in &nested.text_lines {
                        child_lines.push((
                            nested.cell_offsets[*cell_index] + offset,
                            translate(*bbox, 0.0, content.height)?,
                        ));
                    }
                    for (id, values) in nested.block_lines {
                        block_lines.insert(
                            id,
                            values
                                .into_iter()
                                .map(|(offset, bbox)| {
                                    Ok((offset, translate(bbox, 0.0, content.height)?))
                                })
                                .collect::<Result<Vec<_>, DocsightError>>()?,
                        );
                    }
                    child.content = BlockContent::Table(nested.table);
                    if let BlockContent::Table(value) = &mut child.content {
                        translate_cells(value, 0.0, content.height)?;
                    }
                    nested.emitted
                }
                Measured::Atomic { .. } => {
                    emit_atomic(&mut child.clone(), &cell_geometry, 0.0, resources)?
                }
            };
            child.bbox = Some(Rect::new(
                cell_geometry.margin_left,
                content.height,
                cell_geometry.margin_left + cell_geometry.content_width(),
                content.height + emitted.height,
            )?);
            child.page = None;
            child.continuations.clear();
            for (offset, bbox) in &child_lines {
                lines.push((source_offset + offset, *bbox));
            }
            block_lines.insert(child.id.clone(), child_lines);
            if !child_text.is_empty() {
                source_offset += child_text.chars().count();
                has_text = true;
            }
            for mut run in emitted.runs {
                translate_run(&mut run, 0.0, content.height);
                content.runs.push(run);
            }
            for mut border in emitted.borders {
                border.rect = translate(border.rect, 0.0, content.height)?;
                content.borders.push(border);
            }
            for mut image in emitted.images {
                image.rect = translate(image.rect, 0.0, content.height)?;
                content.images.push(image);
            }
            for mut fill in emitted.fills {
                fill.rect = translate(fill.rect, 0.0, content.height)?;
                content.fills.push(fill);
            }
            content.height += emitted.height;
        }
        let a =
            usize::try_from(cell.row).map_err(|_| failure("cell row exceeds platform bounds"))?;
        let b = usize::try_from(
            cell.row
                .checked_add(cell.row_span)
                .ok_or_else(|| failure("cell row span overflows"))?,
        )
        .map_err(|_| failure("cell row span exceeds platform bounds"))?;
        let span = row_heights
            .get_mut(a..b)
            .ok_or_else(|| failure("cell extends beyond table rows"))?;
        let current: f32 = span.iter().sum();
        let deficit = (content.height + top + bottom - current).max(0.0);
        if let Some(last) = span.last_mut() {
            *last += deficit;
        }
        cell_content.push(content);
        cell_lines.push(lines);
    }
    let mut boundaries = vec![0.0];
    let mut height = 0.0;
    for row in row_heights {
        height += row;
        boundaries.push(height);
    }
    let mut emitted = EmittedBlock {
        height,
        runs: Vec::new(),
        borders: Vec::new(),
        images: Vec::new(),
        fills: Vec::new(),
    };
    let mut text_lines = Vec::new();
    let mut cell_offsets = vec![0_usize; table.cells.len()];
    let mut text_offset = 0;
    for row in 0..source.rows {
        let mut in_row = false;
        for (index, cell) in source
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.row == row)
        {
            if in_row {
                text_offset += 1;
            }
            cell_offsets[index] = text_offset;
            text_offset += cell.text.chars().count();
            in_row = true;
        }
        if row + 1 < source.rows {
            text_offset += 1;
        }
    }
    for (index, (cell, content)) in table.cells.iter_mut().zip(cell_content).enumerate() {
        let row = cell.row as usize;
        let end = (cell.row + cell.row_span) as usize;
        let x = geometry.margin_left + widths[..cell.column as usize].iter().sum::<f32>();
        let width = widths[cell.column as usize..(cell.column + cell.column_span) as usize]
            .iter()
            .sum::<f32>();
        let rect = Rect::new(x, boundaries[row], x + width, boundaries[end])?;
        cell.bbox = Some(rect);
        if let Some(color_argb) = cell.format.fill_argb {
            emitted.fills.push(FillLayout { rect, color_argb });
        }
        for (side, border) in cell.format.borders.iter().enumerate() {
            if let Some(border) = border {
                if border.width_pt <= 0.0 {
                    continue;
                }
                let width = border.width_pt;
                let rect = match side {
                    0 => Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + width)?,
                    1 => Rect::new(rect.x1 - width, rect.y0, rect.x1, rect.y1)?,
                    2 => Rect::new(rect.x0, rect.y1 - width, rect.x1, rect.y1)?,
                    _ => Rect::new(rect.x0, rect.y0, rect.x0 + width, rect.y1)?,
                };
                emitted.fills.push(FillLayout {
                    rect,
                    color_argb: border.color_argb,
                });
            }
        }
        let dy = boundaries[row] + cell.format.padding_pt[0];
        for (offset, bbox) in &cell_lines[index] {
            text_lines.push((index, *offset, translate(*bbox, x, dy)?));
        }
        for child in &mut cell.blocks {
            translate_block(child, x, dy)?;
            translate_block_lines(child, x, dy, &mut block_lines)?;
        }
        for mut run in content.runs {
            translate_run(&mut run, x, dy);
            emitted.runs.push(run);
        }
        for mut border in content.borders {
            border.rect = translate(border.rect, x, boundaries[row] + cell.format.padding_pt[0])?;
            emitted.borders.push(border);
        }
        for mut image in content.images {
            image.rect = translate(image.rect, x, boundaries[row] + cell.format.padding_pt[0])?;
            emitted.images.push(image);
        }
        for mut fill in content.fills {
            fill.rect = translate(fill.rect, x, boundaries[row] + cell.format.padding_pt[0])?;
            emitted.fills.push(fill);
        }
    }
    Ok(TableMetrics {
        height,
        rows: boundaries,
        table,
        emitted,
        text_lines,
        cell_offsets,
        block_lines,
    })
}

pub(crate) fn translate_run(run: &mut TextRunLayout, x: f32, y: f32) {
    run.bbox.x0 += x;
    run.bbox.x1 += x;
    run.bbox.y0 += y;
    run.bbox.y1 += y;
    run.baseline += y;
    if let Some(line) = &mut run.underline {
        line.x0 += x;
        line.x1 += x;
        line.y0 += y;
        line.y1 += y;
    }
}

pub(crate) fn translate(rect: Rect, x: f32, y: f32) -> Result<Rect, DocsightError> {
    Rect::new(rect.x0 + x, rect.y0 + y, rect.x1 + x, rect.y1 + y)
}

fn translate_cells(table: &mut TableBlock, x: f32, y: f32) -> Result<(), DocsightError> {
    for cell in &mut table.cells {
        if let Some(rect) = cell.bbox {
            cell.bbox = Some(translate(rect, x, y)?);
        }
        for block in &mut cell.blocks {
            translate_block(block, x, y)?;
        }
    }
    Ok(())
}

fn translate_block(block: &mut Block, x: f32, y: f32) -> Result<(), DocsightError> {
    if let Some(rect) = block.bbox {
        block.bbox = Some(translate(rect, x, y)?);
    }
    if let BlockContent::Table(table) = &mut block.content {
        translate_cells(table, x, y)?;
    }
    Ok(())
}

fn translate_block_lines(
    block: &Block,
    x: f32,
    y: f32,
    lines: &mut std::collections::BTreeMap<docsight_core::ObjectId, Vec<(usize, Rect)>>,
) -> Result<(), DocsightError> {
    if let Some(values) = lines.get_mut(&block.id) {
        for (_, rect) in values {
            *rect = translate(*rect, x, y)?;
        }
    }
    if let BlockContent::Table(table) = &block.content {
        for cell in &table.cells {
            for child in &cell.blocks {
                translate_block_lines(child, x, y, lines)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn clear_placements(table: &mut TableBlock) {
    for cell in &mut table.cells {
        cell.page = None;
        cell.bbox = None;
        cell.continuations.clear();
        for child in &mut cell.blocks {
            child.page = None;
            child.bbox = None;
            child.continuations.clear();
            if let BlockContent::Table(table) = &mut child.content {
                clear_placements(table);
            }
        }
    }
}

fn place(
    page: u32,
    bbox: Rect,
    offset: usize,
    primary_page: &mut Option<u32>,
    primary_bbox: &mut Option<Rect>,
    continuations: &mut Vec<docsight_core::BlockContinuation>,
) -> Result<(), DocsightError> {
    let merge = |a: Rect, b: Rect| {
        Rect::new(
            a.x0.min(b.x0),
            a.y0.min(b.y0),
            a.x1.max(b.x1),
            a.y1.max(b.y1),
        )
    };
    if primary_page.is_none() {
        *primary_page = Some(page);
        *primary_bbox = Some(bbox);
    } else if *primary_page == Some(page) {
        if let Some(previous) = primary_bbox {
            *previous = merge(*previous, bbox)?;
        }
    } else if let Some(previous) = continuations
        .last_mut()
        .filter(|previous| previous.page == page)
    {
        previous.bbox = merge(previous.bbox, bbox)?;
    } else {
        continuations.push(docsight_core::BlockContinuation {
            page,
            bbox,
            text_start_char: offset,
        });
    }
    Ok(())
}

pub(crate) struct TableFragment {
    pub page: u32,
    pub start: f32,
    pub end: f32,
    pub dx: f32,
    pub dy: f32,
}

impl TableFragment {
    fn project(&self, rect: Rect) -> Result<Option<Rect>, DocsightError> {
        let y0 = rect.y0.max(self.start);
        let y1 = rect.y1.min(self.end);
        if y1 <= y0 {
            return Ok(None);
        }
        Ok(Some(Rect::new(
            rect.x0 + self.dx,
            y0 + self.dy,
            rect.x1 + self.dx,
            y1 + self.dy,
        )?))
    }
}

pub(crate) fn project_cells(
    current: &mut TableBlock,
    source: &TableBlock,
    metrics: &TableMetrics,
    fragment: &TableFragment,
) -> Result<(), DocsightError> {
    for (cell, original) in current.cells.iter_mut().zip(&source.cells) {
        if let Some(bbox) = original
            .bbox
            .map(|rect| fragment.project(rect))
            .transpose()?
            .flatten()
        {
            let mut source_offset = 0;
            let mut has_text = false;
            let mut offset = original.text.chars().count();
            for block in &original.blocks {
                let length = block.text().chars().count();
                if length > 0 && has_text {
                    source_offset += 1;
                }
                if let Some(values) = metrics.block_lines.get(&block.id) {
                    for (start, rect) in values {
                        if rect.y0 >= fragment.start - 0.001 {
                            offset = offset.min(source_offset + start);
                        }
                    }
                }
                if length > 0 {
                    source_offset += length;
                    has_text = true;
                }
            }
            place(
                fragment.page,
                bbox,
                offset,
                &mut cell.page,
                &mut cell.bbox,
                &mut cell.continuations,
            )?;
        }
        for (child, original) in cell.blocks.iter_mut().zip(&original.blocks) {
            if let Some(bbox) = original
                .bbox
                .map(|rect| fragment.project(rect))
                .transpose()?
                .flatten()
            {
                let offset = metrics
                    .block_lines
                    .get(&original.id)
                    .into_iter()
                    .flatten()
                    .filter(|(_, rect)| rect.y0 >= fragment.start - 0.001)
                    .map(|(offset, _)| *offset)
                    .min()
                    .unwrap_or_else(|| original.text().chars().count());
                place(
                    fragment.page,
                    bbox,
                    offset,
                    &mut child.page,
                    &mut child.bbox,
                    &mut child.continuations,
                )?;
            }
            if let (BlockContent::Table(current), BlockContent::Table(source)) =
                (&mut child.content, &original.content)
            {
                project_cells(current, source, metrics, fragment)?;
            }
        }
    }
    Ok(())
}
