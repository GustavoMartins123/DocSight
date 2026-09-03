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
fn evidence_docx_human_and_json() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["evidence", doc_str, "h_515ad605791c12fc"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Evidence for h_515ad605791c12fc:"));
    assert!(stdout.contains("Kind:              Heading"));
    assert!(stdout.contains("Source Path:       /word/document.xml::body/p[1]"));
    assert!(stdout.contains("Page:              1"));
    assert!(stdout.contains("Bounding Box:"));
    assert!(stdout.contains("Fidelity Profile:"));
    assert!(stdout.contains("Text:            1.000"));
    assert!(stdout.contains("Structure:       1.000"));
    assert!(stdout.contains("Render Hash:"));
    assert!(stdout.contains("Source Fragment:"));

    let json_output = docsight()
        .args(["evidence", doc_str, "h_515ad605791c12fc", "--json"])
        .output()?;
    assert!(json_output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["object_id"], "h_515ad605791c12fc");
    assert_eq!(result["kind"], "heading");
    assert_eq!(result["page"], 1);
    assert_eq!(result["fidelity"]["text"], 1.0);
    assert_eq!(result["fidelity"]["structure"], 1.0);
    assert!(
        !result["render_fingerprint"]
            .as_str()
            .ok_or("missing")?
            .is_empty()
    );

    Ok(())
}

#[test]
fn evidence_pdf_human_and_json() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["evidence", pdf_str, "p_92d5e2c3dcf6e69d"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Evidence for p_92d5e2c3dcf6e69d:"));
    assert!(stdout.contains("Kind:              Paragraph"));
    assert!(stdout.contains("Source Path:       pdf::page[1]::p[50_49]"));
    assert!(stdout.contains("Page:              1"));
    assert!(stdout.contains("Confidence:        0.850"));
    assert!(stdout.contains("Render Hash:"));

    let json_output = docsight()
        .args(["evidence", pdf_str, "p_92d5e2c3dcf6e69d", "--json"])
        .output()?;
    assert!(json_output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["object_id"], "p_92d5e2c3dcf6e69d");
    assert!((result["confidence"].as_f64().ok_or("missing")? - 0.85).abs() < 0.01);

    Ok(())
}

#[test]
fn evidence_non_existent_object_returns_exit_code_21() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["evidence", doc_str, "missing_object_id"])
        .output()?;
    assert_eq!(output.status.code(), Some(21));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("OBJECT_NOT_FOUND"));

    Ok(())
}

#[test]
fn coverage_docx_summary_and_regions() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["coverage", doc_str, "--regions"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Coverage Report:"));
    assert!(stdout.contains("Format:              DOCX"));
    assert!(stdout.contains("Overall Fidelity:    0.939"));
    assert!(stdout.contains("Text Fidelity:       1.000 (exact)"));
    assert!(stdout.contains("Structure Fidelity:  1.000 (exact)"));
    assert!(stdout.contains("Geometry Fidelity:   0.943 (approximated)"));
    assert!(stdout.contains("Visual Fidelity:     0.812 (approximated)"));
    assert!(stdout.contains("Affected Objects:    50"));
    assert!(stdout.contains("Reason Codes:        DOCX_FONT_SUBSTITUTED, DOCX_LAYOUT_PAGINATED"));
    assert!(stdout.contains("Addressable Regions:"));
    assert!(stdout.contains("[h_515ad605791c12fc] DOCX_FONT_SUBSTITUTED"));

    Ok(())
}

#[test]
fn coverage_pdf_json_filtered_page() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let json_output = docsight()
        .args(["coverage", pdf_str, "--page", "1", "--regions", "--json"])
        .output()?;
    assert!(json_output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v1");
    let result = &val["result"];
    assert_eq!(result["format"], "pdf");
    assert_eq!(result["global"]["text"]["score"], 1.0);
    assert_eq!(result["pages"].as_array().ok_or("not array")?.len(), 1);
    let page1 = &result["pages"][0];
    assert_eq!(page1["page"], 1);
    assert!(!page1["regions"].as_array().ok_or("not array")?.is_empty());

    Ok(())
}

#[test]
fn coverage_ndjson_streaming() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["coverage", doc_str, "--ndjson"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 4);

    let first: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(first["type"], "meta");

    let second: serde_json::Value = serde_json::from_str(lines[1])?;
    assert_eq!(second["type"], "coverage.global");

    let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1])?;
    assert_eq!(last["type"], "done");

    Ok(())
}

#[test]
fn evidence_and_coverage_deterministic_json() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let out1 = docsight().args(["coverage", doc_str, "--json"]).output()?;
    let out2 = docsight().args(["coverage", doc_str, "--json"]).output()?;
    assert_eq!(out1.stdout, out2.stdout);

    let ev1 = docsight()
        .args(["evidence", doc_str, "h_515ad605791c12fc", "--json"])
        .output()?;
    let ev2 = docsight()
        .args(["evidence", doc_str, "h_515ad605791c12fc", "--json"])
        .output()?;
    assert_eq!(ev1.stdout, ev2.stdout);

    Ok(())
}
