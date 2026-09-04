use std::path::PathBuf;
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn sample_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_features.docx")
}

fn headings_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_headings.docx")
}

#[test]
fn ndjson_streaming_emits_valid_sequence_and_events() -> Result<(), Box<dyn std::error::Error>> {
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let output = docsight()
        .args(["--ndjson", "outline", path_str])
        .output()?;
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 3);

    let first: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(first["seq"], 1);
    assert_eq!(first["type"], "meta");
    assert_eq!(first["schema"], "docsight.agent/v1");

    let mut prev_seq = 1;
    for line in &lines[1..lines.len() - 1] {
        let val: serde_json::Value = serde_json::from_str(line)?;
        let seq = val["seq"].as_u64().ok_or("missing seq")?;
        assert_eq!(seq, prev_seq + 1);
        prev_seq = seq;
        let event_type = val["type"].as_str().ok_or("missing type")?;
        assert!(event_type == "heading" || event_type == "warning");
    }

    let last_line = lines.last().ok_or("empty lines")?;
    let last: serde_json::Value = serde_json::from_str(last_line)?;
    assert_eq!(last["seq"], prev_seq + 1);
    assert_eq!(last["type"], "done");
    assert_eq!(last["limits"]["truncated"], false);

    Ok(())
}

#[test]
fn max_items_and_continuation_token_paging() -> Result<(), Box<dyn std::error::Error>> {
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let first_page = docsight()
        .args(["outline", path_str, "--json", "--max-items", "1"])
        .output()?;
    assert!(first_page.status.success());

    let val1: serde_json::Value = serde_json::from_slice(&first_page.stdout)?;
    assert_eq!(val1["limits"]["truncated"], true);
    let token = val1["limits"]["continuation_token"]
        .as_str()
        .ok_or("missing continuation_token")?;
    let headings1 = val1["result"]["headings"]
        .as_array()
        .ok_or("missing headings array")?;
    assert_eq!(headings1.len(), 1);

    let second_page = docsight()
        .args([
            "outline",
            path_str,
            "--json",
            "--continue",
            token,
            "--max-items",
            "1",
        ])
        .output()?;
    assert!(second_page.status.success());

    let val2: serde_json::Value = serde_json::from_slice(&second_page.stdout)?;
    let headings2 = val2["result"]["headings"]
        .as_array()
        .ok_or("missing headings array")?;
    assert_eq!(headings2.len(), 1);
    assert_ne!(headings1[0]["text"], headings2[0]["text"]);

    Ok(())
}

#[test]
fn continuation_token_fails_on_command_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let output = docsight()
        .args(["outline", path_str, "--json", "--max-items", "1"])
        .output()?;
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let token = val["limits"]["continuation_token"]
        .as_str()
        .ok_or("missing continuation_token")?;

    let invalid = docsight()
        .args(["tables", path_str, "--json", "--continue", token])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    Ok(())
}

#[test]
fn continuation_token_fails_on_document_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let doc1 = headings_fixture();
    let doc2 = sample_fixture();
    let doc1_str = doc1.to_str().ok_or("invalid path doc1")?;
    let doc2_str = doc2.to_str().ok_or("invalid path doc2")?;
    let output = docsight()
        .args(["outline", doc1_str, "--json", "--max-items", "1"])
        .output()?;
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let token = val["limits"]["continuation_token"]
        .as_str()
        .ok_or("missing continuation_token")?;

    let invalid = docsight()
        .args(["outline", doc2_str, "--json", "--continue", token])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    Ok(())
}

#[test]
fn text_limit_truncates_long_strings() -> Result<(), Box<dyn std::error::Error>> {
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let output = docsight()
        .args(["outline", path_str, "--json", "--text-limit", "4"])
        .output()?;
    assert!(output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let headings = val["result"]["headings"]
        .as_array()
        .ok_or("missing headings array")?;
    for h in headings {
        let text = h["text"].as_str().ok_or("missing text field")?;
        assert!(text.chars().count() <= 4);
    }
    Ok(())
}

#[test]
fn field_projection_select_filters_properties() -> Result<(), Box<dyn std::error::Error>> {
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let output = docsight()
        .args(["outline", path_str, "--json", "--select", "text,level"])
        .output()?;
    assert!(output.status.success());
    let val: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let headings = val["result"]["headings"]
        .as_array()
        .ok_or("missing headings array")?;
    assert!(!headings.is_empty());
    for h in headings {
        assert!(h.get("text").is_some());
        assert!(h.get("level").is_some());
        assert!(h.get("id").is_none());
        assert!(h.get("source").is_none());
    }
    Ok(())
}

#[test]
fn quiet_suppresses_stderr_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let output = docsight().args(["--quiet", "inspect", path_str]).output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn json_errors_emits_structured_diagnostic() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight()
        .args(["--json-errors", "inspect", "nonexistent_file_xyz.docx"])
        .output()?;
    assert_eq!(output.status.code(), Some(40));
    let diag: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(diag["code"], "IO_ERROR");
    assert_eq!(diag["severity"], "error");
    assert!(diag["message"].is_string());
    assert!(diag["effect"].is_string());
    Ok(())
}

#[test]
fn byte_for_byte_deterministic_agent_output() -> Result<(), Box<dyn std::error::Error>> {
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let run1 = docsight().args(["outline", path_str, "--json"]).output()?;
    let run2 = docsight().args(["outline", path_str, "--json"]).output()?;
    assert_eq!(run1.stdout, run2.stdout);

    let ndjson1 = docsight()
        .args(["--ndjson", "outline", path_str])
        .output()?;
    let ndjson2 = docsight()
        .args(["--ndjson", "outline", path_str])
        .output()?;
    assert_eq!(ndjson1.stdout, ndjson2.stdout);

    Ok(())
}

#[test]
fn select_with_unknown_field_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let docx_path = headings_fixture();
    let out = docsight()
        .args([
            "inspect",
            docx_path.to_str().ok_or("path")?,
            "--json",
            "--select",
            "nonexistent_field",
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains("nonexistent_field"));
    assert!(out.stdout.is_empty());
    Ok(())
}

#[test]
fn max_bytes_below_envelope_floor_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let docx_path = headings_fixture();
    let out = docsight()
        .args([
            "inspect",
            docx_path.to_str().ok_or("path")?,
            "--json",
            "--max-bytes",
            "64",
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains("--max-bytes"));
    assert!(out.stdout.is_empty());
    Ok(())
}

#[test]
fn max_bytes_never_emits_oversized_single_item() -> Result<(), Box<dyn std::error::Error>> {
    let docx_path = headings_fixture();
    let out = docsight()
        .args([
            "text",
            docx_path.to_str().ok_or("path")?,
            "--json",
            "--max-bytes",
            "700",
        ])
        .output()?;
    if out.status.success() {
        assert!(out.stdout.len() <= 700, "stdout exceeded the hard cap");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert!(value["limits"]["truncated"] == true);
    } else {
        assert_eq!(out.status.code(), Some(2));
    }
    Ok(())
}
