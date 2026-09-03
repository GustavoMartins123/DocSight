use std::path::PathBuf;
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn validation_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
}

#[test]
fn validates_headings_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let file = validation_dir().join("sample_headings.docx");
    let file_str = file.to_str().ok_or("invalid path")?;

    let inspect = docsight().args(["inspect", file_str, "--json"]).output()?;
    assert!(inspect.status.success());
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_json["result"]["format"], "docx");
    assert_eq!(inspect_json["result"]["headings"], 12);
    assert_eq!(inspect_json["result"]["paragraphs"], 38);
    assert_eq!(inspect_json["result"]["tables"], 0);

    let outline = docsight().args(["outline", file_str, "--json"]).output()?;
    assert!(outline.status.success());
    let outline_json: serde_json::Value = serde_json::from_slice(&outline.stdout)?;
    let headings = outline_json["result"]["headings"]
        .as_array()
        .ok_or("headings array missing")?;
    assert_eq!(headings.len(), 12);
    assert_eq!(headings[0]["text"], "Application Architecture Guide");

    let text = docsight().args(["text", file_str, "--json"]).output()?;
    assert!(text.status.success());
    let text_json: serde_json::Value = serde_json::from_slice(&text.stdout)?;
    let blocks = text_json["result"]["blocks"]
        .as_array()
        .ok_or("blocks missing")?;
    assert_eq!(blocks.len(), 50);
    Ok(())
}

#[test]
fn validates_tables_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let file = validation_dir().join("sample_tables.docx");
    let file_str = file.to_str().ok_or("invalid path")?;

    let inspect = docsight().args(["inspect", file_str, "--json"]).output()?;
    assert!(inspect.status.success());
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_json["result"]["format"], "docx");
    assert_eq!(inspect_json["result"]["tables"], 6);

    let tables = docsight().args(["tables", file_str, "--json"]).output()?;
    assert!(tables.status.success());
    let tables_json: serde_json::Value = serde_json::from_slice(&tables.stdout)?;
    let table_list = tables_json["result"]["tables"]
        .as_array()
        .ok_or("tables array missing")?;
    assert_eq!(table_list.len(), 6);

    let first_table_id = table_list[0]["id"].as_str().ok_or("table id missing")?;
    let table_md = docsight()
        .args(["table", file_str, first_table_id, "--format", "markdown"])
        .output()?;
    assert!(table_md.status.success());
    let md_content = String::from_utf8(table_md.stdout)?;
    assert!(md_content.contains('|'));
    Ok(())
}
