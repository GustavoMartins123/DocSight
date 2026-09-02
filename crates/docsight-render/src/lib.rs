use docsight_core::{Diagnostic, DocsightError, DocumentSource, Rect, write_all};
use docsight_pdf::{PdfDocument, RasterizedPage};
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum RenderTarget {
    Page { page: u32 },
    Region { page: u32, bbox: Rect },
    Object { id: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderRequest {
    pub target: RenderTarget,
    pub dpi: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderMetadata {
    pub page: u32,
    pub dpi: u16,
    pub bbox: Rect,
    pub width_px: u32,
    pub height_px: u32,
    pub media_type: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedImage {
    pub metadata: RenderMetadata,
    pub warnings: Vec<Diagnostic>,
    png: Vec<u8>,
}

impl RenderedImage {
    pub fn png(&self) -> &[u8] {
        &self.png
    }

    pub fn write(&self, path: &Path) -> Result<(), DocsightError> {
        write_all(path, &self.png)
    }
}

pub fn render_pdf(
    source: &DocumentSource,
    request: &RenderRequest,
) -> Result<RenderedImage, DocsightError> {
    let document = PdfDocument::open(source)?;
    let raster = match &request.target {
        RenderTarget::Page { page } => document.rasterize(*page, request.dpi, None)?,
        RenderTarget::Region { page, bbox } => {
            document.rasterize(*page, request.dpi, Some(*bbox))?
        }
        RenderTarget::Object { id } => {
            let (page, bbox) = document.locate_span(id)?;
            document.rasterize(page, request.dpi, Some(bbox))?
        }
    };
    Ok(from_raster(raster))
}

fn from_raster(raster: RasterizedPage) -> RenderedImage {
    RenderedImage {
        metadata: RenderMetadata {
            page: raster.page,
            dpi: raster.dpi,
            bbox: raster.bbox,
            width_px: raster.width_px,
            height_px: raster.height_px,
            media_type: "image/png",
        },
        warnings: raster.warnings,
        png: raster.png,
    }
}
