use std::path::PathBuf;
use std::process::Command;

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
fn parses_the_project_specification_structure() -> Result<(), Box<dyn std::error::Error>> {
    let path = specification();
    let path = path.to_str().ok_or("invalid specification path")?;
    let inspect = docsight().args(["inspect", path, "--json"]).output()?;
    assert!(inspect.status.success());
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_json["result"]["tables"], 16);
    assert!(inspect_json["result"]["headings"].as_u64().unwrap_or(0) >= 20);

    let outline = docsight().args(["outline", path, "--json"]).output()?;
    assert!(outline.status.success());
    let repeated_outline = docsight().args(["outline", path, "--json"]).output()?;
    assert_eq!(outline.stdout, repeated_outline.stdout);
    let outline_json: serde_json::Value = serde_json::from_slice(&outline.stdout)?;
    assert_eq!(
        outline_json["result"]["headings"][0]["text"],
        "1. Problem statement"
    );

    let tables = docsight().args(["tables", path, "--json"]).output()?;
    assert!(tables.status.success());
    let tables_json: serde_json::Value = serde_json::from_slice(&tables.stdout)?;
    assert_eq!(
        tables_json["result"]["tables"].as_array().map(Vec::len),
        Some(16)
    );
    let first_table = tables_json["result"]["tables"][0]["id"]
        .as_str()
        .ok_or("table id missing")?;
    let table = docsight()
        .args(["table", path, first_table, "--format", "json"])
        .output()?;
    assert!(table.status.success());
    let table_json: serde_json::Value = serde_json::from_slice(&table.stdout)?;
    assert_eq!(table_json["result"]["rows"], 1);
    Ok(())
}

#[test]
fn missing_table_returns_object_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let path = specification();
    let output = docsight()
        .args([
            "--json-errors",
            "table",
            path.to_str().ok_or("invalid specification path")?,
            "tbl_missing",
            "--format",
            "json",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(21));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["code"], "OBJECT_NOT_FOUND");
    Ok(())
}
