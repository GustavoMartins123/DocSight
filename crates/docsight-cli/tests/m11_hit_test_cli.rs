use std::path::PathBuf;
use std::process::Command;

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
fn hit_docx_point_human_and_json() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["hit", doc_str, "--page", "1", "--point", "100,80"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Hit Test on Page 1 at point (100.0, 80.0):"));
    assert!(stdout.contains("Hits: 1"));
    assert!(stdout.contains("[h_515ad605791c12fc] Heading"));
    assert!(stdout.contains("BBox:     [90.0, 72.0, 522.0, 101.0]"));

    let json_out = docsight()
        .args(["hit", doc_str, "--page", "1", "--point", "100,80", "--json"])
        .output()?;
    assert!(json_out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_out.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["query_page"], 1);
    assert_eq!(result["total_hits"], 1);
    let target = &result["targets"][0];
    assert_eq!(target["object_id"], "h_515ad605791c12fc");
    assert_eq!(target["kind"], "heading");

    Ok(())
}

#[test]
fn hit_docx_bbox_multiple_hits_sorted() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["hit", doc_str, "--page", "1", "--bbox", "90,70,520,150"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Hit Test on Page 1 in bbox [90.0, 70.0, 520.0, 150.0]:"));
    assert!(stdout.contains("Hits: 4"));

    let json_out = docsight()
        .args([
            "hit",
            doc_str,
            "--page",
            "1",
            "--bbox",
            "90,70,520,150",
            "--json",
        ])
        .output()?;
    assert!(json_out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_out.stdout)?;
    let result = &val["result"];
    assert_eq!(result["total_hits"], 4);
    let targets = result["targets"].as_array().ok_or("not array")?;
    assert_eq!(targets[0]["reading_order"], 1);
    assert_eq!(targets[1]["reading_order"], 2);

    Ok(())
}

#[test]
fn hit_pdf_point() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["hit", pdf_str, "--page", "1", "--point", "100,55"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Hit Test on Page 1 at point (100.0, 55.0):"));
    assert!(stdout.contains("Hits: 1"));
    assert!(stdout.contains("[p_c7f2cbe563d57330] Paragraph"));
    assert!(stdout.contains("Source:   pdf::page[1]::content"));

    Ok(())
}

#[test]
fn hit_point_empty_miss() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["hit", doc_str, "--page", "1", "--point", "10,10"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Hits: 0"));

    Ok(())
}

#[test]
fn hit_invalid_arguments_returns_exit_code_2() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let no_args = docsight().args(["hit", doc_str, "--page", "1"]).output()?;
    assert_eq!(no_args.status.code(), Some(2));

    let both_args = docsight()
        .args([
            "hit",
            doc_str,
            "--page",
            "1",
            "--point",
            "100,80",
            "--bbox",
            "10,10,20,20",
        ])
        .output()?;
    assert_eq!(both_args.status.code(), Some(2));

    let bad_page = docsight()
        .args(["hit", doc_str, "--page", "999", "--point", "100,80"])
        .output()?;
    assert_eq!(bad_page.status.code(), Some(2));

    Ok(())
}

#[test]
fn hit_deterministic_json_output() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let out1 = docsight()
        .args(["hit", doc_str, "--page", "1", "--point", "100,80", "--json"])
        .output()?;
    let out2 = docsight()
        .args(["hit", doc_str, "--page", "1", "--point", "100,80", "--json"])
        .output()?;
    assert_eq!(out1.stdout, out2.stdout);

    Ok(())
}
