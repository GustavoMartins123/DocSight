use std::fs;
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
fn diff_identical_documents_reports_zero_changes() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["diff", doc_str, doc_str, "--json"])
        .output()?;
    assert!(output.status.success());

    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["summary"]["semantic_changes"], 0);
    assert_eq!(
        result["package"]["added_parts"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        result["package"]["removed_parts"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        result["package"]["modified_parts"].as_array().map(Vec::len),
        Some(0)
    );

    Ok(())
}

#[test]
fn diff_docx_summary_matches_specification_layout() -> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["diff", before_str, after_str, "--summary"])
        .output()?;
    assert!(output.status.success());

    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains("Pages"));
    assert!(text.contains("3 → 2"));
    assert!(text.contains("Semantic changes"));
    assert!(text.contains("Layout changes"));
    assert!(text.contains("Images"));
    assert!(text.contains("Tables"));

    Ok(())
}

#[test]
fn diff_docx_json_envelope_and_projections() -> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["diff", before_str, after_str, "--json"])
        .output()?;
    assert!(output.status.success());

    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["format_before"], "docx");
    assert_eq!(result["format_after"], "docx");
    assert!(
        result["summary"]["semantic_changes"]
            .as_u64()
            .ok_or("missing")?
            > 0
    );
    assert!(
        result["semantic"]["tables"]["added"]
            .as_u64()
            .ok_or("missing")?
            >= 6
    );

    let projected = docsight()
        .args([
            "diff",
            before_str,
            after_str,
            "--json",
            "--select",
            "summary.pages_before,summary.pages_after",
        ])
        .output()?;
    assert!(projected.status.success());
    let proj_val: serde_json::Value = serde_json::from_slice(&projected.stdout)?;
    assert_eq!(proj_val["result"]["summary"]["pages_before"], 3);
    assert_eq!(proj_val["result"]["summary"]["pages_after"], 2);

    Ok(())
}

#[test]
fn diff_visual_writes_page_png_artifacts() -> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let temp_dir = tempfile::tempdir()?;
    let out_dir = temp_dir.path().join("diffs");

    let output = docsight()
        .args([
            "diff",
            before_str,
            after_str,
            "--visual",
            "--out-dir",
            out_dir.to_str().ok_or("invalid out dir")?,
        ])
        .output()?;
    assert!(output.status.success());

    let p1 = out_dir.join("diff_p0001.png");
    let p2 = out_dir.join("diff_p0002.png");
    assert!(p1.exists());
    assert!(p2.exists());

    let p1_bytes = fs::read(&p1)?;
    assert_eq!(&p1_bytes[..8], b"\x89PNG\r\n\x1a\n");

    Ok(())
}

#[test]
fn diff_pdf_documents() -> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_semantic.pdf");
    let after = fixture("sample_table_ruled.pdf");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["diff", before_str, after_str, "--json"])
        .output()?;
    assert!(output.status.success());

    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let result = &val["result"];
    assert_eq!(result["format_before"], "pdf");
    assert_eq!(result["format_after"], "pdf");
    assert_eq!(result["summary"]["pages_before"], 1);
    assert_eq!(result["summary"]["pages_after"], 1);
    assert!(
        result["summary"]["semantic_changes"]
            .as_u64()
            .ok_or("missing")?
            > 0
    );

    Ok(())
}

#[test]
fn diff_ndjson_streaming_events() -> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["diff", before_str, after_str, "--ndjson"])
        .output()?;
    assert!(output.status.success());

    let lines: Vec<&str> = std::str::from_utf8(&output.stdout)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(lines.len() >= 3);

    let first: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(first["type"], "meta");

    let second: serde_json::Value = serde_json::from_str(lines[1])?;
    assert_eq!(second["type"], "diff.summary");

    let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1])?;
    assert_eq!(last["type"], "done");

    Ok(())
}
