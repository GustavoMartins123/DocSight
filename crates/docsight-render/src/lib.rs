mod docx_raster;
pub mod hit;
pub mod trace;

use docsight_core::{Diagnostic, DocsightError, DocumentFormat, DocumentSource, Rect, write_all};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::{PdfDocument, RasterizedPage};
use docx_raster::rasterize_docx_page;
pub use hit::*;
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

pub use docx_raster::{encode_png, glyph_coverage, raster_font_fingerprint};

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedImage {
    pub metadata: RenderMetadata,
    pub warnings: Vec<Diagnostic>,
    png: Vec<u8>,
    pixels: Vec<u8>,
}

impl RenderedImage {
    pub fn png(&self) -> &[u8] {
        &self.png
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn write(&self, path: &Path) -> Result<(), DocsightError> {
        write_all(path, &self.png)
    }
}

pub fn render_document(
    source: &DocumentSource,
    request: &RenderRequest,
) -> Result<RenderedImage, DocsightError> {
    match source.format() {
        DocumentFormat::Pdf => render_pdf(source, request),
        DocumentFormat::Docx => render_docx(source, request),
    }
}

pub fn render_docx(
    source: &DocumentSource,
    request: &RenderRequest,
) -> Result<RenderedImage, DocsightError> {
    let unpaginated = parse_docx(source)?;
    let laid_out = layout_docx(unpaginated)?;
    let (page_num, crop_box, crop_object) = match &request.target {
        RenderTarget::Page { page } => (*page, None, None),
        RenderTarget::Region { page, bbox } => (*page, Some(*bbox), None),
        RenderTarget::Object { id } => {
            let block = laid_out
                .document
                .find_block(id)
                .ok_or_else(|| DocsightError::ObjectNotFound { object: id.clone() })?;
            let page = block
                .page
                .ok_or_else(|| DocsightError::UnsupportedFeature {
                    feature: "object has no page layout".to_owned(),
                })?;
            let bbox = block
                .bbox
                .ok_or_else(|| DocsightError::UnsupportedFeature {
                    feature: "object has no geometry".to_owned(),
                })?;
            (page, Some(bbox), Some(id.clone()))
        }
    };
    let page = laid_out
        .pages
        .into_iter()
        .find(|p| p.number == page_num)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {page_num}"),
        })?;
    let mut warnings = laid_out.document.warnings;
    let crop_box = match (crop_box, crop_object) {
        (Some(bbox), Some(object)) => {
            let page_bbox = Rect::new(0.0, 0.0, page.width_pt, page.height_pt)?;
            let (clipped, was_clipped) = clip_object_crop(page_bbox, bbox, &object)?;
            if was_clipped {
                warnings.push(Diagnostic {
                    code: "OBJECT_CROP_CLIPPED_TO_PAGE".to_owned(),
                    severity: docsight_core::DiagnosticSeverity::Warning,
                    message: format!("object {object} extends beyond page {page_num}"),
                    effect: "the crop contains only the portion intersecting the assigned page"
                        .to_owned(),
                    object: Some(docsight_core::ObjectId::from_raw(object)),
                    page: Some(page_num),
                });
            }
            Some(clipped)
        }
        (bbox, None) => bbox,
        (None, Some(object)) => {
            return Err(DocsightError::UnsupportedFeature {
                feature: format!("object {object} has no crop geometry"),
            });
        }
    };
    rasterize_docx_page(&page, request.dpi, crop_box, warnings)
}

fn clip_object_crop(
    page_bbox: Rect,
    object_bbox: Rect,
    object: &str,
) -> Result<(Rect, bool), DocsightError> {
    let clipped =
        page_bbox
            .intersection(object_bbox)
            .ok_or_else(|| DocsightError::UnsupportedFeature {
                feature: format!("object {object} lies outside its assigned page"),
            })?;
    Ok((clipped, clipped != object_bbox))
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
            if id.starts_with("span_") {
                let (page, bbox) = document.locate_span(id)?;
                document.rasterize(page, request.dpi, Some(bbox))?
            } else {
                let doc = document.to_document()?;
                let block = doc
                    .find_block(id)
                    .ok_or_else(|| DocsightError::ObjectNotFound { object: id.clone() })?;
                let page = block
                    .page
                    .ok_or_else(|| DocsightError::UnsupportedFeature {
                        feature: "object has no page layout".to_owned(),
                    })?;
                let bbox = block
                    .bbox
                    .ok_or_else(|| DocsightError::UnsupportedFeature {
                        feature: "object has no geometry".to_owned(),
                    })?;
                document.rasterize(page, request.dpi, Some(bbox))?
            }
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
        pixels: raster.pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::clip_object_crop;
    use docsight_core::{DocsightError, Rect};

    #[test]
    fn object_crop_is_clipped_to_the_page() -> Result<(), DocsightError> {
        let page = Rect::new(0.0, 0.0, 100.0, 100.0)?;
        let object = Rect::new(10.0, 90.0, 80.0, 140.0)?;
        let (clipped, was_clipped) = clip_object_crop(page, object, "tbl_example")?;
        assert_eq!(clipped, Rect::new(10.0, 90.0, 80.0, 100.0)?);
        assert!(was_clipped);
        Ok(())
    }
}
