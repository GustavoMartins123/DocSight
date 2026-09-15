use docsight_core::DocumentSource;
use docsight_ingest::ingest_docx;
use docsight_render::{RenderRequest, RenderTarget, render_document};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

#[test]
fn renders_docx_full_page_and_object_crop_to_png() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let source = DocumentSource::open(&path)?;

    let laid_out = ingest_docx(&source)?;
    let first_heading = laid_out
        .document
        .headings()
        .next()
        .ok_or("heading missing")?;
    let heading_id = first_heading.0.id.to_string();

    let page_render = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    assert_eq!(page_render.metadata.page, 1);
    assert_eq!(page_render.metadata.width_px, 612);
    assert_eq!(page_render.metadata.height_px, 792);
    assert!(page_render.png().starts_with(b"\x89PNG\r\n\x1a\n"));

    let crop_render = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Object { id: heading_id },
            dpi: 72,
        },
    )?;
    assert_eq!(crop_render.metadata.page, 1);
    assert!(crop_render.metadata.width_px > 0);
    assert!(crop_render.metadata.height_px > 0);
    assert!(crop_render.png().starts_with(b"\x89PNG\r\n\x1a\n"));

    let repeat_render = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    assert_eq!(page_render.png(), repeat_render.png());
    Ok(())
}
