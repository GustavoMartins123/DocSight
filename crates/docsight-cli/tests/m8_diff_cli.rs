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
    assert_eq!(val["schema"], "docsight.agent/v2");
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
    assert_eq!(val["schema"], "docsight.agent/v2");
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
            "diff", before_str, after_str, "--json", "--select", "summary",
        ])
        .output()?;
    assert!(projected.status.success());
    let proj_val: serde_json::Value = serde_json::from_slice(&projected.stdout)?;
    assert_eq!(proj_val["result"]["summary"]["pages_before"], 3);
    assert_eq!(proj_val["result"]["summary"]["pages_after"], 2);
    assert!(proj_val["result"].get("semantic").is_none());

    Ok(())
}

#[test]
fn diff_visual_fails_closed_for_approximate_docx_rendering()
-> Result<(), Box<dyn std::error::Error>> {
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
    assert_eq!(output.status.code(), Some(20));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("LAYOUT_PARTIAL"));

    let p1 = out_dir.join("diff_p0001.png");
    let p2 = out_dir.join("diff_p0002.png");
    assert!(!p1.exists());
    assert!(!p2.exists());

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

#[test]
fn diff_reports_single_insertion_without_cascade() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Cursor, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn build_docx(paragraphs: &[&str]) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let ns = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let body: String = paragraphs
            .iter()
            .map(|text| format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"))
            .collect();
        let document = format!(
            r#"<w:document xmlns:w="{ns}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
        );
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();
        writer.start_file("[Content_Types].xml", options)?;
        writer.write_all(b"<Types/>")?;
        writer.start_file("word/document.xml", options)?;
        writer.write_all(document.as_bytes())?;
        let bytes = writer.finish()?.into_inner();
        let path = std::env::temp_dir().join(format!("docsight_m8_{}.docx", paragraphs.len()));
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    let before = build_docx(&[
        "First shared paragraph about revenue",
        "Second shared paragraph about costs",
        "Third shared paragraph about margins",
    ])?;
    let after = build_docx(&[
        "First shared paragraph about revenue",
        "Inserted brand new paragraph about guidance",
        "Second shared paragraph about costs",
        "Third shared paragraph about margins",
    ])?;

    let out = docsight()
        .args([
            "diff",
            before.to_str().ok_or("path")?,
            after.to_str().ok_or("path")?,
            "--json",
        ])
        .output()?;
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let semantic = &value["result"]["semantic"];
    assert_eq!(semantic["paragraphs"]["added"], 1);
    assert_eq!(semantic["paragraphs"]["modified"], 0);
    assert_eq!(semantic["paragraphs"]["removed"], 0);
    let lineage = semantic["lineage"]
        .as_array()
        .ok_or("lineage array missing")?;
    assert_eq!(lineage.len(), 3);
    for entry in lineage {
        let id = entry["id"].as_str().ok_or("lineage id missing")?;
        assert!(id.starts_with("lin_"), "{id}");
        assert_eq!(entry["match_score"], 1.0);
        assert!(entry["before_object"].as_str().is_some());
        assert!(entry["after_object"].as_str().is_some());
    }
    std::fs::remove_file(before)?;
    std::fs::remove_file(after)?;
    Ok(())
}

#[test]
fn diff_detects_changed_embedded_image_bytes() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs::File;
    use std::io::{Read, Write};
    use zip::ZipArchive;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let before = fixture("sample_features.docx");
    let temp_dir = tempfile::tempdir()?;
    let after = temp_dir.path().join("changed-image.docx");
    let mut archive = ZipArchive::new(File::open(&before)?)?;
    let mut writer = ZipWriter::new(File::create(&after)?);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let options = SimpleFileOptions::default().compression_method(entry.compression());
        writer.start_file(entry.name(), options)?;
        if entry.name().starts_with("word/media/") {
            writer.write_all(b"different embedded image bytes")?;
        } else {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            writer.write_all(&bytes)?;
        }
    }
    writer.finish()?;

    let output = docsight()
        .args([
            "diff",
            before.to_str().ok_or("invalid before path")?,
            after.to_str().ok_or("invalid after path")?,
            "--json",
        ])
        .output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["result"]["semantic"]["images"]["modified"], 1);
    assert_eq!(value["result"]["semantic"]["images"]["added"], 0);
    assert_eq!(value["result"]["semantic"]["images"]["removed"], 0);
    Ok(())
}
