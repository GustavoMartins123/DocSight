use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

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

fn write_docx(path: &Path, paragraphs: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let ns = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let body: String = paragraphs
        .iter()
        .map(|text| format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"))
        .collect();
    let document =
        format!(r#"<w:document xmlns:w="{ns}"><w:body>{body}<w:sectPr/></w:body></w:document>"#);
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    let bytes = writer.finish()?.into_inner();
    std::fs::write(path, bytes)?;
    Ok(())
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
    assert!(text.contains("4 → 2"));
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
    assert_eq!(proj_val["result"]["summary"]["pages_before"], 4);
    assert_eq!(proj_val["result"]["summary"]["pages_after"], 2);
    assert!(proj_val["result"].get("semantic").is_none());

    Ok(())
}

#[test]
fn diff_visual_docx_is_deterministic_and_evidence_limited() -> Result<(), Box<dyn std::error::Error>>
{
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before_str = before.to_str().ok_or("invalid path")?;
    let after_str = after.to_str().ok_or("invalid path")?;

    let temp_dir = tempfile::tempdir()?;
    let first_dir = temp_dir.path().join("first");
    let second_dir = temp_dir.path().join("second");

    let first = docsight()
        .args([
            "diff",
            before_str,
            after_str,
            "--visual",
            "--dpi",
            "36",
            "--out-dir",
            first_dir.to_str().ok_or("invalid out dir")?,
            "--json",
        ])
        .output()?;
    let second = docsight()
        .args([
            "diff",
            before_str,
            after_str,
            "--visual",
            "--dpi",
            "36",
            "--out-dir",
            second_dir.to_str().ok_or("invalid out dir")?,
            "--json",
        ])
        .output()?;
    assert!(first.status.success());
    assert!(second.status.success());
    assert!(first.stderr.is_empty());
    assert!(second.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);

    let value: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    let visual = &value["result"]["visual"];
    assert_eq!(visual["authoritative"], false);
    assert_eq!(visual["evidence_status"], "evidence_limited");
    assert_eq!(visual["dpi"], 36);
    assert_eq!(visual["threshold"], 8);
    assert!(
        visual["layout_regression_score"]
            .as_f64()
            .is_some_and(|score| score > 0.0 && score <= 1.0)
    );
    assert!(
        visual["reason_codes"]
            .as_array()
            .ok_or("reason codes")?
            .iter()
            .any(|code| code == "DOCX_FONT_SUBSTITUTED")
    );
    let pages = visual["page_diffs"].as_array().ok_or("page diffs")?;
    assert_eq!(pages.len(), 4);
    assert!(pages.iter().all(|page| page["authoritative"] == false));
    let last = pages.last().ok_or("last page diff")?;
    assert_eq!(last["change_fraction"], 1.0);
    assert_eq!(last["changed_pixels"], last["total_pixels"]);
    assert!(
        value["warnings"]
            .as_array()
            .ok_or("warnings")?
            .iter()
            .any(|warning| warning["code"] == "DIFF_VISUAL_EVIDENCE_LIMITED")
    );

    for page in 1..=3 {
        let name = format!("diff_p{page:04}.png");
        let first_bytes = std::fs::read(first_dir.join(&name))?;
        let second_bytes = std::fs::read(second_dir.join(&name))?;
        assert!(!first_bytes.is_empty());
        assert_eq!(first_bytes, second_bytes);
        let artifact = &pages[page - 1]["artifact"];
        assert_eq!(artifact["relative_path"], name);
        assert_eq!(artifact["media_type"], "image/png");
        assert_eq!(artifact["bytes"], first_bytes.len());
        assert!(artifact["width_px"].as_u64().is_some_and(|value| value > 0));
        assert!(
            artifact["height_px"]
                .as_u64()
                .is_some_and(|value| value > 0)
        );
        let digest = Sha256::digest(&first_bytes);
        let expected_sha256 = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(artifact["sha256"], expected_sha256);
    }

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
    assert!(second["before_document"]["sha256"].as_str().is_some());
    assert!(second["after_document"]["sha256"].as_str().is_some());

    assert!(lines.iter().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|value| value["type"] == "diff.lineage")
            .unwrap_or(false)
    }));

    let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1])?;
    assert_eq!(last["type"], "done");

    Ok(())
}

#[test]
fn diff_visual_ndjson_streams_complete_verifiable_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let directory = tempfile::tempdir()?;
    let output = docsight()
        .args([
            "--agent",
            "--ndjson",
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
            "--visual",
            "--dpi",
            "36",
            "--threshold",
            "4",
            "--out-dir",
            directory.path().to_str().ok_or("output path")?,
        ])
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    assert_eq!(records[0]["type"], "meta");
    assert_eq!(records[1]["type"], "diff.summary");
    assert_eq!(records[2]["type"], "diff.visual");
    assert_eq!(records[2]["dpi"], 36);
    assert_eq!(records[2]["threshold"], 4);
    assert_eq!(records[2]["page_diff_count"], 4);
    assert_eq!(records[2]["authoritative"], false);

    let visual_pages = records
        .iter()
        .filter(|record| record["type"] == "diff.visual.page")
        .collect::<Vec<_>>();
    assert_eq!(visual_pages.len(), 4);
    for (index, record) in visual_pages.iter().enumerate() {
        let page = index + 1;
        let relative_path = format!("diff_p{page:04}.png");
        let bytes = std::fs::read(directory.path().join(&relative_path))?;
        assert_eq!(record["page"], page);
        assert_eq!(record["artifact"]["relative_path"], relative_path);
        assert_eq!(record["artifact"]["bytes"], bytes.len());
        assert_eq!(
            record["artifact"]["sha256"].as_str().map(str::len),
            Some(64)
        );
    }
    assert_eq!(records.last().ok_or("done record")?["type"], "done");
    Ok(())
}

#[test]
fn diff_continuation_is_bound_to_both_documents_and_visual_profile()
-> Result<(), Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let alternate_before = fixture("sample_features.docx");
    let after = fixture("sample_tables.docx");
    let first = docsight()
        .args([
            "--agent",
            "--ndjson",
            "--max-items",
            "2",
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
        ])
        .output()?;
    assert!(first.status.success());
    let records = first
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    let token = records
        .last()
        .and_then(|record| record["limits"]["continuation_token"].as_str())
        .ok_or("continuation token")?;

    let wrong_before = docsight()
        .args([
            "--agent",
            "--ndjson",
            "diff",
            alternate_before.to_str().ok_or("alternate before path")?,
            after.to_str().ok_or("after path")?,
            "--continue",
            token,
        ])
        .output()?;
    assert_eq!(wrong_before.status.code(), Some(2));
    assert!(wrong_before.stdout.is_empty());

    let wrong_profile = docsight()
        .args([
            "--agent",
            "--ndjson",
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
            "--visual",
            "--continue",
            token,
        ])
        .output()?;
    assert_eq!(wrong_profile.status.code(), Some(2));
    assert!(wrong_profile.stdout.is_empty());
    Ok(())
}

#[test]
fn diff_reports_single_insertion_without_cascade() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let before = temp_dir.path().join("before.docx");
    let after = temp_dir.path().join("after.docx");
    write_docx(
        &before,
        &[
            "First shared paragraph about revenue",
            "Second shared paragraph about costs",
            "Third shared paragraph about margins",
        ],
    )?;
    write_docx(
        &after,
        &[
            "First shared paragraph about revenue",
            "Inserted brand new paragraph about guidance",
            "Second shared paragraph about costs",
            "Third shared paragraph about margins",
        ],
    )?;

    let out = docsight()
        .args([
            "diff",
            before.to_str().ok_or("path")?,
            after.to_str().ok_or("path")?,
            "--json",
        ])
        .output()?;
    assert!(out.status.success());
    let repeated = docsight()
        .args([
            "diff",
            before.to_str().ok_or("path")?,
            after.to_str().ok_or("path")?,
            "--json",
        ])
        .output()?;
    assert!(repeated.status.success());
    assert_eq!(out.stdout, repeated.stdout);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let semantic = &value["result"]["semantic"];
    assert_eq!(semantic["paragraphs"]["added"], 1);
    assert_eq!(semantic["paragraphs"]["modified"], 0);
    assert_eq!(semantic["paragraphs"]["removed"], 0);
    let result = &value["result"];
    assert_ne!(
        result["before_document"]["sha256"],
        result["after_document"]["sha256"]
    );
    let lineage = semantic["lineage"]
        .as_array()
        .ok_or("lineage array missing")?;
    assert_eq!(lineage.len(), 3);
    for entry in lineage {
        let id = entry["id"].as_str().ok_or("lineage id missing")?;
        assert!(id.starts_with("lin_"), "{id}");
        assert_eq!(entry["status"], "matched");
        assert_eq!(entry["target_type"], "paragraph");
        assert!(entry["match_score"].as_f64().ok_or("lineage score")? >= 0.5);
        assert!(entry["before_object"].as_str().is_some());
        assert!(entry["after_object"].as_str().is_some());
        let evidence = entry["evidence"].as_array().ok_or("lineage evidence")?;
        assert!(
            evidence
                .iter()
                .any(|item| item["kind"] == "normalized_text")
        );
        assert!(evidence.iter().any(|item| item["kind"] == "source_path"));
    }
    assert_eq!(semantic["lineage_ambiguous"], 0);
    assert!(
        semantic["evidence_limited_changes"]
            .as_u64()
            .ok_or("limited")?
            >= 1
    );
    Ok(())
}

fn build_pdf(content: &str, resources: &str, extra_objects: &[&str]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Resources {resources} /Contents 5 0 R >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    objects.extend(extra_objects.iter().map(|object| (*object).to_owned()));
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn self_diff_summary(path: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let document = path.to_str().ok_or("document path")?;
    let output = docsight()
        .args(["--agent", "diff", document, document])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(value["result"]["summary"].clone())
}

#[test]
fn diff_of_a_document_with_repeated_content_against_itself_is_empty()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let repeated_text = temp_dir.path().join("repeated_text.pdf");
    let lines: String = [360, 240, 120]
        .iter()
        .map(|y| format!("BT /F1 12 Tf 20 {y} Td (Repeated notice) Tj ET "))
        .collect();
    std::fs::write(
        &repeated_text,
        build_pdf(&lines, "<< /Font << /F1 4 0 R >> >>", &[]),
    )?;
    let repeated_image = temp_dir.path().join("repeated_image.pdf");
    let draws: String = [20, 120, 220]
        .iter()
        .map(|x| format!("q 40 0 0 40 {x} 300 cm /Im1 Do Q "))
        .collect();
    std::fs::write(
        &repeated_image,
        build_pdf(
            &format!("{draws}BT /F1 12 Tf 20 100 Td (Caption text) Tj ET"),
            "<< /Font << /F1 4 0 R >> /XObject << /Im1 6 0 R >> >>",
            &[
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n\u{10}\u{20}\u{30}\nendstream",
            ],
        ),
    )?;
    let repeated_docx = temp_dir.path().join("repeated.docx");
    write_docx(
        &repeated_docx,
        &["Repeated heading", "Body", "Repeated heading", "Body"],
    )?;

    for path in [&repeated_text, &repeated_image, &repeated_docx] {
        let summary = self_diff_summary(path)?;
        assert_eq!(summary["semantic_changes"], 0, "{}", path.display());
        assert_eq!(summary["lineage_ambiguous"], 0, "{}", path.display());
    }
    Ok(())
}

#[test]
fn an_insertion_before_repeated_content_is_one_addition() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let before = temp_dir.path().join("before.docx");
    let after = temp_dir.path().join("after.docx");
    write_docx(&before, &["Repeated paragraph", "Repeated paragraph"])?;
    write_docx(
        &after,
        &[
            "Inserted leading paragraph",
            "Repeated paragraph",
            "Repeated paragraph",
        ],
    )?;
    let output = docsight()
        .args([
            "--agent",
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
        ])
        .output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let paragraphs = &value["result"]["semantic"]["paragraphs"];
    assert_eq!(paragraphs["added"], 1);
    assert_eq!(paragraphs["removed"], 0);
    assert_eq!(value["result"]["semantic"]["lineage_ambiguous"], 0);
    Ok(())
}

#[test]
fn diff_reports_ambiguous_lineage_for_real_duplicate_docx_content()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let before = temp_dir.path().join("before.docx");
    let after = temp_dir.path().join("after.docx");
    write_docx(&before, &["Repeated paragraph", "Repeated paragraph"])?;
    write_docx(
        &after,
        &[
            "Inserted leading paragraph",
            "Repeated paragraph",
            "Repeated paragraph",
            "Repeated paragraph",
        ],
    )?;

    let output = docsight()
        .args([
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
            "--json",
        ])
        .output()?;
    assert!(output.status.success());

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let semantic = &value["result"]["semantic"];
    assert!(
        semantic["lineage_ambiguous"]
            .as_u64()
            .ok_or("ambiguity count")?
            >= 1
    );
    let ambiguous = semantic["lineage"]
        .as_array()
        .ok_or("lineage array")?
        .iter()
        .find(|record| record["status"] == "ambiguous")
        .ok_or("ambiguous lineage record")?;
    assert_eq!(ambiguous["target_type"], "paragraph");
    assert!(
        ambiguous["candidates"]
            .as_array()
            .ok_or("candidates")?
            .len()
            >= 2
    );
    assert!(
        value["result"]["warnings"]
            .as_array()
            .ok_or("warnings")?
            .iter()
            .any(|warning| warning["code"] == "DIFF_LINEAGE_AMBIGUOUS")
    );
    assert!(
        semantic["records"]
            .as_array()
            .ok_or("records")?
            .iter()
            .any(|record| {
                record["lineage"]["status"] == "ambiguous" && record["authoritative"] == false
            })
    );
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

#[test]
fn diff_schema_declares_cross_version_lineage_contract() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let schema: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("schemas/v2/diff-result.json"),
    )?)?;
    let required = schema["required"].as_array().ok_or("diff required")?;
    for field in ["before_document", "after_document", "semantic"] {
        assert!(required.iter().any(|value| value == field));
    }
    let semantic_required = schema["properties"]["semantic"]["required"]
        .as_array()
        .ok_or("semantic required")?;
    for field in ["lineage", "lineage_ambiguous", "evidence_limited_changes"] {
        assert!(semantic_required.iter().any(|value| value == field));
    }
    let lineage_status: Vec<&str> =
        schema["$defs"]["lineage_record"]["properties"]["status"]["enum"]
            .as_array()
            .ok_or("lineage status")?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
    assert_eq!(lineage_status, vec!["matched", "ambiguous"]);
    assert_eq!(
        schema["properties"]["visual"]["$ref"],
        "#/$defs/visual_diff"
    );
    let visual_required = schema["$defs"]["visual_diff"]["required"]
        .as_array()
        .ok_or("visual diff required")?;
    for field in [
        "authoritative",
        "evidence_status",
        "reason_codes",
        "dpi",
        "threshold",
        "layout_regression_score",
        "page_diffs",
    ] {
        assert!(visual_required.iter().any(|value| value == field));
    }
    let evidence_status: Vec<&str> =
        schema["$defs"]["visual_diff"]["properties"]["evidence_status"]["enum"]
            .as_array()
            .ok_or("visual evidence status")?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
    assert_eq!(evidence_status, vec!["exact", "evidence_limited"]);

    let ndjson: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("schemas/v2/ndjson-event.json"),
    )?)?;
    let ndjson_events = ndjson["properties"]["type"]["enum"]
        .as_array()
        .ok_or("NDJSON event types")?;
    for event in ["diff.visual", "diff.visual.page", "diff.lineage"] {
        assert!(ndjson_events.iter().any(|value| value == event));
    }
    Ok(())
}
