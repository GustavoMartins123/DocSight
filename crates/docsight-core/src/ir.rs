use crate::{Diagnostic, DocsightError, DocumentFormat, ObjectId, Rect};
use serde::{Deserialize, Serialize};

pub const IR_SCHEMA_VERSION: &str = "1.1";
pub const IR_ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IrVersion {
    pub schema_version: String,
    pub engine_version: String,
}

impl IrVersion {
    pub fn current() -> Self {
        Self {
            schema_version: IR_SCHEMA_VERSION.to_owned(),
            engine_version: IR_ENGINE_VERSION.to_owned(),
        }
    }

    pub fn is_current_schema(&self) -> bool {
        self.schema_version == IR_SCHEMA_VERSION
    }
}

impl Default for IrVersion {
    fn default() -> Self {
        Self::current()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub path: String,
    pub offset: Option<u64>,
    pub length: Option<u64>,
}

impl SourceSpan {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            offset: None,
            length: None,
        }
    }

    pub fn with_range(path: impl Into<String>, offset: u64, length: u64) -> Self {
        Self {
            path: path.into(),
            offset: Some(offset),
            length: Some(length),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphBlock {
    pub text: String,
    pub style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeadingBlock {
    pub level: u8,
    pub text: String,
    pub style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListItemBlock {
    pub level: u8,
    pub marker: Option<String>,
    pub format: Option<String>,
    pub pattern: Option<String>,
    pub ordered: Option<bool>,
    pub text: String,
    pub style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub id: ObjectId,
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub bbox: Option<Rect>,
    pub text: String,
    pub blocks: Vec<Block>,
    pub source: SourceSpan,
}

impl TableCell {
    pub fn nested_tables(&self) -> impl Iterator<Item = (&Block, &TableBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Table(table) => Some((block, table)),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableBlock {
    pub rows: u32,
    pub columns: u32,
    pub header_rows: u32,
    pub cells: Vec<TableCell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_widths_pt: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigureBlock {
    pub alt_text: Option<String>,
    pub caption: Option<String>,
    pub resource_id: Option<String>,
    pub width_pt: Option<f32>,
    pub height_pt: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeBlock {
    pub shape_type: String,
    pub label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Footnote,
    Endnote,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteBlock {
    pub kind: NoteKind,
    pub note_id: String,
    pub text: String,
    #[serde(default)]
    pub anchor_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnknownBlock {
    pub raw_tag: String,
    pub details: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockContent {
    Paragraph(ParagraphBlock),
    Heading(HeadingBlock),
    ListItem(ListItemBlock),
    Table(TableBlock),
    Figure(FigureBlock),
    Shape(ShapeBlock),
    Note(NoteBlock),
    Unknown(UnknownBlock),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParagraphFormat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<TextAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_before_pt: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_after_pt: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent_left_pt: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent_right_pt: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent_first_line_pt: Option<f32>,
}

impl ParagraphFormat {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LayoutFlags {
    #[serde(default)]
    pub page_break_before: bool,
    #[serde(default)]
    pub break_after: bool,
    #[serde(default)]
    pub keep_with_next: bool,
    #[serde(default)]
    pub keep_lines: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: ObjectId,
    pub kind: BlockKind,
    pub page: Option<u32>,
    pub bbox: Option<Rect>,
    pub z_index: i32,
    pub reading_order: u32,
    pub source: SourceSpan,
    pub confidence: f32,
    #[serde(default)]
    pub flags: LayoutFlags,
    #[serde(default, skip_serializing_if = "ParagraphFormat::is_default")]
    pub format: ParagraphFormat,
    pub content: BlockContent,
}

impl Block {
    pub fn to_ref(&self) -> Option<BlockRef> {
        let page = self.page?;
        let bbox_pt = self.bbox?;
        Some(BlockRef {
            id: self.id.clone(),
            kind: self.kind,
            page,
            bbox_pt,
            z_index: self.z_index,
            reading_order: self.reading_order,
            source: self.source.clone(),
            confidence: self.confidence,
        })
    }

    pub fn text(&self) -> String {
        match &self.content {
            BlockContent::Paragraph(block) => block.text.clone(),
            BlockContent::Heading(block) => block.text.clone(),
            BlockContent::ListItem(block) => block.text.clone(),
            BlockContent::Table(block) => table_to_tsv_string(block),
            BlockContent::Figure(block) => block
                .caption
                .clone()
                .or_else(|| block.alt_text.clone())
                .unwrap_or_default(),
            BlockContent::Shape(block) => block.label.clone().unwrap_or_default(),
            BlockContent::Note(block) => block.text.clone(),
            BlockContent::Unknown(block) => block.details.clone().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockRef {
    pub id: ObjectId,
    pub kind: BlockKind,
    pub page: u32,
    pub bbox_pt: Rect,
    pub z_index: i32,
    pub reading_order: u32,
    pub source: SourceSpan,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageRef {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    pub id: ObjectId,
    pub kind: OverlayKind,
    pub page: u32,
    pub bbox: Option<Rect>,
    pub text: String,
    pub source: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub block_ids: Vec<ObjectId>,
    pub overlays: Vec<Overlay>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: ObjectId,
    pub section_index: u32,
    pub page_width_pt: Option<f32>,
    pub page_height_pt: Option<f32>,
    pub margin_top_pt: Option<f32>,
    pub margin_right_pt: Option<f32>,
    pub margin_bottom_pt: Option<f32>,
    pub margin_left_pt: Option<f32>,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub id: String,
    pub name: Option<String>,
    pub based_on: Option<String>,
    pub font_family: Option<String>,
    pub font_size_pt: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Font,
    Image,
    Relationship,
    EmbeddedObject,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub id: ObjectId,
    pub kind: ResourceKind,
    pub name: String,
    pub target: String,
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub producer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hyperlink {
    pub id: ObjectId,
    pub text: String,
    pub target: String,
    pub is_external: bool,
    pub page: Option<u32>,
    #[serde(default)]
    pub anchor_path: Option<String>,
    pub source: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: ObjectId,
    pub author: Option<String>,
    pub date: Option<String>,
    pub text: String,
    pub page: Option<u32>,
    #[serde(default)]
    pub anchor_path: Option<String>,
    pub source: SourceSpan,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrackedChanges {
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub version: IrVersion,
    pub id: String,
    pub sha256: String,
    pub format: DocumentFormat,
    pub size_bytes: u64,
    pub metadata: DocumentMetadata,
    pub styles: Vec<Style>,
    pub sections: Vec<Section>,
    pub pages: Vec<Page>,
    pub blocks: Vec<Block>,
    pub resources: Vec<Resource>,
    pub links: Vec<Hyperlink>,
    pub comments: Vec<Comment>,
    pub tracked_changes: TrackedChanges,
    pub warnings: Vec<Diagnostic>,
}

impl Document {
    pub fn paragraphs(&self) -> impl Iterator<Item = (&Block, &ParagraphBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Paragraph(p) => Some((block, p)),
            _ => None,
        })
    }

    pub fn headings(&self) -> impl Iterator<Item = (&Block, &HeadingBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Heading(h) => Some((block, h)),
            _ => None,
        })
    }

    pub fn list_items(&self) -> impl Iterator<Item = (&Block, &ListItemBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::ListItem(li) => Some((block, li)),
            _ => None,
        })
    }

    pub fn tables(&self) -> impl Iterator<Item = (&Block, &TableBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Table(t) => Some((block, t)),
            _ => None,
        })
    }

    pub fn figures(&self) -> impl Iterator<Item = (&Block, &FigureBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Figure(f) => Some((block, f)),
            _ => None,
        })
    }

    pub fn notes(&self) -> impl Iterator<Item = (&Block, &NoteBlock)> {
        self.blocks.iter().filter_map(|block| match &block.content {
            BlockContent::Note(n) => Some((block, n)),
            _ => None,
        })
    }

    pub fn find_table(&self, id: &str) -> Option<(&Block, &TableBlock)> {
        self.tables().find(|(block, _)| block.id.as_str() == id)
    }

    pub fn find_block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|block| block.id.as_str() == id)
    }

    pub fn page(&self, number: u32) -> Option<&Page> {
        self.pages.iter().find(|page| page.number == number)
    }

    pub fn page_blocks(&self, number: u32) -> impl Iterator<Item = &Block> {
        self.blocks
            .iter()
            .filter(move |block| block.page == Some(number))
    }
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
    let mut grid_cells: std::collections::BTreeMap<(u32, u32), &TableCell> =
        std::collections::BTreeMap::new();
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
