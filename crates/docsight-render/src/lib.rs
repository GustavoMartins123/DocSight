mod contact_sheet;
mod docx_raster;
pub mod hit;
pub mod trace;

pub use contact_sheet::*;
use docsight_core::{
    Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, Rect, write_all,
};
use docsight_ingest::ingest_docx;
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

pub use docsight_core::encode_png;
pub use docx_raster::{glyph_coverage, raster_font_fingerprint};

pub fn document_glyph_coverage(
    _document: &Document,
    source: &DocumentSource,
) -> Result<f32, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => {
            let laid = ingest_docx(source)?;
            let mut total = 0_u64;
            let mut covered = 0_u64;
            for page in &laid.pages {
                for run in &page.runs {
                    let face = laid.fonts.resolve(&run.font_family, run.bold, run.italic)?;
                    let (run_covered, run_total) = face.glyph_coverage(&run.text)?;
                    total += run_total;
                    covered += run_covered;
                }
            }
            Ok(if total == 0 {
                1.0
            } else {
                covered as f32 / total as f32
            })
        }
        DocumentFormat::Pdf => Ok(1.0),
    }
}

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
    render_document_with_password(source, request, b"")
}

pub fn render_document_with_password(
    source: &DocumentSource,
    request: &RenderRequest,
    password: &[u8],
) -> Result<RenderedImage, DocsightError> {
    match source.format() {
        DocumentFormat::Pdf => render_pdf_with_password(source, request, password),
        DocumentFormat::Docx if !password.is_empty() => Err(DocsightError::InvalidArgument {
            message: "a PDF password cannot be applied to a DOCX document".to_owned(),
        }),
        DocumentFormat::Docx => render_docx(source, request),
    }
}

pub fn render_docx(
    source: &DocumentSource,
    request: &RenderRequest,
) -> Result<RenderedImage, DocsightError> {
    let laid_out = ingest_docx(source)?;
    let (page_num, crop_box, crop_object) = match &request.target {
        RenderTarget::Page { page } => (*page, None, None),
        RenderTarget::Region { page, bbox } => (*page, Some(*bbox), None),
        RenderTarget::Object { id } => {
            let (page, bbox) = object_crop_target(&laid_out.document, id)?;
            (page, Some(bbox), Some(id.clone()))
        }
    };
    let continuation_warning = crop_object
        .as_deref()
        .and_then(|id| continued_crop_warning(&laid_out.document, id, page_num));
    let page = laid_out
        .pages
        .into_iter()
        .find(|p| p.number == page_num)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {page_num}"),
        })?;
    let mut warnings = laid_out.document.warnings;
    warnings.extend(continuation_warning);
    let crop_box = match (crop_box, crop_object) {
        (Some(bbox), Some(object)) => {
            let page_bbox = Rect::new(0.0, 0.0, page.width_pt, page.height_pt)?;
            let (clipped, was_clipped) = clip_object_crop(page_bbox, bbox, &object)?;
            if was_clipped {
                warnings.push(clipped_crop_warning(&object, page_num));
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
    let images = decode_page_images(source, &page)?;
    rasterize_docx_page(
        &page,
        request.dpi,
        crop_box,
        warnings,
        &images,
        &laid_out.fonts,
    )
}

fn decode_page_images(
    source: &DocumentSource,
    page: &docsight_layout::LaidOutPage,
) -> Result<std::collections::BTreeMap<String, docsight_core::DecodedImage>, DocsightError> {
    let mut decoded = std::collections::BTreeMap::new();
    for image in &page.images {
        if decoded.contains_key(&image.target) {
            continue;
        }
        let bytes =
            docsight_ooxml::read_media_part(source.bytes(), &image.target)?.ok_or_else(|| {
                DocsightError::MalformedDocument {
                    message: format!(
                        "image resource {} for object {} is absent",
                        image.target, image.object_id
                    ),
                }
            })?;
        let data = if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
            docsight_core::decode_png(&bytes)?
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            docsight_core::decode_jpeg(&bytes)?
        } else {
            return Err(DocsightError::UnsupportedFeature {
                feature: format!(
                    "embedded image {} for object {} is not a supported PNG or JPEG",
                    image.target, image.object_id
                ),
            });
        };
        decoded.insert(image.target.clone(), data);
    }
    Ok(decoded)
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
    render_pdf_with_password(source, request, b"")
}

pub fn render_pdf_with_password(
    source: &DocumentSource,
    request: &RenderRequest,
    password: &[u8],
) -> Result<RenderedImage, DocsightError> {
    let document = PdfDocument::open_with_password(source, password)?;
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
                let (page, bbox) = object_crop_target(&doc, id)?;
                let page_record = doc
                    .page(page)
                    .ok_or_else(|| DocsightError::ObjectNotFound {
                        object: format!("page {page}"),
                    })?;
                let page_bbox = Rect::new(0.0, 0.0, page_record.width_pt, page_record.height_pt)?;
                let (clipped, was_clipped) = clip_object_crop(page_bbox, bbox, id)?;
                let mut raster = document.rasterize(page, request.dpi, Some(clipped))?;
                if was_clipped {
                    raster.warnings.push(clipped_crop_warning(id, page));
                }
                raster
            }
        }
    };
    from_raster(raster)
}

fn object_crop_target(document: &Document, id: &str) -> Result<(u32, Rect), DocsightError> {
    let object = document
        .resolve_object(id)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: id.to_owned(),
        })?;
    let page = object
        .page()
        .ok_or_else(|| DocsightError::UnsupportedFeature {
            feature: format!("object {id} has no page placement"),
        })?;
    let bbox = object
        .bbox()
        .ok_or_else(|| missing_crop_geometry(document, &object, id))?;
    Ok((page, bbox))
}

fn missing_crop_geometry(
    document: &Document,
    object: &docsight_core::DocumentObject<'_>,
    id: &str,
) -> DocsightError {
    let anchor = match object {
        docsight_core::DocumentObject::Hyperlink(link) => {
            link.anchor_path.as_deref().and_then(|path| {
                document
                    .blocks
                    .iter()
                    .find(|block| block.source.path == path)
            })
        }
        other => other.anchor_block(),
    };
    let feature = match anchor {
        Some(block) if block.bbox.is_some() => format!(
            "object {id} has no crop geometry of its own; crop its anchoring block {} instead",
            block.id
        ),
        _ => format!("object {id} has no crop geometry"),
    };
    DocsightError::UnsupportedFeature { feature }
}

fn continued_crop_warning(document: &Document, id: &str, page: u32) -> Option<Diagnostic> {
    let block = document.find_block(id)?;
    let continued: Vec<String> = block
        .continuations
        .iter()
        .map(|continuation| continuation.page.to_string())
        .collect();
    if continued.is_empty() {
        return None;
    }
    Some(Diagnostic {
        code: "OBJECT_CROP_CONTINUES_ON_OTHER_PAGES".to_owned(),
        severity: docsight_core::DiagnosticSeverity::Warning,
        message: format!(
            "object {id} starts on page {page} and continues on pages {}",
            continued.join(", ")
        ),
        effect: "the crop contains only the first page fragment; crop each continuation page by its region to see the rest".to_owned(),
        object: Some(block.id.clone()),
        page: Some(page),
        occurrences: None,
    })
}

fn clipped_crop_warning(object: &str, page: u32) -> Diagnostic {
    Diagnostic {
        code: "OBJECT_CROP_CLIPPED_TO_PAGE".to_owned(),
        severity: docsight_core::DiagnosticSeverity::Warning,
        message: format!("object {object} extends beyond page {page}"),
        effect: "the crop contains only the portion intersecting the assigned page".to_owned(),
        object: Some(docsight_core::ObjectId::from_raw(object)),
        page: Some(page),
        occurrences: None,
    }
}

fn from_raster(raster: RasterizedPage) -> Result<RenderedImage, DocsightError> {
    let png_bytes = u64::try_from(raster.png.len()).map_err(|_| DocsightError::ResourceLimit {
        resource: "raster PNG bytes".to_owned(),
        limit: docsight_core::MAX_RASTER_OUTPUT_BYTES,
    })?;
    if png_bytes > docsight_core::MAX_RASTER_OUTPUT_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "raster PNG bytes".to_owned(),
            limit: docsight_core::MAX_RASTER_OUTPUT_BYTES,
        });
    }
    Ok(RenderedImage {
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
    })
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
