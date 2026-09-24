use docsight_core::{
    Block, BlockContent, BlockKind, DocsightError, Document, DocumentSource, ObjectId, OverlayKind,
    Rect,
};
use docsight_ingest::ingest;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitCell {
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitTarget {
    pub object_id: ObjectId,
    pub kind: HitKind,
    pub page: u32,
    pub bbox: Rect,
    pub z_index: i32,
    pub reading_order: u32,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<HitCell>,
    pub text_snippet: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
}

impl From<BlockKind> for HitKind {
    fn from(kind: BlockKind) -> Self {
        match kind {
            BlockKind::Paragraph => Self::Paragraph,
            BlockKind::Heading => Self::Heading,
            BlockKind::ListItem => Self::ListItem,
            BlockKind::Table => Self::Table,
            BlockKind::Figure => Self::Figure,
            BlockKind::Shape => Self::Shape,
            BlockKind::Note => Self::Note,
            BlockKind::Unknown => Self::Unknown,
        }
    }
}

impl From<OverlayKind> for HitKind {
    fn from(kind: OverlayKind) -> Self {
        match kind {
            OverlayKind::Header => Self::Header,
            OverlayKind::Footer => Self::Footer,
            OverlayKind::Watermark => Self::Watermark,
            OverlayKind::CommentMarker => Self::CommentMarker,
            OverlayKind::Annotation => Self::Annotation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitResult {
    pub query_page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_point: Option<(f32, f32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_bbox: Option<Rect>,
    pub total_hits: usize,
    pub targets: Vec<HitTarget>,
}

pub enum HitQuery {
    Point(f32, f32),
    BBox(Rect),
}

struct HitBlock<'a> {
    block: &'a Block,
    bbox: Rect,
    cells: Vec<&'a docsight_core::TableCell>,
}

struct HitPageIndex<'a> {
    blocks: Vec<HitBlock<'a>>,
    base_reading_order: u32,
}

impl<'a> HitPageIndex<'a> {
    fn new(document: &'a Document, page_number: u32) -> Self {
        let blocks = document
            .blocks
            .iter()
            .filter_map(|block| {
                let bbox = block.bbox_on_page(page_number)?;
                let cells = match &block.content {
                    BlockContent::Table(table) => table.cells.iter().collect(),
                    _ => Vec::new(),
                };
                Some(HitBlock { block, bbox, cells })
            })
            .collect();
        let base_reading_order = document
            .blocks
            .iter()
            .map(|block| block.reading_order)
            .fold(0, u32::max);
        Self {
            blocks,
            base_reading_order,
        }
    }
}

pub fn hit_test_document(
    source: &DocumentSource,
    page_number: u32,
    query: &HitQuery,
) -> Result<HitResult, DocsightError> {
    let document = ingest(source)?;

    hit_test(&document, page_number, query)
}

pub fn hit_test(
    document: &Document,
    page_number: u32,
    query: &HitQuery,
) -> Result<HitResult, DocsightError> {
    validate_query(query)?;
    let page = document.pages.iter().find(|p| p.number == page_number);
    let Some(page) = page else {
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "page number {} exceeds document page count {}",
                page_number,
                document.pages.len()
            ),
        });
    };

    let mut hits = Vec::new();
    let index = HitPageIndex::new(document, page_number);

    for entry in &index.blocks {
        let block = entry.block;
        let bbox = entry.bbox;
        let intersects = match query {
            HitQuery::Point(x, y) => bbox.contains_point(*x, *y),
            HitQuery::BBox(rect) => bbox.intersects(*rect),
        };

        if !intersects {
            continue;
        }

        let mut hit_cell = None;
        let mut snippet =
            block
                .text_on_page(page_number)?
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: format!(
                        "block {} has geometry on page {page_number} without fragment text",
                        block.id
                    ),
                })?;

        for cell in &entry.cells {
            if let Some(cell_box) = cell.bbox {
                let cell_hit = match query {
                    HitQuery::Point(x, y) => cell_box.contains_point(*x, *y),
                    HitQuery::BBox(rect) => cell_box.intersects(*rect),
                };
                if cell_hit {
                    hit_cell = Some(HitCell {
                        row: cell.row,
                        column: cell.column,
                        row_span: cell.row_span,
                        column_span: cell.column_span,
                        bbox: Some(cell_box),
                    });
                    snippet = cell.text.clone();
                    break;
                }
            }
        }

        snippet = truncate_snippet(&snippet);

        hits.push(HitTarget {
            object_id: block.id.clone(),
            kind: block.kind.into(),
            page: page_number,
            bbox,
            z_index: block.z_index,
            reading_order: block.reading_order,
            source_path: block.source.path.clone(),
            confidence: if block.confidence < 0.999 {
                Some(block.confidence)
            } else {
                None
            },
            cell: hit_cell,
            text_snippet: snippet,
        });
    }

    let base_reading_order = index.base_reading_order;
    for (index, overlay) in page.overlays.iter().enumerate() {
        let Some(bbox) = overlay.bbox else {
            continue;
        };
        let intersects = match query {
            HitQuery::Point(x, y) => bbox.contains_point(*x, *y),
            HitQuery::BBox(rect) => bbox.intersects(*rect),
        };
        if !intersects {
            continue;
        }
        let overlay_index = u32::try_from(index + 1).map_err(|_| DocsightError::ResourceLimit {
            resource: "page overlay reading order".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
        let overlay_order = base_reading_order
            .checked_add(overlay_index)
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "page overlay reading order".to_owned(),
                limit: u64::from(u32::MAX),
            })?;
        hits.push(HitTarget {
            object_id: overlay.id.clone(),
            kind: overlay.kind.into(),
            page: page_number,
            bbox,
            z_index: 1,
            reading_order: overlay_order,
            source_path: overlay.source.path.clone(),
            confidence: None,
            cell: None,
            text_snippet: truncate_snippet(&overlay.text),
        });
    }

    hits.sort_by(|a, b| {
        b.z_index
            .cmp(&a.z_index)
            .then_with(|| a.reading_order.cmp(&b.reading_order))
            .then_with(|| a.object_id.cmp(&b.object_id))
    });

    let (query_point, query_bbox) = match query {
        HitQuery::Point(x, y) => (Some((*x, *y)), None),
        HitQuery::BBox(rect) => (None, Some(*rect)),
    };

    Ok(HitResult {
        query_page: page_number,
        query_point,
        query_bbox,
        total_hits: hits.len(),
        targets: hits,
    })
}

fn validate_query(query: &HitQuery) -> Result<(), DocsightError> {
    match query {
        HitQuery::Point(x, y) if !x.is_finite() || !y.is_finite() => {
            Err(DocsightError::InvalidArgument {
                message: "point coordinates must be finite".to_owned(),
            })
        }
        HitQuery::BBox(rect) => Rect::new(rect.x0, rect.y0, rect.x1, rect.y1).map(|_| ()),
        HitQuery::Point(_, _) => Ok(()),
    }
}

fn truncate_snippet(text: &str) -> String {
    if text.chars().count() <= 120 {
        return text.to_owned();
    }
    let mut snippet: String = text.chars().take(117).collect();
    snippet.push_str("...");
    snippet
}
