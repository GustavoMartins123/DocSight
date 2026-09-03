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
fn detects_ruled_pdf_table_and_exports_formats() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let tables_out = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tables_out.status.success());

    let val: serde_json::Value = serde_json::from_slice(&tables_out.stdout)?;
    let tables = val["result"]["tables"]
        .as_array()
        .ok_or("tables array missing")?;
    assert_eq!(tables.len(), 1);

    let tbl = &tables[0];
    assert_eq!(tbl["rows"], 3);
    assert_eq!(tbl["columns"], 3);
    assert_eq!(tbl["detector"], "ruled");
    let confidence = tbl["confidence"].as_f64().ok_or("confidence missing")?;
    assert!(confidence >= 0.95);

    let table_id = tbl["id"].as_str().ok_or("table id missing")?;

    let md_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "markdown"])
        .output()?;
    assert!(md_out.status.success());
    let md = String::from_utf8(md_out.stdout)?;
    assert!(md.contains("Category"));
    assert!(md.contains("Target"));
    assert!(md.contains("Actual"));
    assert!(md.contains("Sales"));
    assert!(md.contains("Marketing"));

    let csv_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "csv"])
        .output()?;
    assert!(csv_out.status.success());
    let csv = String::from_utf8(csv_out.stdout)?;
    assert!(csv.contains("Category,Target,Actual"));
    assert!(csv.contains("Sales,100,105"));

    let tsv_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "tsv"])
        .output()?;
    assert!(tsv_out.status.success());
    let tsv = String::from_utf8(tsv_out.stdout)?;
    assert!(tsv.contains("Category\tTarget\tActual"));

    let html_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "html"])
        .output()?;
    assert!(html_out.status.success());
    let html = String::from_utf8(html_out.stdout)?;
    assert!(html.contains("<table>"));
    assert!(html.contains("<th>Category</th>"));

    let json_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "json"])
        .output()?;
    assert!(json_out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&json_out.stdout)?;
    assert_eq!(json["result"]["rows"], 3);
    assert_eq!(json["result"]["columns"], 3);

    Ok(())
}

#[test]
fn detects_alignment_pdf_table_and_exports_csv() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_alignment.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let tables_out = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tables_out.status.success());

    let val: serde_json::Value = serde_json::from_slice(&tables_out.stdout)?;
    let tables = val["result"]["tables"]
        .as_array()
        .ok_or("tables array missing")?;
    assert_eq!(tables.len(), 1);

    let tbl = &tables[0];
    assert_eq!(tbl["rows"], 4);
    assert_eq!(tbl["columns"], 3);
    assert_eq!(tbl["detector"], "alignment");
    let confidence = tbl["confidence"].as_f64().ok_or("confidence missing")?;
    assert!(confidence >= 0.80);

    let table_id = tbl["id"].as_str().ok_or("table id missing")?;

    let csv_out = docsight()
        .args(["table", pdf_str, table_id, "--format", "csv"])
        .output()?;
    assert!(csv_out.status.success());
    let csv = String::from_utf8(csv_out.stdout)?;
    assert!(csv.contains("Account,Q1,Q2"));
    assert!(csv.contains("Revenue"));
    assert!(csv.contains("Expenses"));
    assert!(csv.contains("Net Income"));

    let human_out = docsight().args(["tables", pdf_str]).output()?;
    assert!(human_out.status.success());
    let human = String::from_utf8(human_out.stdout)?;
    assert!(human.contains("ID"));
    assert!(human.contains("Confidence"));
    assert!(human.contains("Detector"));
    assert!(human.contains("alignment"));

    Ok(())
}

#[test]
fn reconstructs_pdf_outline_and_semantic_text() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_semantic.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let outline_out = docsight().args(["outline", pdf_str, "--json"]).output()?;
    assert!(outline_out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&outline_out.stdout)?;
    let headings = val["result"]["headings"]
        .as_array()
        .ok_or("headings missing")?;
    assert_eq!(headings.len(), 3);
    assert_eq!(headings[0]["level"], 1);
    assert_eq!(headings[0]["text"], "Project Architecture Report");
    assert_eq!(headings[1]["level"], 2);
    assert_eq!(headings[1]["text"], "1. System Overview");
    assert_eq!(headings[2]["level"], 2);
    assert_eq!(headings[2]["text"], "2. Performance Metrics");

    let text_out = docsight().args(["text", pdf_str, "--json"]).output()?;
    assert!(text_out.status.success());
    let text_val: serde_json::Value = serde_json::from_slice(&text_out.stdout)?;
    let blocks = text_val["result"]["blocks"]
        .as_array()
        .ok_or("blocks missing")?;

    let has_merged_p = blocks.iter().any(|b| {
        b["kind"] == "paragraph"
            && b["text"]
                .as_str()
                .map(|t| t.contains("native architecture") && t.contains("without remote"))
                .unwrap_or(false)
    });
    assert!(has_merged_p);

    let has_tbl = blocks.iter().any(|b| b["kind"] == "table");
    assert!(has_tbl);

    let inspect_out = docsight().args(["inspect", pdf_str, "--json"]).output()?;
    assert!(inspect_out.status.success());
    let inspect_val: serde_json::Value = serde_json::from_slice(&inspect_out.stdout)?;
    assert_eq!(inspect_val["result"]["headings"], 3);
    assert_eq!(inspect_val["result"]["tables"], 1);
    assert!(
        inspect_val["result"]["paragraphs"]
            .as_u64()
            .ok_or("paragraphs missing")?
            >= 3
    );

    Ok(())
}

#[test]
fn crops_detected_pdf_table_object() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_semantic.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let tables_out = docsight().args(["tables", pdf_str, "--json"]).output()?;
    let val: serde_json::Value = serde_json::from_slice(&tables_out.stdout)?;
    let tbl_id = val["result"]["tables"][0]["id"]
        .as_str()
        .ok_or("table id missing")?;

    let dir = tempfile::tempdir()?;
    let out_png = dir.path().join("table.png");
    let crop_out = docsight()
        .args([
            "crop",
            pdf_str,
            "--object",
            tbl_id,
            "--out",
            out_png.to_str().ok_or("invalid out path")?,
        ])
        .output()?;
    assert!(crop_out.status.success());
    assert!(out_png.exists());
    let bytes = fs::read(&out_png)?;
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

    Ok(())
}

#[test]
fn pdf_reconstruction_is_byte_for_byte_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_semantic.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let out1 = docsight().args(["text", pdf_str, "--json"]).output()?;
    let out2 = docsight().args(["text", pdf_str, "--json"]).output()?;
    assert!(out1.status.success());
    assert_eq!(out1.stdout, out2.stdout);

    let tbl1 = docsight().args(["tables", pdf_str, "--json"]).output()?;
    let tbl2 = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tbl1.status.success());
    assert_eq!(tbl1.stdout, tbl2.stdout);

    Ok(())
}

#[test]
fn detects_multiple_ruled_tables_on_one_page() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_two_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;

    let tables_out = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tables_out.status.success());

    let val: serde_json::Value = serde_json::from_slice(&tables_out.stdout)?;
    let tables = val["result"]["tables"]
        .as_array()
        .ok_or("tables array missing")?;
    assert_eq!(tables.len(), 2);
    for table in tables {
        assert_eq!(table["rows"], 3);
        assert_eq!(table["columns"], 3);
        assert_eq!(table["detector"], "ruled");
        assert_eq!(table["review"], false);
    }

    let first_text = tables[0]["source"]
        .as_str()
        .ok_or("source missing")?
        .to_owned();
    let second_text = tables[1]["source"]
        .as_str()
        .ok_or("source missing")?
        .to_owned();
    assert_ne!(first_text, second_text);

    Ok(())
}

#[test]
fn review_flag_is_exposed_in_agent_json() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_path = fixture("sample_table_ruled.pdf");
    let pdf_str = pdf_path.to_str().ok_or("invalid path")?;
    let tables_out = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tables_out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&tables_out.stdout)?;
    let table = &val["result"]["tables"][0];
    assert_eq!(table["review"], false);
    Ok(())
}
