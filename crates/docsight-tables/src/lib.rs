mod alignment;

use docsight_core::{
    Block, BlockContent, BlockKind, DocsightError, LayoutFlags, ObjectId, Rect, SourceSpan,
    TableBlock, TableCell,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
            Self::Alignment => "alignment-v2",
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
                    page: None,
                    continuations: Vec::new(),
                    format: Default::default(),
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
            inline_runs: Vec::new(),
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
) -> Result<Vec<InferredTable>, DocsightError> {
    let mut detected = Vec::new();

    let ruled_tables = detect_ruled_tables(page, spans, rulings)?;
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

    let remaining_spans: Vec<TextSpanItem> = spans
        .iter()
        .enumerate()
        .filter(|(idx, _)| !covered_span_indices.contains(idx))
        .map(|(_, span)| span.clone())
        .collect();

    let alignment_tables = alignment::detect(page, &remaining_spans)?;
    detected.extend(alignment_tables);

    detected.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(detected)
}

pub const MAX_INFERRED_TABLE_CELLS: usize = 262_144;

fn checked_grid_size(rows: usize, columns: usize) -> Result<(u32, u32), DocsightError> {
    let limit = || DocsightError::ResourceLimit {
        resource: "inferred PDF table cells".to_owned(),
        limit: MAX_INFERRED_TABLE_CELLS as u64,
    };
    rows.checked_mul(columns)
        .filter(|cells| *cells <= MAX_INFERRED_TABLE_CELLS)
        .ok_or_else(limit)?;
    Ok((
        u32::try_from(rows).map_err(|_| limit())?,
        u32::try_from(columns).map_err(|_| limit())?,
    ))
}

const RULING_CONNECT_GAP_PT: f32 = 6.0;

const TABLE_ROW_INDEX_CELL_SIZE: f32 = 32.0;
const MAX_TABLE_ROW_INDEX_CELLS: usize = 4096;

struct SpanRowIndex {
    buckets: BTreeMap<i64, Vec<usize>>,
    wide: Vec<usize>,
    span_count: usize,
}

impl SpanRowIndex {
    fn new(spans: &[TextSpanItem]) -> Self {
        let mut buckets: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        let mut wide = Vec::new();
        for (index, span) in spans.iter().enumerate() {
            let center_y = (span.bbox.y0 + span.bbox.y1) * 0.5;
            if center_y.is_finite() {
                let bucket = (center_y / TABLE_ROW_INDEX_CELL_SIZE).floor() as i64;
                buckets.entry(bucket).or_default().push(index);
            } else {
                wide.push(index);
            }
        }
        Self {
            buckets,
            wide,
            span_count: spans.len(),
        }
    }

    fn candidates(&self, top: f32, bottom: f32) -> Vec<usize> {
        if !top.is_finite() || !bottom.is_finite() {
            return (0..self.span_count).collect();
        }
        let minimum = (top / TABLE_ROW_INDEX_CELL_SIZE).floor() as i64;
        let maximum = (bottom / TABLE_ROW_INDEX_CELL_SIZE).floor() as i64;
        let width = maximum.saturating_sub(minimum).saturating_add(1);
        if width > MAX_TABLE_ROW_INDEX_CELLS as i64 {
            return (0..self.span_count).collect();
        }
        let mut candidates = self.wide.clone();
        for bucket in minimum..=maximum {
            if let Some(indices) = self.buckets.get(&bucket) {
                candidates.extend(indices.iter().copied());
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }
}

fn detect_ruled_tables(
    page: u32,
    spans: &[TextSpanItem],
    rulings: &[RulingSegment],
) -> Result<Vec<InferredTable>, DocsightError> {
    let span_index = SpanRowIndex::new(spans);
    let mut tables = Vec::new();
    for component in segment_rulings(rulings) {
        if let Some(table) = build_ruled_table(page, spans, &span_index, &component)? {
            tables.push(table);
        }
    }
    Ok(tables)
}

const RULING_INDEX_CELL_SIZE: f32 = 32.0;
const MAX_RULING_INDEX_CELLS: usize = 4096;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct RulingKey {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl From<&RulingSegment> for RulingKey {
    fn from(ruling: &RulingSegment) -> Self {
        Self {
            x0: ruling.x0.to_bits(),
            y0: ruling.y0.to_bits(),
            x1: ruling.x1.to_bits(),
            y1: ruling.y1.to_bits(),
        }
    }
}

fn ruling_is_finite(ruling: &RulingSegment) -> bool {
    [ruling.x0, ruling.y0, ruling.x1, ruling.y1]
        .iter()
        .all(|value| value.is_finite())
}

fn ruling_cell_coordinate(value: f32) -> i64 {
    if value.is_finite() {
        (value / RULING_INDEX_CELL_SIZE).floor() as i64
    } else {
        0
    }
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

    let mut groups: BTreeMap<RulingKey, Vec<usize>> = BTreeMap::new();
    let mut representatives = Vec::with_capacity(count);
    for (index, ruling) in rulings.iter().enumerate() {
        if ruling_is_finite(ruling) {
            groups.entry(ruling.into()).or_default().push(index);
        } else {
            representatives.push(index);
        }
    }
    for indices in groups.values_mut() {
        indices.sort_unstable();
        let representative = indices[0];
        for &index in indices.iter().skip(1) {
            union(&mut parent, representative, index);
        }
        representatives.push(representative);
    }
    representatives.sort_unstable();

    let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    let mut wide = Vec::new();
    for &representative in &representatives {
        let ruling = &rulings[representative];
        if !ruling_is_finite(ruling) {
            wide.push(representative);
            continue;
        }
        let min_x = ruling.x0.min(ruling.x1) - RULING_CONNECT_GAP_PT;
        let max_x = ruling.x0.max(ruling.x1) + RULING_CONNECT_GAP_PT;
        let min_y = ruling.y0.min(ruling.y1) - RULING_CONNECT_GAP_PT;
        let max_y = ruling.y0.max(ruling.y1) + RULING_CONNECT_GAP_PT;
        let min_cell_x = ruling_cell_coordinate(min_x);
        let max_cell_x = ruling_cell_coordinate(max_x);
        let min_cell_y = ruling_cell_coordinate(min_y);
        let max_cell_y = ruling_cell_coordinate(max_y);
        let cell_width = max_cell_x.saturating_sub(min_cell_x).saturating_add(1);
        let cell_height = max_cell_y.saturating_sub(min_cell_y).saturating_add(1);
        let cell_count = cell_width.saturating_mul(cell_height);
        if cell_count <= MAX_RULING_INDEX_CELLS as i64 {
            for cell_x in min_cell_x..=max_cell_x {
                for cell_y in min_cell_y..=max_cell_y {
                    grid.entry((cell_x, cell_y))
                        .or_default()
                        .push(representative);
                }
            }
        } else {
            wide.push(representative);
        }
    }

    let mut pairs = BTreeSet::new();
    for indices in grid.values() {
        for (offset, &left) in indices.iter().enumerate() {
            for &right in indices.iter().skip(offset + 1) {
                let (left, right) = if left < right {
                    (left, right)
                } else {
                    (right, left)
                };
                if left != right && rulings_near(&rulings[left], &rulings[right]) {
                    pairs.insert((left, right));
                }
            }
        }
    }
    for &left in &wide {
        for &right in &representatives {
            if left != right && rulings_near(&rulings[left], &rulings[right]) {
                let pair = if left < right {
                    (left, right)
                } else {
                    (right, left)
                };
                pairs.insert(pair);
            }
        }
    }
    for (left, right) in pairs {
        union(&mut parent, left, right);
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
    span_index: &SpanRowIndex,
    component: &[RulingSegment],
) -> Result<Option<InferredTable>, DocsightError> {
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
        return Ok(None);
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
        return Ok(None);
    }

    let (rows, cols) = checked_grid_size(clustered_y.len() - 1, clustered_x.len() - 1)?;

    let x0 = *clustered_x
        .first()
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "inferred table boundary is missing".to_owned(),
        })?;
    let x1 = *clustered_x
        .last()
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "inferred table boundary is missing".to_owned(),
        })?;
    let y0 = *clustered_y
        .first()
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "inferred table boundary is missing".to_owned(),
        })?;
    let y1 = *clustered_y
        .last()
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "inferred table boundary is missing".to_owned(),
        })?;

    let bbox = Rect::new(x0, y0, x1, y1)?;

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

            let cell_bbox = Some(Rect::new(col_left, row_top, col_right, row_bot)?);

            let mut cell_spans: Vec<&TextSpanItem> = span_index
                .candidates(row_top, row_bot)
                .into_iter()
                .filter_map(|index| spans.get(index))
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
        return Ok(None);
    }

    let confidence = if non_empty_cells >= total_cells / 3 {
        0.991
    } else {
        0.940
    };

    Ok(Some(InferredTable {
        page,
        bbox,
        rows,
        columns: cols,
        header_rows: 1,
        cells,
        confidence,
        detector: TableDetectorKind::Ruled,
    }))
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

pub fn table_to_tsv_string(table: &TableBlock) -> String {
    let rows = table.rows as usize;
    let mut grid = vec![Vec::new(); rows];
    for cell in &table.cells {
        if let Some(row) = grid.get_mut(cell.row as usize) {
            row.push(cell.text.clone());
        }
    }
    grid.into_iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn table_to_markdown(table: &TableBlock) -> Result<String, DocsightError> {
    let columns = usize::try_from(table.columns).map_err(|_| DocsightError::ResourceLimit {
        resource: "table columns".to_owned(),
        limit: usize::MAX as u64,
    })?;
    let rows = usize::try_from(table.rows).map_err(|_| DocsightError::ResourceLimit {
        resource: "table rows".to_owned(),
        limit: usize::MAX as u64,
    })?;
    if columns == 0 || rows == 0 {
        return Ok(String::new());
    }
    let mut grid = vec![vec![String::new(); columns]; rows];
    for cell in &table.cells {
        let row = cell.row as usize;
        let col = cell.column as usize;
        let row_span = (cell.row_span as usize).max(1);
        let col_span = (cell.column_span as usize).max(1);
        for r in 0..row_span {
            for c in 0..col_span {
                if let Some(slot) = grid
                    .get_mut(row.saturating_add(r))
                    .and_then(|row_cells| row_cells.get_mut(col.saturating_add(c)))
                {
                    if r == 0 && c == 0 {
                        *slot = cell.text.replace('\n', " ").replace('|', "\\|");
                    } else if slot.is_empty() {
                        *slot = format!("(merged r{row}c{col})");
                    }
                }
            }
        }
    }
    let mut widths = vec![3_usize; columns];
    for row in &grid {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    let mut output = String::new();
    for (row_index, row) in grid.iter().enumerate() {
        output.push('|');
        for (col_index, cell) in row.iter().enumerate() {
            let width = widths.get(col_index).copied().unwrap_or(3);
            output.push(' ');
            output.push_str(cell);
            let padding = width.saturating_sub(cell.chars().count());
            output.push_str(&" ".repeat(padding));
            output.push_str(" |");
        }
        output.push('\n');
        if row_index == 0 {
            output.push('|');
            for width in &widths {
                output.push(' ');
                output.push_str(&"-".repeat(*width));
                output.push_str(" |");
            }
            output.push('\n');
        }
    }
    Ok(output)
}

pub fn table_to_csv(table: &TableBlock) -> Result<String, DocsightError> {
    let columns = usize::try_from(table.columns).map_err(|_| DocsightError::ResourceLimit {
        resource: "table columns".to_owned(),
        limit: usize::MAX as u64,
    })?;
    let rows = usize::try_from(table.rows).map_err(|_| DocsightError::ResourceLimit {
        resource: "table rows".to_owned(),
        limit: usize::MAX as u64,
    })?;
    if columns == 0 || rows == 0 {
        return Ok(String::new());
    }
    let mut grid = vec![vec![String::new(); columns]; rows];
    for cell in &table.cells {
        let row = cell.row as usize;
        let col = cell.column as usize;
        if let Some(slot) = grid
            .get_mut(row)
            .and_then(|row_cells| row_cells.get_mut(col))
        {
            *slot = cell.text.clone();
        }
    }
    let mut output = String::new();
    for row in grid {
        let escaped_row: Vec<String> = row
            .into_iter()
            .map(|cell| {
                if cell.contains(',') || cell.contains('"') || cell.contains('\n') {
                    format!("\"{}\"", cell.replace('"', "\"\""))
                } else {
                    cell
                }
            })
            .collect();
        output.push_str(&escaped_row.join(","));
        output.push('\n');
    }
    Ok(output)
}

pub fn table_to_tsv(table: &TableBlock) -> Result<String, DocsightError> {
    let columns = usize::try_from(table.columns).map_err(|_| DocsightError::ResourceLimit {
        resource: "table columns".to_owned(),
        limit: usize::MAX as u64,
    })?;
    let rows = usize::try_from(table.rows).map_err(|_| DocsightError::ResourceLimit {
        resource: "table rows".to_owned(),
        limit: usize::MAX as u64,
    })?;
    if columns == 0 || rows == 0 {
        return Ok(String::new());
    }
    let mut grid = vec![vec![String::new(); columns]; rows];
    for cell in &table.cells {
        let row = cell.row as usize;
        let col = cell.column as usize;
        if let Some(slot) = grid
            .get_mut(row)
            .and_then(|row_cells| row_cells.get_mut(col))
        {
            *slot = cell.text.replace(['\t', '\n'], " ");
        }
    }
    let mut output = String::new();
    for row in grid {
        output.push_str(&row.join("\t"));
        output.push('\n');
    }
    Ok(output)
}

pub fn table_to_html(table: &TableBlock) -> Result<String, DocsightError> {
    let mut output = String::from("<table>\n");
    let mut grid_cells: BTreeMap<(u32, u32), &TableCell> = BTreeMap::new();
    for cell in &table.cells {
        grid_cells.insert((cell.row, cell.column), cell);
    }
    for r in 0..table.rows {
        output.push_str("  <tr>\n");
        for c in 0..table.columns {
            if let Some(cell) = grid_cells.get(&(r, c)) {
                let tag = if r < table.header_rows { "th" } else { "td" };
                let mut attrs = String::new();
                if cell.row_span > 1 {
                    attrs.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
                }
                if cell.column_span > 1 {
                    attrs.push_str(&format!(" colspan=\"{}\"", cell.column_span));
                }
                let escaped = cell
                    .text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;");
                output.push_str(&format!("    <{tag}{attrs}>{escaped}</{tag}>\n"));
            }
        }
        output.push_str("  </tr>\n");
    }
    output.push_str("</table>\n");
    Ok(output)
}

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

        let tables = detect_tables(1, &spans, &rulings, 300.0, 400.0)?;
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
        assert_eq!(
            table_to_tsv_string(&block),
            "Header 1\tHeader 2\nVal 1\tVal 2"
        );

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

        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &rulings, 300.0, 400.0)?;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0].text, "ABCD EF");
        Ok(())
    }

    #[test]
    fn ordinary_word_spacing_does_not_define_twenty_two_columns()
    -> Result<(), Box<dyn std::error::Error>> {
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
        let tables = detect_tables(1, &spans, &[], 500.0, 400.0)?;
        assert!(tables.is_empty());
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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
        let tables = detect_tables(1, &spans, &[], 300.0, 400.0)?;
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

#[cfg(test)]
mod alignment_evidence_tests {
    use super::*;

    fn span(
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        bold: bool,
    ) -> Result<TextSpanItem, DocsightError> {
        Ok(TextSpanItem {
            text: text.to_owned(),
            bbox: Rect::new(x, y, x + width, y + 12.0)?,
            font_size: 10.0,
            bold,
        })
    }

    #[test]
    fn centered_title_and_subtitle_are_not_a_table() -> Result<(), DocsightError> {
        let spans = vec![
            span("Field", 150.0, 50.0, 25.0, true)?,
            span("Guide", 180.0, 50.0, 30.0, true)?,
            span("A", 100.0, 70.0, 6.0, false)?,
            span("practical", 111.0, 70.0, 42.0, false)?,
            span("reference", 158.0, 70.0, 50.0, false)?,
            span("for", 213.0, 70.0, 15.0, false)?,
            span("visitors", 233.0, 70.0, 40.0, false)?,
        ];
        assert!(detect_tables(1, &spans, &[], 400.0, 500.0)?.is_empty());
        Ok(())
    }

    #[test]
    fn unrelated_fragment_starts_do_not_define_columns() -> Result<(), DocsightError> {
        let spans = vec![
            span("North", 30.0, 50.0, 25.0, false)?,
            span("entrance", 160.0, 50.0, 40.0, false)?,
            span("Open", 75.0, 70.0, 20.0, false)?,
            span("daily", 215.0, 70.0, 25.0, false)?,
        ];
        assert!(detect_tables(1, &spans, &[], 400.0, 500.0)?.is_empty());
        Ok(())
    }

    #[test]
    fn right_aligned_values_keep_one_column_without_an_invented_header() -> Result<(), DocsightError>
    {
        let spans = vec![
            span("North", 30.0, 50.0, 25.0, false)?,
            span("1", 195.0, 50.0, 5.0, false)?,
            span("South", 30.0, 70.0, 25.0, false)?,
            span("12000", 175.0, 70.0, 25.0, false)?,
            span("East", 30.0, 90.0, 20.0, false)?,
            span("600", 185.0, 90.0, 15.0, false)?,
        ];
        let tables = detect_tables(1, &spans, &[], 400.0, 500.0)?;
        assert_eq!(tables.len(), 1);
        assert_eq!((tables[0].rows, tables[0].columns), (3, 2));
        assert_eq!(tables[0].header_rows, 0);
        assert_eq!(tables[0].cells[3].text, "12000");
        Ok(())
    }

    #[test]
    fn short_textual_grid_without_header_requires_review() -> Result<(), DocsightError> {
        let spans = vec![
            span("North", 30.0, 50.0, 25.0, false)?,
            span("Open", 160.0, 50.0, 20.0, false)?,
            span("South", 30.0, 70.0, 25.0, false)?,
            span("Closed", 160.0, 70.0, 30.0, false)?,
        ];
        let tables = detect_tables(1, &spans, &[], 400.0, 500.0)?;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].header_rows, 0);
        assert!(tables[0].confidence < 0.70);
        Ok(())
    }

    #[test]
    fn multiword_cells_keep_spaces_and_real_gutters() -> Result<(), DocsightError> {
        let spans = vec![
            span("Site", 30.0, 50.0, 20.0, true)?,
            span("name", 55.0, 50.0, 20.0, true)?,
            span("Status", 160.0, 50.0, 30.0, true)?,
            span("North", 30.0, 70.0, 25.0, false)?,
            span("building", 60.0, 70.0, 40.0, false)?,
            span("Open", 160.0, 70.0, 20.0, false)?,
        ];
        let tables = detect_tables(1, &spans, &[], 400.0, 500.0)?;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 2);
        assert_eq!(tables[0].cells[0].text, "Site name");
        assert_eq!(tables[0].cells[2].text, "North building");
        assert_eq!(tables[0].header_rows, 1);
        Ok(())
    }

    #[test]
    fn newspaper_columns_are_not_a_tabular_grid() -> Result<(), DocsightError> {
        let mut spans = Vec::new();
        for row in 0..4 {
            let y = 50.0 + row as f32 * 14.0;
            for x in [30.0, 240.0] {
                spans.push(span(
                    "The visitors walked through the quiet garden",
                    x,
                    y,
                    180.0,
                    false,
                )?);
            }
        }
        assert!(detect_tables(1, &spans, &[], 500.0, 500.0)?.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod inference_limit_tests {
    use super::*;

    #[test]
    fn inferred_grid_budget_checks_boundaries_and_overflow() -> Result<(), DocsightError> {
        assert_eq!(
            checked_grid_size(1, MAX_INFERRED_TABLE_CELLS - 1)?.1,
            262_143
        );
        assert_eq!(checked_grid_size(512, 512)?, (512, 512));
        for (rows, columns) in [(1, MAX_INFERRED_TABLE_CELLS + 1), (usize::MAX, 2)] {
            assert!(
                matches!(checked_grid_size(rows, columns), Err(DocsightError::ResourceLimit { resource, limit }) if resource == "inferred PDF table cells" && limit == 262_144)
            );
        }
        Ok(())
    }
}
