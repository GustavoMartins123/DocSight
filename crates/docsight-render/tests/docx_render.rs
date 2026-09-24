use docsight_core::{DocsightError, DocumentSource};
use docsight_ingest::ingest_docx;
use docsight_render::{
    CONTACT_SHEET_MAX_PAGES, ContactSheetRequest, RenderRequest, RenderTarget,
    render_contact_sheet, render_document,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn contact_sheet_golden() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("goldens")
        .join("contact-sheet.sample_features.golden.json")
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

#[test]
fn renders_bounded_contact_sheet_deterministically() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_features.docx"))?;
    let request = ContactSheetRequest {
        pages: vec![1],
        dpi: 72,
    };
    let first = render_contact_sheet(&source, &request)?;
    let second = render_contact_sheet(&source, &request)?;
    assert_eq!(first.png(), second.png());
    assert_eq!(first.metadata.pages, vec![1]);
    assert_eq!(first.metadata.labels, vec!["p. 1"]);
    assert_eq!((first.metadata.columns, first.metadata.rows), (1, 1));
    assert_eq!(
        (first.metadata.width_px, first.metadata.height_px),
        (280, 360)
    );
    assert!(first.png().starts_with(b"\x89PNG\r\n\x1a\n"));
    let output_sha256 = Sha256::digest(first.png())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let golden: serde_json::Value =
        serde_json::from_slice(&std::fs::read(contact_sheet_golden())?)?;
    assert_eq!(golden["pages"], serde_json::json!([1]));
    assert_eq!(golden["dpi"], 72);
    assert_eq!(golden["width_px"], 280);
    assert_eq!(golden["height_px"], 360);
    assert_eq!(golden["media_type"], "image/png");
    assert_eq!(golden["output_sha256"], output_sha256);
    assert!(matches!(
        render_contact_sheet(
            &source,
            &ContactSheetRequest {
                pages: vec![2],
                dpi: 72,
            },
        ),
        Err(DocsightError::ObjectNotFound { .. })
    ));
    assert!(matches!(
        render_contact_sheet(
            &source,
            &ContactSheetRequest {
                pages: vec![1; CONTACT_SHEET_MAX_PAGES + 1],
                dpi: 72,
            },
        ),
        Err(DocsightError::ResourceLimit { .. })
    ));
    Ok(())
}
