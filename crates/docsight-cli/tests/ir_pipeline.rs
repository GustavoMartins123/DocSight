use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn specification() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Projeto_DOCSIGHT_Especificacao.docx")
}

#[test]
fn text_command_works_on_both_docx_and_pdf() -> Result<(), Box<dyn std::error::Error>> {
    let spec_path = specification();
    let spec_str = spec_path.to_str().ok_or("invalid specification path")?;
    let docx_text = docsight().args(["text", spec_str, "--json"]).output()?;
    assert!(docx_text.status.success());
    let docx_json: serde_json::Value = serde_json::from_slice(&docx_text.stdout)?;
    let docx_blocks = docx_json["result"]["blocks"]
        .as_array()
        .ok_or("blocks array missing")?;
    assert!(docx_blocks.len() > 100);

    let directory = tempfile::tempdir()?;
    let pdf_path = directory.path().join("sample.pdf");
    fs::write(&pdf_path, pdf_fixture::sample_pdf())?;
    let pdf_str = pdf_path.to_str().ok_or("invalid pdf path")?;

    let pdf_text = docsight().args(["text", pdf_str, "--json"]).output()?;
    assert!(pdf_text.status.success());
    let pdf_json: serde_json::Value = serde_json::from_slice(&pdf_text.stdout)?;
    let pdf_blocks = pdf_json["result"]["blocks"]
        .as_array()
        .ok_or("blocks array missing")?;
    assert_eq!(pdf_blocks.len(), 1);
    assert_eq!(pdf_blocks[0]["text"], "Hello DOCSIGHT");
    assert_eq!(pdf_blocks[0]["kind"], "paragraph");
    Ok(())
}

#[test]
fn outline_and_tables_commands_work_on_pdf() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let pdf_path = directory.path().join("sample.pdf");
    fs::write(&pdf_path, pdf_fixture::sample_pdf())?;
    let pdf_str = pdf_path.to_str().ok_or("invalid pdf path")?;

    let outline = docsight().args(["outline", pdf_str, "--json"]).output()?;
    assert!(outline.status.success());
    let outline_json: serde_json::Value = serde_json::from_slice(&outline.stdout)?;
    assert_eq!(
        outline_json["result"]["headings"].as_array().map(Vec::len),
        Some(0)
    );

    let tables = docsight().args(["tables", pdf_str, "--json"]).output()?;
    assert!(tables.status.success());
    let tables_json: serde_json::Value = serde_json::from_slice(&tables.stdout)?;
    assert_eq!(
        tables_json["result"]["tables"].as_array().map(Vec::len),
        Some(0)
    );
    Ok(())
}

#[test]
fn table_formats_csv_tsv_html_markdown() -> Result<(), Box<dyn std::error::Error>> {
    let spec_path = specification();
    let spec_str = spec_path.to_str().ok_or("invalid specification path")?;
    let tables = docsight().args(["tables", spec_str, "--json"]).output()?;
    let tables_json: serde_json::Value = serde_json::from_slice(&tables.stdout)?;
    let table_entry = tables_json["result"]["tables"]
        .as_array()
        .ok_or("tables array missing")?
        .iter()
        .find(|tbl| tbl["columns"].as_u64().unwrap_or(0) > 1)
        .ok_or("multi-column table missing")?;
    let first_table_id = table_entry["id"].as_str().ok_or("table id missing")?;

    let markdown = docsight()
        .args(["table", spec_str, first_table_id, "--format", "markdown"])
        .output()?;
    assert!(markdown.status.success());
    let md_str = String::from_utf8(markdown.stdout)?;
    assert!(md_str.contains('|'));

    let csv = docsight()
        .args(["table", spec_str, first_table_id, "--format", "csv"])
        .output()?;
    assert!(csv.status.success());
    let csv_str = String::from_utf8(csv.stdout)?;
    assert!(csv_str.contains(','));

    let tsv = docsight()
        .args(["table", spec_str, first_table_id, "--format", "tsv"])
        .output()?;
    assert!(tsv.status.success());
    let tsv_str = String::from_utf8(tsv.stdout)?;
    assert!(tsv_str.contains('\t'));

    let html = docsight()
        .args(["table", spec_str, first_table_id, "--format", "html"])
        .output()?;
    assert!(html.status.success());
    let html_str = String::from_utf8(html.stdout)?;
    assert!(html_str.contains("<table>"));
    assert!(html_str.contains("</table>"));
    Ok(())
}

#[test]
fn page_command_on_unpaginated_docx_returns_unsupported_feature()
-> Result<(), Box<dyn std::error::Error>> {
    let spec_path = specification();
    let spec_str = spec_path.to_str().ok_or("invalid specification path")?;
    let output = docsight()
        .args(["--json-errors", "page", spec_str, "1"])
        .output()?;
    assert_eq!(output.status.code(), Some(20));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["code"], "UNSUPPORTED_FEATURE");
    Ok(())
}
