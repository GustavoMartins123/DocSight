use std::fs;
use std::process::Command;

#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

#[test]
fn exposes_pdf_page_spans_as_deterministic_json() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sample.pdf");
    fs::write(&path, pdf_fixture::sample_pdf())?;
    let first = docsight()
        .args(["page", path.to_str().ok_or("invalid path")?, "1", "--json"])
        .output()?;
    let second = docsight()
        .args(["page", path.to_str().ok_or("invalid path")?, "1", "--json"])
        .output()?;
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let value: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(value["result"]["number"], 1);
    assert_eq!(value["result"]["width_pt"], 200.0);
    assert_eq!(value["result"]["spans"][0]["text"], "Hello DOCSIGHT");
    assert_eq!(value["warnings"][0]["code"], "APPROXIMATED_PDF_FONT");
    Ok(())
}

#[test]
fn renders_page_bbox_and_object_crops() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sample.pdf");
    let full = directory.path().join("full.png");
    let region = directory.path().join("region.png");
    let object = directory.path().join("object.png");
    fs::write(&path, pdf_fixture::sample_pdf())?;
    let render = docsight()
        .args([
            "render",
            path.to_str().ok_or("invalid path")?,
            "--page",
            "1",
            "--dpi",
            "144",
            "--out",
            full.to_str().ok_or("invalid output path")?,
        ])
        .output()?;
    assert!(render.status.success());
    assert!(String::from_utf8(render.stderr)?.contains("APPROXIMATED_PDF_FONT"));
    assert_png_dimensions(&fs::read(&full)?, 400, 200)?;
    let crop = docsight()
        .args([
            "crop",
            path.to_str().ok_or("invalid path")?,
            "--page",
            "1",
            "--bbox",
            "10,10,110,60",
            "--dpi",
            "144",
            "--out",
            region.to_str().ok_or("invalid output path")?,
        ])
        .output()?;
    assert!(crop.status.success());
    assert_png_dimensions(&fs::read(&region)?, 200, 100)?;
    let page = docsight()
        .args(["page", path.to_str().ok_or("invalid path")?, "1", "--json"])
        .output()?;
    let value: serde_json::Value = serde_json::from_slice(&page.stdout)?;
    let span = value["result"]["spans"][0]["id"]
        .as_str()
        .ok_or("span id missing")?;
    let crop = docsight()
        .args([
            "crop",
            path.to_str().ok_or("invalid path")?,
            "--object",
            span,
            "--dpi",
            "144",
            "--out",
            object.to_str().ok_or("invalid output path")?,
        ])
        .output()?;
    assert!(crop.status.success());
    assert!(fs::metadata(object)?.len() > 50);
    Ok(())
}

#[test]
fn invalid_crop_contract_returns_usage_error() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sample.pdf");
    let output = directory.path().join("invalid.png");
    fs::write(&path, pdf_fixture::sample_pdf())?;
    let result = docsight()
        .args([
            "--json-errors",
            "crop",
            path.to_str().ok_or("invalid path")?,
            "--page",
            "1",
            "--out",
            output.to_str().ok_or("invalid output path")?,
        ])
        .output()?;
    assert_eq!(result.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&result.stderr)?;
    assert_eq!(error["code"], "USAGE");
    assert!(!output.exists());
    Ok(())
}

fn assert_png_dimensions(
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("invalid PNG signature".into());
    }
    let width_bytes: [u8; 4] = bytes.get(16..20).ok_or("PNG width missing")?.try_into()?;
    let height_bytes: [u8; 4] = bytes.get(20..24).ok_or("PNG height missing")?.try_into()?;
    assert_eq!(u32::from_be_bytes(width_bytes), width);
    assert_eq!(u32::from_be_bytes(height_bytes), height);
    Ok(())
}
