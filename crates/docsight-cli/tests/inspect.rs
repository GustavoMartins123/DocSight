use std::fs;
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

#[test]
fn inspect_pdf_json_is_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sample.bin");
    fs::write(&path, b"%PDF-1.7\n")?;
    let first = docsight()
        .args(["inspect", path.to_str().ok_or("invalid path")?, "--json"])
        .output()?;
    let second = docsight()
        .args(["inspect", path.to_str().ok_or("invalid path")?, "--json"])
        .output()?;
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let value: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(value["schema"], "docsight.agent/v1");
    assert_eq!(value["result"]["format"], "pdf");
    assert_eq!(value["result"]["capabilities"]["render"], true);
    Ok(())
}

#[test]
fn unsupported_input_uses_typed_exit_code_and_json_error() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("fake.docx");
    fs::write(&path, b"not a document")?;
    let output = docsight()
        .args([
            "--json-errors",
            "inspect",
            path.to_str().ok_or("invalid path")?,
            "--json",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(10));
    assert!(output.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(value["code"], "UNSUPPORTED_FORMAT");
    assert_eq!(value["severity"], "error");
    Ok(())
}
