#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

use std::process::Command;

fn duplicated_trailer(conflicting: bool) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = pdf_fixture::build_outline_pdf(
        "BT /F1 12 Tf 20 70 Td (Generic reading sample) Tj ET",
        "[0 0 300 100]",
    );
    let trailer = bytes
        .windows(7)
        .rposition(|window| window == b"trailer")
        .ok_or("trailer missing")?;
    let suffix = String::from_utf8(bytes.split_off(trailer))?.replace(
        "/Root 1 0 R",
        if conflicting {
            "/Root 1 0 R /Root 2 0 R"
        } else {
            "/Root 1 0 R /Root 1 0 R"
        },
    );
    bytes.extend_from_slice(suffix.as_bytes());
    Ok(bytes)
}

#[test]
fn identical_dictionary_entries_are_readable_and_diagnosed_in_agent_and_sandbox_modes()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("sample.pdf");
    std::fs::write(&input, duplicated_trailer(false)?)?;
    let mut previous = None;
    for sandbox in [false, true, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_docsight"));
        command.arg("--agent");
        if sandbox {
            command.arg("--sandbox");
        }
        let output = command.arg("inspect").arg(&input).output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["result"]["pages"], 1);
        let warnings = value["warnings"].as_array().ok_or("warnings missing")?;
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| warning["code"] == "PDF_DICTIONARY_DUPLICATE_NORMALIZED")
                .count(),
            1
        );
        if let Some(previous) = &previous {
            assert_eq!(previous, &output.stdout);
        }
        previous = Some(output.stdout);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .args(["--agent", "--sandbox", "text"])
        .arg(&input)
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        value["result"]["blocks"]
            .as_array()
            .ok_or("blocks missing")?
            .iter()
            .any(|block| block["text"] == "Generic reading sample")
    );
    let png = directory.path().join("page.png");
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .args(["--agent", "--sandbox", "render"])
        .arg(&input)
        .args(["--page", "1", "--out"])
        .arg(&png)
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty() && png.is_file());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        value["warnings"]
            .as_array()
            .ok_or("warnings missing")?
            .iter()
            .any(|warning| warning["code"] == "PDF_DICTIONARY_DUPLICATE_NORMALIZED")
    );
    Ok(())
}

#[test]
fn conflicting_dictionary_entries_return_a_typed_error_without_an_artifact()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("conflicting.pdf");
    let png = directory.path().join("page.png");
    std::fs::write(&input, duplicated_trailer(true)?)?;
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .args(["--agent", "--sandbox", "render"])
        .arg(&input)
        .args(["--page", "1", "--out"])
        .arg(&png)
        .output()?;
    assert_eq!(output.status.code(), Some(11));
    assert!(output.stdout.is_empty() && !png.exists());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "MALFORMED_DOCUMENT");
    assert!(error["error"]["location"]["offset"].as_u64().is_some());
    Ok(())
}
