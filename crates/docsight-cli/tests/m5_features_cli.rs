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
fn cli_inspects_and_processes_m5_features() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = fixture("sample_features.docx");
    let path_str = path.to_str().ok_or("invalid path")?;

    let inspect_out = docsight().args(["inspect", path_str, "--json"]).output()?;
    assert_eq!(inspect_out.status.code(), Some(0));
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect_out.stdout)?;
    assert_eq!(inspect_json["result"]["figures"], 1);
    assert_eq!(inspect_json["result"]["comments"], 1);
    assert_eq!(inspect_json["result"]["tracked"]["insertions"], 1);
    assert_eq!(inspect_json["result"]["tracked"]["deletions"], 1);

    let images_out = docsight().args(["images", path_str, "--json"]).output()?;
    assert_eq!(images_out.status.code(), Some(0));
    let images_json: serde_json::Value = serde_json::from_slice(&images_out.stdout)?;
    let images = images_json["result"]["images"]
        .as_array()
        .ok_or("missing images")?;
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["alt_text"], "System Architecture Diagram");
    assert_eq!(images[0]["width_pt"], 200.0);
    assert_eq!(images[0]["height_pt"], 100.0);
    assert_eq!(images[0]["page"], 1);
    let fig_id = images[0]["id"].as_str().ok_or("missing figure id")?;

    let links_out = docsight().args(["links", path_str, "--json"]).output()?;
    assert_eq!(links_out.status.code(), Some(0));
    let links_json: serde_json::Value = serde_json::from_slice(&links_out.stdout)?;
    let links = links_json["result"]["links"]
        .as_array()
        .ok_or("missing links")?;
    assert_eq!(links.len(), 2);
    assert_eq!(links[0]["text"], "Project Homepage");
    assert_eq!(links[0]["target"], "https://docsight.dev");
    assert_eq!(links[0]["is_external"], true);
    assert_eq!(links[0]["page"], 1);

    assert_eq!(links[1]["text"], "Go to Appendix");
    assert_eq!(links[1]["target"], "#appendix");
    assert_eq!(links[1]["is_external"], false);
    assert_eq!(links[1]["page"], 1);

    let page_out = docsight()
        .args(["page", path_str, "1", "--json"])
        .output()?;
    assert_eq!(page_out.status.code(), Some(0));
    let page_json: serde_json::Value = serde_json::from_slice(&page_out.stdout)?;
    let overlays = page_json["result"]["overlays"]
        .as_array()
        .ok_or("missing overlays")?;
    assert_eq!(overlays.len(), 2);
    assert_eq!(overlays[0]["kind"], "header");
    assert_eq!(overlays[0]["text"], "STANDARD TECHNICAL REPORT");
    assert_eq!(overlays[1]["kind"], "footer");
    assert_eq!(overlays[1]["text"], "CONFIDENTIAL  •  PAGE 1");

    let page_png = dir.path().join("page1.png");
    let render_out = docsight()
        .args([
            "render",
            path_str,
            "--page",
            "1",
            "--out",
            page_png.to_str().ok_or("invalid out")?,
        ])
        .output()?;
    assert_eq!(render_out.status.code(), Some(0));
    assert!(page_png.exists());

    let crop_png = dir.path().join("fig.png");
    let crop_out = docsight()
        .args([
            "crop",
            path_str,
            "--object",
            fig_id,
            "--out",
            crop_png.to_str().ok_or("invalid out")?,
        ])
        .output()?;
    assert_eq!(crop_out.status.code(), Some(0));
    assert!(crop_png.exists());

    Ok(())
}
