use docsight_core::{
    BlockContent, BlockKind, DocsightError, Document, DocumentFormat, DocumentSource, ObjectId,
    Rect,
};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
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
    pub kind: BlockKind,
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

pub fn hit_test_document(
    source: &DocumentSource,
    page_number: u32,
    query: &HitQuery,
) -> Result<HitResult, DocsightError> {
    let document = match source.format() {
        DocumentFormat::Docx => {
            let unpaginated = parse_docx(source)?;
            let laid_out = layout_docx(unpaginated)?;
            laid_out.document
        }
        DocumentFormat::Pdf => {
            let pdf = PdfDocument::open(source)?;
            pdf.to_document()?
        }
    };

    hit_test(&document, page_number, query)
}

pub fn hit_test(
    document: &Document,
    page_number: u32,
    query: &HitQuery,
) -> Result<HitResult, DocsightError> {
    let page_exists = document.pages.iter().any(|p| p.number == page_number);
    if !page_exists {
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "page number {} exceeds document page count {}",
                page_number,
                document.pages.len()
            ),
        });
    }

    let mut hits = Vec::new();

    for block in &document.blocks {
        if block.page != Some(page_number) {
            continue;
        }

        let Some(bbox) = block.bbox else {
            continue;
        };

        let intersects = match query {
            HitQuery::Point(x, y) => bbox.contains_point(*x, *y),
            HitQuery::BBox(rect) => bbox.intersects(*rect),
        };

        if !intersects {
            continue;
        }

        let mut hit_cell = None;
        let mut snippet = block.text();

        if let BlockContent::Table(table) = &block.content {
            for cell in &table.cells {
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
                        let cell_text: Vec<String> = cell.blocks.iter().map(|b| b.text()).collect();
                        snippet = cell_text.join(" ");
                        break;
                    }
                }
            }
        }

        if snippet.len() > 120 {
            snippet.truncate(117);
            snippet.push_str("...");
        }

        hits.push(HitTarget {
            object_id: block.id.clone(),
            kind: block.kind,
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
