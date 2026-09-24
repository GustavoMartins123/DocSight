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

#[test]
fn cli_writes_bounded_contact_sheet_with_agent_metadata() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let path = fixture("sample_features.docx");
    let path_str = path.to_str().ok_or("invalid path")?;
    let first_path = dir.path().join("contact-first.png");
    let second_path = dir.path().join("contact-second.png");
    let first = docsight()
        .args([
            "--agent",
            "contact-sheet",
            path_str,
            "--pages",
            "1-1",
            "--out",
            first_path.to_str().ok_or("invalid first output path")?,
        ])
        .output()?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(first_json["result"]["pages"], serde_json::json!([1]));
    assert_eq!(first_json["result"]["labels"], serde_json::json!(["p. 1"]));
    assert_eq!(first_json["result"]["limits"]["max_pages"], 64);
    assert_eq!(
        first_json["result"]["limits"]["max_output_pixels"],
        16_000_000
    );
    assert_eq!(first_json["result"]["media_type"], "image/png");
    assert!(first_json["result"]["output_bytes"].as_u64().unwrap_or(0) > 0);
    let second = docsight()
        .args([
            "--agent",
            "contact-sheet",
            path_str,
            "--pages",
            "1",
            "--out",
            second_path.to_str().ok_or("invalid second output path")?,
        ])
        .output()?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(std::fs::read(&first_path)?, std::fs::read(&second_path)?);
    assert!(std::fs::read(&first_path)?.starts_with(b"\x89PNG\r\n\x1a\n"));

    let invalid_path = dir.path().join("invalid.png");
    let invalid = docsight()
        .args([
            "--agent",
            "contact-sheet",
            path_str,
            "--pages",
            "1,1",
            "--out",
            invalid_path.to_str().ok_or("invalid output path")?,
        ])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid_path.exists());

    let limited_path = dir.path().join("limited.png");
    let limited = docsight()
        .args([
            "--agent",
            "--max-bytes",
            "1",
            "contact-sheet",
            path_str,
            "--pages",
            "1",
            "--out",
            limited_path.to_str().ok_or("invalid limited output path")?,
        ])
        .output()?;
    assert!(!limited.status.success());
    assert!(limited.stdout.is_empty());
    assert!(!limited_path.exists());

    let cache_dir = dir.path().join("cache");
    std::fs::create_dir(&cache_dir)?;
    let cached = docsight()
        .args([
            "--agent",
            "--cache-dir",
            cache_dir.to_str().ok_or("invalid cache path")?,
            "contact-sheet",
            path_str,
            "--pages",
            "1",
            "--out",
            first_path.to_str().ok_or("invalid cached output path")?,
        ])
        .output()?;
    assert_eq!(cached.status.code(), Some(2));
    assert!(cached.stdout.is_empty());
    Ok(())
}
