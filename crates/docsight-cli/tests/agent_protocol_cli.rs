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

fn tables_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_tables.docx")
}

fn assert_ndjson_event(
    output: std::process::Output,
    event_type: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    assert_eq!(records.first().ok_or("missing meta")?["type"], "meta");
    assert!(records.iter().any(|record| record["type"] == event_type));
    assert_eq!(records.last().ok_or("missing done")?["type"], "done");
    Ok(())
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
    assert_eq!(first["schema"], "docsight.agent/v2");

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
fn ndjson_is_honored_by_single_result_commands() -> Result<(), Box<dyn std::error::Error>> {
    let headings = headings_fixture();
    let tables = tables_fixture();
    let headings_str = headings.to_str().ok_or("invalid headings path")?;
    let tables_str = tables.to_str().ok_or("invalid tables path")?;
    let heading_id = "h_515ad605791c12fc496c1c18d79f6526";
    let table_id = "tbl_6c22c8dd17adf7e32604b29241d9ab99";
    let temp_dir = tempfile::tempdir()?;

    assert_ndjson_event(
        docsight()
            .args(["--ndjson", "table", tables_str, table_id])
            .output()?,
        "table",
    )?;
    assert_ndjson_event(
        docsight()
            .args(["--ndjson", "fingerprint", headings_str])
            .output()?,
        "fingerprint",
    )?;
    assert_ndjson_event(
        docsight()
            .args(["--ndjson", "evidence", headings_str, heading_id])
            .output()?,
        "evidence",
    )?;
    assert_ndjson_event(
        docsight()
            .args([
                "--ndjson",
                "hit",
                headings_str,
                "--page",
                "1",
                "--point",
                "100,80",
            ])
            .output()?,
        "hit",
    )?;
    assert_ndjson_event(
        docsight()
            .args([
                "--ndjson",
                "render",
                headings_str,
                "--page",
                "1",
                "--out",
                temp_dir
                    .path()
                    .join("page.png")
                    .to_str()
                    .ok_or("render path")?,
            ])
            .output()?,
        "render",
    )?;
    assert_ndjson_event(
        docsight()
            .args([
                "--ndjson",
                "crop",
                headings_str,
                "--object",
                heading_id,
                "--out",
                temp_dir
                    .path()
                    .join("crop.png")
                    .to_str()
                    .ok_or("crop path")?,
            ])
            .output()?,
        "crop",
    )?;
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
    assert_eq!(val["limits"]["text_truncated"], true);
    for h in headings {
        let text = h["text"].as_str().ok_or("missing text field")?;
        assert!(text.chars().count() <= 4);
        assert_eq!(h["id"].as_str().ok_or("missing id")?.len(), 34);
        assert!(h["source"].as_str().ok_or("missing source")?.len() > 4);
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
fn agent_mode_normalizes_json_errors_capabilities_and_artifacts()
-> Result<(), Box<dyn std::error::Error>> {
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let inspect = docsight().args(["--agent", "inspect", path_str]).output()?;
    assert!(inspect.status.success());
    assert!(inspect.stderr.is_empty());
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_json["schema"], "docsight.agent/v2");
    assert_eq!(
        inspect_json["result"]["capability_details"]["render"]["available"],
        true
    );
    assert_eq!(
        inspect_json["result"]["capability_details"]["render"]["source_faithful"],
        false
    );

    let directory = tempfile::tempdir()?;
    let output_path = directory.path().join("page.png");
    let output_path_str = output_path.to_str().ok_or("invalid output path")?;
    let render = docsight()
        .args([
            "--agent",
            "render",
            path_str,
            "--page",
            "1",
            "--out",
            output_path_str,
        ])
        .output()?;
    assert!(render.status.success());
    assert!(render.stderr.is_empty());
    let render_json: serde_json::Value = serde_json::from_slice(&render.stdout)?;
    assert_eq!(render_json["result"]["output_path"], output_path_str);
    assert_eq!(
        render_json["result"]["output_bytes"].as_u64(),
        Some(std::fs::metadata(&output_path)?.len())
    );
    assert_eq!(
        render_json["result"]["output_sha256"]
            .as_str()
            .ok_or("missing output digest")?
            .len(),
        64
    );
    assert_eq!(render_json["result"]["media_type"], "image/png");

    let invalid = docsight()
        .args(["--agent", "inspect", "nonexistent_file_xyz.docx"])
        .output()?;
    assert_eq!(invalid.status.code(), Some(40));
    assert!(invalid.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr)?;
    assert_eq!(error["schema"], "docsight.agent/v2");
    assert_eq!(error["error"]["code"], "IO_ERROR");
    assert_eq!(error["error"]["exit_code"], 40);
    Ok(())
}

#[test]
fn capabilities_command_is_machine_discoverable() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight().args(["--agent", "capabilities"]).output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let repeat = docsight().args(["--agent", "capabilities"]).output()?;
    assert_eq!(output.stdout, repeat.stdout);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["schema"], "docsight.agent/v2");
    assert_eq!(value["result"]["profile"], "agent-first-v1");
    assert_eq!(value["result"]["invocation_prefix"], "docsight --agent");
    assert_eq!(value["result"]["sandbox"]["flag"], "--sandbox");
    assert_eq!(value["result"]["sandbox"]["agent_default"], false);
    assert_eq!(
        value["result"]["sandbox"]["recommended_for_untrusted_input"],
        true
    );
    assert_eq!(
        value["result"]["sandbox"]["supported_platforms"],
        serde_json::json!(["linux", "macos", "windows"])
    );
    assert_eq!(
        value["result"]["sandbox"]["enforced_controls"],
        serde_json::json!([
            "memory",
            "cpu",
            "network",
            "filesystem",
            "isolated_temp_directory",
            "bounded_output"
        ])
    );
    assert_eq!(
        value["result"]["sandbox"]["unsupported_platform_behavior"],
        "reject"
    );
    assert_eq!(value["result"]["sandbox"]["failure_mode"], "fail_closed");
    let commands = value["result"]["commands"]
        .as_array()
        .ok_or("missing commands")?;
    assert!(commands.iter().any(|command| command["name"] == "evidence"));
    assert!(commands.iter().any(|command| command["name"] == "hit"));
    assert!(commands.iter().all(|command| {
        command["invocation"]
            .as_str()
            .is_some_and(|invocation| !invocation.is_empty())
            && command["ndjson_events"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
    }));
    let diff = commands
        .iter()
        .find(|command| command["name"] == "diff")
        .ok_or("diff capability")?;
    assert_eq!(
        diff["result_schema"],
        "https://docsight.dev/schemas/v2/diff-result.json"
    );
    assert!(
        diff["ndjson_events"]
            .as_array()
            .ok_or("diff NDJSON events")?
            .iter()
            .any(|event| event == "diff.visual.page")
    );

    let ndjson = docsight()
        .args(["--agent", "--ndjson", "capabilities"])
        .output()?;
    assert!(ndjson.status.success());
    let records = ndjson
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    assert_eq!(records[0]["type"], "meta");
    assert_eq!(records[1]["type"], "capabilities");
    assert_eq!(records[2]["type"], "done");

    let schema_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v2/capabilities-result.json");
    let schema: serde_json::Value = serde_json::from_slice(&std::fs::read(schema_path)?)?;
    let required = schema["properties"]["commands"]["items"]["required"]
        .as_array()
        .ok_or("capability command required fields")?;
    for field in ["invocation", "ndjson_events", "result_schema"] {
        assert!(required.iter().any(|value| value == field));
    }
    Ok(())
}

#[test]
fn agent_mode_structures_cli_parse_errors() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight().args(["--agent", "unknown-command"]).output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["schema"], "docsight.agent/v2");
    assert_eq!(error["error"]["code"], "USAGE");
    assert_eq!(error["error"]["exit_code"], 2);
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
        assert!(value["limits"]["warnings_truncated"] == true);
        assert_eq!(value["limits"]["total_warnings"], 3);
    } else {
        assert_eq!(out.status.code(), Some(2));
    }
    Ok(())
}

#[test]
fn max_bytes_truncates_warnings_without_losing_single_result()
-> Result<(), Box<dyn std::error::Error>> {
    let docx_path = headings_fixture();
    let out = docsight()
        .args([
            "inspect",
            docx_path.to_str().ok_or("path")?,
            "--json",
            "--max-bytes",
            "1000",
        ])
        .output()?;
    assert!(out.status.success());
    assert!(out.stdout.len() <= 1000);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["limits"]["truncated"], false);
    assert_eq!(value["limits"]["warnings_truncated"], true);
    assert_eq!(value["limits"]["total_warnings"], 3);
    assert_eq!(value["limits"]["returned_warnings"], 1);
    Ok(())
}

#[test]
fn ndjson_reports_warning_truncation_separately() -> Result<(), Box<dyn std::error::Error>> {
    let docx_path = headings_fixture();
    let out = docsight()
        .args([
            "--ndjson",
            "inspect",
            docx_path.to_str().ok_or("path")?,
            "--max-bytes",
            "1000",
        ])
        .output()?;
    assert!(out.status.success());
    assert!(out.stdout.len() <= 1000);
    let records = out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    let done = records.last().ok_or("missing done")?;
    assert_eq!(done["type"], "done");
    assert_eq!(done["limits"]["truncated"], true);
    assert_eq!(done["limits"]["warnings_truncated"], true);
    assert_eq!(done["limits"]["total_warnings"], 3);
    assert_eq!(done["limits"]["returned_warnings"], 0);
    Ok(())
}
