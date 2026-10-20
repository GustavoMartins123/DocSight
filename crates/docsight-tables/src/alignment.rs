use super::{
    InferredTable, InferredTableCell, TableDetectorKind, TextSpanItem, checked_grid_size,
    merge_contiguous_line_spans,
};
use docsight_core::{DocsightError, Rect};

struct Row {
    y0: f32,
    y1: f32,
    spans: Vec<TextSpanItem>,
}

type GridRow<'a> = Vec<(usize, &'a TextSpanItem)>;

pub(super) fn detect(
    page: u32,
    spans: &[TextSpanItem],
) -> Result<Vec<InferredTable>, DocsightError> {
    let rows = cluster_rows(spans)?;
    let mut tables = Vec::new();
    let mut run = Vec::new();
    for row in &rows {
        let continues = row.spans.len() >= 2
            && run.last().is_none_or(|previous: &&Row| {
                row.y0 - previous.y1 <= (previous.y1 - previous.y0) * 2.5
            });
        if !continues {
            if let Some(table) = infer(page, &run)? {
                tables.push(table);
            }
            run.clear();
        }
        if row.spans.len() >= 2 {
            run.push(row);
        }
    }
    if let Some(table) = infer(page, &run)? {
        tables.push(table);
    }
    Ok(tables)
}

fn cluster_rows(spans: &[TextSpanItem]) -> Result<Vec<Row>, DocsightError> {
    let mut sorted = spans.to_vec();
    sorted.sort_by(|a, b| {
        a.bbox
            .y0
            .total_cmp(&b.bbox.y0)
            .then_with(|| a.bbox.x0.total_cmp(&b.bbox.x0))
            .then_with(|| a.text.cmp(&b.text))
    });
    let mut rows: Vec<Row> = Vec::new();
    for span in sorted {
        let center = (span.bbox.y0 + span.bbox.y1) * 0.5;
        if let Some(row) = rows.iter_mut().rev().find(|row| {
            let row_center = (row.y0 + row.y1) * 0.5;
            (center - row_center).abs() <= (row.y1 - row.y0).min(span.bbox.height()) * 0.4
        }) {
            row.y0 = row.y0.min(span.bbox.y0);
            row.y1 = row.y1.max(span.bbox.y1);
            row.spans.push(span);
        } else {
            rows.push(Row {
                y0: span.bbox.y0,
                y1: span.bbox.y1,
                spans: vec![span],
            });
        }
    }
    for row in &mut rows {
        row.spans.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
        row.spans = merge_words(&merge_contiguous_line_spans(&row.spans))?;
    }
    rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    Ok(rows)
}

fn merge_words(spans: &[TextSpanItem]) -> Result<Vec<TextSpanItem>, DocsightError> {
    let mut cells: Vec<TextSpanItem> = Vec::new();
    for span in spans {
        if let Some(previous) = cells.last_mut() {
            let gap = span.bbox.x0 - previous.bbox.x1;
            let size = previous.font_size.min(span.font_size);
            if gap >= 0.0 && gap <= size * 0.75 {
                if !previous.text.ends_with(char::is_whitespace)
                    && !span.text.starts_with(char::is_whitespace)
                {
                    previous.text.push(' ');
                }
                previous.text.push_str(&span.text);
                previous.bbox = Rect::new(
                    previous.bbox.x0.min(span.bbox.x0),
                    previous.bbox.y0.min(span.bbox.y0),
                    previous.bbox.x1.max(span.bbox.x1),
                    previous.bbox.y1.max(span.bbox.y1),
                )?;
                previous.bold &= span.bold;
                continue;
            }
        }
        cells.push(span.clone());
    }
    Ok(cells)
}

fn edge_distance(a: &TextSpanItem, b: &TextSpanItem) -> f32 {
    (a.bbox.x0 - b.bbox.x0)
        .abs()
        .min((a.bbox.x1 - b.bbox.x1).abs())
}

fn assign<'a>(run: &[&'a Row], seed: &Row) -> Option<Vec<GridRow<'a>>> {
    let mut grid = Vec::with_capacity(run.len());
    for row in run {
        if row.spans.len() == seed.spans.len() {
            grid.push(row.spans.iter().enumerate().collect());
            continue;
        }
        let mut assigned = Vec::new();
        let mut first = 0;
        for span in &row.spans {
            let (column, anchor) =
                seed.spans
                    .iter()
                    .enumerate()
                    .skip(first)
                    .min_by(|(_, a), (_, b)| {
                        edge_distance(span, a).total_cmp(&edge_distance(span, b))
                    })?;
            if edge_distance(span, anchor) > span.font_size.max(anchor.font_size) * 0.65 {
                return None;
            }
            assigned.push((column, span));
            first = column + 1;
        }
        grid.push(assigned);
    }
    Some(grid)
}

fn numeric(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || "+-.,%$€£¥()".contains(c))
}

fn numeric_columns(grid: &[GridRow<'_>], columns: usize, skip: usize) -> Vec<bool> {
    let mut counts = vec![0; columns];
    for row in grid.iter().skip(skip) {
        for (column, span) in row {
            if numeric(&span.text) {
                counts[*column] += 1;
            }
        }
    }
    counts
        .into_iter()
        .map(|count| count == grid.len() - skip)
        .collect()
}

fn header_likelihood(grid: &[GridRow<'_>], columns: usize) -> bool {
    let Some(first) = grid.first() else {
        return false;
    };
    let body = grid.iter().skip(1).flatten().collect::<Vec<_>>();
    let bold_header = first.iter().filter(|(_, span)| span.bold).count() * 2 >= columns
        && body.iter().filter(|(_, span)| span.bold).count() * 2 < body.len();
    let numeric_body = numeric_columns(grid, columns, 1);
    let typed_header = first
        .iter()
        .any(|(column, span)| !numeric(&span.text) && numeric_body[*column]);
    bold_header || typed_header
}

fn stable_columns(grid: &[GridRow<'_>], columns: usize, header: bool) -> bool {
    let mut observations = vec![Vec::new(); columns];
    for (column, span) in grid
        .iter()
        .skip(usize::from(header && grid.len() > 2))
        .flatten()
    {
        observations[*column].push(*span);
    }
    for spans in observations {
        if spans.len() < 2 {
            return false;
        }
        let left_min = spans
            .iter()
            .map(|span| span.bbox.x0)
            .fold(f32::INFINITY, f32::min);
        let left_max = spans
            .iter()
            .map(|span| span.bbox.x0)
            .fold(f32::NEG_INFINITY, f32::max);
        let right_min = spans
            .iter()
            .map(|span| span.bbox.x1)
            .fold(f32::INFINITY, f32::min);
        let right_max = spans
            .iter()
            .map(|span| span.bbox.x1)
            .fold(f32::NEG_INFINITY, f32::max);
        let size = spans
            .iter()
            .map(|span| span.font_size)
            .fold(f32::INFINITY, f32::min);
        if (left_max - left_min).min(right_max - right_min) > size * 0.65 {
            return false;
        }
    }
    let mut gutters = vec![(0_usize, 0_usize); columns - 1];
    for row in grid.iter().skip(usize::from(header)) {
        for pair in row.windows(2) {
            let (column, left) = pair[0];
            let (next_column, right) = pair[1];
            if next_column == column + 1 {
                let (adjacent, separated) = &mut gutters[column];
                *adjacent += 1;
                if right.bbox.x0 - left.bbox.x1 > left.font_size.min(right.font_size) * 0.75 {
                    *separated += 1;
                }
            }
        }
    }
    gutters
        .into_iter()
        .all(|(adjacent, separated)| adjacent > 0 && separated * 4 >= adjacent * 3)
}

fn prose_columns(run: &[&Row], grid: &[GridRow<'_>], header: bool) -> bool {
    if header || grid.iter().flatten().any(|(_, span)| numeric(&span.text)) {
        return false;
    }
    let long_cells = grid
        .iter()
        .flatten()
        .filter(|(_, span)| span.text.split_whitespace().count() >= 6)
        .count();
    let count = grid.iter().map(Vec::len).sum::<usize>();
    let tight_rows = run
        .windows(2)
        .all(|rows| rows[1].y0 - rows[0].y1 <= (rows[0].y1 - rows[0].y0) * 0.75);
    long_cells * 4 >= count * 3 && tight_rows
}

fn infer(page: u32, run: &[&Row]) -> Result<Option<InferredTable>, DocsightError> {
    if run.len() < 2 {
        return Ok(None);
    }
    let Some(seed) = run.iter().max_by_key(|row| row.spans.len()) else {
        return Ok(None);
    };
    let Some(grid) = assign(run, seed) else {
        return Ok(None);
    };
    let columns = seed.spans.len();
    let non_empty = grid.iter().map(Vec::len).sum::<usize>();
    let grid_size = run
        .len()
        .checked_mul(columns)
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "inferred PDF table cells".to_owned(),
            limit: super::MAX_INFERRED_TABLE_CELLS as u64,
        })?;
    let density = non_empty as f32 / grid_size as f32;
    if density < 0.75 {
        return Ok(None);
    }
    let (rows, columns_count) = checked_grid_size(run.len(), columns)?;
    let header = header_likelihood(&grid, columns);
    if !stable_columns(&grid, columns, header) || prose_columns(run, &grid, header) {
        return Ok(None);
    }
    let bounds = run
        .iter()
        .flat_map(|row| &row.spans)
        .map(|span| span.bbox)
        .reduce(|left, right| Rect {
            x0: left.x0.min(right.x0),
            y0: left.y0.min(right.y0),
            x1: left.x1.max(right.x1),
            y1: left.y1.max(right.y1),
        })
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "inferred table has no text geometry".to_owned(),
        })?;
    let mut cells = Vec::with_capacity(grid_size);
    for (row_index, row) in grid.iter().enumerate() {
        let mut present = row.iter().peekable();
        for column in 0..columns {
            let span = if present.peek().is_some_and(|(index, _)| *index == column) {
                present.next().map(|(_, span)| *span)
            } else {
                None
            };
            cells.push(InferredTableCell {
                row: u32::try_from(row_index).map_err(|_| DocsightError::ResourceLimit {
                    resource: "inferred PDF table cells".to_owned(),
                    limit: super::MAX_INFERRED_TABLE_CELLS as u64,
                })?,
                column: u32::try_from(column).map_err(|_| DocsightError::ResourceLimit {
                    resource: "inferred PDF table cells".to_owned(),
                    limit: super::MAX_INFERRED_TABLE_CELLS as u64,
                })?,
                row_span: 1,
                column_span: 1,
                bbox: span.map(|span| span.bbox),
                text: span.map(|span| span.text.clone()).unwrap_or_default(),
            });
        }
    }
    let numeric_column = numeric_columns(&grid, columns, usize::from(header))
        .into_iter()
        .any(|column| column);
    let confidence = (0.45
        + 0.15 * density
        + if rows >= 3 { 0.04 } else { 0.0 }
        + if header { 0.20 } else { 0.0 }
        + if numeric_column { 0.10 } else { 0.0 })
    .min(0.95);
    Ok(Some(InferredTable {
        page,
        bbox: bounds,
        rows,
        columns: columns_count,
        header_rows: u32::from(header),
        cells,
        confidence,
        detector: TableDetectorKind::Alignment,
    }))
}
