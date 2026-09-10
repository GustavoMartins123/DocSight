use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

#[test]
fn cli_renders_docx_page_and_crops_object_to_png() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = fixture("sample_headings.docx");
    let path_str = path.to_str().ok_or("invalid path")?;

    let inspect_out = docsight().args(["inspect", path_str, "--json"]).output()?;
    assert_eq!(inspect_out.status.code(), Some(0));
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect_out.stdout)?;
    assert_eq!(inspect_json["result"]["capabilities"]["render"], true);
    assert_eq!(inspect_json["result"]["source_faithful"]["render"], false);
    assert_eq!(
        inspect_json["result"]["capability_details"]["render"]["fidelity"],
        "approximated"
    );
    assert!(inspect_json["result"]["pages"].as_u64().unwrap_or(0) >= 1);

    let outline_out = docsight().args(["outline", path_str, "--json"]).output()?;
    assert_eq!(outline_out.status.code(), Some(0));
    let outline_json: serde_json::Value = serde_json::from_slice(&outline_out.stdout)?;
    let first_heading_id = outline_json["result"]["headings"][0]["id"]
        .as_str()
        .ok_or("missing heading id")?;

    let page_png = dir.path().join("page1.png");
    let render_out = docsight()
        .args([
            "render",
            path_str,
            "--page",
            "1",
            "--dpi",
            "72",
            "--out",
            page_png.to_str().ok_or("invalid png path")?,
        ])
        .output()?;
    assert_eq!(render_out.status.code(), Some(0));
    let png_bytes = std::fs::read(&page_png)?;
    assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    let crop_png = dir.path().join("crop.png");
    let crop_out = docsight()
        .args([
            "crop",
            path_str,
            "--object",
            first_heading_id,
            "--dpi",
            "72",
            "--out",
            crop_png.to_str().ok_or("invalid crop path")?,
        ])
        .output()?;
    assert_eq!(crop_out.status.code(), Some(0));
    let crop_bytes = std::fs::read(&crop_png)?;
    assert!(crop_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    Ok(())
}
