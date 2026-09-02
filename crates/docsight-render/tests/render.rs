use docsight_core::{DocsightError, DocumentSource, Rect};
use docsight_pdf::PdfDocument;
use docsight_render::{RenderRequest, RenderTarget, render_pdf};

#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

#[test]
fn renders_page_region_and_object_through_one_pipeline() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(pdf_fixture::sample_pdf())?;
    let page = render_pdf(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 144,
        },
    )?;
    assert_eq!(
        (page.metadata.width_px, page.metadata.height_px),
        (400, 200)
    );
    let region = render_pdf(
        &source,
        &RenderRequest {
            target: RenderTarget::Region {
                page: 1,
                bbox: Rect::new(10.0, 10.0, 110.0, 60.0)?,
            },
            dpi: 144,
        },
    )?;
    assert_eq!(
        (region.metadata.width_px, region.metadata.height_px),
        (200, 100)
    );
    let span = PdfDocument::open(&source)?.page(1)?.spans.remove(0);
    let object = render_pdf(
        &source,
        &RenderRequest {
            target: RenderTarget::Object {
                id: span.id.to_string(),
            },
            dpi: 144,
        },
    )?;
    assert_eq!(object.metadata.bbox, span.bbox);
    assert_eq!(&object.png()[..8], b"\x89PNG\r\n\x1a\n");
    Ok(())
}
