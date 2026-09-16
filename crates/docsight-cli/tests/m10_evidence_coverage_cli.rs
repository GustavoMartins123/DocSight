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
        .args(["evidence", doc_str, "h_515ad605791c12fc496c1c18d79f6526"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Evidence for h_515ad605791c12fc496c1c18d79f6526:"));
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
        .args([
            "evidence",
            doc_str,
            "h_515ad605791c12fc496c1c18d79f6526",
            "--json",
        ])
        .output()?;
    assert!(json_output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v2");
    let result = &val["result"];
    assert_eq!(result["object_id"], "h_515ad605791c12fc496c1c18d79f6526");
    assert_eq!(result["kind"], "heading");
    assert_eq!(result["page"], 1);
    assert_eq!(result["fidelity"]["text"], 1.0);
    assert_eq!(result["fidelity"]["structure"], 1.0);
    let geometry = result["fidelity"]["geometry"].as_f64().ok_or("geometry")?;
    assert_eq!(geometry, 0.0);
    assert_eq!(result["fidelity"]["visual"], 0.0);
    assert!(
        result["fidelity"]["reasons"]
            .as_array()
            .ok_or("reasons")?
            .iter()
            .any(|reason| reason == "DOCX_FONT_SUBSTITUTED")
    );
    assert!(
        !result["render_fingerprint"]
            .as_str()
            .ok_or("missing")?
            .is_empty()
    );
    assert!(
        !result["text_fragment"]
            .as_str()
            .ok_or("missing")?
            .is_empty()
    );

    Ok(())
}

#[test]
fn evidence_pdf_carries_content_byte_anchor() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["evidence", pdf_str, "p_c7f2cbe563d57330ee6d1a3578bfe5d7"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Evidence for p_c7f2cbe563d57330ee6d1a3578bfe5d7:"));
    assert!(stdout.contains("Kind:              Paragraph"));
    assert!(stdout.contains("Source Path:       pdf::page[1]::content"));
    assert!(stdout.contains("Page:              1"));
    assert!(stdout.contains("Confidence:        0.850"));
    assert!(stdout.contains("Render Hash:"));

    let json_output = docsight()
        .args([
            "evidence",
            pdf_str,
            "p_c7f2cbe563d57330ee6d1a3578bfe5d7",
            "--json",
        ])
        .output()?;
    assert!(json_output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
    let result = &val["result"];
    assert_eq!(result["object_id"], "p_c7f2cbe563d57330ee6d1a3578bfe5d7");
    assert!((result["confidence"].as_f64().ok_or("missing")? - 0.85).abs() < 0.01);
    assert_eq!(result["source_path"], "pdf::page[1]::content");
    let offset = result["source_offset"].as_u64().ok_or("missing offset")?;
    assert!(offset > 0, "paragraph must anchor into content bytes");
    let structure = result["fidelity"]["structure"].as_f64().ok_or("st")?;
    assert!(
        (structure - 0.85).abs() < 0.01,
        "structure is the inference confidence"
    );
    assert_eq!(result["fidelity"]["visual"], 0.0);
    assert!(
        result["fidelity"]["reasons"]
            .as_array()
            .ok_or("reasons")?
            .iter()
            .any(|reason| reason == "APPROXIMATED_PDF_FONT")
    );

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
fn coverage_docx_reports_measured_penalties() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["coverage", doc_str, "--regions"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Coverage Report:"));
    assert!(stdout.contains("Format:              DOCX"));
    assert!(stdout.contains("Text Fidelity:       1.000 (exact)"));
    assert!(stdout.contains("Structure Fidelity:  1.000 (exact)"));
    assert!(stdout.contains("Geometry Fidelity:   0.000 (unsupported)"));
    assert!(stdout.contains("Visual Fidelity:     0.000 (unsupported)"));
    assert!(stdout.contains("Affected Objects:    50"));
    assert!(stdout.contains("DOCX_FONT_SUBSTITUTED"));

    Ok(())
}

#[test]
fn coverage_enumerates_unsupported_figure_region() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let doc_path = directory.path().join("jpeg_figure.docx");
    std::fs::write(&doc_path, package_with_jpeg_figure()?)?;
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["coverage", doc_str, "--regions", "--json"])
        .output()?;
    assert!(output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let result = &val["result"];

    let mut unsupported_regions = 0;
    for page in result["pages"].as_array().ok_or("pages")? {
        for region in page["regions"].as_array().ok_or("regions")? {
            if region["reason_code"] == "DOCX_FIGURE_RASTER_PLACEHOLDER" {
                unsupported_regions += 1;
                assert_eq!(region["status"], "unsupported");
                assert!(
                    region["object_id"]
                        .as_str()
                        .ok_or("id")?
                        .starts_with("fig_")
                );
            }
        }
    }
    assert_eq!(unsupported_regions, 1);
    assert!(
        result["reason_codes"]
            .as_array()
            .ok_or("codes")?
            .iter()
            .any(|code| code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
    );
    assert!(
        result["global"]["resource"]["score"]
            .as_f64()
            .ok_or("resource score")?
            < 1.0,
        "an unrasterizable figure must reduce resource coverage"
    );

    Ok(())
}

#[test]
fn coverage_counts_affected_objects_for_a_rasterizable_document()
-> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_features.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["coverage", doc_str, "--regions", "--json"])
        .output()?;
    assert!(output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let result = &val["result"];

    assert_eq!(result["affected_objects_count"], 8);
    assert!(
        !result["reason_codes"]
            .as_array()
            .ok_or("codes")?
            .iter()
            .any(|code| code == "DOCX_FIGURE_RASTER_PLACEHOLDER"),
        "a PNG figure must not be reported as an unrasterized placeholder"
    );

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
    assert_eq!(val["schema"], "docsight.agent/v2");
    let result = &val["result"];
    assert_eq!(result["format"], "pdf");
    assert_eq!(result["global"]["text"]["score"], 1.0);
    assert_eq!(result["pages"].as_array().ok_or("not array")?.len(), 1);
    let page1 = &result["pages"][0];
    assert_eq!(page1["page"], 1);
    assert!(!page1["regions"].as_array().ok_or("not array")?.is_empty());
    assert!(result["global"]["page"] == 0_u64 || result["global"]["page"].is_u64());

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
        .args([
            "evidence",
            doc_str,
            "h_515ad605791c12fc496c1c18d79f6526",
            "--json",
        ])
        .output()?;
    let ev2 = docsight()
        .args([
            "evidence",
            doc_str,
            "h_515ad605791c12fc496c1c18d79f6526",
            "--json",
        ])
        .output()?;
    assert_eq!(ev1.stdout, ev2.stdout);

    Ok(())
}

#[test]
fn schemas_stay_in_sync_with_serialized_contracts() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    let evidence_schema: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        manifest_root.join("schemas/v2/evidence-record.json"),
    )?)?;
    let schema_fields: Vec<&str> = evidence_schema["properties"]
        .as_object()
        .ok_or("evidence schema properties")?
        .keys()
        .map(String::as_str)
        .collect();
    for expected in [
        "document_digest",
        "object_id",
        "kind",
        "source_path",
        "source_offset",
        "page",
        "bbox",
        "z_index",
        "reading_order",
        "confidence",
        "fidelity",
        "render_fingerprint",
        "text_fragment",
    ] {
        assert!(
            schema_fields.contains(&expected),
            "evidence-record.json is missing field {expected}"
        );
    }
    assert!(!schema_fields.contains(&"safe_source_fragment"));

    let coverage_schema: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        manifest_root.join("schemas/v2/coverage-report.json"),
    )?)?;
    let status_enum: Vec<&str> =
        coverage_schema["$defs"]["coverage_metric"]["properties"]["status"]["enum"]
            .as_array()
            .ok_or("status enum")?
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
    assert_eq!(
        status_enum,
        vec!["exact", "inferred", "approximated", "unsupported"]
    );

    Ok(())
}

fn package_with_jpeg_figure() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::io::{Cursor, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="1270000" cy="635000"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/photo.jpg"/></Relationships>"#;
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(rels.as_bytes())?;
    writer.start_file("word/media/photo.jpg", options)?;
    writer.write_all(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46])?;
    Ok(writer.finish()?.into_inner())
}
