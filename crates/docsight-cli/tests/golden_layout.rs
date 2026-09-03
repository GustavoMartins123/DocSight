use docsight_core::DocumentSource;
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_render::{RenderRequest, RenderTarget, render_document};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GoldenGeometry {
    page_count: usize,
    pages: Vec<GoldenPage>,
    blocks: Vec<GoldenBlock>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GoldenPage {
    number: u32,
    width_pt: f32,
    height_pt: f32,
    block_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GoldenBlock {
    id: String,
    page: u32,
    reading_order: u32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GoldenFile {
    geometry: GoldenGeometry,
    render_page1_sha256: String,
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("goldens")
        .join(format!("{name}.golden.json"))
}

fn build_golden(fixture_name: &str) -> Result<GoldenFile, Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture(fixture_name))?;
    let parsed = parse_docx(&source)?;
    let laid_out = layout_docx(parsed)?;

    let mut blocks = Vec::new();
    for block in &laid_out.document.blocks {
        let page = block.page.unwrap_or(0);
        let bbox = block
            .bbox
            .unwrap_or(docsight_core::Rect::new(0.0, 0.0, 1.0, 1.0)?);
        blocks.push(GoldenBlock {
            id: block.id.to_string(),
            page,
            reading_order: block.reading_order,
            x0: (bbox.x0 * 100.0).round() / 100.0,
            y0: (bbox.y0 * 100.0).round() / 100.0,
            x1: (bbox.x1 * 100.0).round() / 100.0,
            y1: (bbox.y1 * 100.0).round() / 100.0,
        });
    }
    let pages: Vec<GoldenPage> = laid_out
        .document
        .pages
        .iter()
        .map(|page| GoldenPage {
            number: page.number,
            width_pt: page.width_pt,
            height_pt: page.height_pt,
            block_count: page.block_ids.len(),
        })
        .collect();

    let render = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;

    Ok(GoldenFile {
        geometry: GoldenGeometry {
            page_count: pages.len(),
            pages,
            blocks,
        },
        render_page1_sha256: sha256_hex(render.png()),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn check_golden(fixture_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let current = build_golden(fixture_name)?;
    let path = golden_path(fixture_name);
    if std::env::var("DOCSIGHT_UPDATE_GOLDENS").is_ok_and(|value| value == "1") {
        std::fs::create_dir_all(path.parent().ok_or("golden dir missing")?)?;
        std::fs::write(&path, serde_json::to_string_pretty(&current)?)?;
        return Ok(());
    }
    let stored = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "golden snapshot is missing for {fixture_name}; run DOCSIGHT_UPDATE_GOLDENS=1 cargo test to create it: {error}"
        )
    })?;
    let expected: GoldenFile = serde_json::from_str(&stored)?;
    if expected != current {
        let mut differences: BTreeMap<String, (String, String)> = BTreeMap::new();
        if expected.geometry.page_count != current.geometry.page_count {
            differences.insert(
                "page_count".to_owned(),
                (
                    expected.geometry.page_count.to_string(),
                    current.geometry.page_count.to_string(),
                ),
            );
        }
        for (index, (stored_block, current_block)) in expected
            .geometry
            .blocks
            .iter()
            .zip(current.geometry.blocks.iter())
            .enumerate()
        {
            if stored_block != current_block {
                differences.insert(
                    format!("block[{index}]"),
                    (format!("{stored_block:?}"), format!("{current_block:?}")),
                );
            }
        }
        if expected.render_page1_sha256 != current.render_page1_sha256 {
            differences.insert(
                "render_page1_sha256".to_owned(),
                (expected.render_page1_sha256, current.render_page1_sha256),
            );
        }
        return Err(format!("golden mismatch for {fixture_name}: {differences:#?}").into());
    }
    Ok(())
}

#[test]
fn golden_headings_geometry_and_png_match_stored_snapshot() -> Result<(), Box<dyn std::error::Error>>
{
    check_golden("sample_headings.docx")
}

#[test]
fn golden_tables_geometry_and_png_match_stored_snapshot() -> Result<(), Box<dyn std::error::Error>>
{
    check_golden("sample_tables.docx")
}

#[test]
fn golden_features_geometry_and_png_match_stored_snapshot() -> Result<(), Box<dyn std::error::Error>>
{
    check_golden("sample_features.docx")
}

#[test]
fn golden_layout_is_strictly_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "sample_headings.docx",
        "sample_tables.docx",
        "sample_features.docx",
    ] {
        let first = build_golden(name)?;
        let second = build_golden(name)?;
        assert_eq!(first, second, "{name} must be deterministic");
    }
    Ok(())
}
