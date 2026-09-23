use docsight_core::{
    Block, BlockContent, BlockKind, LayoutFlags, ObjectId, Rect, SourceSpan, TableBlock, TableCell,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableDetectorKind {
    Ruled,
    Alignment,
    Structural,
}

impl std::fmt::Display for TableDetectorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl TableDetectorKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ruled => "ruled",
            Self::Alignment => "alignment",
            Self::Structural => "structural",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSpanItem {
    pub text: String,
    pub bbox: Rect,
    pub font_size: f32,
    pub bold: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RulingSegment {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InferredTableCell {
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub bbox: Option<Rect>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InferredTable {
    pub page: u32,
    pub bbox: Rect,
    pub rows: u32,
    pub columns: u32,
    pub header_rows: u32,
    pub cells: Vec<InferredTableCell>,
    pub confidence: f32,
    pub detector: TableDetectorKind,
}

impl InferredTable {
    pub fn to_table_block(&self, document_digest: &str) -> TableBlock {
        let cells = self
            .cells
            .iter()
            .map(|cell| {
                let cell_id = ObjectId::new(
                    "cell",
                    document_digest,
                    &format!(
                        "pdf::page[{}]::table[{:.0},{:.0}]::cell[{},{}]",
                        self.page, self.bbox.x0, self.bbox.y0, cell.row, cell.column
                    ),
                );
                TableCell {
                    id: cell_id,
                    row: cell.row,
                    column: cell.column,
                    row_span: cell.row_span,
                    column_span: cell.column_span,
                    bbox: cell.bbox,
                    text: cell.text.clone(),
                    blocks: Vec::new(),
                    source: SourceSpan::new(format!(
                        "pdf::page[{}]::table::cell[{},{}]",
                        self.page, cell.row, cell.column
                    )),
                }
            })
            .collect();

        TableBlock {
            rows: self.rows,
            columns: self.columns,
            header_rows: self.header_rows,
            cells,
            column_widths_pt: None,
            detector: Some(self.detector.label().to_owned()),
        }
    }

    pub fn to_block(&self, document_digest: &str, index: usize, reading_order: u32) -> Block {
        let table_id = ObjectId::new(
            "tbl",
            document_digest,
            &format!(
                "pdf::page[{}]::table[{index}]::{:.0}_{:.0}",
                self.page, self.bbox.x0, self.bbox.y0
            ),
        );
        let table_block = self.to_table_block(document_digest);
        Block {
            id: table_id,
            kind: BlockKind::Table,
            page: Some(self.page),
            bbox: Some(self.bbox),
            z_index: 0,
            reading_order,
            source: SourceSpan::new(format!(
                "pdf::page[{}]::table[{index}]::{}",
                self.page, self.detector
            )),
            confidence: self.confidence,
            flags: LayoutFlags::default(),
            format: Default::default(),
            continuations: Vec::new(),
            content: BlockContent::Table(table_block),
        }
    }
}

pub fn detect_tables(
    page: u32,
    spans: &[TextSpanItem],
    rulings: &[RulingSegment],
    _page_width: f32,
    _page_height: f32,
) -> Vec<InferredTable> {
    let mut detected = Vec::new();

    let ruled_tables = detect_ruled_tables(page, spans, rulings);
    let mut covered_span_indices = std::collections::BTreeSet::new();

    for table in &ruled_tables {
        for (idx, span) in spans.iter().enumerate() {
            let cx = (span.bbox.x0 + span.bbox.x1) * 0.5;
            let cy = (span.bbox.y0 + span.bbox.y1) * 0.5;
            if cx >= table.bbox.x0
                && cx <= table.bbox.x1
                && cy >= table.bbox.y0
                && cy <= table.bbox.y1
            {
                covered_span_indices.insert(idx);
            }
        }
    }
    detected.extend(ruled_tables);

    let remaining_spans: Vec<(usize, TextSpanItem)> = spans
        .iter()
        .enumerate()
        .filter(|(idx, _)| !covered_span_indices.contains(idx))
        .map(|(idx, span)| (idx, span.clone()))
        .collect();

    let alignment_tables = detect_alignment_tables(page, &remaining_spans);
    detected.extend(alignment_tables);

    detected.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    detected
}

const RULING_CONNECT_GAP_PT: f32 = 6.0;

fn detect_ruled_tables(
    page: u32,
    spans: &[TextSpanItem],
    rulings: &[RulingSegment],
) -> Vec<InferredTable> {
    segment_rulings(rulings)
        .into_iter()
        .filter_map(|component| build_ruled_table(page, spans, &component))
        .collect()
}

fn segment_rulings(rulings: &[RulingSegment]) -> Vec<Vec<RulingSegment>> {
    let count = rulings.len();
    let mut parent: Vec<usize> = (0..count).collect();
    fn find(parent: &mut [usize], index: usize) -> usize {
        let mut current = index;
        while parent[current] != current {
            parent[current] = parent[parent[current]];
            current = parent[current];
        }
        current
    }
    fn union(parent: &mut [usize], left: usize, right: usize) {
        let left_root = find(parent, left);
        let right_root = find(parent, right);
        if left_root != right_root {
            parent[left_root] = right_root;
        }
    }
    for left in 0..count {
        for right in (left + 1)..count {
            if rulings_near(&rulings[left], &rulings[right]) {
                union(&mut parent, left, right);
            }
        }
    }
    let mut components: BTreeMap<usize, Vec<RulingSegment>> = BTreeMap::new();
    for (index, ruling) in rulings.iter().enumerate() {
        let root = find(&mut parent, index);
        components.entry(root).or_default().push(ruling.clone());
    }
    let mut grouped: Vec<Vec<RulingSegment>> = components.into_values().collect();
    grouped.sort_by(|a, b| {
        let a_key = a
            .iter()
            .map(|r| r.y0.min(r.y1))
            .fold(f32::INFINITY, f32::min);
        let b_key = b
            .iter()
            .map(|r| r.y0.min(r.y1))
            .fold(f32::INFINITY, f32::min);
        a_key
            .partial_cmp(&b_key)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    grouped
}

fn rulings_near(left: &RulingSegment, right: &RulingSegment) -> bool {
    let gap = RULING_CONNECT_GAP_PT;
    let left_x0 = left.x0.min(left.x1) - gap;
    let left_x1 = left.x0.max(left.x1) + gap;
    let left_y0 = left.y0.min(left.y1) - gap;
    let left_y1 = left.y0.max(left.y1) + gap;
    let right_x0 = right.x0.min(right.x1);
    let right_x1 = right.x0.max(right.x1);
    let right_y0 = right.y0.min(right.y1);
    let right_y1 = right.y0.max(right.y1);
    left_x0 <= right_x1 && right_x0 <= left_x1 && left_y0 <= right_y1 && right_y0 <= left_y1
}

fn build_ruled_table(
    page: u32,
    spans: &[TextSpanItem],
    component: &[RulingSegment],
) -> Option<InferredTable> {
    let mut h_lines: Vec<(f32, f32, f32)> = Vec::new();
    let mut v_lines: Vec<(f32, f32, f32)> = Vec::new();

    for r in component {
        let dx = (r.x1 - r.x0).abs();
        let dy = (r.y1 - r.y0).abs();
        if dy <= 2.0 && dx >= 6.0 {
            let y = (r.y0 + r.y1) * 0.5;
            let x_min = r.x0.min(r.x1);
            let x_max = r.x0.max(r.x1);
            h_lines.push((y, x_min, x_max));
        } else if dx <= 2.0 && dy >= 6.0 {
            let x = (r.x0 + r.x1) * 0.5;
            let y_min = r.y0.min(r.y1);
            let y_max = r.y0.max(r.y1);
            v_lines.push((x, y_min, y_max));
        }
    }

    if h_lines.len() < 2 || v_lines.len() < 2 {
        return None;
    }

    h_lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v_lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut clustered_y: Vec<f32> = Vec::new();
    for (y, _, _) in &h_lines {
        if let Some(last) = clustered_y.last_mut()
            && (*y - *last).abs() <= 2.5
        {
            *last = (*last + *y) * 0.5;
            continue;
        }
        clustered_y.push(*y);
    }

    let mut clustered_x: Vec<f32> = Vec::new();
    for (x, _, _) in &v_lines {
        if let Some(last) = clustered_x.last_mut()
            && (*x - *last).abs() <= 2.5
        {
            *last = (*last + *x) * 0.5;
            continue;
        }
        clustered_x.push(*x);
    }

    if clustered_y.len() < 2 || clustered_x.len() < 2 {
        return None;
    }

    let rows = (clustered_y.len() - 1) as u32;
    let cols = (clustered_x.len() - 1) as u32;

    let x0 = *clustered_x.first()?;
    let x1 = *clustered_x.last()?;
    let y0 = *clustered_y.first()?;
    let y1 = *clustered_y.last()?;

    let bbox = Rect::new(x0, y0, x1, y1).ok()?;

    let mut cells = Vec::new();
    for r in 0..rows {
        let row_top = clustered_y[r as usize];
        let row_bot = clustered_y[(r + 1) as usize];
        let mut c = 0;
        while c < cols {
            let col_left = clustered_x[c as usize];
            let mut end_column = c + 1;
            let middle_y = (row_top + row_bot) * 0.5;
            while end_column < cols
                && !v_lines.iter().any(|(x, top, bottom)| {
                    (*x - clustered_x[end_column as usize]).abs() <= 2.5
                        && *top <= middle_y
                        && *bottom >= middle_y
                })
            {
                end_column += 1;
            }
            let col_right = clustered_x[end_column as usize];

            let cell_bbox = Rect::new(col_left, row_top, col_right, row_bot).ok();

            let mut cell_spans: Vec<&TextSpanItem> = spans
                .iter()
                .filter(|s| {
                    let cx = (s.bbox.x0 + s.bbox.x1) * 0.5;
                    let cy = (s.bbox.y0 + s.bbox.y1) * 0.5;
                    cx >= col_left && cx < col_right && cy >= row_top && cy < row_bot
                })
                .collect();

            cell_spans.sort_by(|a, b| {
                a.bbox
                    .y0
                    .partial_cmp(&b.bbox.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        a.bbox
                            .x0
                            .partial_cmp(&b.bbox.x0)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });

            let mut cell_text = String::new();
            let mut previous: Option<&TextSpanItem> = None;
            for span in cell_spans {
                if let Some(previous) = previous {
                    let same_line = (span.bbox.y0 - previous.bbox.y0).abs()
                        < span.bbox.height().min(previous.bbox.height()) * 0.5;
                    if !same_line {
                        cell_text.push('\n');
                    } else if span.bbox.x0 - previous.bbox.x1 >= 2.0
                        && !cell_text.ends_with(char::is_whitespace)
                        && !span.text.starts_with(char::is_whitespace)
                    {
                        cell_text.push(' ');
                    }
                }
                cell_text.push_str(&span.text);
                previous = Some(span);
            }

            cells.push(InferredTableCell {
                row: r,
                column: c,
                row_span: 1,
                column_span: end_column - c,
                bbox: cell_bbox,
                text: cell_text,
            });
            c = end_column;
        }
    }

    let non_empty_cells = cells.iter().filter(|c| !c.text.trim().is_empty()).count();
    let total_cells = (rows * cols) as usize;
    if total_cells == 0 || non_empty_cells == 0 {
        return None;
    }

    let confidence = if non_empty_cells >= total_cells / 3 {
        0.991
    } else {
        0.940
    };

    Some(InferredTable {
        page,
        bbox,
        rows,
        columns: cols,
        header_rows: 1,
        cells,
        confidence,
        detector: TableDetectorKind::Ruled,
    })
}

#[derive(Clone, Debug)]
struct LineCluster {
    y0: f32,
    y1: f32,
    spans: Vec<TextSpanItem>,
}

fn detect_alignment_tables(page: u32, spans: &[(usize, TextSpanItem)]) -> Vec<InferredTable> {
    if spans.len() < 4 {
        return Vec::new();
    }

    let mut sorted_spans: Vec<TextSpanItem> = spans.iter().map(|(_, s)| s.clone()).collect();
    sorted_spans.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut lines: Vec<LineCluster> = Vec::new();
    for span in sorted_spans {
        let span_cy = (span.bbox.y0 + span.bbox.y1) * 0.5;
        let mut merged = false;
        for line in &mut lines {
            let line_cy = (line.y0 + line.y1) * 0.5;
            let line_h = (line.y1 - line.y0).abs().max(8.0);
            if (span_cy - line_cy).abs() <= line_h * 0.4 {
                line.y0 = line.y0.min(span.bbox.y0);
                line.y1 = line.y1.max(span.bbox.y1);
                line.spans.push(span.clone());
                merged = true;
                break;
            }
        }
        if !merged {
            lines.push(LineCluster {
                y0: span.bbox.y0,
                y1: span.bbox.y1,
                spans: vec![span],
            });
        }
    }

    for line in &mut lines {
        line.spans.sort_by(|a, b| {
            a.bbox
                .x0
                .partial_cmp(&b.bbox.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        line.spans = merge_contiguous_line_spans(&line.spans);
    }

    let multi_col_lines: Vec<&LineCluster> = lines.iter().filter(|l| l.spans.len() >= 2).collect();
    if multi_col_lines.len() < 2 {
        return Vec::new();
    }

    let mut consecutive_runs: Vec<Vec<&LineCluster>> = Vec::new();
    let mut current_run: Vec<&LineCluster> = Vec::new();

    for line in &lines {
        if line.spans.len() >= 2 {
            if let Some(prev) = current_run.last() {
                let gap = line.y0 - prev.y1;
                let prev_h = prev.y1 - prev.y0;
                if gap <= prev_h * 2.5 {
                    current_run.push(line);
                    continue;
                }
            } else {
                current_run.push(line);
                continue;
            }
        }
        if current_run.len() >= 2 {
            consecutive_runs.push(current_run);
        }
        current_run = Vec::new();
        if line.spans.len() >= 2 {
            current_run.push(line);
        }
    }
    if current_run.len() >= 2 {
        consecutive_runs.push(current_run);
    }

    let mut tables = Vec::new();
    for run in consecutive_runs {
        let mut x_starts: Vec<f32> = Vec::new();
        for line in &run {
            for span in &line.spans {
                x_starts.push(span.bbox.x0);
            }
        }
        x_starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mut col_lefts: Vec<f32> = Vec::new();
        for x in x_starts {
            if let Some(last) = col_lefts.last_mut()
                && (x - *last).abs() <= 12.0
            {
                *last = (*last + x) * 0.5;
                continue;
            }
            col_lefts.push(x);
        }

        if col_lefts.len() < 2 {
            continue;
        }

        let num_cols = col_lefts.len() as u32;
        let num_rows = run.len() as u32;

        let table_x0 = run
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.bbox.x0))
            .fold(f32::INFINITY, f32::min);
        let table_x1 = run
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.bbox.x1))
            .fold(f32::NEG_INFINITY, f32::max);
        let table_y0 = run.first().map(|l| l.y0).unwrap_or(0.0);
        let table_y1 = run.last().map(|l| l.y1).unwrap_or(0.0);

        let bbox = match Rect::new(table_x0, table_y0, table_x1, table_y1) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let mut cells = Vec::new();
        for (r_idx, line) in run.iter().enumerate() {
            let row_idx = r_idx as u32;
            let mut columns: Vec<Vec<&TextSpanItem>> =
                col_lefts.iter().map(|_| Vec::new()).collect();
            for span in &line.spans {
                let mut best = 0_usize;
                let mut best_distance = f32::INFINITY;
                for (c_idx, col_x) in col_lefts.iter().enumerate() {
                    let distance = (span.bbox.x0 - *col_x).abs();
                    if distance < best_distance {
                        best_distance = distance;
                        best = c_idx;
                    }
                }
                columns[best].push(span);
            }
            for (c_idx, cell_spans) in columns.iter().enumerate() {
                let col_idx = c_idx as u32;
                let mut text = String::new();
                let mut previous: Option<&TextSpanItem> = None;
                for span in cell_spans {
                    if let Some(previous) = previous {
                        let same_line = (span.bbox.y0 - previous.bbox.y0).abs()
                            < span.bbox.height().min(previous.bbox.height()) * 0.5;
                        if !same_line {
                            text.push('\n');
                        } else if span.bbox.x0 - previous.bbox.x1 >= 2.0
                            && !text.ends_with(char::is_whitespace)
                            && !span.text.starts_with(char::is_whitespace)
                        {
                            text.push(' ');
                        }
                    }
                    text.push_str(&span.text);
                    previous = Some(*span);
                }
                let cell_bbox = if let Some(first_span) = cell_spans.first() {
                    let mut b_x0 = first_span.bbox.x0;
                    let mut b_y0 = first_span.bbox.y0;
                    let mut b_x1 = first_span.bbox.x1;
                    let mut b_y1 = first_span.bbox.y1;
                    for s in &cell_spans[1..] {
                        b_x0 = b_x0.min(s.bbox.x0);
                        b_y0 = b_y0.min(s.bbox.y0);
                        b_x1 = b_x1.max(s.bbox.x1);
                        b_y1 = b_y1.max(s.bbox.y1);
                    }
                    Rect::new(b_x0, b_y0, b_x1, b_y1).ok()
                } else {
                    None
                };

                cells.push(InferredTableCell {
                    row: row_idx,
                    column: col_idx,
                    row_span: 1,
                    column_span: 1,
                    bbox: cell_bbox,
                    text,
                });
            }
        }

        let non_empty = cells.iter().filter(|c| !c.text.trim().is_empty()).count();
        if non_empty < (num_rows * 2) as usize {
            continue;
        }

        let base_confidence: f32 = if num_rows >= 3 && num_cols >= 3 {
            0.934
        } else if num_rows >= 2 && num_cols >= 2 {
            0.850
        } else {
            0.612
        };
        let mut col_widths = Vec::with_capacity(col_lefts.len());
        for (index, left) in col_lefts.iter().enumerate() {
            let right = col_lefts.get(index + 1).copied().unwrap_or(table_x1);
            col_widths.push(right - *left);
        }
        let min_width = col_widths.iter().copied().fold(f32::INFINITY, f32::min);
        let total_cells = (num_rows * num_cols) as usize;
        let empty_fraction = 1.0 - non_empty as f32 / total_cells as f32;
        let mut max_column_empty_fraction = 0.0_f32;
        for c_idx in 0..num_cols {
            let column_cells = cells.iter().filter(|cell| cell.column == c_idx).count();
            let column_empty = cells
                .iter()
                .filter(|cell| cell.column == c_idx && cell.text.trim().is_empty())
                .count();
            if column_cells > 0 {
                max_column_empty_fraction =
                    max_column_empty_fraction.max(column_empty as f32 / column_cells as f32);
            }
        }
        let mut confidence = base_confidence;
        if num_cols > num_rows * 4 || num_cols >= 12 {
            confidence -= 0.15_f32;
        }
        if min_width < 20.0 {
            confidence -= 0.10_f32;
        }
        if max_column_empty_fraction >= 0.9 || empty_fraction > 0.5 {
            confidence -= 0.10_f32;
        }
        confidence = confidence.max(0.35_f32);

        tables.push(InferredTable {
            page,
            bbox,
            rows: num_rows,
            columns: num_cols,
            header_rows: 1,
            cells,
            confidence,
            detector: TableDetectorKind::Alignment,
        });
    }

    tables
}

fn merge_contiguous_line_spans(spans: &[TextSpanItem]) -> Vec<TextSpanItem> {
    if spans.is_empty() {
        return Vec::new();
    }
    let mut merged: Vec<TextSpanItem> = Vec::new();
    for span in spans {
        if let Some(last) = merged.last_mut() {
            let gap = span.bbox.x0 - last.bbox.x1;
            let font_size = last.font_size.max(span.font_size);
            let intra_gap_limit = (font_size * 0.22).max(2.2);
            let min_negative_gap = -font_size * 0.3;
            let is_intra_word = gap <= intra_gap_limit
                && gap >= min_negative_gap
                && !last.text.ends_with(char::is_whitespace)
                && !span.text.starts_with(char::is_whitespace);
            if is_intra_word {
                last.text.push_str(&span.text);
                last.bbox = match Rect::new(
                    last.bbox.x0.min(span.bbox.x0),
                    last.bbox.y0.min(span.bbox.y0),
                    last.bbox.x1.max(span.bbox.x1),
                    last.bbox.y1.max(span.bbox.y1),
                ) {
                    Ok(r) => r,
                    Err(_) => last.bbox,
                };
                continue;
            }
        }
        merged.push(span.clone());
    }
    merged
}

pub use docsight_core::{
    table_to_csv, table_to_html, table_to_markdown, table_to_tsv, table_to_tsv_string,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ruled_table() -> Result<(), Box<dyn std::error::Error>> {
        let rulings = vec![
            RulingSegment {
                x0: 20.0,
                y0: 50.0,
                x1: 200.0,
                y1: 50.0,
            },
            RulingSegment {
                x0: 20.0,
                y0: 80.0,
                x1: 200.0,
                y1: 80.0,
            },
            RulingSegment {
                x0: 20.0,
                y0: 110.0,
                x1: 200.0,
                y1: 110.0,
            },
            RulingSegment {
                x0: 20.0,
                y0: 50.0,
                x1: 20.0,
                y1: 110.0,
            },
            RulingSegment {
                x0: 110.0,
                y0: 50.0,
                x1: 110.0,
                y1: 110.0,
            },
            RulingSegment {
                x0: 200.0,
                y0: 50.0,
                x1: 200.0,
                y1: 110.0,
            },
        ];
        let spans = vec![
            TextSpanItem {
                text: "Header 1".into(),
                bbox: Rect::new(25.0, 55.0, 80.0, 75.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Header 2".into(),
                bbox: Rect::new(115.0, 55.0, 170.0, 75.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Val 1".into(),
                bbox: Rect::new(25.0, 85.0, 60.0, 105.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "Val 2".into(),
                bbox: Rect::new(115.0, 85.0, 150.0, 105.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];

        let tables = detect_tables(1, &spans, &rulings, 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].detector, TableDetectorKind::Ruled);
        assert_eq!(tables[0].rows, 2);
        assert_eq!(tables[0].columns, 2);
        assert!(tables[0].confidence >= 0.95);
        assert_eq!(tables[0].cells[0].text, "Header 1");
        assert_eq!(tables[0].cells[3].text, "Val 2");

        let block = tables[0].to_table_block("test_digest");
        assert_eq!(block.rows, 2);
        assert_eq!(block.columns, 2);

        let md = table_to_markdown(&block)?;
        assert!(md.contains("Header 1"));
        assert!(md.contains("Val 2"));
        Ok(())
    }

    #[test]
    fn detects_alignment_table() -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "Item".into(),
                bbox: Rect::new(30.0, 50.0, 60.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Qty".into(),
                bbox: Rect::new(100.0, 50.0, 120.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Price".into(),
                bbox: Rect::new(170.0, 50.0, 200.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Apples".into(),
                bbox: Rect::new(30.0, 70.0, 70.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "10".into(),
                bbox: Rect::new(100.0, 70.0, 115.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "$5.00".into(),
                bbox: Rect::new(170.0, 70.0, 205.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "Bananas".into(),
                bbox: Rect::new(30.0, 90.0, 75.0, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "20".into(),
                bbox: Rect::new(100.0, 90.0, 115.0, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "$8.00".into(),
                bbox: Rect::new(170.0, 90.0, 205.0, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];

        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].detector, TableDetectorKind::Alignment);
        assert_eq!(tables[0].rows, 3);
        assert_eq!(tables[0].columns, 3);
        assert!(tables[0].confidence >= 0.85);

        let block = tables[0].to_table_block("test_digest");
        let csv = table_to_csv(&block)?;
        assert!(csv.contains("Apples,10,$5.00"));
        Ok(())
    }

    #[test]
    fn alignment_joins_contiguous_glyphs_without_spaces() -> Result<(), Box<dyn std::error::Error>>
    {
        let spans = vec![
            TextSpanItem {
                text: "Q".into(),
                bbox: Rect::new(100.0, 50.0, 108.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "t".into(),
                bbox: Rect::new(108.0, 50.0, 113.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "y".into(),
                bbox: Rect::new(113.0, 50.0, 121.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Item".into(),
                bbox: Rect::new(30.0, 50.0, 60.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Apples".into(),
                bbox: Rect::new(30.0, 70.0, 70.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "Q".into(),
                bbox: Rect::new(100.0, 70.0, 108.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "t".into(),
                bbox: Rect::new(108.0, 70.0, 113.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "y".into(),
                bbox: Rect::new(113.0, 70.0, 121.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        let qty_texts: Vec<&str> = tables[0]
            .cells
            .iter()
            .filter(|cell| cell.row == 0 && cell.column == 1)
            .map(|cell| cell.text.as_str())
            .collect();
        assert_eq!(qty_texts, vec!["Qty"]);
        Ok(())
    }

    #[test]
    fn alignment_assigns_straddling_run_to_a_single_column()
    -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "Description".into(),
                bbox: Rect::new(30.0, 50.0, 95.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Amount".into(),
                bbox: Rect::new(90.0, 50.0, 140.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Widget".into(),
                bbox: Rect::new(30.0, 70.0, 80.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "$9.00".into(),
                bbox: Rect::new(100.0, 70.0, 140.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        let occurrences = tables[0]
            .cells
            .iter()
            .filter(|cell| cell.text.contains("Amount"))
            .count();
        assert_eq!(occurrences, 1);
        Ok(())
    }

    #[test]
    fn ruled_cell_join_uses_paragraph_gap_threshold() -> Result<(), Box<dyn std::error::Error>> {
        let rulings = vec![
            RulingSegment {
                x0: 20.0,
                y0: 50.0,
                x1: 200.0,
                y1: 50.0,
            },
            RulingSegment {
                x0: 20.0,
                y0: 110.0,
                x1: 200.0,
                y1: 110.0,
            },
            RulingSegment {
                x0: 20.0,
                y0: 50.0,
                x1: 20.0,
                y1: 110.0,
            },
            RulingSegment {
                x0: 200.0,
                y0: 50.0,
                x1: 200.0,
                y1: 110.0,
            },
        ];
        let spans = vec![
            TextSpanItem {
                text: "AB".into(),
                bbox: Rect::new(25.0, 60.0, 45.0, 72.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "CD".into(),
                bbox: Rect::new(46.0, 60.0, 66.0, 72.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "EF".into(),
                bbox: Rect::new(70.0, 60.0, 90.0, 72.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &rulings, 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0].text, "ABCD EF");
        Ok(())
    }

    #[test]
    fn alignment_penalizes_implausible_column_counts() -> Result<(), Box<dyn std::error::Error>> {
        let mut spans = Vec::new();
        for row in 0..3_u32 {
            let y0 = 50.0 + row as f32 * 20.0;
            for column in 0..22_u32 {
                let x0 = 10.0 + column as f32 * 15.0;
                spans.push(TextSpanItem {
                    text: format!("c{column}"),
                    bbox: Rect::new(x0, y0, x0 + 10.0, y0 + 12.0)?,
                    font_size: 10.0,
                    bold: false,
                });
            }
        }
        let tables = detect_tables(1, &spans, &[], 500.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!((tables[0].rows, tables[0].columns), (3, 22));
        assert!(
            tables[0].confidence < 0.70,
            "over-segmented table must not present as reliable: {}",
            tables[0].confidence
        );
        Ok(())
    }

    #[test]
    fn alignment_does_not_fragment_habilidades() -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "HABILID".into(),
                bbox: Rect::new(30.0, 50.0, 75.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ADES".into(),
                bbox: Rect::new(75.5, 50.0, 105.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert!(tables.is_empty());
        Ok(())
    }

    #[test]
    fn alignment_does_not_fragment_educacao_with_accents() -> Result<(), Box<dyn std::error::Error>>
    {
        let spans = vec![
            TextSpanItem {
                text: "ED".into(),
                bbox: Rect::new(30.0, 50.0, 45.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "UCAÇÃO".into(),
                bbox: Rect::new(45.5, 50.0, 95.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert!(tables.is_empty());
        Ok(())
    }

    #[test]
    fn alignment_preserves_spaces_between_words() -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "PRIMEIRA".into(),
                bbox: Rect::new(30.0, 50.0, 80.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "SEGUNDA".into(),
                bbox: Rect::new(85.0, 50.0, 135.0, 62.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let merged = merge_contiguous_line_spans(&spans);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "PRIMEIRA");
        assert_eq!(merged[1].text, "SEGUNDA");
        Ok(())
    }

    #[test]
    fn alignment_preserves_distinct_columns() -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "Nome".into(),
                bbox: Rect::new(30.0, 50.0, 60.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Cargo".into(),
                bbox: Rect::new(120.0, 50.0, 150.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "Alice".into(),
                bbox: Rect::new(30.0, 70.0, 60.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "Dev".into(),
                bbox: Rect::new(120.0, 70.0, 140.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "Bob".into(),
                bbox: Rect::new(30.0, 90.0, 50.0, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "QA".into(),
                bbox: Rect::new(120.0, 90.0, 135.0, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 2);
        assert_eq!(tables[0].rows, 3);
        let block = tables[0].to_table_block("test_digest");
        let csv = table_to_csv(&block)?;
        assert!(csv.contains("Alice,Dev"));
        assert!(csv.contains("Bob,QA"));
        Ok(())
    }

    #[test]
    fn alignment_detects_unruled_table_with_slightly_irregular_spacing()
    -> Result<(), Box<dyn std::error::Error>> {
        let spans = vec![
            TextSpanItem {
                text: "ColA".into(),
                bbox: Rect::new(30.0, 50.0, 60.0, 62.0)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "ColB".into(),
                bbox: Rect::new(101.5, 49.8, 131.0, 61.8)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "ColC".into(),
                bbox: Rect::new(170.2, 50.1, 200.0, 62.1)?,
                font_size: 10.0,
                bold: true,
            },
            TextSpanItem {
                text: "ValA1".into(),
                bbox: Rect::new(30.5, 70.2, 62.0, 82.2)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ValB1".into(),
                bbox: Rect::new(100.0, 70.0, 130.0, 82.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ValC1".into(),
                bbox: Rect::new(169.5, 69.8, 199.0, 81.8)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ValA2".into(),
                bbox: Rect::new(29.8, 90.0, 61.5, 102.0)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ValB2".into(),
                bbox: Rect::new(100.8, 90.2, 130.5, 102.2)?,
                font_size: 10.0,
                bold: false,
            },
            TextSpanItem {
                text: "ValC2".into(),
                bbox: Rect::new(170.0, 90.1, 200.0, 102.1)?,
                font_size: 10.0,
                bold: false,
            },
        ];
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 3);
        assert_eq!(tables[0].rows, 3);
        assert_eq!(tables[0].detector, TableDetectorKind::Alignment);
        Ok(())
    }

    #[test]
    fn alignment_handles_different_font_sizes() -> Result<(), Box<dyn std::error::Error>> {
        let large_spans = vec![
            TextSpanItem {
                text: "TITU".into(),
                bbox: Rect::new(30.0, 50.0, 80.0, 68.0)?,
                font_size: 16.0,
                bold: true,
            },
            TextSpanItem {
                text: "LO".into(),
                bbox: Rect::new(82.5, 50.0, 110.0, 68.0)?,
                font_size: 16.0,
                bold: true,
            },
        ];
        let merged_large = merge_contiguous_line_spans(&large_spans);
        assert_eq!(merged_large.len(), 1);
        assert_eq!(merged_large[0].text, "TITULO");

        let small_spans = vec![
            TextSpanItem {
                text: "SUB".into(),
                bbox: Rect::new(30.0, 50.0, 50.0, 58.0)?,
                font_size: 8.0,
                bold: false,
            },
            TextSpanItem {
                text: "TITULO".into(),
                bbox: Rect::new(51.2, 50.0, 85.0, 58.0)?,
                font_size: 8.0,
                bold: false,
            },
        ];
        let merged_small = merge_contiguous_line_spans(&small_spans);
        assert_eq!(merged_small.len(), 1);
        assert_eq!(merged_small[0].text, "SUBTITULO");
        Ok(())
    }
}
