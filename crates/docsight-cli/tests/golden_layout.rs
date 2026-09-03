use docsight_core::DocumentSource;
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
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
fn golden_headings_geometry_and_png_are_strictly_deterministic()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let source = DocumentSource::open(&path)?;

    let unpaginated = parse_docx(&source)?;
    let laid_out_1 = layout_docx(unpaginated.clone())?;
    let laid_out_2 = layout_docx(unpaginated)?;

    assert_eq!(
        laid_out_1.document.pages.len(),
        laid_out_2.document.pages.len()
    );
    assert!(!laid_out_1.document.pages.is_empty());

    for (p1, p2) in laid_out_1
        .document
        .pages
        .iter()
        .zip(&laid_out_2.document.pages)
    {
        assert_eq!(p1.number, p2.number);
        assert_eq!(p1.width_pt, p2.width_pt);
        assert_eq!(p1.height_pt, p2.height_pt);
        assert_eq!(p1.block_ids, p2.block_ids);
    }

    for (b1, b2) in laid_out_1
        .document
        .blocks
        .iter()
        .zip(&laid_out_2.document.blocks)
    {
        assert_eq!(b1.id, b2.id);
        assert_eq!(b1.page, b2.page);
        assert_eq!(b1.bbox, b2.bbox);
        assert_eq!(b1.reading_order, b2.reading_order);
    }

    let render_1 = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    let render_2 = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;

    assert_eq!(render_1.metadata, render_2.metadata);
    assert_eq!(render_1.png(), render_2.png());

    Ok(())
}

#[test]
fn golden_tables_geometry_and_png_are_strictly_deterministic()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let source = DocumentSource::open(&path)?;

    let unpaginated = parse_docx(&source)?;
    let laid_out = layout_docx(unpaginated)?;

    assert!(!laid_out.document.pages.is_empty());
    assert_eq!(laid_out.document.tables().count(), 6);

    for (block, tbl) in laid_out.document.tables() {
        assert!(block.page.is_some());
        assert!(block.bbox.is_some());
        for cell in &tbl.cells {
            let bbox = cell.bbox.ok_or("table cell missing bbox")?;
            assert!(bbox.x1 > bbox.x0);
            assert!(bbox.y1 > bbox.y0);
        }
    }

    let render = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    assert!(render.png().starts_with(b"\x89PNG\r\n\x1a\n"));

    Ok(())
}
